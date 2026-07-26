use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    agent_context, backend,
    config::{AsrProvider, Config, HudConfig, HudPosition},
    llm, output, paths,
    state::{Phase, Snapshot, StateHandle},
    wav,
    waveform::{
        AsrPacketizer, WAVEFORM_BAR_COUNT, WaveformAnalyzer, WaveformPublisher,
        socket_path as waveform_socket_path,
    },
};
use anyhow::{Context, Result, anyhow, bail};

const PROCESSING_WAVEFORM: [f32; WAVEFORM_BAR_COUNT] = [0.22; WAVEFORM_BAR_COUNT];
const SPEECH_EVENT_GRACE_MS: u64 = 350;

pub fn run(config: Config) -> Result<()> {
    let runtime_dir = paths::runtime_dir()?;
    fs::create_dir_all(&runtime_dir)?;
    let socket_path = paths::control_socket_path()?;
    if socket_path.exists() {
        fs::remove_file(&socket_path).ok();
    }

    let state = StateHandle::new(config.clone())?;
    let waveform =
        WaveformPublisher::start(waveform_socket_path(&runtime_dir), config.audio.sample_rate)?;
    let daemon = Arc::new(Mutex::new(Daemon::new(config, state, waveform)?));
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind control socket at {}", socket_path.display()))?;
    println!("Voice Input daemon listening on {}", socket_path.display());

    loop {
        let (mut stream, _) = listener
            .accept()
            .context("failed to accept control socket")?;
        let mut buffer = String::new();
        stream
            .read_to_string(&mut buffer)
            .context("failed to read control command")?;
        let response = match handle_control(&daemon, buffer.trim()) {
            Ok(response) => response,
            Err(error) => format!("error: {error:#}\n"),
        };
        stream
            .write_all(response.as_bytes())
            .context("failed to write control response")?;
    }
}

pub fn send_control_command(command: &str) -> Result<String> {
    let socket_path = paths::control_socket_path()?;
    let mut stream = UnixStream::connect(&socket_path)
        .with_context(|| format!("failed to connect to daemon at {}", socket_path.display()))?;
    stream
        .write_all(command.as_bytes())
        .context("failed to send command to daemon")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to close control socket for writing")?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .context("failed to read daemon response")?;
    Ok(response)
}

pub fn send_record_command(command: &str) -> Result<String> {
    if command.split_whitespace().next() == Some("toggle") {
        send_control_command(&format!("{command} requested_at_ms={}", unix_time_ms()))
    } else {
        send_control_command(command)
    }
}

fn handle_control(daemon: &Arc<Mutex<Daemon>>, command: &str) -> Result<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let Some(head) = parts.first().copied() else {
        bail!("empty control command");
    };
    // Log receipt before waiting for the daemon mutex. This distinguishes a
    // compositor/keybinding miss from a request queued behind finalization.
    if matches!(head, "start" | "stop" | "toggle" | "cancel") {
        eprintln!("voice-input control: received {head}");
    }

    let mut daemon = daemon.lock().expect("daemon mutex poisoned");
    let result = match head {
        "start" => daemon
            .start_recording(parse_output_target_hint_arg(parts.get(1).copied())?)
            .map(|_| "ok\n".to_string()),
        "stop" => daemon.finish_recording(false).map(|_| "ok\n".to_string()),
        "toggle" => {
            if daemon.has_session() {
                daemon.finish_recording(false)?;
            } else if toggle_request_is_stale(&parts) {
                // A second key press can sit in the control socket backlog while
                // final ASR, refinement, and output are running. Do not turn that
                // old press into a new recording after processing completes.
                return Ok("ignored stale toggle\n".to_string());
            } else {
                let target_hint = parts
                    .get(1)
                    .copied()
                    .filter(|value| !value.starts_with("requested_at_ms="));
                daemon.start_recording(parse_output_target_hint_arg(target_hint)?)?;
            }
            Ok("ok\n".to_string())
        }
        "cancel" => daemon.finish_recording(true).map(|_| "ok\n".to_string()),
        "hud" => handle_hud_control(&mut daemon, &parts[1..]),
        other => bail!("unknown control command `{other}`"),
    };

    if let Err(error) = &result {
        let message = error.to_string();
        let _ = daemon.state.update(|snapshot| {
            snapshot.phase = Phase::Error;
            snapshot.class = "error".into();
            snapshot.icon = "󰅙".into();
            snapshot.tooltip = message.clone();
            snapshot.error = Some(message.clone());
            snapshot.bars = [0.0; WAVEFORM_BAR_COUNT];
        });
    }
    result
}

fn toggle_request_is_stale(parts: &[&str]) -> bool {
    const MAX_TOGGLE_QUEUE_AGE_MS: u128 = 750;

    let requested_at = parts
        .iter()
        .find_map(|part| part.strip_prefix("requested_at_ms="))
        .and_then(|value| value.parse::<u128>().ok());

    requested_at
        .map(|timestamp| unix_time_ms().saturating_sub(timestamp) > MAX_TOGGLE_QUEUE_AGE_MS)
        .unwrap_or(false)
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn handle_hud_control(daemon: &mut Daemon, args: &[&str]) -> Result<String> {
    let Some(action) = args.first().copied() else {
        bail!("hud requires move|position|center|reset");
    };

    match action {
        "move" => {
            let direction = match args.get(1).copied() {
                Some("left") => HudMoveDirection::Left,
                Some("right") => HudMoveDirection::Right,
                Some("up") => HudMoveDirection::Up,
                Some("down") => HudMoveDirection::Down,
                Some(other) => bail!("unknown hud move direction `{other}`"),
                None => bail!("hud move requires left|right|up|down"),
            };
            let amount = args
                .get(2)
                .map(|value| {
                    value
                        .parse::<i32>()
                        .map_err(|_| anyhow!("hud move amount must be an integer"))
                })
                .transpose()?;
            daemon.nudge_hud(direction, amount)?;
            Ok("ok\n".into())
        }
        "position" => {
            let position = parse_hud_position(args.get(1).copied().ok_or_else(|| {
                anyhow!("hud position requires bottom-center|bottom-left|bottom-right")
            })?)?;
            daemon.set_hud_position(position)?;
            Ok("ok\n".into())
        }
        "center" => {
            daemon.center_hud()?;
            Ok("ok\n".into())
        }
        "reset" => {
            daemon.reset_hud()?;
            Ok("ok\n".into())
        }
        other => bail!("unknown hud command `{other}`"),
    }
}

#[derive(Debug, Clone, Copy)]
enum HudMoveDirection {
    Left,
    Right,
    Up,
    Down,
}

fn parse_hud_position(value: &str) -> Result<HudPosition> {
    match value {
        "bottom-center" | "center" => Ok(HudPosition::Center),
        "bottom-left" | "left" => Ok(HudPosition::Left),
        "bottom-right" | "right" => Ok(HudPosition::Right),
        other => bail!("unknown hud position `{other}`"),
    }
}

fn label_output_target_hint(hint: Option<output::OutputTargetHint>) -> &'static str {
    match hint {
        Some(output::OutputTargetHint::Wayland) => "wayland",
        Some(output::OutputTargetHint::XWayland) => "xwayland",
        None => "unknown",
    }
}

fn parse_output_target_hint_arg(value: Option<&str>) -> Result<Option<output::OutputTargetHint>> {
    match value {
        None => Ok(None),
        Some("wayland") => Ok(Some(output::OutputTargetHint::Wayland)),
        Some("xwayland") => Ok(Some(output::OutputTargetHint::XWayland)),
        Some(other) => bail!("unknown output target hint `{other}`"),
    }
}

struct Daemon {
    config: Config,
    state: StateHandle,
    next_session_id: AtomicU64,
    capture: CaptureService,
    waveform: WaveformPublisher,
    session: Option<Session>,
}

struct Session {
    stop_flag: Arc<AtomicBool>,
    cancel_flag: Arc<AtomicBool>,
    audio_buffer: Arc<Mutex<Vec<i16>>>,
    output_target_hint: Option<output::OutputTargetHint>,
    agent_context_handle: Option<thread::JoinHandle<Option<agent_context::AgentSessionLocator>>>,
    asr_packetizer: Option<Arc<Mutex<AsrPacketizer>>>,
    speech_detected: Arc<AtomicBool>,
    capture_mode: SessionCaptureMode,
    asr_runtime: SessionAsrRuntime,
}

enum SessionCaptureMode {
    Dedicated {
        child: Child,
        reader_handle: thread::JoinHandle<Result<()>>,
    },
    SharedPreRoll,
}

enum SessionAsrRuntime {
    Local {
        partial_handle: thread::JoinHandle<Result<()>>,
    },
    Realtime {
        control_tx: mpsc::Sender<backend::AsrControl>,
        event_handle: thread::JoinHandle<Result<Option<String>>>,
        backend_handle: thread::JoinHandle<Result<()>>,
    },
}

#[derive(Clone)]
struct ActiveCaptureSession {
    session_id: u64,
    audio_buffer: Arc<Mutex<Vec<i16>>>,
    capture_ready: Arc<AtomicBool>,
    asr_ready: Arc<AtomicBool>,
    voice_active: Arc<AtomicBool>,
    asr_control_tx: Option<mpsc::Sender<backend::AsrControl>>,
    asr_packetizer: Option<Arc<Mutex<AsrPacketizer>>>,
    waveform_analyzer: Arc<Mutex<WaveformAnalyzer>>,
    waveform: WaveformPublisher,
}

struct CaptureService {
    enabled: bool,
    waveform: WaveformPublisher,
    capture_hot: Arc<AtomicBool>,
    ring_buffer: Arc<Mutex<VecDeque<i16>>>,
    active_session: Arc<Mutex<Option<ActiveCaptureSession>>>,
}

impl CaptureService {
    fn new(config: Config, state: StateHandle, waveform: WaveformPublisher) -> Result<Self> {
        if !config.audio.pre_roll_enabled {
            return Ok(Self {
                enabled: false,
                waveform,
                capture_hot: Arc::new(AtomicBool::new(false)),
                ring_buffer: Arc::new(Mutex::new(VecDeque::new())),
                active_session: Arc::new(Mutex::new(None)),
            });
        }

        let capture_hot = Arc::new(AtomicBool::new(false));
        let ring_buffer = Arc::new(Mutex::new(VecDeque::new()));
        let active_session = Arc::new(Mutex::new(None));

        let capture_hot_thread = capture_hot.clone();
        let ring_buffer_thread = ring_buffer.clone();
        let active_session_thread = active_session.clone();
        let thread_config = config.clone();
        let thread_state = state.clone();

        thread::spawn(move || {
            loop {
                capture_hot_thread.store(false, Ordering::SeqCst);
                if let Err(error) = run_capture_service(
                    thread_config.clone(),
                    thread_state.clone(),
                    capture_hot_thread.clone(),
                    ring_buffer_thread.clone(),
                    active_session_thread.clone(),
                ) {
                    eprintln!("voice-input capture service failed: {error:#}");
                }
                thread::sleep(Duration::from_secs(1));
            }
        });

        Ok(Self {
            enabled: true,
            waveform,
            capture_hot,
            ring_buffer,
            active_session,
        })
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn is_capture_hot(&self) -> bool {
        !self.enabled || self.capture_hot.load(Ordering::SeqCst)
    }

    fn seed_audio(&self) -> Vec<i16> {
        if !self.enabled {
            return Vec::new();
        }

        self.ring_buffer
            .lock()
            .expect("capture ring buffer mutex poisoned")
            .iter()
            .copied()
            .collect()
    }

    fn attach_session(&self, session: ActiveCaptureSession) {
        if !self.enabled {
            return;
        }

        *self
            .active_session
            .lock()
            .expect("active capture session mutex poisoned") = Some(session);
    }

    fn detach_session(&self) {
        if !self.enabled {
            return;
        }

        *self
            .active_session
            .lock()
            .expect("active capture session mutex poisoned") = None;
    }
}

impl Daemon {
    fn new(config: Config, state: StateHandle, waveform: WaveformPublisher) -> Result<Self> {
        let capture = CaptureService::new(config.clone(), state.clone(), waveform.clone())?;
        Ok(Self {
            config,
            state,
            next_session_id: AtomicU64::new(1),
            capture,
            waveform,
            session: None,
        })
    }

    fn has_session(&self) -> bool {
        self.session.is_some()
    }

    fn start_recording(
        &mut self,
        output_target_hint_override: Option<output::OutputTargetHint>,
    ) -> Result<()> {
        if self.session.is_some() {
            return Ok(());
        }
        if self.config.asr.provider == AsrProvider::AlibabaQwenRealtime
            && self.config.asr.alibaba.api_key.trim().is_empty()
        {
            bail!("Alibaba realtime ASR requires an API key");
        }

        let session_id = self.next_session_id.fetch_add(1, Ordering::SeqCst);
        let output_target_hint =
            output_target_hint_override.or_else(|| output::detect_output_target_hint().ok());
        let agent_context_handle = self.config.llm.agent_context_enabled.then(|| {
            thread::spawn(|| match agent_context::capture_focused_session() {
                Ok(locator) => {
                    if locator.is_none() {
                        eprintln!(
                            "voice-input agent context: no supported focused session captured"
                        );
                    }
                    locator
                }
                Err(_) => {
                    eprintln!("voice-input agent context: focused-session discovery failed");
                    None
                }
            })
        });
        let pre_roll_audio = self.capture.seed_audio();
        let asr_packetizer = (self.config.asr.provider == AsrProvider::AlibabaQwenRealtime)
            .then(|| Arc::new(Mutex::new(AsrPacketizer::default())));
        let waveform_analyzer = Arc::new(Mutex::new(WaveformAnalyzer::new(
            self.config.audio.sample_rate,
        )));
        self.waveform.try_reset(session_id);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let audio_buffer = Arc::new(Mutex::new(pre_roll_audio.clone()));
        let partial_transcript = Arc::new(Mutex::new(String::new()));
        let capture_ready = Arc::new(AtomicBool::new(self.capture.is_capture_hot()));
        let asr_ready = Arc::new(AtomicBool::new(
            self.config.asr.provider == AsrProvider::LocalCli,
        ));
        let voice_active = Arc::new(AtomicBool::new(
            self.config.asr.provider == AsrProvider::LocalCli,
        ));
        let speech_detected = Arc::new(AtomicBool::new(
            self.config.asr.provider == AsrProvider::LocalCli,
        ));

        self.state.update(|snapshot| {
            *snapshot = Snapshot {
                phase: Phase::Arming,
                class: "recording".into(),
                icon: "󰍬".into(),
                text: String::new(),
                tooltip: "Arming microphone…".into(),
                transcript: String::new(),
                bars: [0.0; WAVEFORM_BAR_COUNT],
                language: self.config.asr.language.label().into(),
                engine: self.config.asr.active_engine_label(),
                model: self.config.asr.active_model_label(),
                hud_enabled: self.config.hud.enabled,
                hud_margin_bottom: self.config.hud.margin_bottom,
                hud_height: self.config.hud.height,
                hud_position: self.config.hud.position,
                hud_offset_x: self.config.hud.offset_x,
                hud_offset_y: self.config.hud.offset_y,
                raw_transcript: None,
                refined_transcript: None,
                refinement_status: None,
                refinement_changed: None,
                output_target_hint: Some(label_output_target_hint(output_target_hint).into()),
                output_target_resolved: None,
                output_mode: None,
                output_driver: None,
                error: None,
                updated_at_ms: snapshot.updated_at_ms,
            };
        })?;

        let (asr_control_tx, asr_runtime) = match self.config.asr.provider {
            AsrProvider::LocalCli => {
                let partial_handle = spawn_partial_thread(
                    session_id,
                    self.config.clone(),
                    self.state.clone(),
                    stop_flag.clone(),
                    cancel_flag.clone(),
                    audio_buffer.clone(),
                    partial_transcript.clone(),
                );
                (None, SessionAsrRuntime::Local { partial_handle })
            }
            AsrProvider::AlibabaQwenRealtime => {
                let asr = backend::build(&self.config);
                let session = asr.spawn_session(
                    &self.config,
                    backend::AudioSpec {
                        sample_rate_hz: self.config.audio.sample_rate,
                    },
                )?;
                let control_tx = session.control_tx.clone();
                let event_handle = spawn_realtime_event_thread(RealtimeEventThreadContext {
                    session_id,
                    config: self.config.clone(),
                    state: self.state.clone(),
                    partial_transcript: partial_transcript.clone(),
                    capture_ready: capture_ready.clone(),
                    asr_ready: asr_ready.clone(),
                    voice_active: voice_active.clone(),
                    speech_detected: speech_detected.clone(),
                    event_rx: session.event_rx,
                });
                (
                    Some(control_tx),
                    SessionAsrRuntime::Realtime {
                        control_tx: session.control_tx,
                        event_handle,
                        backend_handle: session.join,
                    },
                )
            }
        };

        if let (Some(tx), Some(packetizer)) = (asr_control_tx.as_ref(), asr_packetizer.as_ref()) {
            let packets = packetizer
                .lock()
                .expect("ASR packetizer mutex poisoned")
                .push(&pre_roll_audio);
            for packet in packets {
                let _ = tx.send(backend::AsrControl::AppendPcm16(packet));
            }
        }

        let capture_mode = if self.capture.is_enabled() {
            self.capture.attach_session(ActiveCaptureSession {
                session_id,
                audio_buffer: audio_buffer.clone(),
                capture_ready: capture_ready.clone(),
                asr_ready: asr_ready.clone(),
                voice_active: voice_active.clone(),
                asr_control_tx,
                asr_packetizer: asr_packetizer.clone(),
                waveform_analyzer: waveform_analyzer.clone(),
                waveform: self.capture.waveform.clone(),
            });
            refresh_recording_readiness(
                &self.state,
                &self.config,
                session_id,
                capture_ready.load(Ordering::SeqCst),
                asr_ready.load(Ordering::SeqCst),
            )?;
            SessionCaptureMode::SharedPreRoll
        } else {
            let (child, stdout) = spawn_pw_record(&self.config)?;
            let reader_handle = spawn_reader_thread(
                stdout,
                ReaderThreadContext {
                    session_id,
                    config: self.config.clone(),
                    state: self.state.clone(),
                    stop_flag: stop_flag.clone(),
                    cancel_flag: cancel_flag.clone(),
                    audio_buffer: audio_buffer.clone(),
                    capture_ready,
                    asr_ready,
                    voice_active,
                    asr_control_tx,
                    asr_packetizer: asr_packetizer.clone(),
                    waveform_analyzer,
                    waveform: self.waveform.clone(),
                },
            );
            SessionCaptureMode::Dedicated {
                child,
                reader_handle,
            }
        };

        self.session = Some(Session {
            stop_flag,
            cancel_flag,
            audio_buffer,
            output_target_hint,
            agent_context_handle,
            asr_packetizer,
            speech_detected,
            capture_mode,
            asr_runtime,
        });
        Ok(())
    }

    fn nudge_hud(&mut self, direction: HudMoveDirection, amount: Option<i32>) -> Result<()> {
        let step = amount.unwrap_or(self.config.hud.nudge_step).max(1);
        self.update_hud_config(|hud| match direction {
            HudMoveDirection::Left => hud.offset_x -= step,
            HudMoveDirection::Right => hud.offset_x += step,
            HudMoveDirection::Up => hud.offset_y += step,
            HudMoveDirection::Down => hud.offset_y -= step,
        })
    }

    fn set_hud_position(&mut self, position: HudPosition) -> Result<()> {
        self.update_hud_config(|hud| hud.position = position)
    }

    fn center_hud(&mut self) -> Result<()> {
        self.update_hud_config(|hud| {
            hud.position = HudPosition::Center;
            hud.offset_x = 0;
        })
    }

    fn reset_hud(&mut self) -> Result<()> {
        self.update_hud_config(|hud| {
            hud.position = HudPosition::Center;
            hud.offset_x = 0;
            hud.offset_y = 0;
        })
    }

    fn update_hud_config<F>(&mut self, update: F) -> Result<()>
    where
        F: FnOnce(&mut HudConfig),
    {
        update(&mut self.config.hud);
        self.config.hud.nudge_step = self.config.hud.nudge_step.max(1);
        self.config.save()?;

        let hud = self.config.hud.clone();
        self.state.update(move |snapshot| {
            snapshot.hud_enabled = hud.enabled;
            snapshot.hud_margin_bottom = hud.margin_bottom;
            snapshot.hud_height = hud.height;
            snapshot.hud_position = hud.position;
            snapshot.hud_offset_x = hud.offset_x;
            snapshot.hud_offset_y = hud.offset_y;
        })?;
        Ok(())
    }

    fn finish_recording(&mut self, mut cancel: bool) -> Result<()> {
        let Some(mut session) = self.session.take() else {
            return Ok(());
        };
        let output_target_hint = session.output_target_hint;

        if cancel {
            session.cancel_flag.store(true, Ordering::SeqCst);
        }
        session.stop_flag.store(true, Ordering::SeqCst);
        match session.capture_mode {
            SessionCaptureMode::Dedicated {
                mut child,
                reader_handle,
            } => {
                child.kill().ok();
                let _ = child.wait();
                join_session_handle(reader_handle, "audio reader")?;
            }
            SessionCaptureMode::SharedPreRoll => {
                self.capture.detach_session();
            }
        }

        if !cancel && matches!(&session.asr_runtime, SessionAsrRuntime::Realtime { .. }) {
            // Speech/transcript events can trail the final PCM packet slightly.
            // Give the event pump a brief chance to observe them, then treat a
            // session with no server-detected speech as a cancellation. This
            // avoids sending silence through final ASR, LLM refinement, or the
            // active text input and also avoids Qwen waiting on an empty commit.
            let deadline = Instant::now() + Duration::from_millis(SPEECH_EVENT_GRACE_MS);
            while !session.speech_detected.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if !session.speech_detected.load(Ordering::SeqCst) {
                eprintln!(
                    "voice-input realtime ASR: no speech detected; cancelling empty dictation"
                );
                session.cancel_flag.store(true, Ordering::SeqCst);
                cancel = true;
            }
        }

        let final_asr_packet = if cancel {
            None
        } else {
            session.asr_packetizer.take().and_then(|packetizer| {
                packetizer
                    .lock()
                    .expect("ASR packetizer mutex poisoned")
                    .flush()
            })
        };
        self.waveform.try_reset(0);

        let agent_locator = session
            .agent_context_handle
            .take()
            .and_then(|handle| handle.join().ok().flatten());
        let audio = session
            .audio_buffer
            .lock()
            .expect("audio buffer mutex poisoned")
            .clone();

        let raw_transcript = match session.asr_runtime {
            SessionAsrRuntime::Local { partial_handle } => {
                join_session_handle(partial_handle, "partial transcriber")?;

                if cancel {
                    String::new()
                } else {
                    if audio.is_empty() {
                        self.state.update(|snapshot| {
                            *snapshot = Snapshot::idle(&self.config);
                            snapshot.tooltip = "Voice Input idle\nNo audio captured".into();
                        })?;
                        return Ok(());
                    }

                    self.state.update(|snapshot| {
                        snapshot.phase = Phase::Transcribing;
                        snapshot.class = "transcribing".into();
                        snapshot.icon = "󰔟".into();
                        snapshot.tooltip = "Transcribing…".into();
                        snapshot.bars = PROCESSING_WAVEFORM;
                    })?;

                    self.transcribe_local_audio(&audio)?
                }
            }
            SessionAsrRuntime::Realtime {
                control_tx,
                event_handle,
                backend_handle,
            } => {
                if !cancel {
                    self.state.update(|snapshot| {
                        snapshot.phase = Phase::Transcribing;
                        snapshot.class = "transcribing".into();
                        snapshot.icon = "󰔟".into();
                        snapshot.tooltip = "Transcribing…".into();
                        snapshot.bars = PROCESSING_WAVEFORM;
                    })?;
                }

                if let Some(packet) = final_asr_packet {
                    let _ = control_tx.send(backend::AsrControl::AppendPcm16(packet));
                }
                let _ = control_tx.send(if cancel {
                    backend::AsrControl::Cancel
                } else {
                    backend::AsrControl::Finish
                });
                let backend_result = join_session_handle(backend_handle, "realtime ASR worker");
                let event_result = join_value_handle(event_handle, "realtime ASR event pump");

                if cancel {
                    backend_result?;
                    let _ = event_result?;
                    String::new()
                } else {
                    if audio.is_empty() {
                        self.state.update(|snapshot| {
                            *snapshot = Snapshot::idle(&self.config);
                            snapshot.tooltip = "Voice Input idle\nNo audio captured".into();
                        })?;
                        return Ok(());
                    }

                    let remote_transcript = match (backend_result, event_result) {
                        (Ok(()), Ok(Some(text))) if !text.trim().is_empty() => Some(
                            backend::apply_script_conversion(self.config.asr.language, &text)?,
                        ),
                        (Ok(()), Ok(_)) => None,
                        (Err(_), Ok(Some(text))) if !text.trim().is_empty() => Some(
                            backend::apply_script_conversion(self.config.asr.language, &text)?,
                        ),
                        (Err(error), Ok(_)) | (Ok(()), Err(error)) => {
                            if self.config.asr.fallback_to_local {
                                let _ = self.state.update(|snapshot| {
                                    snapshot.tooltip = format!(
                                        "Realtime ASR failed, falling back locally…\n{error}"
                                    );
                                });
                                None
                            } else {
                                return Err(error);
                            }
                        }
                        (Err(error), Err(_)) => {
                            if self.config.asr.fallback_to_local {
                                let _ = self.state.update(|snapshot| {
                                    snapshot.tooltip = format!(
                                        "Realtime ASR failed, falling back locally…\n{error}"
                                    );
                                });
                                None
                            } else {
                                return Err(error);
                            }
                        }
                    };

                    if self.config.asr.alibaba.final_pass_enabled {
                        self.state.update(|snapshot| {
                            snapshot.phase = Phase::Transcribing;
                            snapshot.class = "transcribing".into();
                            snapshot.icon = "󰔟".into();
                            snapshot.tooltip = "Retranscribing full audio…".into();
                            snapshot.bars = PROCESSING_WAVEFORM;
                        })?;

                        match self.transcribe_alibaba_full_audio(&audio) {
                            Ok(text) => text,
                            Err(error) => {
                                if let Some(text) = remote_transcript {
                                    let _ = self.state.update(|snapshot| {
                                        snapshot.tooltip = format!(
                                            "Full-audio retranscription failed, using realtime final text…\n{error}"
                                        );
                                    });
                                    text
                                } else if self.config.asr.fallback_to_local {
                                    let _ = self.state.update(|snapshot| {
                                        snapshot.tooltip = format!(
                                            "Full-audio retranscription failed, falling back locally…\n{error}"
                                        );
                                    });
                                    self.transcribe_local_audio(&audio)?
                                } else {
                                    return Err(error);
                                }
                            }
                        }
                    } else if let Some(text) = remote_transcript {
                        text
                    } else if self.config.asr.fallback_to_local {
                        self.transcribe_local_audio(&audio)?
                    } else {
                        bail!("Alibaba realtime ASR returned no final transcript");
                    }
                }
            }
        };

        if cancel {
            self.state
                .update(|snapshot| *snapshot = Snapshot::idle(&self.config))?;
            return Ok(());
        }

        self.state.update(|snapshot| {
            snapshot.transcript = raw_transcript.clone();
            snapshot.tooltip = raw_transcript.clone();
            snapshot.raw_transcript = Some(raw_transcript.clone());
            snapshot.refined_transcript = None;
            snapshot.refinement_status = Some(if self.config.llm.enabled {
                "pending".into()
            } else {
                "disabled".into()
            });
            snapshot.refinement_changed = None;
        })?;

        let agent_reference = agent_locator.as_ref().and_then(|locator| {
            match agent_context::load_reference(
                locator,
                self.config.llm.agent_context_max_chars,
            ) {
                Ok(reference) => {
                    if reference.is_none() {
                        eprintln!(
                            "voice-input agent context: captured session has no usable completed assistant message"
                        );
                    }
                    reference
                }
                Err(_) => {
                    eprintln!("voice-input agent context: captured session could not be read");
                    None
                }
            }
        });
        if let Some(reference) = agent_reference.as_ref() {
            eprintln!(
                "voice-input refinement: using {} agent context ({} chars)",
                reference.agent.label(),
                reference.text.chars().count()
            );
        }

        let (final_transcript, refinement_status, refinement_changed) = if self.config.llm.enabled {
            self.state.update(|snapshot| {
                snapshot.phase = Phase::Refining;
                snapshot.class = "refining".into();
                snapshot.icon = "󰚩".into();
                snapshot.tooltip = "Refining transcript…".into();
                snapshot.refinement_status = Some("running".into());
            })?;

            match llm::maybe_refine(&self.config, &raw_transcript, agent_reference.as_ref()) {
                Ok(value) => {
                    let changed = value.trim() != raw_transcript.trim();
                    let status = if changed { "applied" } else { "unchanged" };
                    (value, status.to_string(), Some(changed))
                }
                Err(error) => {
                    let message = format!("failed: {}", truncate_for_tooltip(&error.to_string()));
                    (raw_transcript.clone(), message, Some(false))
                }
            }
        } else {
            (raw_transcript.clone(), "disabled".into(), None)
        };

        self.state.update(|snapshot| {
            snapshot.phase = Phase::Outputting;
            snapshot.class = "outputting".into();
            snapshot.icon = "󰌌".into();
            snapshot.tooltip = "Sending text…".into();
            snapshot.transcript = final_transcript.clone();
            snapshot.raw_transcript = Some(raw_transcript.clone());
            snapshot.refined_transcript = Some(final_transcript.clone());
            snapshot.refinement_status = Some(refinement_status.clone());
            snapshot.refinement_changed = refinement_changed;
            snapshot.output_target_hint = Some(label_output_target_hint(output_target_hint).into());
            snapshot.output_target_resolved = None;
            snapshot.output_mode = None;
            snapshot.output_driver = None;
        })?;

        let emit_report =
            match output::emit_text(&self.config, &final_transcript, output_target_hint) {
                Ok(report) => report,
                Err(error) => {
                    self.state.update(|snapshot| {
                        snapshot.phase = Phase::Error;
                        snapshot.class = "error".into();
                        snapshot.icon = "󰅙".into();
                        snapshot.tooltip = error.to_string();
                        snapshot.error = Some(error.to_string());
                        snapshot.transcript = final_transcript.clone();
                        snapshot.raw_transcript = Some(raw_transcript.clone());
                        snapshot.refined_transcript = Some(final_transcript.clone());
                        snapshot.refinement_status = Some(refinement_status.clone());
                        snapshot.refinement_changed = refinement_changed;
                        snapshot.output_target_hint =
                            Some(label_output_target_hint(output_target_hint).into());
                    })?;
                    return Err(error);
                }
            };
        self.state.update(|snapshot| {
            *snapshot = Snapshot::idle(&self.config);
            snapshot.transcript = final_transcript.clone();
            snapshot.raw_transcript = Some(raw_transcript.clone());
            snapshot.refined_transcript = Some(final_transcript.clone());
            snapshot.refinement_status = Some(refinement_status.clone());
            snapshot.refinement_changed = refinement_changed;
            snapshot.output_target_hint = Some(label_output_target_hint(output_target_hint).into());
            snapshot.output_target_resolved = Some(emit_report.target.clone());
            snapshot.output_mode = Some(emit_report.mode.clone());
            snapshot.output_driver = Some(emit_report.driver.clone());
            snapshot.tooltip = format!(
                "Voice Input idle\nLanguage: {}\nOutput: {} / {} / {}\nLast transcript: {}",
                self.config.asr.language.label(),
                emit_report.target,
                emit_report.mode,
                emit_report.driver,
                truncate_for_tooltip(&final_transcript)
            );
        })?;
        Ok(())
    }
}

fn refresh_recording_readiness(
    state: &StateHandle,
    config: &Config,
    session_id: u64,
    capture_ready: bool,
    asr_ready: bool,
) -> Result<()> {
    state.update(|snapshot| {
        if snapshot.phase == Phase::Idle
            || snapshot.phase == Phase::Transcribing
            || snapshot.phase == Phase::Refining
            || snapshot.phase == Phase::Error
        {
            return;
        }

        snapshot.class = "recording".into();
        snapshot.icon = "󰍬".into();
        snapshot.text = format!("session-{session_id}");

        if capture_ready && asr_ready {
            snapshot.phase = Phase::Recording;
            snapshot.tooltip = if snapshot.transcript.is_empty() {
                format!("Listening…\nLanguage: {}", config.asr.language.label())
            } else {
                snapshot.transcript.clone()
            };
        } else {
            snapshot.phase = Phase::Arming;
            if snapshot.transcript.is_empty() {
                snapshot.tooltip = arming_tooltip(config, capture_ready, asr_ready);
            }
        }
    })
}

fn arming_tooltip(config: &Config, capture_ready: bool, asr_ready: bool) -> String {
    if !capture_ready {
        "Arming microphone…".into()
    } else if !asr_ready {
        format!(
            "Connecting ASR…\nLanguage: {}\nEngine: {}",
            config.asr.language.label(),
            config.asr.active_engine_label()
        )
    } else {
        format!("Listening…\nLanguage: {}", config.asr.language.label())
    }
}

fn capture_warmup_samples(config: &Config) -> usize {
    ((config.audio.sample_rate as usize) * 320) / 1_000
}

fn pre_roll_samples(config: &Config) -> usize {
    ((config.audio.sample_rate as usize) * (config.audio.pre_roll_ms as usize)) / 1_000
}

fn spawn_pw_record(config: &Config) -> Result<(Child, std::process::ChildStdout)> {
    let mut command = Command::new("pw-record");
    command
        .arg("--raw")
        .arg("--rate")
        .arg(config.audio.sample_rate.to_string())
        .arg("--channels")
        .arg("1")
        .arg("--format")
        .arg("s16")
        .arg("-");
    if config.audio.device != "default" {
        command.arg("--target").arg(&config.audio.device);
    }

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start pw-record")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("pw-record did not provide stdout"))?;
    Ok((child, stdout))
}

fn run_capture_service(
    config: Config,
    state: StateHandle,
    capture_hot: Arc<AtomicBool>,
    ring_buffer: Arc<Mutex<VecDeque<i16>>>,
    active_session: Arc<Mutex<Option<ActiveCaptureSession>>>,
) -> Result<()> {
    let max_ring_samples = pre_roll_samples(&config).max(capture_warmup_samples(&config).max(1));
    let (mut child, mut stdout) = spawn_pw_record(&config)?;
    let mut bytes = [0u8; 512];

    loop {
        let read = stdout
            .read(&mut bytes)
            .context("failed reading from pre-roll pw-record")?;
        if read == 0 {
            break;
        }

        let mut chunk = Vec::with_capacity(read / 2);
        for pair in bytes[..read].chunks_exact(2) {
            chunk.push(i16::from_le_bytes([pair[0], pair[1]]));
        }

        {
            let mut ring = ring_buffer
                .lock()
                .expect("capture ring buffer mutex poisoned");
            for sample in &chunk {
                ring.push_back(*sample);
            }
            while ring.len() > max_ring_samples {
                ring.pop_front();
            }
            if ring.len() >= capture_warmup_samples(&config) {
                capture_hot.store(true, Ordering::SeqCst);
            }
        }

        let active = active_session
            .lock()
            .expect("active capture session mutex poisoned")
            .clone();

        if let Some(active) = active {
            {
                let mut buffer = active
                    .audio_buffer
                    .lock()
                    .expect("audio buffer mutex poisoned");
                buffer.extend_from_slice(&chunk);
            }

            if capture_hot.load(Ordering::SeqCst)
                && !active.capture_ready.swap(true, Ordering::SeqCst)
            {
                refresh_recording_readiness(
                    &state,
                    &config,
                    active.session_id,
                    true,
                    active.asr_ready.load(Ordering::SeqCst),
                )?;
            }

            if let (Some(tx), Some(packetizer)) = (
                active.asr_control_tx.as_ref(),
                active.asr_packetizer.as_ref(),
            ) {
                let packets = packetizer
                    .lock()
                    .expect("ASR packetizer mutex poisoned")
                    .push(&chunk);
                for packet in packets {
                    let _ = tx.send(backend::AsrControl::AppendPcm16(packet));
                }
            }

            let frames = active
                .waveform_analyzer
                .lock()
                .expect("waveform analyzer mutex poisoned")
                .push(&chunk, active.voice_active.load(Ordering::Relaxed));
            for bars in frames {
                active.waveform.try_publish(active.session_id, bars);
            }
        }
    }

    let _ = child.wait();
    bail!("pre-roll capture stream ended unexpectedly")
}

struct ReaderThreadContext {
    session_id: u64,
    config: Config,
    state: StateHandle,
    stop_flag: Arc<AtomicBool>,
    cancel_flag: Arc<AtomicBool>,
    audio_buffer: Arc<Mutex<Vec<i16>>>,
    capture_ready: Arc<AtomicBool>,
    asr_ready: Arc<AtomicBool>,
    voice_active: Arc<AtomicBool>,
    asr_control_tx: Option<mpsc::Sender<backend::AsrControl>>,
    asr_packetizer: Option<Arc<Mutex<AsrPacketizer>>>,
    waveform_analyzer: Arc<Mutex<WaveformAnalyzer>>,
    waveform: WaveformPublisher,
}

fn spawn_reader_thread(
    mut stdout: impl Read + Send + 'static,
    context: ReaderThreadContext,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        let ReaderThreadContext {
            session_id,
            config,
            state,
            stop_flag,
            cancel_flag,
            audio_buffer,
            capture_ready,
            asr_ready,
            voice_active,
            asr_control_tx,
            asr_packetizer,
            waveform_analyzer,
            waveform,
        } = context;
        let started = Instant::now();
        let mut bytes = [0u8; 512];
        let mut asr_control_tx = asr_control_tx;

        loop {
            if stop_flag.load(Ordering::SeqCst) || cancel_flag.load(Ordering::SeqCst) {
                break;
            }
            let read = stdout
                .read(&mut bytes)
                .context("failed reading from pw-record")?;
            if read == 0 {
                break;
            }

            let mut chunk = Vec::with_capacity(read / 2);
            for pair in bytes[..read].chunks_exact(2) {
                chunk.push(i16::from_le_bytes([pair[0], pair[1]]));
            }

            let total_samples = {
                let mut buffer = audio_buffer.lock().expect("audio buffer mutex poisoned");
                buffer.extend_from_slice(&chunk);
                buffer.len()
            };

            if total_samples >= capture_warmup_samples(&config)
                && !capture_ready.swap(true, Ordering::SeqCst)
            {
                refresh_recording_readiness(
                    &state,
                    &config,
                    session_id,
                    capture_ready.load(Ordering::SeqCst),
                    asr_ready.load(Ordering::SeqCst),
                )?;
            }

            if let (Some(tx), Some(packetizer)) = (asr_control_tx.as_ref(), asr_packetizer.as_ref())
            {
                let packets = packetizer
                    .lock()
                    .expect("ASR packetizer mutex poisoned")
                    .push(&chunk);
                for packet in packets {
                    if tx.send(backend::AsrControl::AppendPcm16(packet)).is_err() {
                        asr_control_tx = None;
                        break;
                    }
                }
            }

            let frames = waveform_analyzer
                .lock()
                .expect("waveform analyzer mutex poisoned")
                .push(&chunk, voice_active.load(Ordering::Relaxed));
            for bars in frames {
                waveform.try_publish(session_id, bars);
            }

            if started.elapsed() >= Duration::from_secs(config.audio.max_duration_secs) {
                stop_flag.store(true, Ordering::SeqCst);
                break;
            }
        }

        Ok(())
    })
}

struct RealtimeEventThreadContext {
    session_id: u64,
    config: Config,
    state: StateHandle,
    partial_transcript: Arc<Mutex<String>>,
    capture_ready: Arc<AtomicBool>,
    asr_ready: Arc<AtomicBool>,
    voice_active: Arc<AtomicBool>,
    speech_detected: Arc<AtomicBool>,
    event_rx: mpsc::Receiver<backend::AsrEvent>,
}

fn spawn_realtime_event_thread(
    context: RealtimeEventThreadContext,
) -> thread::JoinHandle<Result<Option<String>>> {
    thread::spawn(move || {
        let RealtimeEventThreadContext {
            session_id,
            config,
            state,
            partial_transcript,
            capture_ready,
            asr_ready,
            voice_active,
            speech_detected,
            event_rx,
        } = context;
        let mut final_transcript = None;

        while let Ok(event) = event_rx.recv() {
            match event {
                backend::AsrEvent::Ready => {
                    asr_ready.store(true, Ordering::SeqCst);
                    refresh_recording_readiness(
                        &state,
                        &config,
                        session_id,
                        capture_ready.load(Ordering::SeqCst),
                        asr_ready.load(Ordering::SeqCst),
                    )?;
                }
                backend::AsrEvent::SpeechStarted => {
                    speech_detected.store(true, Ordering::SeqCst);
                    voice_active.store(true, Ordering::Relaxed);
                }
                backend::AsrEvent::SpeechStopped => {
                    voice_active.store(false, Ordering::Relaxed);
                }
                backend::AsrEvent::Partial {
                    committed,
                    unstable,
                } => {
                    let transcript = format!("{committed}{unstable}");
                    if !transcript.trim().is_empty() {
                        speech_detected.store(true, Ordering::SeqCst);
                    }
                    *partial_transcript
                        .lock()
                        .expect("partial transcript mutex poisoned") = transcript.clone();
                    state.update(|snapshot| {
                        if snapshot.phase == Phase::Arming
                            || snapshot.phase == Phase::Recording
                            || snapshot.phase == Phase::Transcribing
                        {
                            snapshot.transcript = transcript.clone();
                            snapshot.tooltip =
                                if snapshot.phase == Phase::Arming && transcript.is_empty() {
                                    arming_tooltip(
                                        &config,
                                        capture_ready.load(Ordering::SeqCst),
                                        asr_ready.load(Ordering::SeqCst),
                                    )
                                } else if transcript.is_empty() {
                                    format!("Listening…\nLanguage: {}", config.asr.language.label())
                                } else {
                                    transcript.clone()
                                };
                            snapshot.text = format!("session-{session_id}");
                        }
                    })?;
                }
                backend::AsrEvent::SegmentFinal { text } => {
                    if !text.trim().is_empty() {
                        speech_detected.store(true, Ordering::SeqCst);
                    }
                    *partial_transcript
                        .lock()
                        .expect("partial transcript mutex poisoned") = text.clone();
                    state.update(|snapshot| {
                        if snapshot.phase != Phase::Idle {
                            snapshot.transcript = text.clone();
                            snapshot.tooltip = text.clone();
                            snapshot.text = format!("session-{session_id}");
                        }
                    })?;
                }
                backend::AsrEvent::Final { text } => {
                    if !text.trim().is_empty() {
                        speech_detected.store(true, Ordering::SeqCst);
                    }
                    *partial_transcript
                        .lock()
                        .expect("partial transcript mutex poisoned") = text.clone();
                    final_transcript = Some(text.clone());
                    state.update(|snapshot| {
                        if snapshot.phase != Phase::Idle {
                            snapshot.transcript = text.clone();
                            snapshot.tooltip = text.clone();
                        }
                    })?;
                }
                backend::AsrEvent::Error { message } => {
                    return Err(anyhow!("Alibaba realtime ASR failed: {message}"));
                }
            }
        }

        Ok(final_transcript)
    })
}

fn spawn_partial_thread(
    session_id: u64,
    config: Config,
    state: StateHandle,
    stop_flag: Arc<AtomicBool>,
    cancel_flag: Arc<AtomicBool>,
    audio_buffer: Arc<Mutex<Vec<i16>>>,
    partial_transcript: Arc<Mutex<String>>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        let mut last_sample_count = 0usize;

        while !stop_flag.load(Ordering::SeqCst) && !cancel_flag.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(config.audio.partial_interval_ms));

            let audio = audio_buffer
                .lock()
                .expect("audio buffer mutex poisoned")
                .clone();
            if audio.len() < (config.audio.sample_rate / 2) as usize {
                continue;
            }
            if audio.len().saturating_sub(last_sample_count)
                < (config.audio.sample_rate / 3) as usize
            {
                continue;
            }
            last_sample_count = audio.len();

            let temp_file = tempfile::NamedTempFile::new()
                .context("failed to create temp WAV for partial ASR")?;
            wav::write_pcm16_wav(temp_file.path(), config.audio.sample_rate, &audio)?;

            if let Ok(transcript) = backend::transcribe(&config, temp_file.path()) {
                *partial_transcript
                    .lock()
                    .expect("partial transcript mutex poisoned") = transcript.clone();
                state.update(|snapshot| {
                    if snapshot.phase == Phase::Recording {
                        snapshot.transcript = transcript.clone();
                        snapshot.tooltip = transcript.clone();
                        snapshot.text = format!("session-{session_id}");
                    }
                })?;
            }
        }

        Ok(())
    })
}

fn join_session_handle(handle: thread::JoinHandle<Result<()>>, label: &str) -> Result<()> {
    match handle.join() {
        Ok(result) => result.with_context(|| format!("{label} thread failed")),
        Err(_) => bail!("{label} thread panicked"),
    }
}

fn join_value_handle<T>(handle: thread::JoinHandle<Result<T>>, label: &str) -> Result<T> {
    match handle.join() {
        Ok(result) => result.with_context(|| format!("{label} thread failed")),
        Err(_) => bail!("{label} thread panicked"),
    }
}

fn truncate_for_tooltip(text: &str) -> String {
    const MAX_LEN: usize = 96;
    let mut shortened = text.chars().take(MAX_LEN).collect::<String>();
    if text.chars().count() > MAX_LEN {
        shortened.push('…');
    }
    shortened
}

impl Daemon {
    fn transcribe_alibaba_full_audio(&self, audio: &[i16]) -> Result<String> {
        let temp_file = tempfile::NamedTempFile::new()
            .context("failed to create WAV temp file for Alibaba full-audio retranscription")?;
        wav::write_pcm16_wav(temp_file.path(), self.config.audio.sample_rate, audio)?;
        backend::transcribe_alibaba_full_audio(&self.config, temp_file.path())
    }

    fn transcribe_local_audio(&self, audio: &[i16]) -> Result<String> {
        let temp_file = tempfile::NamedTempFile::new().context("failed to create WAV temp file")?;
        wav::write_pcm16_wav(temp_file.path(), self.config.audio.sample_rate, audio)?;

        let mut local_config = self.config.clone();
        local_config.asr.provider = AsrProvider::LocalCli;
        backend::transcribe(&local_config, temp_file.path())
    }
}
