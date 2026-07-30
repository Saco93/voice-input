use std::{
    net::{TcpStream, ToSocketAddrs},
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tungstenite::{
    Message, WebSocket,
    client::IntoClientRequest,
    client_tls_with_config,
    http::{HeaderValue, header::AUTHORIZATION},
    stream::MaybeTlsStream,
};
use url::Url;

use crate::{
    backend::{
        ASR_CONTROL_QUEUE_CAPACITY, AsrBackend, AsrControl, AsrEvent, AsrSessionHandle, AudioSpec,
    },
    config::{AlibabaTurnMode, Config},
};

type QwenSocket = WebSocket<MaybeTlsStream<TcpStream>>;
type QwenHandshakeResponse = tungstenite::http::Response<Option<Vec<u8>>>;
type QwenConnection = (QwenSocket, QwenHandshakeResponse);

pub struct QwenRealtimeBackend;

impl QwenRealtimeBackend {
    pub fn new() -> Self {
        Self
    }
}

impl AsrBackend for QwenRealtimeBackend {
    fn spawn_session(&self, config: &Config, spec: AudioSpec) -> Result<AsrSessionHandle> {
        let (control_tx, control_rx) = mpsc::sync_channel(ASR_CONTROL_QUEUE_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel();
        let config = config.clone();

        let join = thread::spawn(move || run_session(config, spec, control_rx, event_tx));

        Ok(AsrSessionHandle {
            control_tx,
            event_rx,
            join,
        })
    }

    fn transcribe_file(&self, _config: &Config, _wav_path: &Path) -> Result<String> {
        bail!("qwen realtime backend does not support file transcription")
    }
}

fn run_session(
    config: Config,
    spec: AudioSpec,
    control_rx: mpsc::Receiver<AsrControl>,
    event_tx: mpsc::Sender<AsrEvent>,
) -> Result<()> {
    let alibaba = &config.asr.alibaba;
    if alibaba.api_key.trim().is_empty() {
        bail!("Alibaba realtime ASR requires an API key");
    }

    let url = build_url(alibaba.endpoint.as_str(), alibaba.model.as_str())?;
    let mut request = url
        .as_str()
        .into_client_request()
        .context("failed to build Qwen realtime websocket request")?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("bearer {}", alibaba.api_key.trim()))
            .context("Alibaba API key contains an invalid header value")?,
    );

    let (mut socket, _) = connect_with_timeout(
        request,
        Duration::from_millis(config.asr.connect_timeout_ms.max(1_000)),
    )?;
    configure_socket(socket.get_mut())?;

    let mut next_event_id = 1_u64;
    send_json(
        &mut socket,
        session_update_payload(&config, spec, format!("event-{}", next_event_id).as_str()),
    )?;
    next_event_id += 1;

    let mut ready = false;
    let mut finish_requested = false;
    let mut waiting_for_commit = false;
    let mut finalize_deadline = None;
    let mut assembler = TranscriptAssembler::default();
    let session_started = Instant::now();
    let mut partial_event_count = 0_u64;
    let mut completed_event_count = 0_u64;
    let mut speech_started_count = 0_u64;
    let mut speech_stopped_count = 0_u64;
    let mut appended_sample_count = 0_u64;
    let mut noise_gate = PcmNoiseGate::new((spec.sample_rate_hz as usize) / 2);
    let mut samples_since_commit = 0_usize;
    let mut locally_voiced_samples_since_commit = 0_usize;
    let mut forced_commit_count = 0_u64;
    let mut forced_commit_in_flight = false;
    let mut last_transcription_activity = Instant::now();

    loop {
        while let Ok(control) = control_rx.try_recv() {
            match control {
                AsrControl::AppendPcm16(samples) => {
                    if finish_requested {
                        continue;
                    }
                    let has_local_speech = pcm_chunk_has_local_speech(&samples);
                    let filtered_samples = noise_gate.filter(samples);
                    send_json(
                        &mut socket,
                        json!({
                            "event_id": format!("event-{}", next_event_id),
                            "type": "input_audio_buffer.append",
                            "audio": encode_pcm16_chunk(&filtered_samples),
                        }),
                    )?;
                    appended_sample_count += filtered_samples.len() as u64;
                    samples_since_commit += filtered_samples.len();
                    if has_local_speech {
                        locally_voiced_samples_since_commit += filtered_samples.len();
                    }
                    next_event_id += 1;
                }
                AsrControl::Finish => {
                    if finish_requested {
                        continue;
                    }
                    finish_requested = true;
                    finalize_deadline = Some(
                        Instant::now()
                            + Duration::from_millis(config.asr.finalize_timeout_ms.max(1_000)),
                    );

                    match config.asr.alibaba.turn_mode {
                        AlibabaTurnMode::ServerVad => {
                            if forced_commit_in_flight {
                                waiting_for_commit = true;
                            } else {
                                send_json(
                                    &mut socket,
                                    json!({
                                        "event_id": format!("event-{}", next_event_id),
                                        "type": "session.finish",
                                    }),
                                )?;
                                next_event_id += 1;
                            }
                        }
                        AlibabaTurnMode::Manual => {
                            waiting_for_commit = true;
                            send_json(
                                &mut socket,
                                json!({
                                    "event_id": format!("event-{}", next_event_id),
                                    "type": "input_audio_buffer.commit",
                                }),
                            )?;
                            next_event_id += 1;
                        }
                    }
                }
                AsrControl::Cancel => {
                    let _ = socket.close(None);
                    return Ok(());
                }
            }
        }

        match socket.read() {
            Ok(message) => {
                if let Some(event) = parse_server_event(message)? {
                    match event {
                        ServerEvent::SessionCreated => {}
                        ServerEvent::SessionUpdated => {
                            if !ready {
                                let _ = event_tx.send(AsrEvent::Ready);
                                ready = true;
                            }
                        }
                        ServerEvent::SpeechStarted => {
                            if noise_gate.speech_observed() {
                                speech_started_count += 1;
                                let _ = event_tx.send(AsrEvent::SpeechStarted);
                            }
                        }
                        ServerEvent::SpeechStopped => {
                            if noise_gate.speech_observed() {
                                speech_stopped_count += 1;
                                let _ = event_tx.send(AsrEvent::SpeechStopped);
                            }
                        }
                        ServerEvent::InputCommitted => {
                            samples_since_commit = 0;
                            locally_voiced_samples_since_commit = 0;
                            forced_commit_in_flight = false;
                            if waiting_for_commit {
                                waiting_for_commit = false;
                                send_json(
                                    &mut socket,
                                    json!({
                                        "event_id": format!("event-{}", next_event_id),
                                        "type": "session.finish",
                                    }),
                                )?;
                                next_event_id += 1;
                            }
                        }
                        ServerEvent::Partial {
                            item_id,
                            text,
                            stash,
                        } => {
                            if noise_gate.speech_observed() {
                                partial_event_count += 1;
                                last_transcription_activity = Instant::now();
                                if partial_event_count == 1 {
                                    eprintln!(
                                        "voice-input realtime ASR: first partial after {} ms",
                                        session_started.elapsed().as_millis()
                                    );
                                }
                                let (committed, unstable) =
                                    assembler.apply_partial(item_id, text, stash);
                                let _ = event_tx.send(AsrEvent::Partial {
                                    committed,
                                    unstable,
                                });
                            }
                        }
                        ServerEvent::Completed {
                            item_id,
                            transcript,
                        } => {
                            if noise_gate.speech_observed() {
                                completed_event_count += 1;
                                last_transcription_activity = Instant::now();
                                let text = assembler.apply_completed(item_id, transcript);
                                let _ = event_tx.send(AsrEvent::SegmentFinal { text });
                            }
                        }
                        ServerEvent::TranscriptionFailed { message } => {
                            let _ = event_tx.send(AsrEvent::Error {
                                message: message.clone(),
                            });
                            bail!("Alibaba realtime transcription failed: {message}");
                        }
                        ServerEvent::Error { message } => {
                            let _ = event_tx.send(AsrEvent::Error {
                                message: message.clone(),
                            });
                            bail!("Alibaba realtime ASR error: {message}");
                        }
                        ServerEvent::SessionFinished => {
                            eprintln!(
                                "voice-input realtime ASR: session finished with {partial_event_count} partial, {completed_event_count} completed, {speech_started_count} speech-started, {speech_stopped_count} speech-stopped, {forced_commit_count} forced-commit events, {appended_sample_count} appended samples, {} noise-gate openings, and {} suppressed samples",
                                noise_gate.reopen_count(),
                                noise_gate.suppressed_sample_count()
                            );
                            let final_text = assembler.final_text();
                            if !final_text.is_empty() {
                                let _ = event_tx.send(AsrEvent::Final { text: final_text });
                            }
                            let _ = socket.close(None);
                            return Ok(());
                        }
                    }
                }
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                eprintln!(
                    "voice-input realtime ASR: connection closed with {partial_event_count} partial, {completed_event_count} completed, {speech_started_count} speech-started, {speech_stopped_count} speech-stopped, {forced_commit_count} forced-commit events, {appended_sample_count} appended samples, {} noise-gate openings, and {} suppressed samples",
                    noise_gate.reopen_count(),
                    noise_gate.suppressed_sample_count()
                );
                let final_text = assembler.final_text();
                if finish_requested && !final_text.is_empty() {
                    let _ = event_tx.send(AsrEvent::Final { text: final_text });
                }
                return Ok(());
            }
            Err(error) => {
                let _ = event_tx.send(AsrEvent::Error {
                    message: error.to_string(),
                });
                return Err(error).context("Alibaba realtime websocket failed");
            }
        }

        // Qwen's server VAD occasionally accepts initial audio without any
        // speech or transcription event. Allow one recovery commit only before
        // the server has demonstrated normal VAD/transcription activity. A
        // commit after an established segment can prevent a later speech turn
        // from producing partials until session.finish.
        let forced_commit_min_samples = (spec.sample_rate_hz as usize) * 3;
        let forced_commit_min_voiced_samples = (spec.sample_rate_hz as usize) / 2;
        if should_force_initial_commit(
            finish_requested,
            matches!(config.asr.alibaba.turn_mode, AlibabaTurnMode::ServerVad),
            forced_commit_in_flight,
            forced_commit_count > 0,
            speech_started_count > 0 || partial_event_count > 0 || completed_event_count > 0,
            samples_since_commit >= forced_commit_min_samples
                && locally_voiced_samples_since_commit >= forced_commit_min_voiced_samples,
            last_transcription_activity.elapsed() >= Duration::from_secs(3),
        ) {
            send_json(
                &mut socket,
                json!({
                    "event_id": format!("event-{}", next_event_id),
                    "type": "input_audio_buffer.commit",
                }),
            )?;
            next_event_id += 1;
            forced_commit_count += 1;
            forced_commit_in_flight = true;
            last_transcription_activity = Instant::now();
            // The local gate has observed sustained speech even though the
            // server has emitted no VAD or transcript event. Preserve that
            // fact so stopping the session performs full-audio recovery
            // instead of discarding it as empty.
            let _ = event_tx.send(AsrEvent::RealtimeTranscriptDelayed);
            eprintln!(
                "voice-input realtime ASR: forced buffered-audio commit after transcription inactivity"
            );
        }

        if let Some(deadline) = finalize_deadline
            && Instant::now() > deadline
        {
            bail!("Alibaba realtime ASR finalize timed out");
        }

        if !finish_requested && !ready {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

struct PcmNoiseGate {
    hangover_samples: usize,
    remaining_hangover_samples: usize,
    speech_observed: bool,
    reopen_count: u64,
    suppressed_sample_count: u64,
}

impl PcmNoiseGate {
    fn new(hangover_samples: usize) -> Self {
        Self {
            hangover_samples,
            remaining_hangover_samples: 0,
            speech_observed: false,
            reopen_count: 0,
            suppressed_sample_count: 0,
        }
    }

    fn filter(&mut self, mut samples: Vec<i16>) -> Vec<i16> {
        // The measured idle microphone RMS peaks around 0.0015 of full scale
        // on the target hardware. Open at roughly 0.0022 so quiet speech after
        // a long pause can restart realtime ASR, then retain half a second of
        // quiet audio for soft endings and server-VAD stop detection.
        const NOISE_GATE_OPEN_RMS: i64 = 72;
        if pcm_chunk_exceeds_rms(&samples, NOISE_GATE_OPEN_RMS) {
            if self.remaining_hangover_samples == 0 {
                self.reopen_count += 1;
            }
            self.remaining_hangover_samples = self.hangover_samples;
            self.speech_observed = true;
        } else if self.remaining_hangover_samples > 0 {
            self.remaining_hangover_samples = self
                .remaining_hangover_samples
                .saturating_sub(samples.len());
        } else {
            self.suppressed_sample_count += samples.len() as u64;
            samples.fill(0);
        }
        samples
    }

    fn speech_observed(&self) -> bool {
        self.speech_observed
    }

    fn reopen_count(&self) -> u64 {
        self.reopen_count
    }

    fn suppressed_sample_count(&self) -> u64 {
        self.suppressed_sample_count
    }
}

fn pcm_chunk_exceeds_rms(samples: &[i16], minimum_rms: i64) -> bool {
    if samples.is_empty() {
        return false;
    }

    // Integer squared energy avoids an extra normalization pass over every ASR
    // chunk and remains well within i64 for the capture packet sizes in use.
    let squared_energy: i64 = samples.iter().map(|sample| i64::from(*sample).pow(2)).sum();
    squared_energy >= minimum_rms.pow(2) * samples.len() as i64
}

fn pcm_chunk_has_local_speech(samples: &[i16]) -> bool {
    // This stricter threshold gates forced-commit recovery; opening the audio
    // noise gate itself uses a lower threshold to preserve quiet speech.
    const LOCAL_SPEECH_RMS: i64 = 197; // approximately 0.006 of i16 full scale
    pcm_chunk_exceeds_rms(samples, LOCAL_SPEECH_RMS)
}

fn should_force_initial_commit(
    finish_requested: bool,
    server_vad: bool,
    forced_commit_in_flight: bool,
    has_forced_commit: bool,
    has_server_activity: bool,
    has_recovery_audio: bool,
    inactive_long_enough: bool,
) -> bool {
    !finish_requested
        && server_vad
        && !forced_commit_in_flight
        && !has_forced_commit
        && !has_server_activity
        && has_recovery_audio
        && inactive_long_enough
}

fn send_json(socket: &mut QwenSocket, payload: Value) -> Result<()> {
    socket
        .send(Message::Text(payload.to_string()))
        .context("failed to send websocket event")
}

fn build_url(endpoint: &str, model: &str) -> Result<Url> {
    let mut url =
        Url::parse(endpoint).with_context(|| format!("invalid ASR endpoint `{endpoint}`"))?;
    let has_model = url.query_pairs().any(|(key, _)| key == "model");
    if !has_model {
        url.query_pairs_mut().append_pair("model", model);
    }
    Ok(url)
}

fn session_update_payload(config: &Config, spec: AudioSpec, event_id: &str) -> Value {
    let turn_detection = match config.asr.alibaba.turn_mode {
        AlibabaTurnMode::ServerVad => json!({
            "type": "server_vad",
            "threshold": config.asr.alibaba.vad_threshold,
            "silence_duration_ms": config.asr.alibaba.silence_duration_ms,
        }),
        AlibabaTurnMode::Manual => Value::Null,
    };

    json!({
        "event_id": event_id,
        "type": "session.update",
        "session": {
            "input_audio_format": "pcm",
            "sample_rate": spec.sample_rate_hz,
            "input_audio_transcription": {
                "language": config.asr.language.asr_code(),
            },
            "turn_detection": turn_detection,
        }
    })
}

fn encode_pcm16_chunk(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    STANDARD.encode(bytes)
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

fn connect_with_timeout(
    request: tungstenite::http::Request<()>,
    timeout: Duration,
) -> Result<QwenConnection> {
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
        .context("failed to resolve Alibaba realtime ASR endpoint")?
        .next()
        .ok_or_else(|| anyhow!("Alibaba realtime ASR endpoint resolved to no addresses"))?;
    let stream = TcpStream::connect_timeout(&address, timeout)
        .with_context(|| format!("failed to connect to Alibaba realtime ASR at {host}:{port}"))?;
    stream.set_nodelay(true).ok();

    match client_tls_with_config(request, stream, None, None) {
        Ok(value) => Ok(value),
        Err(tungstenite::HandshakeError::Failure(error)) => Err(error.into()),
        Err(tungstenite::HandshakeError::Interrupted(_)) => {
            bail!("unexpected interrupted websocket handshake")
        }
    }
}

#[derive(Debug)]
enum ServerEvent {
    SessionCreated,
    SessionUpdated,
    SpeechStarted,
    SpeechStopped,
    InputCommitted,
    Partial {
        item_id: String,
        text: String,
        stash: String,
    },
    Completed {
        item_id: String,
        transcript: String,
    },
    TranscriptionFailed {
        message: String,
    },
    SessionFinished,
    Error {
        message: String,
    },
}

fn parse_server_event(message: Message) -> Result<Option<ServerEvent>> {
    let Message::Text(text) = message else {
        return Ok(None);
    };

    let payload: Value =
        serde_json::from_str(&text).context("failed to parse Qwen realtime server JSON")?;
    let event_type = payload["type"].as_str().unwrap_or_default();

    let event = match event_type {
        "session.created" => ServerEvent::SessionCreated,
        "session.updated" => ServerEvent::SessionUpdated,
        "input_audio_buffer.speech_started" => ServerEvent::SpeechStarted,
        "input_audio_buffer.speech_stopped" => ServerEvent::SpeechStopped,
        "input_audio_buffer.committed" => ServerEvent::InputCommitted,
        "conversation.item.input_audio_transcription.text" => ServerEvent::Partial {
            item_id: required_string(&payload, &["item_id"])?,
            text: payload["text"].as_str().unwrap_or_default().to_string(),
            stash: payload["stash"].as_str().unwrap_or_default().to_string(),
        },
        "conversation.item.input_audio_transcription.completed" => ServerEvent::Completed {
            item_id: required_string(&payload, &["item_id"])?,
            transcript: payload["transcript"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        },
        "conversation.item.input_audio_transcription.failed" => ServerEvent::TranscriptionFailed {
            message: payload["error"]["message"]
                .as_str()
                .unwrap_or("transcription failed")
                .to_string(),
        },
        "session.finished" => ServerEvent::SessionFinished,
        "error" => ServerEvent::Error {
            message: payload["error"]["message"]
                .as_str()
                .unwrap_or("unknown websocket error")
                .to_string(),
        },
        _ => return Ok(None),
    };

    Ok(Some(event))
}

fn required_string(payload: &Value, path: &[&str]) -> Result<String> {
    let mut current = payload;
    for key in path {
        current = &current[*key];
    }
    current
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("missing required websocket field {}", path.join(".")))
}

#[derive(Default)]
struct TranscriptAssembler {
    completed_segments: Vec<String>,
    current_item_id: Option<String>,
    current_committed: String,
    current_unstable: String,
}

impl TranscriptAssembler {
    fn apply_partial(&mut self, item_id: String, text: String, stash: String) -> (String, String) {
        if self.current_item_id.as_deref() != Some(item_id.as_str()) {
            self.current_item_id = Some(item_id);
            self.current_committed.clear();
            self.current_unstable.clear();
        }

        self.current_committed = text;
        self.current_unstable = stash;
        (self.committed_text(), self.current_unstable.clone())
    }

    fn apply_completed(&mut self, item_id: String, transcript: String) -> String {
        if !transcript.is_empty() {
            self.completed_segments.push(transcript);
        }

        if self.current_item_id.as_deref() == Some(item_id.as_str()) {
            self.current_item_id = None;
            self.current_committed.clear();
            self.current_unstable.clear();
        }

        self.final_text()
    }

    fn committed_text(&self) -> String {
        let mut text = String::new();
        for segment in &self.completed_segments {
            push_transcript_piece(&mut text, segment);
        }
        push_transcript_piece(&mut text, &self.current_committed);
        text
    }

    fn final_text(&self) -> String {
        let mut text = self.committed_text();
        push_transcript_piece(&mut text, &self.current_unstable);
        text
    }
}

fn push_transcript_piece(target: &mut String, piece: &str) {
    if piece.is_empty() {
        return;
    }

    let needs_space = target
        .chars()
        .next_back()
        .zip(piece.chars().next())
        .map(|(left, right)| left.is_ascii_alphanumeric() && right.is_ascii_alphanumeric())
        .unwrap_or(false);

    if needs_space
        && !target.ends_with(char::is_whitespace)
        && !piece.starts_with(char::is_whitespace)
    {
        target.push(' ');
    }
    target.push_str(piece);
}

#[cfg(test)]
mod tests {
    use super::{
        Message, PcmNoiseGate, TranscriptAssembler, parse_server_event, pcm_chunk_has_local_speech,
        push_transcript_piece, should_force_initial_commit,
    };

    #[test]
    fn noise_gate_zeros_idle_noise_and_preserves_speech_with_hangover() {
        let mut gate = PcmNoiseGate::new(4);

        assert_eq!(gate.filter(vec![40; 4]), vec![0; 4]);
        assert!(!gate.speech_observed());
        assert_eq!(gate.suppressed_sample_count(), 4);
        assert_eq!(gate.filter(vec![300; 4]), vec![300; 4]);
        assert!(gate.speech_observed());
        assert_eq!(gate.reopen_count(), 1);
        assert_eq!(gate.filter(vec![40; 4]), vec![40; 4]);
        assert_eq!(gate.filter(vec![40; 4]), vec![0; 4]);
        assert_eq!(gate.suppressed_sample_count(), 8);
    }

    #[test]
    fn local_speech_gate_rejects_silence_and_accepts_voice_energy() {
        assert!(!pcm_chunk_has_local_speech(&[]));
        assert!(!pcm_chunk_has_local_speech(&[0; 320]));
        assert!(!pcm_chunk_has_local_speech(&[120; 320]));
        assert!(pcm_chunk_has_local_speech(&[600; 320]));
    }

    #[test]
    fn forced_commit_is_only_an_initial_no_event_recovery() {
        let eligible = |has_server_activity, has_forced_commit, has_recovery_audio| {
            should_force_initial_commit(
                false,
                true,
                false,
                has_forced_commit,
                has_server_activity,
                has_recovery_audio,
                true,
            )
        };

        assert!(eligible(false, false, true));
        assert!(!eligible(false, false, false));
        assert!(!eligible(true, false, true));
        assert!(!eligible(false, true, true));
    }

    #[test]
    fn parses_partial_event() {
        let message = Message::Text(
            r#"{"type":"conversation.item.input_audio_transcription.text","item_id":"item_1","text":"hello","stash":" wor"}"#.into(),
        );

        let event = parse_server_event(message).expect("parse").expect("event");
        match event {
            super::ServerEvent::Partial {
                item_id,
                text,
                stash,
            } => {
                assert_eq!(item_id, "item_1");
                assert_eq!(text, "hello");
                assert_eq!(stash, " wor");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn transcript_assembler_builds_preview_and_final_text() {
        let mut assembler = TranscriptAssembler::default();
        let (committed, unstable) =
            assembler.apply_partial("item_1".into(), "hello".into(), " world".into());
        assert_eq!(committed, "hello");
        assert_eq!(unstable, " world");

        let final_text = assembler.apply_completed("item_1".into(), "hello world".into());
        assert_eq!(final_text, "hello world");
    }

    #[test]
    fn transcript_join_inserts_spaces_for_adjacent_ascii_words() {
        let mut text = String::from("hello");
        push_transcript_piece(&mut text, "world");
        assert_eq!(text, "hello world");
    }
}
