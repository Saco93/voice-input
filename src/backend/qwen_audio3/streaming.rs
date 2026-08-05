use std::{
    net::{TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tungstenite::{
    Message, WebSocket,
    client::IntoClientRequest,
    client_tls_with_config,
    http::{HeaderValue, header::AUTHORIZATION},
    protocol::WebSocketConfig,
    stream::MaybeTlsStream,
};

use crate::{
    backend::{ASR_CONTROL_QUEUE_CAPACITY, AsrControl, AsrEvent, AsrSessionHandle, AudioSpec},
    config::{Audio3VocabularyTerm, Config, Language},
    diagnostics::{FailureKind, ProviderErrorCode},
};

type Audio3Socket = WebSocket<MaybeTlsStream<TcpStream>>;
type HandshakeResponse = tungstenite::http::Response<Option<Vec<u8>>>;
type Connection = (Audio3Socket, HandshakeResponse);

type SocketResult<T> = std::result::Result<T, Box<tungstenite::Error>>;

trait SocketIo {
    fn send_message(&mut self, message: Message) -> SocketResult<()>;
    fn read_message(&mut self) -> SocketResult<Message>;
    fn close_socket(&mut self) -> SocketResult<()>;
}

impl SocketIo for Audio3Socket {
    fn send_message(&mut self, message: Message) -> SocketResult<()> {
        self.send(message).map_err(Box::new)
    }

    fn read_message(&mut self) -> SocketResult<Message> {
        self.read().map_err(Box::new)
    }

    fn close_socket(&mut self) -> SocketResult<()> {
        self.close(None).map_err(Box::new)
    }
}

trait DeadlineClock {
    type Deadline: Copy;

    fn deadline_after(&self, duration: Duration) -> Self::Deadline;
    fn is_expired(&self, deadline: Self::Deadline) -> bool;
}

#[derive(Clone, Copy)]
struct ProductionClock;

impl DeadlineClock for ProductionClock {
    type Deadline = Instant;

    fn deadline_after(&self, duration: Duration) -> Self::Deadline {
        Instant::now() + duration
    }

    fn is_expired(&self, deadline: Self::Deadline) -> bool {
        Instant::now() > deadline
    }
}

const MAX_CONTROLS_PER_TICK: usize = 8;
const MAX_SERVER_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024;
static TASK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(super) fn spawn_session(config: &Config, spec: AudioSpec) -> Result<AsrSessionHandle> {
    let (control_tx, control_rx) = mpsc::sync_channel(ASR_CONTROL_QUEUE_CAPACITY);
    let (event_tx, event_rx) = mpsc::channel();
    let abort_flag = Arc::new(AtomicBool::new(false));
    let worker_abort_flag = abort_flag.clone();
    let config = config.clone();

    let join =
        thread::spawn(move || run_session(config, spec, control_rx, worker_abort_flag, event_tx));

    Ok(AsrSessionHandle {
        control_tx,
        abort_flag,
        event_rx,
        join,
    })
}

fn run_session(
    config: Config,
    spec: AudioSpec,
    control_rx: mpsc::Receiver<AsrControl>,
    abort_flag: Arc<AtomicBool>,
    event_tx: mpsc::Sender<AsrEvent>,
) -> Result<()> {
    let audio3 = &config.asr.alibaba_audio3;
    if !audio3.experimental_enabled {
        bail!("experimental Qwen-Audio-3 ASR is not enabled");
    }
    if audio3.api_key.trim().is_empty() {
        bail!("experimental Qwen-Audio-3 ASR requires an API key");
    }

    let task_id = new_task_id();
    let connect_timeout = Duration::from_millis(config.asr.connect_timeout_ms);
    let clock = ProductionClock;
    // Preserve the existing startup budget: connection establishment consumes
    // time from the same deadline as the task-started wait.
    let connect_deadline = clock.deadline_after(connect_timeout);
    let mut socket = open_socket(&audio3.endpoint, &audio3.api_key, connect_timeout)?;
    configure_socket(socket.get_mut())?;
    run_established_socket(
        &mut socket,
        &clock,
        EstablishedSession {
            config: &config,
            spec,
            task_id: &task_id,
            startup_deadline: connect_deadline,
            control_rx,
            abort_flag: &abort_flag,
            event_tx: &event_tx,
        },
    )
}

struct EstablishedSession<'a, D> {
    config: &'a Config,
    spec: AudioSpec,
    task_id: &'a str,
    startup_deadline: D,
    control_rx: mpsc::Receiver<AsrControl>,
    abort_flag: &'a AtomicBool,
    event_tx: &'a mpsc::Sender<AsrEvent>,
}

fn run_established_socket<S: SocketIo, C: DeadlineClock>(
    socket: &mut S,
    clock: &C,
    session: EstablishedSession<'_, C::Deadline>,
) -> Result<()> {
    let EstablishedSession {
        config,
        spec,
        task_id,
        startup_deadline,
        control_rx,
        abort_flag,
        event_tx,
    } = session;
    let audio3 = &config.asr.alibaba_audio3;
    send_json(
        socket,
        run_task_envelope(
            task_id,
            &audio3.model,
            spec,
            Audio3RequestControls {
                language: config.asr.language,
                language_hints_enabled: audio3.language_hints_enabled,
                heartbeat_enabled: audio3.heartbeat_enabled,
                max_sentence_silence_ms: audio3.max_sentence_silence_ms,
                semantic_punctuation_enabled: audio3.semantic_punctuation_enabled,
                vocabulary: &audio3.vocabulary,
            },
        ),
    )?;
    await_task_started(
        socket,
        clock,
        task_id,
        startup_deadline,
        abort_flag,
        event_tx,
    )?;

    if abort_flag.load(Ordering::SeqCst) {
        let _ = socket.close_socket();
        return Ok(());
    }
    let _ = event_tx.send(AsrEvent::Ready);

    let mut finish_sent = false;
    let mut finalize_deadline = None;
    let mut assembler = TranscriptAssembler::default();
    let mut audio_packet_count = 0_u64;
    let mut audio_sample_count = 0_u64;
    let mut max_audio_queue_delay_ms = 0_u64;
    let mut last_audio_queue_delay_ms = 0_u64;

    loop {
        if abort_flag.load(Ordering::SeqCst) {
            let _ = socket.close_socket();
            return Ok(());
        }

        if !finish_sent {
            for _ in 0..MAX_CONTROLS_PER_TICK {
                match control_rx.try_recv() {
                    Ok(AsrControl::AppendPcm16 {
                        samples,
                        enqueued_at,
                    }) => {
                        let queue_delay_ms = duration_ms(enqueued_at.elapsed());
                        socket
                            .send_message(Message::Binary(pcm16_le_bytes(&samples)))
                            .context("failed to send Qwen-Audio-3 PCM")?;
                        audio_packet_count = audio_packet_count.saturating_add(1);
                        audio_sample_count = audio_sample_count
                            .saturating_add(u64::try_from(samples.len()).unwrap_or(u64::MAX));
                        max_audio_queue_delay_ms = max_audio_queue_delay_ms.max(queue_delay_ms);
                        last_audio_queue_delay_ms = queue_delay_ms;
                    }
                    Ok(AsrControl::Finish) => {
                        send_json(socket, finish_task_envelope(task_id))?;
                        let _ = event_tx.send(AsrEvent::AudioDeliveryCompleted {
                            packet_count: audio_packet_count,
                            sample_count: audio_sample_count,
                            max_queue_delay_ms: max_audio_queue_delay_ms,
                            last_queue_delay_ms: last_audio_queue_delay_ms,
                        });
                        finish_sent = true;
                        finalize_deadline =
                            Some(clock.deadline_after(Duration::from_millis(
                                config.asr.finalize_timeout_ms,
                            )));
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        bail!("Qwen-Audio-3 control channel disconnected before finish")
                    }
                }
            }
        }

        match socket.read_message() {
            Ok(Message::Close(_)) => {
                bail!("Qwen-Audio-3 websocket closed before task completion")
            }
            Ok(message) => {
                if let Some(event) = parse_server_event(message, task_id)? {
                    match event {
                        ServerEvent::TaskStarted => {
                            bail!("Qwen-Audio-3 server sent duplicate task-started event")
                        }
                        ServerEvent::ResultGenerated {
                            text,
                            sentence_final,
                        } => {
                            if sentence_final {
                                let text = assembler.apply_segment_final(text);
                                if !text.is_empty() {
                                    let _ = event_tx.send(AsrEvent::SegmentFinal { text });
                                }
                            } else {
                                let (committed, unstable) = assembler.apply_partial(text);
                                let _ = event_tx.send(AsrEvent::Partial {
                                    committed,
                                    unstable,
                                });
                            }
                        }
                        ServerEvent::TaskFinished { text } => {
                            let final_text = assembler.finish(text);
                            if !final_text.is_empty() {
                                let _ = event_tx.send(AsrEvent::Final { text: final_text });
                            }
                            let _ = event_tx.send(AsrEvent::Finished);
                            let _ = socket.close_socket();
                            return Ok(());
                        }
                        ServerEvent::TaskFailed {
                            kind,
                            provider_error_code,
                        } => {
                            return report_task_failure(event_tx, kind, provider_error_code);
                        }
                    }
                }
            }
            Err(error) if socket_error_is_retryable(&error) => {}
            Err(error)
                if matches!(
                    error.as_ref(),
                    tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed
                ) =>
            {
                bail!("Qwen-Audio-3 connection closed before task completion")
            }
            Err(error) => return Err(error).context("failed to read Qwen-Audio-3 websocket"),
        }

        if finalize_deadline.is_some_and(|deadline| clock.is_expired(deadline)) {
            let _ = socket.close_socket();
            bail!("Qwen-Audio-3 finalization timed out");
        }
    }
}

fn await_task_started<S: SocketIo, C: DeadlineClock>(
    socket: &mut S,
    clock: &C,
    task_id: &str,
    deadline: C::Deadline,
    abort_flag: &AtomicBool,
    event_tx: &mpsc::Sender<AsrEvent>,
) -> Result<()> {
    loop {
        if abort_flag.load(Ordering::SeqCst) {
            let _ = socket.close_socket();
            return Ok(());
        }
        if clock.is_expired(deadline) {
            let _ = socket.close_socket();
            bail!("Qwen-Audio-3 task-started timed out");
        }

        match socket.read_message() {
            Ok(Message::Close(_)) => {
                bail!("Qwen-Audio-3 websocket closed before task-started")
            }
            Ok(message) => match parse_server_event(message, task_id)? {
                Some(ServerEvent::TaskStarted) => return Ok(()),
                Some(ServerEvent::TaskFailed {
                    kind,
                    provider_error_code,
                }) => {
                    return report_task_failure(event_tx, kind, provider_error_code);
                }
                Some(_) => bail!("Qwen-Audio-3 server event arrived before task-started"),
                None => {}
            },
            Err(error) if socket_error_is_retryable(&error) => {}
            Err(error)
                if matches!(
                    error.as_ref(),
                    tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed
                ) =>
            {
                bail!("Qwen-Audio-3 connection closed before task-started")
            }
            Err(error) => return Err(error).context("failed to read Qwen-Audio-3 websocket"),
        }
    }
}

fn socket_error_is_retryable(error: &tungstenite::Error) -> bool {
    matches!(
        error,
        tungstenite::Error::Io(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.kind() == std::io::ErrorKind::TimedOut
    )
}

fn report_task_failure(
    event_tx: &mpsc::Sender<AsrEvent>,
    kind: FailureKind,
    provider_error_code: Option<ProviderErrorCode>,
) -> Result<()> {
    let _ = event_tx.send(AsrEvent::TaskFailed {
        kind,
        provider_error_code,
    });
    bail!("Qwen-Audio-3 streaming ASR failed ({})", kind.as_str())
}

fn open_socket(endpoint: &str, api_key: &str, timeout: Duration) -> Result<Audio3Socket> {
    let request = websocket_request(endpoint, api_key)?;
    let websocket_config = WebSocketConfig {
        max_message_size: Some(MAX_SERVER_MESSAGE_BYTES),
        max_frame_size: Some(MAX_SERVER_MESSAGE_BYTES),
        ..WebSocketConfig::default()
    };
    let (socket, _) = connect_with_timeout(request, timeout, websocket_config)?;
    Ok(socket)
}

fn websocket_request(endpoint: &str, api_key: &str) -> Result<tungstenite::http::Request<()>> {
    let mut request = endpoint
        .into_client_request()
        .context("failed to build Qwen-Audio-3 websocket request")?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))
            .context("Alibaba API key contains an invalid header value")?,
    );
    Ok(request)
}

fn connect_with_timeout(
    request: tungstenite::http::Request<()>,
    timeout: Duration,
    websocket_config: WebSocketConfig,
) -> Result<Connection> {
    let uri = request.uri();
    let host = uri
        .host()
        .ok_or_else(|| anyhow!("ASR endpoint is missing a host"))?;
    let host = if host.starts_with('[') && host.ends_with(']') {
        &host[1..host.len() - 1]
    } else {
        host
    };
    let port = uri.port_u16().unwrap_or_else(|| {
        if uri.scheme_str() == Some("ws") {
            80
        } else {
            443
        }
    });
    let address = (host, port)
        .to_socket_addrs()
        .context("failed to resolve Qwen-Audio-3 endpoint")?
        .next()
        .ok_or_else(|| anyhow!("Qwen-Audio-3 endpoint resolved to no addresses"))?;
    let stream = TcpStream::connect_timeout(&address, timeout)
        .context("failed to connect to Qwen-Audio-3 endpoint")?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    match client_tls_with_config(request, stream, Some(websocket_config), None) {
        Ok(value) => Ok(value),
        Err(tungstenite::HandshakeError::Failure(error)) => Err(error.into()),
        Err(tungstenite::HandshakeError::Interrupted(_)) => {
            bail!("unexpected interrupted Qwen-Audio-3 websocket handshake")
        }
    }
}

fn configure_socket(stream: &mut MaybeTlsStream<TcpStream>) -> Result<()> {
    let read_timeout = Some(Duration::from_millis(50));
    let write_timeout = Some(Duration::from_secs(5));
    match stream {
        MaybeTlsStream::Plain(tcp) => {
            tcp.set_read_timeout(read_timeout)?;
            tcp.set_write_timeout(write_timeout)?;
        }
        MaybeTlsStream::Rustls(tls) => {
            let tcp = tls.get_mut();
            tcp.set_read_timeout(read_timeout)?;
            tcp.set_write_timeout(write_timeout)?;
        }
        _ => {}
    }
    Ok(())
}

fn send_json(socket: &mut impl SocketIo, payload: Value) -> Result<()> {
    socket
        .send_message(Message::Text(payload.to_string()))
        .context("failed to send Qwen-Audio-3 websocket event")
}

struct Audio3RequestControls<'a> {
    language: Language,
    language_hints_enabled: bool,
    heartbeat_enabled: bool,
    max_sentence_silence_ms: u32,
    semantic_punctuation_enabled: bool,
    vocabulary: &'a [Audio3VocabularyTerm],
}

fn run_task_envelope(
    task_id: &str,
    model: &str,
    spec: AudioSpec,
    controls: Audio3RequestControls<'_>,
) -> Value {
    let Audio3RequestControls {
        language,
        language_hints_enabled,
        heartbeat_enabled,
        max_sentence_silence_ms,
        semantic_punctuation_enabled,
        vocabulary,
    } = controls;
    let mut envelope = json!({
        "header": {
            "action": "run-task",
            "task_id": task_id,
            "streaming": "duplex"
        },
        "payload": {
            "task_group": "audio",
            "task": "asr",
            "function": "recognition",
            "model": model,
            "input": {},
            "parameters": {
                "format": "pcm",
                "sample_rate": spec.sample_rate_hz,
                "heartbeat": heartbeat_enabled,
                "max_sentence_silence": max_sentence_silence_ms,
                "semantic_punctuation_enabled": semantic_punctuation_enabled
            }
        }
    });
    if language_hints_enabled {
        envelope["payload"]["parameters"]["language_hints"] = super::language_hints(language);
    }
    if let Some(vocabulary) = super::vocabulary_value(vocabulary) {
        envelope["payload"]["parameters"]["vocabulary"] = vocabulary;
    }
    envelope
}

fn finish_task_envelope(task_id: &str) -> Value {
    json!({
        "header": {
            "action": "finish-task",
            "task_id": task_id,
            "streaming": "duplex"
        },
        "payload": {
            "input": {}
        }
    })
}

fn pcm16_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn new_task_id() -> String {
    let sequence = u128::from(TASK_SEQUENCE.fetch_add(1, Ordering::Relaxed));
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let process = u128::from(std::process::id()) << 64;
    let value = timestamp ^ process ^ sequence;
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[derive(Debug, PartialEq, Eq)]
enum ServerEvent {
    TaskStarted,
    ResultGenerated {
        text: String,
        sentence_final: bool,
    },
    TaskFinished {
        text: Option<String>,
    },
    TaskFailed {
        kind: FailureKind,
        provider_error_code: Option<ProviderErrorCode>,
    },
}

fn parse_server_event(message: Message, expected_task_id: &str) -> Result<Option<ServerEvent>> {
    let Message::Text(text) = message else {
        return Ok(None);
    };
    if text.len() > MAX_SERVER_MESSAGE_BYTES {
        bail!("Qwen-Audio-3 server event exceeds the size limit");
    }
    let payload: Value =
        serde_json::from_str(&text).context("failed to parse Qwen-Audio-3 server JSON")?;
    let header = payload
        .get("header")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("Qwen-Audio-3 server event is missing header"))?;
    let event = header
        .get("event")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Qwen-Audio-3 server event is missing header.event"))?;
    let task_id = header
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Qwen-Audio-3 server event is missing header.task_id"))?;
    if task_id != expected_task_id {
        bail!("Qwen-Audio-3 server event task ID does not match the active task");
    }

    match event {
        "task-started" => Ok(Some(ServerEvent::TaskStarted)),
        "result-generated" => {
            if payload
                .pointer("/payload/output/sentence/heartbeat")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(None);
            }
            let (text, sentence_final) = extract_result(&payload)?;
            Ok(Some(ServerEvent::ResultGenerated {
                text,
                sentence_final,
            }))
        }
        "task-finished" => Ok(Some(ServerEvent::TaskFinished {
            text: extract_optional_text(&payload),
        })),
        "task-failed" => Ok(Some(ServerEvent::TaskFailed {
            kind: provider_failure_kind(&payload),
            provider_error_code: provider_error_code(&payload),
        })),
        _ => Ok(None),
    }
}

fn extract_result(payload: &Value) -> Result<(String, bool)> {
    let output = payload
        .pointer("/payload/output")
        .ok_or_else(|| anyhow!("Qwen-Audio-3 result is missing payload.output"))?;
    if let Some(sentence) = output.get("sentence") {
        let text = sentence
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Qwen-Audio-3 result sentence is missing text"))?;
        let sentence_final = sentence
            .get("sentence_end")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return Ok((
            bounded(text, MAX_TRANSCRIPT_BYTES).to_string(),
            sentence_final,
        ));
    }
    let text = output
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Qwen-Audio-3 result is missing sentence/text"))?;
    Ok((bounded(text, MAX_TRANSCRIPT_BYTES).to_string(), false))
}

fn extract_optional_text(payload: &Value) -> Option<String> {
    let output = payload.pointer("/payload/output")?;
    let text = output
        .pointer("/sentence/text")
        .or_else(|| output.get("text"))?
        .as_str()?;
    Some(bounded(text, MAX_TRANSCRIPT_BYTES).to_string())
}

fn provider_error_code(payload: &Value) -> Option<ProviderErrorCode> {
    payload
        .pointer("/header/error_code")
        .or_else(|| payload.pointer("/payload/code"))
        .or_else(|| payload.pointer("/payload/error/code"))
        .and_then(Value::as_str)
        .and_then(ProviderErrorCode::try_new)
}

fn provider_failure_kind(payload: &Value) -> FailureKind {
    let code = payload
        .pointer("/header/error_code")
        .or_else(|| payload.pointer("/payload/code"))
        .or_else(|| payload.pointer("/payload/error/code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    if code.contains("timeout") {
        FailureKind::Timeout
    } else if code.contains("throttl") || code.contains("rate") || code.contains("quota") {
        FailureKind::RateLimited
    } else if code.contains("auth") || code.contains("api_key") || code.contains("unauthorized") {
        FailureKind::Authentication
    } else if code.contains("permission") || code.contains("forbidden") {
        FailureKind::PermissionDenied
    } else {
        FailureKind::Service
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn bounded(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= maximum_bytes)
        .last()
        .unwrap_or(0);
    &value[..boundary]
}

#[derive(Default)]
struct TranscriptAssembler {
    committed: String,
    unstable: String,
}

impl TranscriptAssembler {
    fn apply_partial(&mut self, text: String) -> (String, String) {
        let remaining = MAX_TRANSCRIPT_BYTES.saturating_sub(self.committed.len());
        self.unstable = bounded(&text, remaining).to_string();
        (self.committed.clone(), self.unstable.clone())
    }

    fn apply_segment_final(&mut self, text: String) -> String {
        append_transcript_piece(&mut self.committed, &text);
        self.unstable.clear();
        self.committed.clone()
    }

    fn finish(&mut self, authoritative_text: Option<String>) -> String {
        if let Some(text) = authoritative_text.filter(|text| !text.is_empty()) {
            return text;
        }
        let unstable = std::mem::take(&mut self.unstable);
        append_transcript_piece(&mut self.committed, &unstable);
        self.committed.clone()
    }
}

fn append_transcript_piece(joined: &mut String, part: &str) {
    if part.is_empty() || joined.len() >= MAX_TRANSCRIPT_BYTES {
        return;
    }
    let needs_space = joined
        .chars()
        .next_back()
        .zip(part.chars().next())
        .is_some_and(|(left, right)| left.is_ascii_alphanumeric() && right.is_ascii_alphanumeric());
    if needs_space && joined.len() < MAX_TRANSCRIPT_BYTES {
        joined.push(' ');
    }
    let remaining = MAX_TRANSCRIPT_BYTES.saturating_sub(joined.len());
    joined.push_str(bounded(part, remaining));
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tungstenite::Message;

    use std::{
        io,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    use crate::{
        backend::AsrEvent,
        config::{Audio3VocabularyTerm, Language},
        diagnostics::{FailureKind, ProviderErrorCode},
    };

    use super::{
        Audio3RequestControls, AudioSpec, DeadlineClock, EstablishedSession, ServerEvent, SocketIo,
        SocketResult, TranscriptAssembler, finish_task_envelope, new_task_id, parse_server_event,
        pcm16_le_bytes, report_task_failure, run_established_socket, run_task_envelope,
        websocket_request,
    };

    const TASK_ID: &str = "0123456789abcdef0123456789abcdef";
    const HEARTBEAT_TEXT_SENTINEL: &str = "heartbeat-must-not-become-transcript";
    const LONG_IDLE_SECONDS: usize = 65;
    const SAMPLES_PER_PACKET: usize = 16_000;
    const STARTUP_DEADLINE_MS: u64 = 10_000;
    const FINALIZE_DEADLINE_MS: u64 = 8_000;

    #[derive(Clone, Default)]
    struct ManualClock(Arc<AtomicU64>);

    impl ManualClock {
        fn advance(&self, duration: Duration) {
            self.0.fetch_add(
                u64::try_from(duration.as_millis()).unwrap(),
                Ordering::SeqCst,
            );
        }
    }

    impl DeadlineClock for ManualClock {
        type Deadline = u64;

        fn deadline_after(&self, duration: Duration) -> Self::Deadline {
            self.0
                .load(Ordering::SeqCst)
                .saturating_add(u64::try_from(duration.as_millis()).unwrap())
        }

        fn is_expired(&self, deadline: Self::Deadline) -> bool {
            self.0.load(Ordering::SeqCst) > deadline
        }
    }

    enum ScriptRead {
        Message(Message),
        WouldBlock,
    }

    #[derive(Debug)]
    enum SocketCheckpoint {
        Sent(Message),
        ReadWaiting,
        Closed,
    }

    struct ScriptedSocket {
        read_rx: mpsc::Receiver<ScriptRead>,
        checkpoint_tx: mpsc::Sender<SocketCheckpoint>,
    }

    impl SocketIo for ScriptedSocket {
        fn send_message(&mut self, message: Message) -> SocketResult<()> {
            self.checkpoint_tx
                .send(SocketCheckpoint::Sent(message))
                .expect("test checkpoint receiver dropped");
            Ok(())
        }

        fn read_message(&mut self) -> SocketResult<Message> {
            self.checkpoint_tx
                .send(SocketCheckpoint::ReadWaiting)
                .expect("test checkpoint receiver dropped");
            match self.read_rx.recv().expect("test script sender dropped") {
                ScriptRead::Message(message) => Ok(message),
                ScriptRead::WouldBlock => Err(Box::new(tungstenite::Error::Io(io::Error::from(
                    io::ErrorKind::WouldBlock,
                )))),
            }
        }

        fn close_socket(&mut self) -> SocketResult<()> {
            self.checkpoint_tx
                .send(SocketCheckpoint::Closed)
                .expect("test checkpoint receiver dropped");
            Ok(())
        }
    }

    fn provider_event(event: &str, payload: serde_json::Value) -> Message {
        Message::Text(
            json!({
                "header": {"event": event, "task_id": TASK_ID},
                "payload": payload,
            })
            .to_string(),
        )
    }

    fn heartbeat_event() -> Message {
        provider_event(
            "result-generated",
            json!({"output": {"sentence": {
                "text": HEARTBEAT_TEXT_SENTINEL,
                "heartbeat": true,
                "sentence_end": false,
            }}}),
        )
    }

    fn expect_sent(checkpoint_rx: &mpsc::Receiver<SocketCheckpoint>) -> Message {
        match checkpoint_rx.recv().unwrap() {
            SocketCheckpoint::Sent(message) => message,
            checkpoint => panic!("expected sent message, got {checkpoint:?}"),
        }
    }

    fn expect_read_waiting(checkpoint_rx: &mpsc::Receiver<SocketCheckpoint>) {
        assert!(matches!(
            checkpoint_rx.recv().unwrap(),
            SocketCheckpoint::ReadWaiting
        ));
    }

    fn expect_closed(checkpoint_rx: &mpsc::Receiver<SocketCheckpoint>) {
        assert!(matches!(
            checkpoint_rx.recv().unwrap(),
            SocketCheckpoint::Closed
        ));
    }

    fn assert_run_task_heartbeat(message: Message) {
        let Message::Text(text) = message else {
            panic!("run-task must be text");
        };
        let payload: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(payload["header"]["action"], "run-task");
        assert_eq!(payload["payload"]["parameters"]["heartbeat"], true);
    }

    type ScriptedLifecycle = (
        mpsc::SyncSender<crate::backend::AsrControl>,
        mpsc::Sender<ScriptRead>,
        mpsc::Receiver<SocketCheckpoint>,
        mpsc::Receiver<AsrEvent>,
        thread::JoinHandle<anyhow::Result<()>>,
    );

    fn spawn_scripted_lifecycle(
        clock: ManualClock,
        abort_flag: Arc<AtomicBool>,
    ) -> ScriptedLifecycle {
        let mut config = crate::config::Config::default();
        config.asr.alibaba_audio3.heartbeat_enabled = true;
        config.asr.finalize_timeout_ms = FINALIZE_DEADLINE_MS;
        let (control_tx, control_rx) = mpsc::sync_channel(1);
        let (read_tx, read_rx) = mpsc::channel();
        let (checkpoint_tx, checkpoint_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let deadline = clock.deadline_after(Duration::from_millis(STARTUP_DEADLINE_MS));
        let join = thread::spawn(move || {
            let mut socket = ScriptedSocket {
                read_rx,
                checkpoint_tx,
            };
            run_established_socket(
                &mut socket,
                &clock,
                EstablishedSession {
                    config: &config,
                    spec: AudioSpec {
                        sample_rate_hz: 16_000,
                    },
                    task_id: TASK_ID,
                    startup_deadline: deadline,
                    control_rx,
                    abort_flag: &abort_flag,
                    event_tx: &event_tx,
                },
            )
        });
        (control_tx, read_tx, checkpoint_rx, event_rx, join)
    }

    fn start_scripted_lifecycle(
        read_tx: &mpsc::Sender<ScriptRead>,
        checkpoint_rx: &mpsc::Receiver<SocketCheckpoint>,
        event_rx: &mpsc::Receiver<AsrEvent>,
    ) {
        assert_run_task_heartbeat(expect_sent(checkpoint_rx));
        expect_read_waiting(checkpoint_rx);
        read_tx
            .send(ScriptRead::Message(provider_event(
                "task-started",
                json!({}),
            )))
            .unwrap();
        assert!(matches!(event_rx.recv().unwrap(), AsrEvent::Ready));
        expect_read_waiting(checkpoint_rx);
    }

    fn drive_one_silent_second(
        control_tx: &mpsc::SyncSender<crate::backend::AsrControl>,
        read_tx: &mpsc::Sender<ScriptRead>,
        checkpoint_rx: &mpsc::Receiver<SocketCheckpoint>,
        clock: &ManualClock,
    ) {
        control_tx
            .send(crate::backend::AsrControl::append_pcm16(vec![
                0;
                SAMPLES_PER_PACKET
            ]))
            .unwrap();
        read_tx.send(ScriptRead::WouldBlock).unwrap();
        let Message::Binary(bytes) = expect_sent(checkpoint_rx) else {
            panic!("PCM packet must be binary");
        };
        assert_eq!(bytes.len(), SAMPLES_PER_PACKET * 2);
        assert!(bytes.iter().all(|byte| *byte == 0));
        expect_read_waiting(checkpoint_rx);
        clock.advance(Duration::from_secs(1));
        read_tx
            .send(ScriptRead::Message(heartbeat_event()))
            .unwrap();
        expect_read_waiting(checkpoint_rx);
    }

    #[test]
    fn startup_deadline_expiration_closes_with_stable_timeout_error() {
        let clock = ManualClock::default();
        let abort_flag = Arc::new(AtomicBool::new(false));
        let (control_tx, read_tx, checkpoint_rx, event_rx, join) =
            spawn_scripted_lifecycle(clock.clone(), abort_flag);

        assert_run_task_heartbeat(expect_sent(&checkpoint_rx));
        expect_read_waiting(&checkpoint_rx);
        clock.advance(Duration::from_millis(STARTUP_DEADLINE_MS + 1));
        read_tx.send(ScriptRead::WouldBlock).unwrap();
        expect_closed(&checkpoint_rx);
        drop(control_tx);

        let error = join.join().unwrap().unwrap_err();
        assert_eq!(error.to_string(), "Qwen-Audio-3 task-started timed out");
        assert!(event_rx.try_iter().next().is_none());
    }

    #[test]
    fn finalization_deadline_expiration_closes_with_stable_timeout_error() {
        let clock = ManualClock::default();
        let abort_flag = Arc::new(AtomicBool::new(false));
        let (control_tx, read_tx, checkpoint_rx, event_rx, join) =
            spawn_scripted_lifecycle(clock.clone(), abort_flag);
        start_scripted_lifecycle(&read_tx, &checkpoint_rx, &event_rx);

        control_tx.send(crate::backend::AsrControl::Finish).unwrap();
        read_tx.send(ScriptRead::WouldBlock).unwrap();
        let Message::Text(finish_text) = expect_sent(&checkpoint_rx) else {
            panic!("finish-task must be text");
        };
        let finish_payload: serde_json::Value = serde_json::from_str(&finish_text).unwrap();
        assert_eq!(finish_payload["header"]["action"], "finish-task");
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::AudioDeliveryCompleted {
                packet_count: 0,
                sample_count: 0,
                ..
            }
        ));
        expect_read_waiting(&checkpoint_rx);
        clock.advance(Duration::from_millis(FINALIZE_DEADLINE_MS + 1));
        read_tx.send(ScriptRead::WouldBlock).unwrap();
        expect_closed(&checkpoint_rx);
        drop(control_tx);

        let error = join.join().unwrap().unwrap_err();
        assert_eq!(error.to_string(), "Qwen-Audio-3 finalization timed out");
        assert!(event_rx.try_iter().all(|event| !matches!(
            event,
            AsrEvent::Partial { .. }
                | AsrEvent::SegmentFinal { .. }
                | AsrEvent::Final { .. }
                | AsrEvent::Finished
        )));
    }

    #[test]
    fn heartbeat_long_idle_finish_lifecycle_is_deterministic() {
        let clock = ManualClock::default();
        let abort_flag = Arc::new(AtomicBool::new(false));
        let (control_tx, read_tx, checkpoint_rx, event_rx, join) =
            spawn_scripted_lifecycle(clock.clone(), abort_flag);
        start_scripted_lifecycle(&read_tx, &checkpoint_rx, &event_rx);

        for _ in 0..LONG_IDLE_SECONDS {
            drive_one_silent_second(&control_tx, &read_tx, &checkpoint_rx, &clock);
        }

        control_tx.send(crate::backend::AsrControl::Finish).unwrap();
        read_tx.send(ScriptRead::WouldBlock).unwrap();
        let Message::Text(finish_text) = expect_sent(&checkpoint_rx) else {
            panic!("finish-task must be text");
        };
        let finish_payload: serde_json::Value = serde_json::from_str(&finish_text).unwrap();
        assert_eq!(finish_payload["header"]["action"], "finish-task");
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::AudioDeliveryCompleted {
                packet_count: 65,
                sample_count: 1_040_000,
                ..
            }
        ));
        expect_read_waiting(&checkpoint_rx);
        read_tx
            .send(ScriptRead::Message(provider_event(
                "task-finished",
                json!({}),
            )))
            .unwrap();
        assert!(matches!(event_rx.recv().unwrap(), AsrEvent::Finished));
        expect_closed(&checkpoint_rx);
        join.join().unwrap().unwrap();

        assert!(event_rx.try_iter().all(|event| !matches!(
            event,
            AsrEvent::Partial { .. } | AsrEvent::SegmentFinal { .. } | AsrEvent::Final { .. }
        )));
    }

    #[test]
    fn heartbeat_long_idle_cancellation_is_observed_on_next_retryable_read_and_closes() {
        let clock = ManualClock::default();
        let abort_flag = Arc::new(AtomicBool::new(false));
        let (control_tx, read_tx, checkpoint_rx, event_rx, join) =
            spawn_scripted_lifecycle(clock.clone(), abort_flag.clone());
        start_scripted_lifecycle(&read_tx, &checkpoint_rx, &event_rx);

        for _ in 0..LONG_IDLE_SECONDS {
            drive_one_silent_second(&control_tx, &read_tx, &checkpoint_rx, &clock);
        }

        abort_flag.store(true, Ordering::SeqCst);
        read_tx.send(ScriptRead::WouldBlock).unwrap();
        expect_closed(&checkpoint_rx);
        drop(control_tx);
        join.join().unwrap().unwrap();

        assert!(event_rx.try_iter().all(|event| !matches!(
            event,
            AsrEvent::Partial { .. }
                | AsrEvent::SegmentFinal { .. }
                | AsrEvent::Final { .. }
                | AsrEvent::AudioDeliveryCompleted { .. }
                | AsrEvent::Finished
        )));
    }

    #[test]
    fn websocket_request_uses_bearer_authorization() {
        let request = websocket_request("ws://127.0.0.1:1234/inference", "test-key").unwrap();
        assert_eq!(
            request.headers()[tungstenite::http::header::AUTHORIZATION],
            "Bearer test-key"
        );
    }

    #[test]
    fn run_task_envelope_matches_dashscope_protocol() {
        assert_eq!(
            run_task_envelope(
                TASK_ID,
                "qwen-audio-3.0-asr-flash-streaming",
                AudioSpec {
                    sample_rate_hz: 16_000,
                },
                Audio3RequestControls {
                    language: Language::SimplifiedChinese,
                    language_hints_enabled: true,
                    heartbeat_enabled: true,
                    max_sentence_silence_ms: 800,
                    semantic_punctuation_enabled: false,
                    vocabulary: &[],
                },
            ),
            json!({
                "header": {
                    "action": "run-task",
                    "task_id": TASK_ID,
                    "streaming": "duplex"
                },
                "payload": {
                    "task_group": "audio",
                    "task": "asr",
                    "function": "recognition",
                    "model": "qwen-audio-3.0-asr-flash-streaming",
                    "input": {},
                    "parameters": {
                        "format": "pcm",
                        "sample_rate": 16_000,
                        "language_hints": ["zh", "en"],
                        "heartbeat": true,
                        "max_sentence_silence": 800,
                        "semantic_punctuation_enabled": false
                    }
                }
            })
        );
    }

    #[test]
    fn run_task_envelope_sends_explicit_controls_and_omits_empty_vocabulary() {
        let disabled = run_task_envelope(
            TASK_ID,
            "model",
            AudioSpec {
                sample_rate_hz: 16_000,
            },
            Audio3RequestControls {
                language: Language::English,
                language_hints_enabled: false,
                heartbeat_enabled: false,
                max_sentence_silence_ms: 200,
                semantic_punctuation_enabled: true,
                vocabulary: &[],
            },
        );
        assert!(
            disabled["payload"]["parameters"]
                .get("language_hints")
                .is_none()
        );
        assert_eq!(disabled["payload"]["parameters"]["heartbeat"], false);
        assert_eq!(
            disabled["payload"]["parameters"]["max_sentence_silence"],
            200
        );
        assert_eq!(
            disabled["payload"]["parameters"]["semantic_punctuation_enabled"],
            true
        );
        assert!(
            disabled["payload"]["parameters"]
                .get("vocabulary")
                .is_none()
        );

        let vocabulary = [
            Audio3VocabularyTerm {
                term: " Voice Input ".into(),
                weight: 5,
            },
            Audio3VocabularyTerm {
                term: "语音输入".into(),
                weight: 50,
            },
        ];
        let configured = run_task_envelope(
            TASK_ID,
            "model",
            AudioSpec {
                sample_rate_hz: 16_000,
            },
            Audio3RequestControls {
                language: Language::Japanese,
                language_hints_enabled: true,
                heartbeat_enabled: true,
                max_sentence_silence_ms: 6_000,
                semantic_punctuation_enabled: false,
                vocabulary: &vocabulary,
            },
        );
        assert_eq!(
            configured["payload"]["parameters"]["language_hints"],
            json!(["ja", "en"])
        );
        assert_eq!(
            configured["payload"]["parameters"]["vocabulary"],
            json!({"Voice Input": 5, "语音输入": 50})
        );
        assert_eq!(
            configured["payload"]["parameters"]["max_sentence_silence"],
            6_000
        );
    }

    #[test]
    fn finish_task_envelope_matches_dashscope_protocol() {
        assert_eq!(
            finish_task_envelope(TASK_ID),
            json!({
                "header": {
                    "action": "finish-task",
                    "task_id": TASK_ID,
                    "streaming": "duplex"
                },
                "payload": { "input": {} }
            })
        );
    }

    #[test]
    fn parses_correlated_task_lifecycle_events() {
        let started = Message::Text(
            json!({
                "header": {"event": "task-started", "task_id": TASK_ID},
                "payload": {}
            })
            .to_string(),
        );
        assert_eq!(
            parse_server_event(started, TASK_ID).unwrap(),
            Some(ServerEvent::TaskStarted)
        );

        let finished = Message::Text(
            json!({
                "header": {"event": "task-finished", "task_id": TASK_ID},
                "payload": {"output": {"text": "authoritative final"}}
            })
            .to_string(),
        );
        assert_eq!(
            parse_server_event(finished, TASK_ID).unwrap(),
            Some(ServerEvent::TaskFinished {
                text: Some("authoritative final".into())
            })
        );
    }

    #[test]
    fn parses_correlated_partial_and_final_sentence_results() {
        let partial = Message::Text(
            json!({
                "header": {"event": "result-generated", "task_id": TASK_ID},
                "payload": {"output": {"sentence": {
                    "text": "hello",
                    "sentence_end": false
                }}}
            })
            .to_string(),
        );
        assert_eq!(
            parse_server_event(partial, TASK_ID).unwrap(),
            Some(ServerEvent::ResultGenerated {
                text: "hello".into(),
                sentence_final: false,
            })
        );

        let final_sentence = Message::Text(
            json!({
                "header": {"event": "result-generated", "task_id": TASK_ID},
                "payload": {"output": {"sentence": {
                    "text": "hello world",
                    "sentence_end": true
                }}}
            })
            .to_string(),
        );
        assert_eq!(
            parse_server_event(final_sentence, TASK_ID).unwrap(),
            Some(ServerEvent::ResultGenerated {
                text: "hello world".into(),
                sentence_final: true,
            })
        );
    }

    #[test]
    fn assembler_emits_conservative_partial_segment_and_final_text() {
        let mut assembler = TranscriptAssembler::default();
        assert_eq!(
            assembler.apply_partial("hello".into()),
            (String::new(), "hello".into())
        );
        assert_eq!(
            assembler.apply_segment_final("hello world".into()),
            "hello world"
        );
        assert_eq!(
            assembler.apply_partial("again".into()),
            ("hello world".into(), "again".into())
        );
        assert_eq!(assembler.finish(None), "hello world again");
    }

    #[test]
    fn task_failure_message_is_discarded_and_only_category_is_emitted() {
        const SENTINEL: &str = "private-transcript-sentinel";
        let event = Message::Text(
            json!({
                "header": {
                    "event": "task-failed",
                    "task_id": TASK_ID,
                    "error_code": "ServiceUnavailable"
                },
                "payload": {"message": SENTINEL}
            })
            .to_string(),
        );
        let Some(ServerEvent::TaskFailed {
            kind,
            provider_error_code,
        }) = parse_server_event(event, TASK_ID).unwrap()
        else {
            panic!("expected task failure");
        };
        assert_eq!(kind, FailureKind::Service);
        assert_eq!(
            provider_error_code,
            ProviderErrorCode::try_new("ServiceUnavailable")
        );

        let (event_tx, event_rx) = mpsc::channel();
        let error = report_task_failure(&event_tx, kind, provider_error_code).unwrap_err();
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::TaskFailed {
                kind: FailureKind::Service,
                provider_error_code: Some(code),
            } if code.as_str() == "ServiceUnavailable"
        ));
        assert_eq!(
            error.to_string(),
            "Qwen-Audio-3 streaming ASR failed (service)"
        );
        assert!(!format!("{error:#}").contains(SENTINEL));
    }

    #[test]
    fn malformed_provider_error_codes_are_discarded_whole() {
        for code in [
            "private value",
            "path/value",
            "line\nbreak",
            "私密内容",
            &"A".repeat(65),
        ] {
            let event = Message::Text(
                json!({
                    "header": {
                        "event": "task-failed",
                        "task_id": TASK_ID,
                        "error_code": code
                    },
                    "payload": {}
                })
                .to_string(),
            );
            assert!(matches!(
                parse_server_event(event, TASK_ID).unwrap(),
                Some(ServerEvent::TaskFailed {
                    provider_error_code: None,
                    ..
                })
            ));
        }
    }

    #[test]
    fn rejects_malformed_or_mismatched_events() {
        let mismatched = Message::Text(
            json!({
                "header": {"event": "task-started", "task_id": "other-task"},
                "payload": {}
            })
            .to_string(),
        );
        assert!(parse_server_event(mismatched, TASK_ID).is_err());

        let malformed = Message::Text(
            json!({
                "header": {"event": "result-generated", "task_id": TASK_ID},
                "payload": {"output": {"sentence": {"sentence_end": true}}}
            })
            .to_string(),
        );
        assert!(parse_server_event(malformed, TASK_ID).is_err());
    }

    #[test]
    fn heartbeat_results_are_ignored() {
        let heartbeat = Message::Text(
            json!({
                "header": {"event": "result-generated", "task_id": TASK_ID},
                "payload": {"output": {"sentence": {
                    "text": "",
                    "heartbeat": true,
                    "sentence_end": false
                }}}
            })
            .to_string(),
        );
        assert_eq!(parse_server_event(heartbeat, TASK_ID).unwrap(), None);
    }

    #[test]
    fn generated_task_ids_use_uuid_shape() {
        let task_id = new_task_id();
        assert_eq!(task_id.len(), 36);
        assert_eq!(
            task_id
                .chars()
                .filter(|character| *character == '-')
                .count(),
            4
        );
        assert!(
            task_id
                .chars()
                .all(|character| character == '-' || character.is_ascii_hexdigit())
        );
    }

    #[test]
    fn pcm_frames_are_signed_16_bit_little_endian() {
        assert_eq!(
            pcm16_le_bytes(&[0, 1, -1, i16::MIN, i16::MAX]),
            vec![0x00, 0x00, 0x01, 0x00, 0xff, 0xff, 0x00, 0x80, 0xff, 0x7f]
        );
    }
}
