use std::{
    borrow::Cow,
    fmt,
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
use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
};
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
    backend::{
        ASR_CONTROL_QUEUE_CAPACITY, AsrControl, AsrEvent, AsrSessionHandle, AudioSpec,
        TimestampDiagnosticsDelta,
    },
    config::{
        AlibabaAudio3Config, Audio3VocabularyTerm, Config, EffectiveAudio3RecognitionControls,
        Language,
    },
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
// Complete messages beyond this transport cap are protocol errors. Within the
// cap, timestamp overflow is handled semantically and never drops transcript
// text, so oversized timing arrays can be received and counted as truncated.
const MAX_SERVER_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024;
const MAX_TIMED_UNITS_PER_RESULT: usize = 512;
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
    let mut socket = open_socket(audio3, &audio3.api_key, connect_timeout)?;
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
    let recognition = audio3.effective_recognition_controls();
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
                recognition,
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
                            timestamp_summary,
                        } => {
                            let _ = event_tx.send(AsrEvent::TimestampDiagnostics {
                                delta: timestamp_summary.into(),
                            });
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

fn open_socket(
    audio3: &AlibabaAudio3Config,
    api_key: &str,
    timeout: Duration,
) -> Result<Audio3Socket> {
    let request = websocket_request_for_config(audio3, api_key)?;
    let websocket_config = WebSocketConfig {
        max_message_size: Some(MAX_SERVER_MESSAGE_BYTES),
        max_frame_size: Some(MAX_SERVER_MESSAGE_BYTES),
        ..WebSocketConfig::default()
    };
    let (socket, _) = connect_with_timeout(request, timeout, websocket_config)?;
    Ok(socket)
}

fn websocket_request_for_config(
    audio3: &AlibabaAudio3Config,
    api_key: &str,
) -> Result<tungstenite::http::Request<()>> {
    let endpoints = audio3.resolve_endpoints();
    websocket_request(endpoints.streaming(), api_key)
}

fn websocket_request(endpoint: &str, api_key: &str) -> Result<tungstenite::http::Request<()>> {
    let mut request = endpoint
        .into_client_request()
        .map_err(|_| anyhow!("failed to build Qwen-Audio-3 websocket request"))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))
            .map_err(|_| anyhow!("Alibaba API key contains an invalid header value"))?,
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
        .map_err(|_| anyhow!("failed to resolve Qwen-Audio-3 endpoint"))?
        .next()
        .ok_or_else(|| anyhow!("Qwen-Audio-3 endpoint resolved to no addresses"))?;
    let stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|_| anyhow!("failed to connect to Qwen-Audio-3 endpoint"))?;
    stream.set_nodelay(true).ok();
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| anyhow!("failed to configure Qwen-Audio-3 websocket transport"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| anyhow!("failed to configure Qwen-Audio-3 websocket transport"))?;

    match client_tls_with_config(request, stream, Some(websocket_config), None) {
        Ok(value) => Ok(value),
        Err(tungstenite::HandshakeError::Failure(error)) => {
            Err(sanitize_websocket_handshake_failure(error))
        }
        Err(tungstenite::HandshakeError::Interrupted(_)) => {
            bail!("unexpected interrupted Qwen-Audio-3 websocket handshake")
        }
    }
}

fn sanitize_websocket_handshake_failure<E>(_error: E) -> anyhow::Error {
    anyhow!("Qwen-Audio-3 websocket handshake failed")
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
    recognition: EffectiveAudio3RecognitionControls,
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
        recognition,
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
                "max_sentence_silence": recognition.max_sentence_silence_ms,
                "semantic_punctuation_enabled": recognition.semantic_punctuation_enabled
            }
        }
    });
    if recognition.multi_threshold_mode_enabled {
        envelope["payload"]["parameters"]["multi_threshold_mode_enabled"] = json!(true);
    }
    if let Some(threshold) = recognition.speech_noise_threshold {
        envelope["payload"]["parameters"]["speech_noise_threshold"] = json!(threshold);
    }
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Audio3TimestampSummary {
    timestamp_bearing_result_count: u64,
    accepted_timed_unit_count: u64,
    result_with_rejected_timestamp_metadata_count: u64,
    truncated_timed_unit_count: u64,
    latest_valid_audio_end_ms: Option<u64>,
}

impl From<Audio3TimestampSummary> for TimestampDiagnosticsDelta {
    fn from(summary: Audio3TimestampSummary) -> Self {
        Self {
            timestamp_bearing_result_count: summary.timestamp_bearing_result_count,
            accepted_timed_unit_count: summary.accepted_timed_unit_count,
            result_with_rejected_timestamp_metadata_count: summary
                .result_with_rejected_timestamp_metadata_count,
            truncated_timed_unit_count: summary.truncated_timed_unit_count,
            latest_valid_audio_end_ms: summary.latest_valid_audio_end_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimedRange {
    begin_ms: u64,
    end_ms: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum ServerEvent {
    TaskStarted,
    ResultGenerated {
        text: String,
        sentence_final: bool,
        timestamp_summary: Audio3TimestampSummary,
    },
    TaskFinished {
        text: Option<String>,
    },
    TaskFailed {
        kind: FailureKind,
        provider_error_code: Option<ProviderErrorCode>,
    },
}

#[derive(Deserialize)]
struct BorrowedEventEnvelope<'a> {
    #[serde(borrow)]
    header: BorrowedEventHeader<'a>,
}

#[derive(Deserialize)]
struct BorrowedEventHeader<'a> {
    #[serde(borrow)]
    event: Cow<'a, str>,
    #[serde(borrow)]
    task_id: Cow<'a, str>,
}

#[derive(Deserialize)]
struct BorrowedResultEnvelope<'a> {
    #[serde(borrow)]
    payload: BorrowedResultPayload<'a>,
}

#[derive(Deserialize)]
struct BorrowedResultPayload<'a> {
    #[serde(borrow)]
    output: BorrowedResultOutput<'a>,
}

#[derive(Deserialize)]
struct BorrowedResultOutput<'a> {
    #[serde(default, borrow)]
    sentence: Option<BorrowedSentence<'a>>,
    #[serde(default, borrow)]
    text: Option<Cow<'a, str>>,
}

#[derive(Clone, Copy, Default)]
struct LooseBool(bool);

impl<'de> Deserialize<'de> for LooseBool {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LooseBoolVisitor;

        impl<'de> Visitor<'de> for LooseBoolVisitor {
            type Value = LooseBool;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a boolean")
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
                Ok(LooseBool(value))
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(LooseBool(false))
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(LooseBool(false))
            }

            fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                deserializer.deserialize_any(self)
            }

            fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(LooseBool(false))
            }

            fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(LooseBool(false))
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(LooseBool(false))
            }

            fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(LooseBool(false))
            }

            fn visit_string<E>(self, _: String) -> std::result::Result<Self::Value, E> {
                Ok(LooseBool(false))
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LooseBool(false))
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LooseBool(false))
            }
        }

        deserializer.deserialize_any(LooseBoolVisitor)
    }
}

#[derive(Debug, Clone, Copy, Default)]
enum NumericField {
    #[default]
    Missing,
    Null,
    Integer(u64),
    Invalid,
}

impl<'de> Deserialize<'de> for NumericField {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NumericFieldVisitor;

        impl<'de> Visitor<'de> for NumericFieldVisitor {
            type Value = NumericField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an unsigned integer or null")
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
                Ok(NumericField::Integer(value))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
                Ok(u64::try_from(value)
                    .map(NumericField::Integer)
                    .unwrap_or(NumericField::Invalid))
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(NumericField::Null)
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(NumericField::Null)
            }

            fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                deserializer.deserialize_any(self)
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(NumericField::Invalid)
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(NumericField::Invalid)
            }

            fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(NumericField::Invalid)
            }

            fn visit_string<E>(self, _: String) -> std::result::Result<Self::Value, E> {
                Ok(NumericField::Invalid)
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(NumericField::Invalid)
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(NumericField::Invalid)
            }
        }

        deserializer.deserialize_any(NumericFieldVisitor)
    }
}

enum TimedUnitField {
    Begin,
    End,
    Other,
}

impl<'de> Deserialize<'de> for TimedUnitField {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TimedUnitFieldVisitor;

        impl Visitor<'_> for TimedUnitFieldVisitor {
            type Value = TimedUnitField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a timed-unit field")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Ok(match value {
                    "begin_time" => TimedUnitField::Begin,
                    "end_time" => TimedUnitField::End,
                    _ => TimedUnitField::Other,
                })
            }
        }

        deserializer.deserialize_identifier(TimedUnitFieldVisitor)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct TimedUnitCandidate {
    object_shape: bool,
    duplicate_timing_field: bool,
    begin: NumericField,
    end: NumericField,
}

impl<'de> Deserialize<'de> for TimedUnitCandidate {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TimedUnitVisitor;

        impl<'de> Visitor<'de> for TimedUnitVisitor {
            type Value = TimedUnitCandidate;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a timed-unit object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut candidate = TimedUnitCandidate {
                    object_shape: true,
                    ..TimedUnitCandidate::default()
                };
                let mut begin_seen = false;
                let mut end_seen = false;
                while let Some(field) = map.next_key::<TimedUnitField>()? {
                    match field {
                        TimedUnitField::Begin if !begin_seen => {
                            begin_seen = true;
                            candidate.begin = map.next_value()?;
                        }
                        TimedUnitField::End if !end_seen => {
                            end_seen = true;
                            candidate.end = map.next_value()?;
                        }
                        TimedUnitField::Begin | TimedUnitField::End => {
                            candidate.duplicate_timing_field = true;
                            map.next_value::<IgnoredAny>()?;
                        }
                        TimedUnitField::Other => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(candidate)
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(TimedUnitCandidate::default())
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(TimedUnitCandidate::default())
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(TimedUnitCandidate::default())
            }

            fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(TimedUnitCandidate::default())
            }

            fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(TimedUnitCandidate::default())
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(TimedUnitCandidate::default())
            }

            fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(TimedUnitCandidate::default())
            }

            fn visit_string<E>(self, _: String) -> std::result::Result<Self::Value, E> {
                Ok(TimedUnitCandidate::default())
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(TimedUnitCandidate::default())
            }
        }

        deserializer.deserialize_any(TimedUnitVisitor)
    }
}

#[derive(Debug, Default)]
struct BoundedTimedUnits {
    array_shape: bool,
    candidates: Vec<TimedUnitCandidate>,
    truncated_count: u64,
}

impl<'de> Deserialize<'de> for BoundedTimedUnits {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BoundedTimedUnitsVisitor;

        impl<'de> Visitor<'de> for BoundedTimedUnitsVisitor {
            type Value = BoundedTimedUnits;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a timed-unit array")
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut units = BoundedTimedUnits {
                    array_shape: true,
                    candidates: Vec::with_capacity(
                        sequence
                            .size_hint()
                            .unwrap_or_default()
                            .min(MAX_TIMED_UNITS_PER_RESULT),
                    ),
                    truncated_count: 0,
                };
                while units.candidates.len() < MAX_TIMED_UNITS_PER_RESULT {
                    let Some(candidate) = sequence.next_element::<TimedUnitCandidate>()? else {
                        return Ok(units);
                    };
                    units.candidates.push(candidate);
                }
                while sequence.next_element::<IgnoredAny>()?.is_some() {
                    units.truncated_count = units.truncated_count.saturating_add(1);
                }
                Ok(units)
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(BoundedTimedUnits::default())
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(BoundedTimedUnits::default())
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(BoundedTimedUnits::default())
            }

            fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(BoundedTimedUnits::default())
            }

            fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(BoundedTimedUnits::default())
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(BoundedTimedUnits::default())
            }

            fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(BoundedTimedUnits::default())
            }

            fn visit_string<E>(self, _: String) -> std::result::Result<Self::Value, E> {
                Ok(BoundedTimedUnits::default())
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(BoundedTimedUnits::default())
            }
        }

        deserializer.deserialize_any(BoundedTimedUnitsVisitor)
    }
}

enum SentenceField {
    Text,
    SentenceEnd,
    Heartbeat,
    SentenceId,
    Begin,
    End,
    Words,
    Other,
}

impl<'de> Deserialize<'de> for SentenceField {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SentenceFieldVisitor;

        impl Visitor<'_> for SentenceFieldVisitor {
            type Value = SentenceField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a result sentence field")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Ok(match value {
                    "text" => SentenceField::Text,
                    "sentence_end" => SentenceField::SentenceEnd,
                    "heartbeat" => SentenceField::Heartbeat,
                    "sentence_id" => SentenceField::SentenceId,
                    "begin_time" => SentenceField::Begin,
                    "end_time" => SentenceField::End,
                    "words" => SentenceField::Words,
                    _ => SentenceField::Other,
                })
            }
        }

        deserializer.deserialize_identifier(SentenceFieldVisitor)
    }
}

struct BorrowedText<'a>(Cow<'a, str>);

impl<'de: 'a, 'a> Deserialize<'de> for BorrowedText<'a> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BorrowedTextVisitor;

        impl<'de> Visitor<'de> for BorrowedTextVisitor {
            type Value = Cow<'de, str>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a transcript string")
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E> {
                Ok(Cow::Borrowed(value))
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Ok(Cow::Owned(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
                Ok(Cow::Owned(value))
            }
        }

        deserializer
            .deserialize_str(BorrowedTextVisitor)
            .map(BorrowedText)
    }
}

struct BorrowedSentence<'a> {
    text: Option<Cow<'a, str>>,
    sentence_final: bool,
    heartbeat: bool,
    timestamp_metadata_present: bool,
    duplicate_timestamp_field: bool,
    sentence_id: NumericField,
    begin: NumericField,
    end: NumericField,
    words: Option<BoundedTimedUnits>,
}

impl<'de: 'a, 'a> Deserialize<'de> for BorrowedSentence<'a> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BorrowedSentenceVisitor<'a>(std::marker::PhantomData<&'a ()>);

        impl<'de: 'a, 'a> Visitor<'de> for BorrowedSentenceVisitor<'a> {
            type Value = BorrowedSentence<'a>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a result sentence object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut sentence = BorrowedSentence {
                    text: None,
                    sentence_final: false,
                    heartbeat: false,
                    timestamp_metadata_present: false,
                    duplicate_timestamp_field: false,
                    sentence_id: NumericField::Missing,
                    begin: NumericField::Missing,
                    end: NumericField::Missing,
                    words: None,
                };
                let mut text_seen = false;
                let mut sentence_end_seen = false;
                let mut heartbeat_seen = false;
                let mut sentence_id_seen = false;
                let mut begin_seen = false;
                let mut end_seen = false;
                let mut words_seen = false;
                while let Some(field) = map.next_key::<SentenceField>()? {
                    match field {
                        SentenceField::Text if !text_seen => {
                            text_seen = true;
                            sentence.text = Some(map.next_value::<BorrowedText<'a>>()?.0);
                        }
                        SentenceField::SentenceEnd if !sentence_end_seen => {
                            sentence_end_seen = true;
                            sentence.sentence_final = map.next_value::<LooseBool>()?.0;
                        }
                        SentenceField::Heartbeat if !heartbeat_seen => {
                            heartbeat_seen = true;
                            sentence.heartbeat = map.next_value::<LooseBool>()?.0;
                        }
                        SentenceField::Text
                        | SentenceField::SentenceEnd
                        | SentenceField::Heartbeat => {
                            return Err(serde::de::Error::custom(
                                "duplicate core result sentence field",
                            ));
                        }
                        SentenceField::SentenceId if !sentence_id_seen => {
                            sentence_id_seen = true;
                            sentence.timestamp_metadata_present = true;
                            sentence.sentence_id = map.next_value()?;
                        }
                        SentenceField::Begin if !begin_seen => {
                            begin_seen = true;
                            sentence.timestamp_metadata_present = true;
                            sentence.begin = map.next_value()?;
                        }
                        SentenceField::End if !end_seen => {
                            end_seen = true;
                            sentence.timestamp_metadata_present = true;
                            sentence.end = map.next_value()?;
                        }
                        SentenceField::Words if !words_seen => {
                            words_seen = true;
                            sentence.timestamp_metadata_present = true;
                            sentence.words = Some(map.next_value()?);
                        }
                        SentenceField::SentenceId
                        | SentenceField::Begin
                        | SentenceField::End
                        | SentenceField::Words => {
                            sentence.timestamp_metadata_present = true;
                            sentence.duplicate_timestamp_field = true;
                            map.next_value::<IgnoredAny>()?;
                        }
                        SentenceField::Other => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(sentence)
            }
        }

        deserializer.deserialize_map(BorrowedSentenceVisitor(std::marker::PhantomData))
    }
}

impl BorrowedSentence<'_> {
    fn timestamp_summary(&self) -> Audio3TimestampSummary {
        if !self.timestamp_metadata_present {
            return Audio3TimestampSummary::default();
        }

        let mut summary = Audio3TimestampSummary {
            timestamp_bearing_result_count: 1,
            ..Audio3TimestampSummary::default()
        };
        let mut rejected = self.duplicate_timestamp_field
            || !matches!(self.sentence_id, NumericField::Integer(value) if value > 0);
        let sentence_begin = match self.begin {
            NumericField::Integer(value) => Some(value),
            NumericField::Missing | NumericField::Null | NumericField::Invalid => {
                rejected = true;
                None
            }
        };
        enum ValidBounds {
            Open { begin_ms: u64 },
            Closed(TimedRange),
        }
        let valid_bounds = match (sentence_begin, self.end, self.sentence_final) {
            (Some(begin_ms), NumericField::Null, false) => Some(ValidBounds::Open { begin_ms }),
            (Some(begin_ms), NumericField::Integer(end_ms), _) if begin_ms <= end_ms => {
                summary.latest_valid_audio_end_ms = Some(end_ms);
                Some(ValidBounds::Closed(TimedRange { begin_ms, end_ms }))
            }
            _ => {
                rejected = true;
                None
            }
        };

        if let Some(words) = &self.words {
            summary.truncated_timed_unit_count = words.truncated_count;
            if !words.array_shape {
                rejected = true;
            } else if let Some(bounds) = valid_bounds {
                let mut previous_end_ms = None;
                for candidate in &words.candidates {
                    let range = match (
                        candidate.object_shape,
                        candidate.duplicate_timing_field,
                        candidate.begin,
                        candidate.end,
                    ) {
                        (
                            true,
                            false,
                            NumericField::Integer(begin_ms),
                            NumericField::Integer(end_ms),
                        ) if begin_ms <= end_ms => Some(TimedRange { begin_ms, end_ms }),
                        _ => None,
                    }
                    .filter(|range| {
                        previous_end_ms.is_none_or(|previous| range.begin_ms >= previous)
                    })
                    .filter(|range| match bounds {
                        ValidBounds::Open { begin_ms } => range.begin_ms >= begin_ms,
                        ValidBounds::Closed(bounds) => {
                            range.begin_ms >= bounds.begin_ms && range.end_ms <= bounds.end_ms
                        }
                    });
                    let Some(range) = range else {
                        rejected = true;
                        continue;
                    };
                    previous_end_ms = Some(range.end_ms);
                    summary.accepted_timed_unit_count =
                        summary.accepted_timed_unit_count.saturating_add(1);
                    if matches!(bounds, ValidBounds::Open { .. }) {
                        summary.latest_valid_audio_end_ms = Some(range.end_ms);
                    }
                }
            }
        }

        summary.result_with_rejected_timestamp_metadata_count = u64::from(rejected);
        summary
    }
}

fn parse_server_event(message: Message, expected_task_id: &str) -> Result<Option<ServerEvent>> {
    let Message::Text(text) = message else {
        return Ok(None);
    };
    if text.len() > MAX_SERVER_MESSAGE_BYTES {
        bail!("Qwen-Audio-3 server event exceeds the transport size limit");
    }
    let envelope: BorrowedEventEnvelope<'_> = serde_json::from_str(&text)
        .map_err(|_| anyhow!("failed to parse Qwen-Audio-3 server event header"))?;
    if envelope.header.task_id != expected_task_id {
        bail!("Qwen-Audio-3 server event task ID does not match the active task");
    }

    match envelope.header.event.as_ref() {
        "task-started" => Ok(Some(ServerEvent::TaskStarted)),
        "result-generated" => parse_result_generated(&text),
        "task-finished" | "task-failed" => {
            let payload: Value =
                serde_json::from_str(&text).context("failed to parse Qwen-Audio-3 server JSON")?;
            if envelope.header.event == "task-finished" {
                Ok(Some(ServerEvent::TaskFinished {
                    text: extract_optional_text(&payload),
                }))
            } else {
                Ok(Some(ServerEvent::TaskFailed {
                    kind: provider_failure_kind(&payload),
                    provider_error_code: provider_error_code(&payload),
                }))
            }
        }
        _ => Ok(None),
    }
}

fn parse_result_generated(text: &str) -> Result<Option<ServerEvent>> {
    let envelope: BorrowedResultEnvelope<'_> = serde_json::from_str(text)
        .map_err(|_| anyhow!("failed to parse Qwen-Audio-3 result-generated event"))?;
    let output = envelope.payload.output;
    if let Some(sentence) = output.sentence {
        // Heartbeats are discarded before timestamp or transcript events are
        // constructed. Their text and timing fields never enter telemetry.
        if sentence.heartbeat {
            return Ok(None);
        }
        let transcript = sentence
            .text
            .as_deref()
            .ok_or_else(|| anyhow!("Qwen-Audio-3 result sentence is missing text"))?;
        let timestamp_summary = sentence.timestamp_summary();
        return Ok(Some(ServerEvent::ResultGenerated {
            text: bounded(transcript, MAX_TRANSCRIPT_BYTES).to_string(),
            sentence_final: sentence.sentence_final,
            timestamp_summary,
        }));
    }

    let transcript = output
        .text
        .as_deref()
        .ok_or_else(|| anyhow!("Qwen-Audio-3 result is missing sentence/text"))?;
    Ok(Some(ServerEvent::ResultGenerated {
        text: bounded(transcript, MAX_TRANSCRIPT_BYTES).to_string(),
        sentence_final: false,
        timestamp_summary: Audio3TimestampSummary::default(),
    }))
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
        borrow::Cow,
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
        config::{
            AlibabaAudio3Config, Audio3EndpointMode, Audio3RecognitionPreset, Audio3Region,
            Audio3VocabularyTerm, EffectiveAudio3RecognitionControls, Language,
        },
        diagnostics::{FailureKind, ProviderErrorCode},
    };

    use super::{
        Audio3RequestControls, Audio3TimestampSummary, AudioSpec, BorrowedResultEnvelope,
        DeadlineClock, EstablishedSession, MAX_SERVER_MESSAGE_BYTES, MAX_TIMED_UNITS_PER_RESULT,
        ServerEvent, SocketIo, SocketResult, TimestampDiagnosticsDelta, TranscriptAssembler,
        finish_task_envelope, new_task_id, parse_server_event, pcm16_le_bytes, report_task_failure,
        run_established_socket, run_task_envelope, sanitize_websocket_handshake_failure,
        websocket_request, websocket_request_for_config,
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

    fn raw_result_sentence(sentence_fields: &str) -> Message {
        Message::Text(
            [
                r#"{"header":{"event":"result-generated","task_id":""#,
                TASK_ID,
                r#""},"payload":{"output":{"sentence":{"#,
                sentence_fields,
                "}}}}",
            ]
            .concat(),
        )
    }

    fn parse_raw_result_sentence(sentence_fields: &str) -> (String, bool, Audio3TimestampSummary) {
        let event = parse_server_event(raw_result_sentence(sentence_fields), TASK_ID)
            .unwrap()
            .expect("result-generated event");
        let ServerEvent::ResultGenerated {
            text,
            sentence_final,
            timestamp_summary,
        } = event
        else {
            panic!("expected result-generated event");
        };
        (text, sentence_final, timestamp_summary)
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

    fn parse_result_sentence(
        sentence: serde_json::Value,
    ) -> (String, bool, Audio3TimestampSummary) {
        let event = parse_server_event(
            provider_event(
                "result-generated",
                json!({"output": {"sentence": sentence}}),
            ),
            TASK_ID,
        )
        .unwrap()
        .expect("result-generated event");
        let ServerEvent::ResultGenerated {
            text,
            sentence_final,
            timestamp_summary,
        } = event
        else {
            panic!("expected result-generated event");
        };
        (text, sentence_final, timestamp_summary)
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
    fn scripted_production_state_path_emits_timing_and_unchanged_transcript_events() {
        let clock = ManualClock::default();
        let abort_flag = Arc::new(AtomicBool::new(false));
        let (control_tx, read_tx, checkpoint_rx, event_rx, join) =
            spawn_scripted_lifecycle(clock, abort_flag);
        start_scripted_lifecycle(&read_tx, &checkpoint_rx, &event_rx);

        read_tx
            .send(ScriptRead::Message(provider_event(
                "result-generated",
                json!({"output": {"sentence": {
                    "sentence_id": 1,
                    "begin_time": 0,
                    "end_time": null,
                    "text": "unchanged partial",
                    "sentence_end": false,
                    "words": [
                        {"begin_time": 0, "end_time": 80, "text": "private-unit-sentinel"},
                        {"begin_time": 80, "end_time": 160, "punctuation": "private-punctuation-sentinel"}
                    ]
                }}}),
            )))
            .unwrap();
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::TimestampDiagnostics { delta }
                if delta.timestamp_bearing_result_count == 1
                    && delta.accepted_timed_unit_count == 2
                    && delta.result_with_rejected_timestamp_metadata_count == 0
                    && delta.truncated_timed_unit_count == 0
                    && delta.latest_valid_audio_end_ms == Some(160)
        ));
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::Partial { committed, unstable }
                if committed.is_empty() && unstable == "unchanged partial"
        ));
        expect_read_waiting(&checkpoint_rx);

        read_tx
            .send(ScriptRead::Message(provider_event(
                "result-generated",
                json!({"output": {"sentence": {
                    "sentence_id": 1,
                    "begin_time": 0,
                    "end_time": null,
                    "text": "unchanged malformed-timing final",
                    "sentence_end": true,
                    "words": [{"begin_time": -1, "end_time": 160}]
                }}}),
            )))
            .unwrap();
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::TimestampDiagnostics { delta }
                if delta.timestamp_bearing_result_count == 1
                    && delta.accepted_timed_unit_count == 0
                    && delta.result_with_rejected_timestamp_metadata_count == 1
        ));
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::SegmentFinal { text } if text == "unchanged malformed-timing final"
        ));
        expect_read_waiting(&checkpoint_rx);

        control_tx.send(crate::backend::AsrControl::Finish).unwrap();
        read_tx.send(ScriptRead::WouldBlock).unwrap();
        let Message::Text(finish_text) = expect_sent(&checkpoint_rx) else {
            panic!("finish-task must be text");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&finish_text).unwrap()["header"]["action"],
            "finish-task"
        );
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::AudioDeliveryCompleted { .. }
        ));
        expect_read_waiting(&checkpoint_rx);
        read_tx
            .send(ScriptRead::Message(provider_event(
                "task-finished",
                json!({"output": {"text": "authoritative unchanged final"}}),
            )))
            .unwrap();
        assert!(matches!(
            event_rx.recv().unwrap(),
            AsrEvent::Final { text } if text == "authoritative unchanged final"
        ));
        assert!(matches!(event_rx.recv().unwrap(), AsrEvent::Finished));
        expect_closed(&checkpoint_rx);
        join.join().unwrap().unwrap();
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
    fn production_websocket_request_seam_uses_resolved_target() {
        let mut audio3 = AlibabaAudio3Config {
            region: Audio3Region::Singapore,
            ..AlibabaAudio3Config::default()
        };
        let request = websocket_request_for_config(&audio3, "test-key").unwrap();
        assert_eq!(
            request.uri().to_string(),
            crate::config::AUDIO3_SINGAPORE_STREAMING_ENDPOINT
        );

        audio3.endpoint_mode = Audio3EndpointMode::Custom;
        audio3.endpoint = "ws://127.0.0.1:1234/custom?opaque=a%2Fb&x=1".into();
        let custom = websocket_request_for_config(&audio3, "test-key").unwrap();
        assert_eq!(
            custom.uri().to_string(),
            "ws://127.0.0.1:1234/custom?opaque=a%2Fb&x=1"
        );

        const ENDPOINT_SENTINEL: &str = "private endpoint construction sentinel";
        audio3.endpoint = ENDPOINT_SENTINEL.into();
        let error = websocket_request_for_config(&audio3, "test-key")
            .expect_err("malformed custom target must fail generically");
        assert_eq!(
            error.to_string(),
            "failed to build Qwen-Audio-3 websocket request"
        );
        assert!(!format!("{error:#}").contains(ENDPOINT_SENTINEL));
    }

    #[test]
    fn websocket_handshake_failure_discards_raw_error_and_source_chain() {
        const SENTINEL: &str = "private TLS certificate host and route sentinel";
        let error = sanitize_websocket_handshake_failure(io::Error::other(SENTINEL));

        assert_eq!(error.to_string(), "Qwen-Audio-3 websocket handshake failed");
        assert_eq!(
            format!("{error:#}"),
            "Qwen-Audio-3 websocket handshake failed"
        );
        assert!(!error.to_string().contains(SENTINEL));
        assert!(!format!("{error:#}").contains(SENTINEL));
    }

    #[test]
    fn standard_run_task_envelope_is_exactly_backward_compatible() {
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
                    recognition: AlibabaAudio3Config::default().effective_recognition_controls(),
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
                recognition: EffectiveAudio3RecognitionControls {
                    max_sentence_silence_ms: 200,
                    semantic_punctuation_enabled: true,
                    multi_threshold_mode_enabled: false,
                    speech_noise_threshold: None,
                },
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
                .get("multi_threshold_mode_enabled")
                .is_none()
        );
        assert!(
            disabled["payload"]["parameters"]
                .get("speech_noise_threshold")
                .is_none()
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
        let custom_audio3 = AlibabaAudio3Config {
            recognition_preset: Audio3RecognitionPreset::Custom,
            max_sentence_silence_ms: 6_000,
            semantic_punctuation_enabled: false,
            multi_threshold_mode_enabled: true,
            speech_noise_threshold: Some(0.25),
            ..AlibabaAudio3Config::default()
        };
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
                recognition: custom_audio3.effective_recognition_controls(),
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
        assert_eq!(
            configured["payload"]["parameters"]["multi_threshold_mode_enabled"],
            true
        );
        assert_eq!(
            configured["payload"]["parameters"]["speech_noise_threshold"],
            0.25
        );
    }

    #[test]
    fn candidate_presets_send_only_their_effective_request_fields() {
        let mut audio3 = AlibabaAudio3Config {
            recognition_preset: Audio3RecognitionPreset::LowLatencyDictation,
            ..AlibabaAudio3Config::default()
        };
        let low_latency = run_task_envelope(
            TASK_ID,
            "model",
            AudioSpec {
                sample_rate_hz: 16_000,
            },
            Audio3RequestControls {
                language: Language::English,
                language_hints_enabled: false,
                heartbeat_enabled: false,
                recognition: audio3.effective_recognition_controls(),
                vocabulary: &[],
            },
        );
        let low_parameters = &low_latency["payload"]["parameters"];
        assert_eq!(low_parameters["max_sentence_silence"], 400);
        assert_eq!(low_parameters["semantic_punctuation_enabled"], false);
        assert_eq!(low_parameters["multi_threshold_mode_enabled"], true);
        assert!(low_parameters.get("speech_noise_threshold").is_none());

        audio3.recognition_preset = Audio3RecognitionPreset::LongForm;
        let long_form = run_task_envelope(
            TASK_ID,
            "model",
            AudioSpec {
                sample_rate_hz: 16_000,
            },
            Audio3RequestControls {
                language: Language::English,
                language_hints_enabled: false,
                heartbeat_enabled: false,
                recognition: audio3.effective_recognition_controls(),
                vocabulary: &[],
            },
        );
        let long_parameters = &long_form["payload"]["parameters"];
        assert_eq!(long_parameters["max_sentence_silence"], 1_300);
        assert_eq!(long_parameters["semantic_punctuation_enabled"], true);
        assert!(
            long_parameters
                .get("multi_threshold_mode_enabled")
                .is_none()
        );
        assert!(long_parameters.get("speech_noise_threshold").is_none());
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
                timestamp_summary: Audio3TimestampSummary::default(),
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
                timestamp_summary: Audio3TimestampSummary::default(),
            })
        );
    }

    #[test]
    fn parses_official_sentence_timing_shape_without_retaining_unit_text() {
        const WORD_TEXT_SENTINEL: &str = "private-word-text-sentinel";
        const PUNCTUATION_SENTINEL: &str = "private-punctuation-sentinel";
        let (partial_text, partial_final, partial_summary) = parse_result_sentence(json!({
            "sentence_id": 1,
            "begin_time": 100,
            "end_time": null,
            "text": "partial transcript",
            "sentence_end": false,
            "words": [
                {"begin_time": 100, "end_time": 180, "text": WORD_TEXT_SENTINEL},
                {"begin_time": 180, "end_time": 250, "punctuation": PUNCTUATION_SENTINEL}
            ]
        }));
        assert_eq!(partial_text, "partial transcript");
        assert!(!partial_final);
        assert_eq!(
            partial_summary,
            Audio3TimestampSummary {
                timestamp_bearing_result_count: 1,
                accepted_timed_unit_count: 2,
                result_with_rejected_timestamp_metadata_count: 0,
                truncated_timed_unit_count: 0,
                latest_valid_audio_end_ms: Some(250),
            }
        );

        let (final_text, final_sentence, final_summary) = parse_result_sentence(json!({
            "sentence_id": 1,
            "begin_time": 100,
            "end_time": 320,
            "text": "final transcript",
            "sentence_end": true,
            "words": [
                {"begin_time": 100, "end_time": 200, "text": WORD_TEXT_SENTINEL},
                {"begin_time": 200, "end_time": 320, "punctuation": PUNCTUATION_SENTINEL}
            ]
        }));
        assert_eq!(final_text, "final transcript");
        assert!(final_sentence);
        assert_eq!(final_summary.accepted_timed_unit_count, 2);
        assert_eq!(final_summary.latest_valid_audio_end_ms, Some(320));
        let telemetry_debug = format!(
            "{:?}{:?}",
            TimestampDiagnosticsDelta::from(partial_summary),
            TimestampDiagnosticsDelta::from(final_summary)
        );
        assert!(!telemetry_debug.contains(WORD_TEXT_SENTINEL));
        assert!(!telemetry_debug.contains(PUNCTUATION_SENTINEL));

        let raw = provider_event(
            "result-generated",
            json!({"output": {"sentence": {
                "sentence_id": 1,
                "begin_time": 100,
                "end_time": null,
                "text": "borrowed transcript",
                "sentence_end": false,
                "words": [{
                    "begin_time": 100,
                    "end_time": 180,
                    "text": WORD_TEXT_SENTINEL,
                    "punctuation": PUNCTUATION_SENTINEL
                }]
            }}}),
        );
        let Message::Text(raw) = raw else {
            panic!("provider event must be text");
        };
        let retained_words = {
            let envelope: BorrowedResultEnvelope<'_> = serde_json::from_str(&raw).unwrap();
            let mut sentence = envelope.payload.output.sentence.unwrap();
            assert!(matches!(sentence.text, Some(Cow::Borrowed(_))));
            sentence.words.take().unwrap()
        };
        drop(raw);
        let retained_debug = format!("{retained_words:?}");
        assert!(!retained_debug.contains(WORD_TEXT_SENTINEL));
        assert!(!retained_debug.contains(PUNCTUATION_SENTINEL));
    }

    #[test]
    fn partial_end_time_missing_is_rejected_but_explicit_null_allows_lower_bounded_units() {
        let (_, _, valid_null) = parse_result_sentence(json!({
            "sentence_id": 1,
            "begin_time": 100,
            "end_time": null,
            "text": "partial",
            "sentence_end": false,
            "words": [{"begin_time": 100, "end_time": 240}]
        }));
        assert_eq!(valid_null.accepted_timed_unit_count, 1);
        assert_eq!(valid_null.result_with_rejected_timestamp_metadata_count, 0);
        assert_eq!(valid_null.latest_valid_audio_end_ms, Some(240));

        let (_, _, missing) = parse_result_sentence(json!({
            "sentence_id": 1,
            "begin_time": 100,
            "text": "partial",
            "sentence_end": false,
            "words": [{"begin_time": 100, "end_time": 240}]
        }));
        assert_eq!(missing.accepted_timed_unit_count, 0);
        assert_eq!(missing.result_with_rejected_timestamp_metadata_count, 1);
        assert_eq!(missing.latest_valid_audio_end_ms, None);
    }

    #[test]
    fn partial_null_end_rejects_units_below_sentence_begin_without_an_upper_bound() {
        let (_, _, summary) = parse_result_sentence(json!({
            "sentence_id": 2,
            "begin_time": 100,
            "end_time": null,
            "text": "partial",
            "sentence_end": false,
            "words": [
                {"begin_time": 99, "end_time": 120},
                {"begin_time": 1_000_000, "end_time": 1_000_100}
            ]
        }));
        assert_eq!(summary.accepted_timed_unit_count, 1);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
        assert_eq!(summary.latest_valid_audio_end_ms, Some(1_000_100));
    }

    #[test]
    fn invalid_required_sentence_bounds_reject_all_units_and_preserve_text() {
        let (text, sentence_final, summary) = parse_result_sentence(json!({
            "sentence_id": 3,
            "begin_time": 500,
            "end_time": 100,
            "text": "preserved despite invalid bounds",
            "sentence_end": true,
            "words": [
                {"begin_time": 100, "end_time": 120},
                {"begin_time": 120, "end_time": 140}
            ]
        }));
        assert_eq!(text, "preserved despite invalid bounds");
        assert!(sentence_final);
        assert_eq!(summary.accepted_timed_unit_count, 0);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
        assert_eq!(summary.latest_valid_audio_end_ms, None);
    }

    #[test]
    fn many_timestamp_defects_increment_rejected_result_count_exactly_once() {
        let (text, _, summary) = parse_result_sentence(json!({
            "sentence_id": 0,
            "begin_time": "invalid-begin",
            "end_time": null,
            "text": "unchanged malformed result",
            "sentence_end": true,
            "words": [
                {"begin_time": -1, "end_time": 2},
                {"begin_time": 5, "end_time": 4},
                "invalid-unit-shape"
            ]
        }));
        assert_eq!(text, "unchanged malformed result");
        assert_eq!(summary.timestamp_bearing_result_count, 1);
        assert_eq!(summary.accepted_timed_unit_count, 0);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
    }

    #[test]
    fn duplicate_sentence_timing_fields_keep_first_and_reject_once_in_either_order() {
        let cases = [
            r#""sentence_id":"private-invalid","sentence_id":7,"begin_time":10,"end_time":20,"text":"preserved transcript","sentence_end":true"#,
            r#""sentence_id":7,"sentence_id":{"private":"invalid"},"begin_time":10,"end_time":20,"text":"preserved transcript","sentence_end":true"#,
            r#""sentence_id":7,"begin_time":"private-invalid","begin_time":10,"end_time":20,"text":"preserved transcript","sentence_end":true"#,
            r#""sentence_id":7,"begin_time":10,"begin_time":{"private":"invalid"},"end_time":20,"text":"preserved transcript","sentence_end":true"#,
            r#""sentence_id":7,"begin_time":10,"end_time":"private-invalid","end_time":20,"text":"preserved transcript","sentence_end":true"#,
            r#""sentence_id":7,"begin_time":10,"end_time":20,"end_time":{"private":"invalid"},"text":"preserved transcript","sentence_end":true"#,
        ];

        for fields in cases {
            let (text, sentence_final, summary) = parse_raw_result_sentence(fields);
            assert_eq!(text, "preserved transcript");
            assert!(sentence_final);
            assert_eq!(summary.timestamp_bearing_result_count, 1);
            assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
        }
    }

    #[test]
    fn duplicate_words_keep_only_first_array_and_handle_truncation_deterministically() {
        let words = (0..=MAX_TIMED_UNITS_PER_RESULT)
            .map(|index| {
                let begin = u64::try_from(index).unwrap().saturating_mul(2);
                json!({"begin_time": begin, "end_time": begin + 1})
            })
            .collect::<Vec<_>>();
        let words = serde_json::to_string(&words).unwrap();

        let first_array = format!(
            r#""sentence_id":7,"begin_time":0,"end_time":2000,"words":{words},"words":{{"private":"ignored"}},"text":"preserved transcript","sentence_end":true"#
        );
        let (text, _, summary) = parse_raw_result_sentence(&first_array);
        assert_eq!(text, "preserved transcript");
        assert_eq!(
            summary.accepted_timed_unit_count,
            u64::try_from(MAX_TIMED_UNITS_PER_RESULT).unwrap()
        );
        assert_eq!(summary.truncated_timed_unit_count, 1);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);

        let first_invalid = format!(
            r#""sentence_id":7,"begin_time":0,"end_time":2000,"words":{{"private":"invalid"}},"words":{words},"text":"preserved transcript","sentence_end":true"#
        );
        let (text, _, summary) = parse_raw_result_sentence(&first_invalid);
        assert_eq!(text, "preserved transcript");
        assert_eq!(summary.accepted_timed_unit_count, 0);
        assert_eq!(summary.truncated_timed_unit_count, 0);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
    }

    #[test]
    fn duplicate_timed_unit_bounds_invalidate_units_in_either_order() {
        let units = [
            r#"{"begin_time":"private-invalid","begin_time":10,"end_time":20}"#,
            r#"{"begin_time":10,"begin_time":{"private":"invalid"},"end_time":20}"#,
            r#"{"begin_time":10,"end_time":"private-invalid","end_time":20}"#,
            r#"{"begin_time":10,"end_time":20,"end_time":{"private":"invalid"}}"#,
        ];

        for unit in units {
            let fields = format!(
                r#""sentence_id":7,"begin_time":0,"end_time":100,"words":[{unit}],"text":"preserved transcript","sentence_end":true"#
            );
            let (text, sentence_final, summary) = parse_raw_result_sentence(&fields);
            assert_eq!(text, "preserved transcript");
            assert!(sentence_final);
            assert_eq!(summary.accepted_timed_unit_count, 0);
            assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
        }
    }

    #[test]
    fn duplicate_core_result_fields_are_sanitized_protocol_errors_in_either_order() {
        const FIRST_SENTINEL: &str = "private-first-core-value";
        const SECOND_SENTINEL: &str = "private-second-core-value";
        let cases = [
            r#""text":"private-first-core-value","text":"private-second-core-value","sentence_end":false"#,
            r#""text":"private-second-core-value","text":"private-first-core-value","sentence_end":false"#,
            r#""text":"transcript","heartbeat":false,"heartbeat":true,"sentence_end":false"#,
            r#""text":"transcript","heartbeat":true,"heartbeat":false,"sentence_end":false"#,
            r#""text":"transcript","sentence_end":false,"sentence_end":true"#,
            r#""text":"transcript","sentence_end":true,"sentence_end":false"#,
        ];

        for fields in cases {
            let error = parse_server_event(raw_result_sentence(fields), TASK_ID).unwrap_err();
            assert_eq!(
                error.to_string(),
                "failed to parse Qwen-Audio-3 result-generated event"
            );
            let error = format!("{error:#}");
            assert!(!error.contains(FIRST_SENTINEL));
            assert!(!error.contains(SECOND_SENTINEL));
            assert!(!error.contains("transcript"));
        }
    }

    #[test]
    fn duplicate_unknown_sentence_and_timed_unit_fields_remain_ignored() {
        let (text, _, summary) = parse_raw_result_sentence(
            r#""sentence_id":7,"begin_time":10,"end_time":20,"unknown":{"private":1},"unknown":[2],"words":[{"begin_time":10,"end_time":20,"unknown":1,"unknown":{"private":2}}],"text":"transcript","sentence_end":true"#,
        );
        assert_eq!(text, "transcript");
        assert_eq!(summary.accepted_timed_unit_count, 1);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 0);
    }

    #[test]
    fn result_larger_than_64_kib_preserves_text_and_semantically_truncates_units() {
        const UNIT_COUNT: usize = 3_000;
        let words = (0..UNIT_COUNT)
            .map(|index| {
                let begin = u64::try_from(index).unwrap().saturating_mul(2);
                json!({"begin_time": begin, "end_time": begin + 1})
            })
            .collect::<Vec<_>>();
        let message = provider_event(
            "result-generated",
            json!({"output": {"sentence": {
                "sentence_id": 9,
                "begin_time": 0,
                "end_time": 10_000,
                "text": "large timing array keeps this transcript",
                "sentence_end": true,
                "words": words
            }}}),
        );
        let Message::Text(raw) = &message else {
            panic!("provider event must be text");
        };
        assert!(raw.len() > 64 * 1024);
        assert!(raw.len() < MAX_SERVER_MESSAGE_BYTES);

        let Some(ServerEvent::ResultGenerated {
            text,
            timestamp_summary,
            ..
        }) = parse_server_event(message, TASK_ID).unwrap()
        else {
            panic!("expected result-generated event");
        };
        assert_eq!(text, "large timing array keeps this transcript");
        assert_eq!(timestamp_summary.accepted_timed_unit_count, 512);
        assert_eq!(
            timestamp_summary.truncated_timed_unit_count,
            u64::try_from(UNIT_COUNT - MAX_TIMED_UNITS_PER_RESULT).unwrap()
        );
        assert_eq!(
            timestamp_summary.result_with_rejected_timestamp_metadata_count,
            0
        );
    }

    #[test]
    fn complete_messages_beyond_transport_cap_are_protocol_errors_without_raw_values() {
        const SENTINEL: &str = "oversize-private-raw-value-sentinel";
        let message = provider_event(
            "result-generated",
            json!({"output": {"text": format!(
                "{}{}",
                SENTINEL,
                "x".repeat(MAX_SERVER_MESSAGE_BYTES)
            )}}),
        );
        let error = parse_server_event(message, TASK_ID).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Qwen-Audio-3 server event exceeds the transport size limit"
        );
        assert!(!error.to_string().contains(SENTINEL));
    }

    #[test]
    fn rejects_invalid_timestamp_scalar_types_without_changing_text() {
        for invalid_sentence_id in [json!(0), json!(-1), json!(1.5), json!("1")] {
            let (text, sentence_final, summary) = parse_result_sentence(json!({
                "sentence_id": invalid_sentence_id,
                "begin_time": 10,
                "end_time": null,
                "text": "unchanged partial",
                "sentence_end": false
            }));
            assert_eq!(text, "unchanged partial");
            assert!(!sentence_final);
            assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
        }

        for invalid_begin in [json!(-1), json!(10.25), json!("10"), json!(true)] {
            let (text, _, summary) = parse_result_sentence(json!({
                "sentence_id": 1,
                "begin_time": invalid_begin,
                "end_time": null,
                "text": "unchanged partial",
                "sentence_end": false
            }));
            assert_eq!(text, "unchanged partial");
            assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
        }

        let overflow: serde_json::Value =
            serde_json::from_str("18446744073709551616").expect("valid overflowing JSON number");
        assert!(overflow.as_u64().is_none());
        let (text, _, overflow_summary) = parse_result_sentence(json!({
            "sentence_id": 1,
            "begin_time": 10,
            "end_time": overflow,
            "text": "unchanged final",
            "sentence_end": true
        }));
        assert_eq!(text, "unchanged final");
        assert_eq!(
            overflow_summary.result_with_rejected_timestamp_metadata_count,
            1
        );

        let (_, _, wrong_end_summary) = parse_result_sentence(json!({
            "sentence_id": 1,
            "begin_time": 10,
            "end_time": {"wrong": "type"},
            "text": "unchanged final",
            "sentence_end": true
        }));
        assert_eq!(
            wrong_end_summary.result_with_rejected_timestamp_metadata_count,
            1
        );
    }

    #[test]
    fn validates_sentence_and_event_local_unit_ranges_only() {
        let (_, _, reversed_sentence) = parse_result_sentence(json!({
            "sentence_id": 3,
            "begin_time": 500,
            "end_time": 100,
            "text": "sentence",
            "sentence_end": true
        }));
        assert_eq!(
            reversed_sentence.result_with_rejected_timestamp_metadata_count,
            1
        );
        assert_eq!(reversed_sentence.latest_valid_audio_end_ms, None);

        let (_, _, summary) = parse_result_sentence(json!({
            "sentence_id": 3,
            "begin_time": 100,
            "end_time": 500,
            "text": "sentence",
            "sentence_end": true,
            "words": [
                {"begin_time": 100, "end_time": 150},
                {"begin_time": 140, "end_time": 160},
                {"begin_time": 90, "end_time": 99},
                {"begin_time": 200, "end_time": 190},
                {"begin_time": 490, "end_time": 510},
                {"begin_time": 160, "end_time": 200}
            ]
        }));
        assert_eq!(summary.accepted_timed_unit_count, 2);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);
        assert_eq!(summary.latest_valid_audio_end_ms, Some(500));

        let (_, _, wrong_words_type) = parse_result_sentence(json!({
            "sentence_id": 3,
            "begin_time": 100,
            "end_time": null,
            "text": "sentence",
            "sentence_end": false,
            "words": "not-an-array"
        }));
        assert_eq!(
            wrong_words_type.result_with_rejected_timestamp_metadata_count,
            1
        );
    }

    #[test]
    fn timed_units_are_bounded_and_excess_entries_are_only_counted_as_truncated() {
        let mut words = (0..MAX_TIMED_UNITS_PER_RESULT)
            .map(|index| {
                let begin = u64::try_from(index).unwrap().saturating_mul(2);
                json!({"begin_time": begin, "end_time": begin.saturating_add(1)})
            })
            .collect::<Vec<_>>();
        words.push(json!({
            "begin_time": "unprocessed-private-sentinel",
            "end_time": "unprocessed-private-sentinel"
        }));
        let (_, _, summary) = parse_result_sentence(json!({
            "sentence_id": 4,
            "begin_time": 0,
            "end_time": 2_000,
            "text": "bounded transcript",
            "sentence_end": true,
            "words": words
        }));

        assert_eq!(summary.timestamp_bearing_result_count, 1);
        assert_eq!(summary.accepted_timed_unit_count, 512);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 0);
        assert_eq!(summary.truncated_timed_unit_count, 1);
        assert_eq!(summary.latest_valid_audio_end_ms, Some(2_000));
    }

    #[test]
    fn malformed_or_missing_final_timing_preserves_exact_transcript_result() {
        const TRANSCRIPT: &str = "verbatim private transcript sentinel";
        let (text, sentence_final, summary) = parse_result_sentence(json!({
            "sentence_id": 7,
            "begin_time": 20,
            "end_time": null,
            "text": TRANSCRIPT,
            "sentence_end": true,
            "words": [{"begin_time": 40.5, "end_time": 90}]
        }));
        assert_eq!(text, TRANSCRIPT);
        assert!(sentence_final);
        assert_eq!(summary.result_with_rejected_timestamp_metadata_count, 1);

        let (_, _, missing_end_summary) = parse_result_sentence(json!({
            "sentence_id": 7,
            "begin_time": 20,
            "text": TRANSCRIPT,
            "sentence_end": true
        }));
        assert_eq!(
            missing_end_summary.result_with_rejected_timestamp_metadata_count,
            1
        );
    }

    #[test]
    fn mixed_timed_and_untimed_revisions_leave_assembly_and_authoritative_output_unchanged() {
        let mut assembler = TranscriptAssembler::default();
        let (timed_partial, partial_final, _) = parse_result_sentence(json!({
            "sentence_id": 10,
            "begin_time": 0,
            "end_time": null,
            "text": "draft",
            "sentence_end": false,
            "words": [{"begin_time": 0, "end_time": 100}]
        }));
        assert!(!partial_final);
        assert_eq!(
            assembler.apply_partial(timed_partial),
            (String::new(), "draft".into())
        );
        let (untimed_final, sentence_final, summary) = parse_result_sentence(json!({
            "text": "revised final",
            "sentence_end": true
        }));
        assert!(sentence_final);
        assert_eq!(summary, Audio3TimestampSummary::default());
        assert_eq!(
            assembler.apply_segment_final(untimed_final),
            "revised final"
        );
        assert_eq!(
            assembler.finish(Some("authoritative task output".into())),
            "authoritative task output"
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
    fn typed_result_parse_errors_do_not_expose_raw_values() {
        const SENTINEL: &str = "private-invalid-result-value-sentinel";
        let event = provider_event(
            "result-generated",
            json!({"output": {"sentence": {
                "text": {"private": SENTINEL},
                "sentence_end": false
            }}}),
        );
        let error = parse_server_event(event, TASK_ID).unwrap_err();
        assert_eq!(
            error.to_string(),
            "failed to parse Qwen-Audio-3 result-generated event"
        );
        assert!(!format!("{error:#}").contains(SENTINEL));
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
