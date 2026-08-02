use std::{
    net::{TcpStream, ToSocketAddrs},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
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
    diagnostics::FailureKind,
    waveform::pcm_has_voiced_speech,
};

type QwenSocket = WebSocket<MaybeTlsStream<TcpStream>>;
type QwenHandshakeResponse = tungstenite::http::Response<Option<Vec<u8>>>;
type QwenConnection = (QwenSocket, QwenHandshakeResponse);

const MAX_CONTROLS_PER_TICK: usize = 8;
const MAX_AUDIO_SENDS_PER_TICK: usize = 8;
const REPLAY_SPEED_MULTIPLIER: f64 = 4.0;
const ACTIVE_TRANSCRIPTION_STALL_TIMEOUT: Duration = Duration::from_secs(8);
const POST_TRANSCRIPT_SPEECH_STALL_TIMEOUT: Duration = Duration::from_secs(8);
const LOCAL_VOICED_EVIDENCE_SECS: usize = 1;

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
        let abort_flag = Arc::new(AtomicBool::new(false));
        let worker_abort_flag = abort_flag.clone();
        let config = config.clone();

        let join = thread::spawn(move || {
            let result = run_session(config, spec, control_rx, worker_abort_flag, event_tx);
            if let Err(error) = result.as_ref() {
                eprintln!("voice-input realtime ASR: worker terminated: {error:#}");
            }
            result
        });

        Ok(AsrSessionHandle {
            control_tx,
            abort_flag,
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
    abort_flag: Arc<AtomicBool>,
    event_tx: mpsc::Sender<AsrEvent>,
) -> Result<()> {
    let alibaba = &config.asr.alibaba;
    if alibaba.api_key.trim().is_empty() {
        bail!("Alibaba realtime ASR requires an API key");
    }

    let OpenedStream {
        mut socket,
        mut next_event_id,
    } = open_stream(&config, spec)?;

    let mut ready = false;
    let mut ready_event_sent = false;
    let mut finish_requested = false;
    let mut finish_sent = false;
    let mut waiting_for_commit = false;
    let mut finalize_deadline = None;
    let mut assembler = TranscriptAssembler::default();
    let mut session_started = Instant::now();
    let mut partial_event_count = 0_u64;
    let mut completed_event_count = 0_u64;
    let mut speech_started_count = 0_u64;
    let mut speech_stopped_count = 0_u64;
    let mut appended_sample_count = 0_u64;
    let mut noise_gate = PcmNoiseGate::new((spec.sample_rate_hz as usize) / 2);
    let mut server_speech_active = false;
    let mut transcript_seen = false;
    let mut last_transcription_activity = Instant::now();
    let mut last_server_activity = Instant::now();
    let mut local_voiced_evidence_samples = 0_usize;
    let mut retained_audio = RetainedAudio::default();
    let mut retry_budget = RetryBudget::default();
    let mut attempt_number = 1_u8;
    let mut replay_pacing = false;
    let mut next_replay_packet_at = Instant::now();
    let mut replacement_ready_event_sent = false;

    macro_rules! reconstruct_or_stop {
        ($label:lifetime, $reason:expr) => {{
            match reconstruct_stream(
                &mut socket,
                &config,
                spec,
                &abort_flag,
                &event_tx,
                &mut retry_budget,
                $reason,
                &retained_audio,
                attempt_number,
            )? {
                Reconstruction::Restarted(opened) => {
                    socket = opened.socket;
                    next_event_id = opened.next_event_id;
                    attempt_number += 1;
                    ready = false;
                    finish_sent = false;
                    waiting_for_commit = false;
                    finalize_deadline = None;
                    assembler = TranscriptAssembler::default();
                    session_started = Instant::now();
                    partial_event_count = 0;
                    completed_event_count = 0;
                    speech_started_count = 0;
                    speech_stopped_count = 0;
                    appended_sample_count = 0;
                    noise_gate = PcmNoiseGate::new((spec.sample_rate_hz as usize) / 2);
                    server_speech_active = false;
                    transcript_seen = false;
                    last_transcription_activity = Instant::now();
                    last_server_activity = Instant::now();
                    local_voiced_evidence_samples = 0;
                    retained_audio.rewind();
                    replay_pacing = true;
                    next_replay_packet_at = Instant::now();
                    replacement_ready_event_sent = false;
                    continue $label;
                }
                Reconstruction::Stop => return Ok(()),
            }
        }};
    }

    'session: loop {
        if abort_flag.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            return Ok(());
        }

        // Drain capture controls even while old audio is being replayed. The
        // retained sequence remains authoritative and ordered, while the
        // bounded control queue stays available to the nonblocking capture
        // thread during reconstruction.
        for _ in 0..MAX_CONTROLS_PER_TICK {
            if finish_requested {
                break;
            }
            match control_rx.try_recv() {
                Ok(control) => {
                    if retained_audio.accept_control(control) {
                        finish_requested = true;
                        finalize_deadline = Some(
                            Instant::now()
                                + Duration::from_millis(config.asr.finalize_timeout_ms.max(1_000)),
                        );
                    }
                }
                Err(_) => break,
            }
        }

        // Replay at a bounded multiple of realtime speed instead of bursting
        // an entire recording into the replacement service. Live packets are
        // sent without pacing once the retained backlog has caught up.
        for _ in 0..MAX_AUDIO_SENDS_PER_TICK {
            if abort_flag.load(Ordering::SeqCst) {
                let _ = socket.close(None);
                return Ok(());
            }
            let now = Instant::now();
            if replay_pacing && now < next_replay_packet_at {
                break;
            }
            let Some(samples) = retained_audio.next_packet() else {
                replay_pacing = false;
                break;
            };
            let packet_duration = Duration::from_secs_f64(
                samples.len() as f64 / f64::from(spec.sample_rate_hz) / REPLAY_SPEED_MULTIPLIER,
            );
            let local_voiced = transcript_seen
                && !server_speech_active
                && pcm_has_voiced_speech(samples, spec.sample_rate_hz);
            local_voiced_evidence_samples = update_local_voiced_evidence(
                local_voiced_evidence_samples,
                samples.len(),
                local_voiced,
                spec.sample_rate_hz as usize * LOCAL_VOICED_EVIDENCE_SECS,
            );
            let filtered_samples = noise_gate.filter(samples);
            if send_json(
                &mut socket,
                json!({
                    "event_id": format!("event-{}", next_event_id),
                    "type": "input_audio_buffer.append",
                    "audio": encode_pcm16_chunk(&filtered_samples),
                }),
            )
            .is_err()
            {
                if !finish_requested {
                    reconstruct_or_stop!('session, InterruptionReason::StreamWrite);
                }
                terminal_stream_failure(
                    &mut socket,
                    &event_tx,
                    attempt_number,
                    InterruptionReason::StreamWrite,
                    &retained_audio,
                );
                return Ok(());
            }
            retained_audio.mark_packet_sent();
            appended_sample_count += filtered_samples.len() as u64;
            next_event_id += 1;
            if replay_pacing {
                next_replay_packet_at = std::cmp::max(next_replay_packet_at, now) + packet_duration;
                if retained_audio.caught_up() {
                    replay_pacing = false;
                }
            }
        }

        if retained_audio.finish_ready(finish_requested) && !finish_sent {
            let finish_result = match config.asr.alibaba.turn_mode {
                AlibabaTurnMode::ServerVad => send_json(
                    &mut socket,
                    json!({
                        "event_id": format!("event-{}", next_event_id),
                        "type": "session.finish",
                    }),
                ),
                AlibabaTurnMode::Manual => {
                    waiting_for_commit = true;
                    send_json(
                        &mut socket,
                        json!({
                            "event_id": format!("event-{}", next_event_id),
                            "type": "input_audio_buffer.commit",
                        }),
                    )
                }
            };
            if finish_result.is_err() {
                terminal_stream_failure(
                    &mut socket,
                    &event_tx,
                    attempt_number,
                    InterruptionReason::StreamWrite,
                    &retained_audio,
                );
                return Ok(());
            }
            finish_sent = true;
            next_event_id += 1;
        }

        match socket.read() {
            Ok(Message::Close(frame)) => {
                let reason = InterruptionReason::WebSocketClose(
                    frame.as_ref().map(|frame| u16::from(frame.code)),
                );
                if !finish_requested {
                    reconstruct_or_stop!('session, reason);
                }
                terminal_stream_failure(
                    &mut socket,
                    &event_tx,
                    attempt_number,
                    reason,
                    &retained_audio,
                );
                return Ok(());
            }
            Ok(message) => {
                if let Some(event) = parse_server_event(message)? {
                    last_server_activity = Instant::now();
                    local_voiced_evidence_samples = 0;
                    match event {
                        ServerEvent::SessionCreated => {}
                        ServerEvent::SessionUpdated => {
                            if !ready {
                                if !ready_event_sent {
                                    let _ = event_tx.send(AsrEvent::Ready);
                                    ready_event_sent = true;
                                }
                                if attempt_number > 1 && !replacement_ready_event_sent {
                                    let _ = event_tx.send(AsrEvent::RealtimeRestarted);
                                    replacement_ready_event_sent = true;
                                }
                                ready = true;
                            }
                        }
                        ServerEvent::SpeechStarted => {
                            speech_started_count += 1;
                            server_speech_active = true;
                            // Server VAD is authoritative for watchdog timing.
                            // A new speech segment gets a fresh observation
                            // window even if the previous segment produced text.
                            last_transcription_activity = Instant::now();
                            let _ = event_tx.send(AsrEvent::SpeechStarted);
                        }
                        ServerEvent::SpeechStopped => {
                            speech_stopped_count += 1;
                            server_speech_active = false;
                            let _ = event_tx.send(AsrEvent::SpeechStopped);
                        }
                        ServerEvent::InputCommitted => {
                            if waiting_for_commit {
                                waiting_for_commit = false;
                                if send_json(
                                    &mut socket,
                                    json!({
                                        "event_id": format!("event-{}", next_event_id),
                                        "type": "session.finish",
                                    }),
                                )
                                .is_err()
                                {
                                    terminal_stream_failure(
                                        &mut socket,
                                        &event_tx,
                                        attempt_number,
                                        InterruptionReason::StreamWrite,
                                        &retained_audio,
                                    );
                                    return Ok(());
                                }
                                next_event_id += 1;
                            }
                        }
                        ServerEvent::Partial {
                            item_id,
                            text,
                            stash,
                        } => {
                            partial_event_count += 1;
                            transcript_seen |= !text.is_empty() || !stash.is_empty();
                            last_transcription_activity = Instant::now();
                            if partial_event_count == 1 {
                                eprintln!(
                                    "voice-input realtime ASR: attempt {attempt_number} first partial after {} ms",
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
                        ServerEvent::Completed {
                            item_id,
                            transcript,
                        } => {
                            completed_event_count += 1;
                            transcript_seen |= !transcript.is_empty();
                            last_transcription_activity = Instant::now();
                            let text = assembler.apply_completed(item_id, transcript);
                            let _ = event_tx.send(AsrEvent::SegmentFinal { text });
                        }
                        ServerEvent::Failed { kind } => {
                            return report_provider_failure(&event_tx, kind);
                        }
                        ServerEvent::SessionFinished => {
                            eprintln!(
                                "voice-input realtime ASR: attempt {attempt_number} finished with {partial_event_count} partial, {completed_event_count} completed, {speech_started_count} speech-started, {speech_stopped_count} speech-stopped, {appended_sample_count} appended samples, {} noise-gate openings, and {} suppressed samples",
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
                if !finish_requested {
                    reconstruct_or_stop!('session, InterruptionReason::ConnectionClosed);
                }
                terminal_stream_failure(
                    &mut socket,
                    &event_tx,
                    attempt_number,
                    InterruptionReason::ConnectionClosed,
                    &retained_audio,
                );
                return Ok(());
            }
            Err(_) => {
                if !finish_requested {
                    reconstruct_or_stop!('session, InterruptionReason::StreamRead);
                }
                terminal_stream_failure(
                    &mut socket,
                    &event_tx,
                    attempt_number,
                    InterruptionReason::StreamRead,
                    &retained_audio,
                );
                return Ok(());
            }
        }

        // Server VAD remains authoritative for the fast watchdog. A second
        // path catches a missed follow-up speech_started event only after the
        // raw audio contains sustained, pitch-correlated local voice evidence.
        // Ordinary silence and stationary broadband microphone noise cannot
        // consume the session's single reconstruction attempt.
        if let Some(reason) = realtime_stall_reason(
            finish_requested,
            matches!(config.asr.alibaba.turn_mode, AlibabaTurnMode::ServerVad),
            server_speech_active,
            transcript_seen,
            local_voiced_evidence_samples
                >= spec.sample_rate_hz as usize * LOCAL_VOICED_EVIDENCE_SECS,
            last_transcription_activity.elapsed(),
            last_server_activity.elapsed(),
        ) {
            reconstruct_or_stop!('session, reason);
        }

        if let Some(deadline) = finalize_deadline
            && Instant::now() > deadline
        {
            terminal_stream_failure(
                &mut socket,
                &event_tx,
                attempt_number,
                InterruptionReason::FinalizeTimeout,
                &retained_audio,
            );
            return Ok(());
        }

        if !finish_requested && !ready {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn report_provider_failure(event_tx: &mpsc::Sender<AsrEvent>, kind: FailureKind) -> Result<()> {
    let _ = event_tx.send(AsrEvent::Error { kind });
    bail!("Alibaba realtime ASR failed ({})", kind.as_str())
}

struct OpenedStream {
    socket: QwenSocket,
    next_event_id: u64,
}

fn open_stream(config: &Config, spec: AudioSpec) -> Result<OpenedStream> {
    let alibaba = &config.asr.alibaba;
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
    send_json(&mut socket, session_update_payload(config, spec, "event-1"))?;

    Ok(OpenedStream {
        socket,
        next_event_id: 2,
    })
}

#[derive(Debug, Default)]
struct RetryBudget {
    replacement_used: bool,
}

impl RetryBudget {
    fn claim_replacement(&mut self) -> bool {
        if self.replacement_used {
            false
        } else {
            self.replacement_used = true;
            true
        }
    }
}

#[derive(Debug, Default)]
struct RetainedAudio {
    packets: Vec<Vec<i16>>,
    next_packet_index: usize,
    sample_count: u64,
}

impl RetainedAudio {
    fn accept_control(&mut self, control: AsrControl) -> bool {
        match control {
            AsrControl::AppendPcm16(samples) => {
                self.retain(samples);
                false
            }
            AsrControl::Finish => true,
        }
    }

    fn retain(&mut self, samples: Vec<i16>) {
        self.sample_count = self.sample_count.saturating_add(samples.len() as u64);
        self.packets.push(samples);
    }

    fn has_unsent_packet(&self) -> bool {
        self.next_packet_index < self.packets.len()
    }

    fn caught_up(&self) -> bool {
        !self.has_unsent_packet()
    }

    fn finish_ready(&self, finish_requested: bool) -> bool {
        finish_requested && self.caught_up()
    }

    fn next_packet(&self) -> Option<&[i16]> {
        self.packets.get(self.next_packet_index).map(Vec::as_slice)
    }

    fn mark_packet_sent(&mut self) {
        debug_assert!(self.has_unsent_packet());
        self.next_packet_index += 1;
    }

    fn rewind(&mut self) {
        self.next_packet_index = 0;
    }

    fn packet_count(&self) -> usize {
        self.packets.len()
    }

    fn sample_count(&self) -> u64 {
        self.sample_count
    }
}

#[derive(Debug, Clone, Copy)]
enum InterruptionReason {
    ActiveTranscriptStall,
    PostTranscriptSpeechStall,
    WebSocketClose(Option<u16>),
    ConnectionClosed,
    StreamRead,
    StreamWrite,
    FinalizeTimeout,
}

impl std::fmt::Display for InterruptionReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveTranscriptStall => formatter.write_str("8-second active transcript stall"),
            Self::PostTranscriptSpeechStall => {
                formatter.write_str("post-transcript local-speech stall")
            }
            Self::WebSocketClose(Some(code)) => write!(formatter, "WebSocket close ({code})"),
            Self::WebSocketClose(None) => formatter.write_str("WebSocket close"),
            Self::ConnectionClosed => formatter.write_str("connection closed"),
            Self::StreamRead => formatter.write_str("stream read failure"),
            Self::StreamWrite => formatter.write_str("stream write failure"),
            Self::FinalizeTimeout => formatter.write_str("finalize timeout"),
        }
    }
}

enum Reconstruction {
    Restarted(Box<OpenedStream>),
    Stop,
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_stream(
    socket: &mut QwenSocket,
    config: &Config,
    spec: AudioSpec,
    abort_flag: &AtomicBool,
    event_tx: &mpsc::Sender<AsrEvent>,
    retry_budget: &mut RetryBudget,
    reason: InterruptionReason,
    retained_audio: &RetainedAudio,
    attempt_number: u8,
) -> Result<Reconstruction> {
    if abort_flag.load(Ordering::SeqCst) {
        let _ = socket.close(None);
        return Ok(Reconstruction::Stop);
    }

    if !retry_budget.claim_replacement() {
        terminal_stream_failure(socket, event_tx, attempt_number, reason, retained_audio);
        return Ok(Reconstruction::Stop);
    }

    let _ = socket.close(None);
    if abort_flag.load(Ordering::SeqCst) {
        return Ok(Reconstruction::Stop);
    }

    let replacement_attempt = attempt_number + 1;
    eprintln!(
        "voice-input realtime ASR: attempt {attempt_number} interrupted by {reason}; starting attempt {replacement_attempt} with {} retained packets / {} samples",
        retained_audio.packet_count(),
        retained_audio.sample_count()
    );
    let _ = event_tx.send(AsrEvent::RealtimeRestarting);
    if abort_flag.load(Ordering::SeqCst) {
        return Ok(Reconstruction::Stop);
    }

    let opened = match open_stream(config, spec) {
        Ok(opened) => opened,
        Err(error) => {
            eprintln!(
                "voice-input realtime ASR: attempt {replacement_attempt} replacement connection failed after {reason}: {error:#}; {} retained packets / {} samples",
                retained_audio.packet_count(),
                retained_audio.sample_count()
            );
            let _ = event_tx.send(AsrEvent::RealtimeTranscriptDelayed);
            return Ok(Reconstruction::Stop);
        }
    };
    if abort_flag.load(Ordering::SeqCst) {
        let mut socket = opened.socket;
        let _ = socket.close(None);
        return Ok(Reconstruction::Stop);
    }

    eprintln!(
        "voice-input realtime ASR: attempt {replacement_attempt} connected; replaying {} retained packets / {} samples",
        retained_audio.packet_count(),
        retained_audio.sample_count()
    );
    Ok(Reconstruction::Restarted(Box::new(opened)))
}

fn terminal_stream_failure(
    socket: &mut QwenSocket,
    event_tx: &mpsc::Sender<AsrEvent>,
    attempt_number: u8,
    reason: InterruptionReason,
    retained_audio: &RetainedAudio,
) {
    eprintln!(
        "voice-input realtime ASR: attempt {attempt_number} ended after {reason}; retry exhausted with {} retained packets / {} samples",
        retained_audio.packet_count(),
        retained_audio.sample_count()
    );
    let _ = socket.close(None);
    let _ = event_tx.send(AsrEvent::RealtimeTranscriptDelayed);
}

struct PcmNoiseGate {
    hangover_samples: usize,
    remaining_hangover_samples: usize,
    reopen_count: u64,
    suppressed_sample_count: u64,
}

impl PcmNoiseGate {
    fn new(hangover_samples: usize) -> Self {
        Self {
            hangover_samples,
            remaining_hangover_samples: 0,
            reopen_count: 0,
            suppressed_sample_count: 0,
        }
    }

    fn filter(&mut self, samples: &[i16]) -> Vec<i16> {
        let mut samples = samples.to_vec();
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

fn update_local_voiced_evidence(
    current_samples: usize,
    packet_samples: usize,
    voiced: bool,
    maximum_samples: usize,
) -> usize {
    if voiced {
        current_samples
            .saturating_add(packet_samples)
            .min(maximum_samples)
    } else {
        current_samples.saturating_sub(packet_samples / 2)
    }
}

fn realtime_stall_reason(
    finish_requested: bool,
    server_vad: bool,
    server_speech_active: bool,
    transcript_seen: bool,
    has_local_voiced_evidence: bool,
    transcription_inactive_for: Duration,
    server_inactive_for: Duration,
) -> Option<InterruptionReason> {
    if finish_requested || !server_vad {
        return None;
    }
    if server_speech_active && transcription_inactive_for >= ACTIVE_TRANSCRIPTION_STALL_TIMEOUT {
        return Some(InterruptionReason::ActiveTranscriptStall);
    }
    if transcript_seen
        && has_local_voiced_evidence
        && server_inactive_for >= POST_TRANSCRIPT_SPEECH_STALL_TIMEOUT
    {
        return Some(InterruptionReason::PostTranscriptSpeechStall);
    }
    None
}

fn send_json(socket: &mut QwenSocket, payload: Value) -> Result<()> {
    socket
        .send(Message::Text(payload.to_string()))
        .context("failed to send websocket event")
}

fn build_url(endpoint: &str, model: &str) -> Result<Url> {
    let mut url = Url::parse(endpoint).context("invalid ASR endpoint configuration")?;
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
    // Bound TLS and HTTP upgrade I/O as well as the TCP connect. DNS remains
    // subject to the operating-system resolver, but an accepted TCP socket can
    // no longer hold Stop indefinitely during a stalled handshake.
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

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
    Failed {
        kind: FailureKind,
    },
    SessionFinished,
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
        "conversation.item.input_audio_transcription.failed" | "session.failed" | "error" => {
            ServerEvent::Failed {
                kind: provider_failure_kind(&payload),
            }
        }
        "session.finished" => ServerEvent::SessionFinished,
        _ => return Ok(None),
    };

    Ok(Some(event))
}

fn provider_failure_kind(payload: &Value) -> FailureKind {
    let code = payload
        .pointer("/error/code")
        .or_else(|| payload.get("code"))
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
    use std::{sync::mpsc, time::Duration};

    use crate::{
        backend::{AsrEvent, AudioSpec},
        config::{AsrProvider, Audio3VocabularyTerm, Config},
        diagnostics::FailureKind,
    };

    use super::{
        ACTIVE_TRANSCRIPTION_STALL_TIMEOUT, AsrControl, InterruptionReason, Message,
        POST_TRANSCRIPT_SPEECH_STALL_TIMEOUT, PcmNoiseGate, RetainedAudio, RetryBudget,
        TranscriptAssembler, parse_server_event, push_transcript_piece, realtime_stall_reason,
        report_provider_failure, session_update_payload, update_local_voiced_evidence,
    };

    #[test]
    fn realtime_provider_ignores_audio3_only_request_controls() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenRealtime;
        config.asr.alibaba_audio3.language_hints_enabled = true;
        config.asr.alibaba_audio3.heartbeat_enabled = true;
        config.asr.alibaba_audio3.vocabulary = vec![Audio3VocabularyTerm {
            term: "Audio3-only term".into(),
            weight: 5,
        }];

        let payload = session_update_payload(
            &config,
            AudioSpec {
                sample_rate_hz: 16_000,
            },
            "event-1",
        );
        let encoded = payload.to_string();
        for audio3_only_value in [
            "language_hints",
            "heartbeat",
            "vocabulary",
            "Audio3-only term",
        ] {
            assert!(!encoded.contains(audio3_only_value));
        }
    }

    #[test]
    fn noise_gate_zeros_idle_noise_and_preserves_speech_with_hangover() {
        let mut gate = PcmNoiseGate::new(4);

        assert_eq!(gate.filter(&[40; 4]), vec![0; 4]);
        assert_eq!(gate.suppressed_sample_count(), 4);
        assert_eq!(gate.filter(&[300; 4]), vec![300; 4]);
        assert_eq!(gate.reopen_count(), 1);
        assert_eq!(gate.filter(&[40; 4]), vec![40; 4]);
        assert_eq!(gate.filter(&[40; 4]), vec![0; 4]);
        assert_eq!(gate.suppressed_sample_count(), 8);
    }

    #[test]
    fn active_stall_requires_server_speech_and_eight_seconds() {
        assert!(matches!(
            realtime_stall_reason(
                false,
                true,
                true,
                false,
                false,
                ACTIVE_TRANSCRIPTION_STALL_TIMEOUT,
                Duration::ZERO,
            ),
            Some(InterruptionReason::ActiveTranscriptStall)
        ));
        assert!(
            realtime_stall_reason(
                false,
                true,
                true,
                false,
                false,
                ACTIVE_TRANSCRIPTION_STALL_TIMEOUT - Duration::from_millis(1),
                Duration::ZERO,
            )
            .is_none()
        );
        assert!(
            realtime_stall_reason(
                true,
                true,
                true,
                true,
                true,
                ACTIVE_TRANSCRIPTION_STALL_TIMEOUT,
                POST_TRANSCRIPT_SPEECH_STALL_TIMEOUT,
            )
            .is_none()
        );
        assert!(
            realtime_stall_reason(
                false,
                false,
                true,
                true,
                true,
                ACTIVE_TRANSCRIPTION_STALL_TIMEOUT,
                POST_TRANSCRIPT_SPEECH_STALL_TIMEOUT,
            )
            .is_none()
        );
    }

    #[test]
    fn inactive_stream_requires_prior_text_local_voice_and_eight_seconds() {
        assert!(matches!(
            realtime_stall_reason(
                false,
                true,
                false,
                true,
                true,
                Duration::ZERO,
                POST_TRANSCRIPT_SPEECH_STALL_TIMEOUT,
            ),
            Some(InterruptionReason::PostTranscriptSpeechStall)
        ));
        for (transcript_seen, local_voice) in [(false, true), (true, false)] {
            assert!(
                realtime_stall_reason(
                    false,
                    true,
                    false,
                    transcript_seen,
                    local_voice,
                    Duration::ZERO,
                    POST_TRANSCRIPT_SPEECH_STALL_TIMEOUT,
                )
                .is_none()
            );
        }
        assert!(
            realtime_stall_reason(
                false,
                true,
                false,
                true,
                true,
                Duration::ZERO,
                POST_TRANSCRIPT_SPEECH_STALL_TIMEOUT - Duration::from_millis(1),
            )
            .is_none()
        );
    }

    #[test]
    fn local_voiced_evidence_accumulates_and_decays() {
        assert_eq!(update_local_voiced_evidence(0, 2_000, true, 16_000), 2_000);
        assert_eq!(
            update_local_voiced_evidence(15_000, 2_000, true, 16_000),
            16_000
        );
        assert_eq!(
            update_local_voiced_evidence(2_000, 2_000, false, 16_000),
            1_000
        );
        assert_eq!(update_local_voiced_evidence(500, 2_000, false, 16_000), 0);
    }

    #[test]
    fn retry_budget_allows_exactly_one_replacement() {
        let mut budget = RetryBudget::default();

        assert!(budget.claim_replacement());
        assert!(!budget.claim_replacement());
        assert!(!budget.claim_replacement());
    }

    #[test]
    fn retained_audio_replays_in_order_before_later_controls() {
        let mut audio = RetainedAudio::default();
        assert!(!audio.accept_control(AsrControl::AppendPcm16(vec![1, 2])));
        assert!(!audio.accept_control(AsrControl::AppendPcm16(vec![3])));
        assert_eq!(audio.packet_count(), 2);
        assert_eq!(audio.sample_count(), 3);

        assert_eq!(audio.next_packet(), Some([1, 2].as_slice()));
        audio.mark_packet_sent();
        assert!(!audio.caught_up());
        assert_eq!(audio.next_packet(), Some([3].as_slice()));
        audio.mark_packet_sent();
        assert!(audio.caught_up());

        audio.rewind();
        assert!(!audio.caught_up());
        assert_eq!(audio.next_packet(), Some([1, 2].as_slice()));
        audio.mark_packet_sent();
        let finish_requested = audio.accept_control(AsrControl::Finish);
        assert!(finish_requested);
        assert!(!audio.finish_ready(finish_requested));
        assert_eq!(audio.next_packet(), Some([3].as_slice()));
        audio.mark_packet_sent();
        assert!(audio.caught_up());
        assert!(audio.finish_ready(finish_requested));
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
    fn provider_failure_messages_are_discarded_and_only_category_is_emitted() {
        const SENTINEL: &str = "private-provider-error-sentinel";
        for event_type in ["session.failed", "error"] {
            let message = Message::Text(
                serde_json::json!({
                    "type": event_type,
                    "error": {"message": SENTINEL}
                })
                .to_string(),
            );
            let event = parse_server_event(message).unwrap().unwrap();
            let super::ServerEvent::Failed { kind } = event else {
                panic!("expected categorized failure");
            };
            assert_eq!(kind, FailureKind::Service);

            let (event_tx, event_rx) = mpsc::channel();
            let error = report_provider_failure(&event_tx, kind).unwrap_err();
            assert!(matches!(
                event_rx.recv().unwrap(),
                AsrEvent::Error {
                    kind: FailureKind::Service
                }
            ));
            assert_eq!(error.to_string(), "Alibaba realtime ASR failed (service)");
            assert!(!format!("{error:#}").contains(SENTINEL));
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
