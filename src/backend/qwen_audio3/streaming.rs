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
    config::Config,
};

type Audio3Socket = WebSocket<MaybeTlsStream<TcpStream>>;
type HandshakeResponse = tungstenite::http::Response<Option<Vec<u8>>>;
type Connection = (Audio3Socket, HandshakeResponse);

const MAX_CONTROLS_PER_TICK: usize = 8;
const MAX_SERVER_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024;
const MAX_ERROR_BYTES: usize = 1_024;
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
    let connect_deadline = Instant::now() + connect_timeout;
    let mut socket = open_socket(&audio3.endpoint, &audio3.api_key, connect_timeout)?;
    configure_socket(socket.get_mut())?;
    send_json(
        &mut socket,
        run_task_envelope(&task_id, &audio3.model, spec),
    )?;
    await_task_started(
        &mut socket,
        &task_id,
        connect_deadline,
        &abort_flag,
        &event_tx,
    )?;

    if abort_flag.load(Ordering::SeqCst) {
        let _ = socket.close(None);
        return Ok(());
    }
    let _ = event_tx.send(AsrEvent::Ready);

    let mut finish_sent = false;
    let mut finalize_deadline = None;
    let mut assembler = TranscriptAssembler::default();

    loop {
        if abort_flag.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            return Ok(());
        }

        if !finish_sent {
            for _ in 0..MAX_CONTROLS_PER_TICK {
                match control_rx.try_recv() {
                    Ok(AsrControl::AppendPcm16(samples)) => {
                        socket
                            .send(Message::Binary(pcm16_le_bytes(&samples)))
                            .context("failed to send Qwen-Audio-3 PCM")?;
                    }
                    Ok(AsrControl::Finish) => {
                        send_json(&mut socket, finish_task_envelope(&task_id))?;
                        finish_sent = true;
                        finalize_deadline = Some(
                            Instant::now() + Duration::from_millis(config.asr.finalize_timeout_ms),
                        );
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        bail!("Qwen-Audio-3 control channel disconnected before finish")
                    }
                }
            }
        }

        match socket.read() {
            Ok(Message::Close(_)) => {
                bail!("Qwen-Audio-3 websocket closed before task completion")
            }
            Ok(message) => {
                if let Some(event) = parse_server_event(message, &task_id)? {
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
                            let _ = socket.close(None);
                            return Ok(());
                        }
                        ServerEvent::TaskFailed { message } => {
                            let _ = event_tx.send(AsrEvent::Error {
                                message: message.clone(),
                            });
                            bail!("Qwen-Audio-3 task failed: {message}");
                        }
                    }
                }
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                bail!("Qwen-Audio-3 connection closed before task completion")
            }
            Err(error) => return Err(error).context("failed to read Qwen-Audio-3 websocket"),
        }

        if finalize_deadline.is_some_and(|deadline| Instant::now() > deadline) {
            let _ = socket.close(None);
            bail!("Qwen-Audio-3 finalization timed out");
        }
    }
}

fn await_task_started(
    socket: &mut Audio3Socket,
    task_id: &str,
    deadline: Instant,
    abort_flag: &AtomicBool,
    event_tx: &mpsc::Sender<AsrEvent>,
) -> Result<()> {
    loop {
        if abort_flag.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            return Ok(());
        }
        if Instant::now() > deadline {
            let _ = socket.close(None);
            bail!("Qwen-Audio-3 task-started timed out");
        }

        match socket.read() {
            Ok(Message::Close(_)) => {
                bail!("Qwen-Audio-3 websocket closed before task-started")
            }
            Ok(message) => match parse_server_event(message, task_id)? {
                Some(ServerEvent::TaskStarted) => return Ok(()),
                Some(ServerEvent::TaskFailed { message }) => {
                    let _ = event_tx.send(AsrEvent::Error {
                        message: message.clone(),
                    });
                    bail!("Qwen-Audio-3 task failed: {message}");
                }
                Some(_) => bail!("Qwen-Audio-3 server event arrived before task-started"),
                None => {}
            },
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                bail!("Qwen-Audio-3 connection closed before task-started")
            }
            Err(error) => return Err(error).context("failed to read Qwen-Audio-3 websocket"),
        }
    }
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

fn send_json(socket: &mut Audio3Socket, payload: Value) -> Result<()> {
    socket
        .send(Message::Text(payload.to_string()))
        .context("failed to send Qwen-Audio-3 websocket event")
}

fn run_task_envelope(task_id: &str, model: &str, spec: AudioSpec) -> Value {
    json!({
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
                "sample_rate": spec.sample_rate_hz
            }
        }
    })
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
    ResultGenerated { text: String, sentence_final: bool },
    TaskFinished { text: Option<String> },
    TaskFailed { message: String },
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
            message: extract_failure_message(&payload),
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

fn extract_failure_message(payload: &Value) -> String {
    let message = payload
        .pointer("/payload/message")
        .or_else(|| payload.pointer("/header/error_message"))
        .and_then(Value::as_str)
        .unwrap_or("task failed");
    bounded(message, MAX_ERROR_BYTES).to_string()
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

    use super::{
        AudioSpec, MAX_ERROR_BYTES, ServerEvent, TranscriptAssembler, finish_task_envelope,
        new_task_id, parse_server_event, pcm16_le_bytes, run_task_envelope, websocket_request,
    };

    const TASK_ID: &str = "0123456789abcdef0123456789abcdef";

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
                        "sample_rate": 16_000
                    }
                }
            })
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
    fn parses_and_bounds_task_failure() {
        let server_message = "x".repeat(MAX_ERROR_BYTES + 50);
        let event = Message::Text(
            json!({
                "header": {"event": "task-failed", "task_id": TASK_ID},
                "payload": {"message": server_message}
            })
            .to_string(),
        );
        let Some(ServerEvent::TaskFailed { message }) = parse_server_event(event, TASK_ID).unwrap()
        else {
            panic!("expected task failure");
        };
        assert_eq!(message.len(), MAX_ERROR_BYTES);
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
