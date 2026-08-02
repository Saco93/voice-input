use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
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
    focused_window::{self, RefinementCategory},
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
const CAPTURE_STOP_DRAIN: Duration = Duration::from_millis(120);
const CONTROL_COMMAND_MAX_BYTES: u64 = 4 * 1024;
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(2);

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
    let server = Arc::new(ControlServer {
        daemon: Mutex::new(Daemon::new(config, state, waveform)?),
        idle_generation: AtomicU64::new(0),
    });
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind control socket at {}", socket_path.display()))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).with_context(|| {
        format!(
            "failed to restrict control socket permissions at {}",
            socket_path.display()
        )
    })?;
    println!("Voice Input daemon listening on {}", socket_path.display());

    loop {
        let (stream, _) = listener
            .accept()
            .context("failed to accept control socket")?;
        let receipt = ControlReceipt {
            idle_generation: server.idle_generation.load(Ordering::SeqCst),
            accepted_at: Instant::now(),
        };
        let server = server.clone();
        thread::spawn(move || {
            if let Err(error) = serve_control_connection(stream, &server, receipt) {
                eprintln!("voice-input control connection failed: {error:#}");
            }
        });
    }
}

struct ControlServer {
    daemon: Mutex<Daemon>,
    idle_generation: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct ControlReceipt {
    idle_generation: u64,
    accepted_at: Instant,
}

fn serve_control_connection(
    mut stream: UnixStream,
    server: &ControlServer,
    receipt: ControlReceipt,
) -> Result<()> {
    stream
        .set_read_timeout(Some(CONTROL_READ_TIMEOUT))
        .context("failed to set control socket read timeout")?;
    let command = read_control_command(&mut stream)?;
    let response = match handle_control(server, receipt, command.trim()) {
        Ok(response) => response,
        Err(error) => format!("error: {error:#}\n"),
    };
    stream
        .write_all(response.as_bytes())
        .context("failed to write control response")
}

fn read_control_command(reader: &mut impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(CONTROL_COMMAND_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read control command")?;
    if bytes.len() as u64 > CONTROL_COMMAND_MAX_BYTES {
        bail!("control command exceeds {CONTROL_COMMAND_MAX_BYTES} bytes");
    }
    String::from_utf8(bytes).context("control command is not valid UTF-8")
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

fn handle_control(
    server: &ControlServer,
    receipt: ControlReceipt,
    command: &str,
) -> Result<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let Some(head) = parts.first().copied() else {
        bail!("empty control command");
    };
    // Log receipt before waiting for the daemon mutex. This distinguishes a
    // compositor/keybinding miss from a request queued behind finalization.
    let is_recording_control = matches!(head, "start" | "stop" | "toggle" | "cancel" | "restart");
    let focused_window_hint = if is_recording_control {
        focused_window::parse_control_hint(&parts[1..])?
    } else {
        None
    };
    if is_recording_control {
        eprintln!(
            "voice-input control: received {head} in generation {}",
            receipt.idle_generation
        );
    }

    let mut daemon = server.daemon.lock().expect("daemon mutex poisoned");
    let current_generation = server.idle_generation.load(Ordering::SeqCst);
    if is_recording_control && !control_receipt_is_current(receipt, current_generation) {
        eprintln!(
            "voice-input control: ignored stale {head} from generation {} (current {current_generation}, queued {} ms)",
            receipt.idle_generation,
            receipt.accepted_at.elapsed().as_millis()
        );
        return Ok("ignored stale control\n".to_string());
    }

    let mut completed_session = false;
    let result = match head {
        "start" => daemon
            .start_recording(parse_output_target_hint_args(&parts[1..])?)
            .map(|_| "ok\n".to_string()),
        "stop" => {
            completed_session = daemon.has_session();
            daemon
                .finish_recording(false, focused_window_hint)
                .map(|_| "ok\n".to_string())
        }
        "toggle" => if daemon.has_session() {
            completed_session = true;
            daemon.finish_recording(false, focused_window_hint)
        } else {
            parse_output_target_hint_args(&parts[1..])
                .and_then(|target_hint| daemon.start_recording(target_hint))
        }
        .map(|_| "ok\n".to_string()),
        "cancel" => {
            completed_session = daemon.has_session();
            daemon
                .finish_recording(true, None)
                .map(|_| "ok\n".to_string())
        }
        "restart" => if daemon.has_session() {
            completed_session = true;
            parse_output_target_hint_args(&parts[1..]).and_then(|target_hint| {
                daemon.finish_recording(true, None)?;
                daemon.start_recording(target_hint)
            })
        } else {
            return Ok("ignored idle restart\n".to_string());
        }
        .map(|_| "ok\n".to_string()),
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
    if completed_session {
        let next_generation = advance_idle_generation(&server.idle_generation);
        eprintln!("voice-input control: entered idle generation {next_generation}");
    }
    result
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

fn control_receipt_is_current(receipt: ControlReceipt, current_generation: u64) -> bool {
    receipt.idle_generation == current_generation
}

fn advance_idle_generation(generation: &AtomicU64) -> u64 {
    generation.fetch_add(1, Ordering::SeqCst) + 1
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn parse_output_target_hint_args(args: &[&str]) -> Result<Option<output::OutputTargetHint>> {
    let value = args
        .iter()
        .copied()
        .find(|value| !value.starts_with("focus="));
    match value {
        None => Ok(None),
        Some("wayland") => Ok(Some(output::OutputTargetHint::Wayland)),
        Some("xwayland") => Ok(Some(output::OutputTargetHint::XWayland)),
        Some(other) => bail!("unknown output target hint `{other}`"),
    }
}

fn should_drain_capture(cancel: bool, already_stopped: bool) -> bool {
    !cancel && !already_stopped
}

fn should_capture_focused_window(cancel: bool, llm_enabled: bool) -> bool {
    !cancel && llm_enabled
}

fn should_capture_agent_context(
    cancel: bool,
    llm_enabled: bool,
    agent_context_enabled: bool,
) -> bool {
    !cancel && llm_enabled && agent_context_enabled
}

#[derive(Default)]
struct StopRefinementContext {
    category: RefinementCategory,
    agent: Option<agent_context::AgentKind>,
    agent_handle: Option<thread::JoinHandle<Option<agent_context::AgentSessionLocator>>>,
}

fn capture_refinement_context_at_stop(
    config: &Config,
    cancel: bool,
    focused_window_hint: Option<focused_window::FocusedWindowSnapshot>,
) -> StopRefinementContext {
    if !should_capture_focused_window(cancel, config.llm.enabled) {
        return StopRefinementContext::default();
    }

    let window = match focused_window_hint
        .map(Ok)
        .unwrap_or_else(focused_window::capture)
    {
        Ok(window) => window,
        Err(_) => {
            eprintln!("voice-input refinement context: focused-window capture failed at stop");
            return StopRefinementContext::default();
        }
    };
    let category = window.refinement_category();
    if category != RefinementCategory::Default {
        eprintln!(
            "voice-input refinement context: captured {} destination at stop",
            category.label()
        );
    }

    let snapshot = match agent_context::capture_focused_agent(&window) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            return StopRefinementContext {
                category,
                ..StopRefinementContext::default()
            };
        }
        Err(_) => {
            eprintln!("voice-input refinement context: focused-agent capture failed at stop");
            return StopRefinementContext {
                category,
                ..StopRefinementContext::default()
            };
        }
    };
    let agent = snapshot.agent();
    eprintln!(
        "voice-input refinement context: captured focused {} destination at stop",
        agent.label()
    );

    if !should_capture_agent_context(cancel, config.llm.enabled, config.llm.agent_context_enabled) {
        return StopRefinementContext {
            category,
            agent: Some(agent),
            agent_handle: None,
        };
    }

    let agent_handle = Some(thread::spawn(
        move || match agent_context::resolve_focused_session(snapshot) {
            Ok(Some(locator)) => Some(locator),
            Ok(None) => {
                eprintln!("voice-input agent context: captured process has no valid session");
                None
            }
            Err(_) => {
                eprintln!("voice-input agent context: captured session discovery failed");
                None
            }
        },
    ));
    StopRefinementContext {
        category,
        agent: Some(agent),
        agent_handle,
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
    realtime_overloaded: Arc<AtomicBool>,
    audio_buffer: Arc<Mutex<Vec<i16>>>,
    output_target_hint: Option<output::OutputTargetHint>,
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
        control_tx: mpsc::SyncSender<backend::AsrControl>,
        abort_flag: Arc<AtomicBool>,
        event_handle: thread::JoinHandle<Result<Option<String>>>,
        backend_handle: thread::JoinHandle<Result<()>>,
    },
}

#[derive(Clone)]
struct ActiveCaptureSession {
    session_id: u64,
    stop_flag: Arc<AtomicBool>,
    automatic_finish_requested: Arc<AtomicBool>,
    realtime_overloaded: Arc<AtomicBool>,
    asr_abort_flag: Option<Arc<AtomicBool>>,
    audio_buffer: Arc<Mutex<Vec<i16>>>,
    capture_ready: Arc<AtomicBool>,
    asr_ready: Arc<AtomicBool>,
    asr_control_tx: Option<mpsc::SyncSender<backend::AsrControl>>,
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
        let remote_api_key = match self.config.asr.provider {
            AsrProvider::AlibabaQwenRealtime => Some(&self.config.asr.alibaba.api_key),
            AsrProvider::AlibabaQwenAudio3 => Some(&self.config.asr.alibaba_audio3.api_key),
            AsrProvider::LocalCli => None,
        };
        if remote_api_key.is_some_and(|api_key| api_key.trim().is_empty()) {
            bail!("Alibaba streaming ASR requires an API key");
        }

        let session_id = self.next_session_id.fetch_add(1, Ordering::SeqCst);
        let output_target_hint =
            output_target_hint_override.or_else(|| output::detect_output_target_hint().ok());
        let pre_roll_audio = self.capture.seed_audio();
        let asr_packetizer = matches!(
            self.config.asr.provider,
            AsrProvider::AlibabaQwenRealtime | AsrProvider::AlibabaQwenAudio3
        )
        .then(|| Arc::new(Mutex::new(AsrPacketizer::default())));
        let waveform_analyzer = Arc::new(Mutex::new(WaveformAnalyzer::new(
            self.config.audio.sample_rate,
        )));
        self.waveform.try_reset(session_id);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let automatic_finish_requested = Arc::new(AtomicBool::new(false));
        let realtime_overloaded = Arc::new(AtomicBool::new(false));
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
                recording_started_at_ms: None,
                recording_duration_ms: 0,
                raw_transcript: None,
                refined_transcript: None,
                refinement_status: None,
                refinement_changed: None,
                output_target_hint: Some(label_output_target_hint(output_target_hint).into()),
                output_target_resolved: None,
                output_mode: None,
                output_driver: None,
                error: None,
                revision: snapshot.revision,
                updated_at_ms: snapshot.updated_at_ms,
            };
        })?;

        let (asr_control_tx, asr_abort_flag, asr_runtime) = match self.config.asr.provider {
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
                (None, None, SessionAsrRuntime::Local { partial_handle })
            }
            AsrProvider::AlibabaQwenRealtime | AsrProvider::AlibabaQwenAudio3 => {
                let asr = backend::build(&self.config);
                let session = asr.spawn_session(
                    &self.config,
                    backend::AudioSpec {
                        sample_rate_hz: self.config.audio.sample_rate,
                    },
                )?;
                let control_tx = session.control_tx.clone();
                let abort_flag = session.abort_flag.clone();
                let event_handle = spawn_realtime_event_thread(RealtimeEventThreadContext {
                    session_id,
                    config: self.config.clone(),
                    state: self.state.clone(),
                    partial_transcript: partial_transcript.clone(),
                    capture_ready: capture_ready.clone(),
                    asr_ready: asr_ready.clone(),
                    voice_active: voice_active.clone(),
                    speech_detected: speech_detected.clone(),
                    realtime_overloaded: realtime_overloaded.clone(),
                    event_rx: session.event_rx,
                });
                (
                    Some(control_tx),
                    Some(abort_flag.clone()),
                    SessionAsrRuntime::Realtime {
                        control_tx: session.control_tx,
                        abort_flag,
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
                let result = try_enqueue_realtime_audio(tx, packet);
                if result != RealtimeAudioEnqueue::Sent {
                    mark_realtime_overloaded(
                        &realtime_overloaded,
                        asr_abort_flag.as_deref(),
                        &self.state,
                        session_id,
                        result,
                    );
                    break;
                }
            }
        }

        let capture_mode = if self.capture.is_enabled() {
            self.capture.attach_session(ActiveCaptureSession {
                session_id,
                stop_flag: stop_flag.clone(),
                automatic_finish_requested: automatic_finish_requested.clone(),
                realtime_overloaded: realtime_overloaded.clone(),
                asr_abort_flag: asr_abort_flag.clone(),
                audio_buffer: audio_buffer.clone(),
                capture_ready: capture_ready.clone(),
                asr_ready: asr_ready.clone(),
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
                    automatic_finish_requested: automatic_finish_requested.clone(),
                    realtime_overloaded: realtime_overloaded.clone(),
                    asr_abort_flag,
                    audio_buffer: audio_buffer.clone(),
                    capture_ready,
                    asr_ready,
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
            realtime_overloaded,
            audio_buffer,
            output_target_hint,
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

    fn finish_recording(
        &mut self,
        cancel: bool,
        focused_window_hint: Option<focused_window::FocusedWindowSnapshot>,
    ) -> Result<()> {
        let Some(mut session) = self.session.take() else {
            return Ok(());
        };
        let output_target_hint = session.output_target_hint;
        let realtime_overloaded_flag = session.realtime_overloaded.clone();
        let mut realtime_overloaded = realtime_overloaded_flag.load(Ordering::SeqCst);

        let stopped_at_ms = unix_time_ms();
        let capture_drain_deadline =
            should_drain_capture(cancel, session.stop_flag.load(Ordering::SeqCst))
                .then(|| Instant::now() + CAPTURE_STOP_DRAIN);
        let refinement_context =
            capture_refinement_context_at_stop(&self.config, cancel, focused_window_hint);
        self.state
            .update(|snapshot| snapshot.stop_recording_clock_at(stopped_at_ms))?;
        // PipeWire capture runs ahead of this control thread. Keep accepting a
        // short post-key interval so samples already buffered in the audio
        // stack, including a final syllable, reach both full-audio and realtime
        // ASR before Stop. The displayed duration still ends at the key press.
        if let Some(remaining) = capture_drain_deadline
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
        {
            thread::sleep(remaining);
        }
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

        realtime_overloaded |= realtime_overloaded_flag.load(Ordering::SeqCst);
        if let SessionAsrRuntime::Realtime { backend_handle, .. } = &session.asr_runtime
            && backend_handle.is_finished()
            && !cancel
        {
            // A realtime worker must remain alive until stop. Preserve local
            // audio even when it exits between the final capture packet and
            // the event-pump update that would otherwise mark degradation.
            realtime_overloaded = true;
            realtime_overloaded_flag.store(true, Ordering::SeqCst);
        }
        if !cancel && matches!(&session.asr_runtime, SessionAsrRuntime::Realtime { .. }) {
            // Speech/transcript events can trail the final PCM packet slightly.
            // Give the event pump a brief chance to observe them. If the server
            // still has not acknowledged speech, continue finalization instead
            // of discarding captured audio: session.finish/manual commit may
            // produce a late result, and full-audio recovery can still decode a
            // real utterance from a stream whose first VAD event was lost.
            if !realtime_overloaded {
                let deadline = Instant::now() + Duration::from_millis(SPEECH_EVENT_GRACE_MS);
                while !session.speech_detected.load(Ordering::SeqCst)
                    && !realtime_overloaded_flag.load(Ordering::SeqCst)
                    && Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            realtime_overloaded |= realtime_overloaded_flag.load(Ordering::SeqCst);
            if !realtime_overloaded && !session.speech_detected.load(Ordering::SeqCst) {
                eprintln!(
                    "voice-input realtime ASR: no server speech event before stop; deferring empty-audio decision until finalization"
                );
            }
        }

        let final_asr_packet = if cancel || realtime_overloaded {
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
                abort_flag,
                event_handle,
                backend_handle,
            } => {
                realtime_overloaded |= realtime_overloaded_flag.load(Ordering::SeqCst);
                if !cancel {
                    self.state.update(|snapshot| {
                        snapshot.phase = Phase::Transcribing;
                        snapshot.class = "transcribing".into();
                        snapshot.icon = "󰔟".into();
                        snapshot.tooltip = "Transcribing…".into();
                        snapshot.bars = PROCESSING_WAVEFORM;
                    })?;
                }

                if cancel || realtime_overloaded {
                    abort_flag.store(true, Ordering::SeqCst);
                } else {
                    if let Some(packet) = final_asr_packet {
                        let _ = control_tx.send(backend::AsrControl::AppendPcm16(packet));
                    }
                    let _ = control_tx.send(backend::AsrControl::Finish);
                }
                let backend_result = join_session_handle(backend_handle, "realtime ASR worker");
                let event_result = join_value_handle(event_handle, "realtime ASR event pump");
                realtime_overloaded |= realtime_overloaded_flag.load(Ordering::SeqCst);

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

                    let final_pass_enabled = self.config.asr.provider
                        == AsrProvider::AlibabaQwenRealtime
                        && self.config.asr.alibaba.final_pass_enabled;
                    if realtime_overloaded
                        && !final_pass_enabled
                        && !self.config.asr.fallback_to_local
                    {
                        bail!(
                            "realtime audio delivery fell behind and no full-audio recovery is enabled"
                        );
                    }

                    let remote_transcript = if realtime_overloaded {
                        let _ = self.state.update(|snapshot| {
                            snapshot.tooltip = "Recovering complete audio…".into();
                        });
                        None
                    } else {
                        match (backend_result, event_result) {
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
                        }
                    };

                    if final_pass_enabled {
                        self.state.update(|snapshot| {
                            snapshot.phase = Phase::Transcribing;
                            snapshot.class = "transcribing".into();
                            snapshot.icon = "󰔟".into();
                            snapshot.tooltip = "Retranscribing full audio…".into();
                            snapshot.bars = PROCESSING_WAVEFORM;
                        })?;

                        match self.transcribe_alibaba_full_audio(&audio) {
                            Ok(Some(text)) => text,
                            Ok(None) => {
                                if let Some(text) = remote_transcript {
                                    // A transcript can arrive during finalization
                                    // after the pre-Finish speech snapshot.
                                    text
                                } else {
                                    eprintln!(
                                        "voice-input final ASR: no transcript; cancelling empty dictation"
                                    );
                                    String::new()
                                }
                            }
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

        if cancel || raw_transcript.trim().is_empty() {
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

        let agent_locator =
            refinement_context
                .agent_handle
                .and_then(|handle| match handle.join() {
                    Ok(locator) => locator,
                    Err(_) => {
                        eprintln!("voice-input agent context: session discovery worker panicked");
                        None
                    }
                });
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

            match llm::maybe_refine(
                &self.config,
                &raw_transcript,
                refinement_context.category,
                refinement_context.agent,
                agent_reference.as_ref(),
            ) {
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
            snapshot.start_recording_clock();
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
        // pw-record diagnostics are not part of the protocol. Leaving stderr
        // piped without a reader can deadlock capture when that pipe fills.
        .stderr(Stdio::null())
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
            if active.stop_flag.load(Ordering::SeqCst) {
                continue;
            }

            let max_samples = max_recording_samples(&config);
            let (accepted_samples, total_samples) = {
                let mut buffer = active
                    .audio_buffer
                    .lock()
                    .expect("audio buffer mutex poisoned");
                let accepted = append_recording_audio(&mut buffer, &chunk, max_samples);
                (accepted, buffer.len())
            };
            if accepted_samples == 0 {
                active.stop_flag.store(true, Ordering::SeqCst);
                request_automatic_finish(
                    &active.automatic_finish_requested,
                    config.audio.max_duration_secs,
                );
                continue;
            }
            let chunk = &chunk[..accepted_samples];

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

            let frames = active
                .waveform_analyzer
                .lock()
                .expect("waveform analyzer mutex poisoned")
                // HUD analysis is local and must remain responsive even when
                // realtime network delivery falls behind.
                .push(chunk, true);
            for frame in frames {
                active.waveform.try_publish(active.session_id, frame);
            }

            if !active.realtime_overloaded.load(Ordering::SeqCst)
                && let (Some(tx), Some(packetizer)) = (
                    active.asr_control_tx.as_ref(),
                    active.asr_packetizer.as_ref(),
                )
            {
                let packets = {
                    let mut packetizer = packetizer.lock().expect("ASR packetizer mutex poisoned");
                    // Stop and the final flush synchronize through this mutex:
                    // an in-flight shared-capture chunk either enters before
                    // the flush or observes Stop and cannot appear after Finish.
                    if active.stop_flag.load(Ordering::SeqCst) {
                        Vec::new()
                    } else {
                        packetizer.push(chunk)
                    }
                };
                for packet in packets {
                    let result = try_enqueue_realtime_audio(tx, packet);
                    if result != RealtimeAudioEnqueue::Sent {
                        mark_realtime_overloaded(
                            &active.realtime_overloaded,
                            active.asr_abort_flag.as_deref(),
                            &state,
                            active.session_id,
                            result,
                        );
                        break;
                    }
                }
            }

            if accepted_samples < bytes.len() / 2 || total_samples >= max_samples {
                active.stop_flag.store(true, Ordering::SeqCst);
                request_automatic_finish(
                    &active.automatic_finish_requested,
                    config.audio.max_duration_secs,
                );
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
    automatic_finish_requested: Arc<AtomicBool>,
    realtime_overloaded: Arc<AtomicBool>,
    asr_abort_flag: Option<Arc<AtomicBool>>,
    audio_buffer: Arc<Mutex<Vec<i16>>>,
    capture_ready: Arc<AtomicBool>,
    asr_ready: Arc<AtomicBool>,
    asr_control_tx: Option<mpsc::SyncSender<backend::AsrControl>>,
    asr_packetizer: Option<Arc<Mutex<AsrPacketizer>>>,
    waveform_analyzer: Arc<Mutex<WaveformAnalyzer>>,
    waveform: WaveformPublisher,
}

fn max_recording_samples(config: &Config) -> usize {
    let samples =
        u64::from(config.audio.sample_rate).saturating_mul(config.audio.max_duration_secs);
    usize::try_from(samples).unwrap_or(usize::MAX)
}

fn append_recording_audio(buffer: &mut Vec<i16>, chunk: &[i16], max_samples: usize) -> usize {
    let accepted = chunk.len().min(max_samples.saturating_sub(buffer.len()));
    buffer.extend_from_slice(&chunk[..accepted]);
    accepted
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RealtimeAudioEnqueue {
    Sent,
    Full,
    Disconnected,
}

fn try_enqueue_realtime_audio(
    sender: &mpsc::SyncSender<backend::AsrControl>,
    packet: Vec<i16>,
) -> RealtimeAudioEnqueue {
    match sender.try_send(backend::AsrControl::AppendPcm16(packet)) {
        Ok(()) => RealtimeAudioEnqueue::Sent,
        Err(mpsc::TrySendError::Full(_)) => RealtimeAudioEnqueue::Full,
        Err(mpsc::TrySendError::Disconnected(_)) => RealtimeAudioEnqueue::Disconnected,
    }
}

fn request_session_finish(requested: &AtomicBool, reason: &str) {
    if requested.swap(true, Ordering::SeqCst) {
        return;
    }

    eprintln!("voice-input capture: {reason}; finishing automatically");
    thread::spawn(|| {
        if let Err(error) = send_control_command("stop") {
            eprintln!("voice-input capture: automatic finish request failed: {error:#}");
        }
    });
}

fn request_automatic_finish(requested: &AtomicBool, maximum_seconds: u64) {
    request_session_finish(
        requested,
        &format!("reached configured {maximum_seconds}-second limit"),
    );
}

fn mark_realtime_overloaded(
    overloaded: &AtomicBool,
    abort_flag: Option<&AtomicBool>,
    state: &StateHandle,
    session_id: u64,
    enqueue_result: RealtimeAudioEnqueue,
) {
    if overloaded.swap(true, Ordering::SeqCst) {
        return;
    }

    if let Some(abort_flag) = abort_flag {
        abort_flag.store(true, Ordering::SeqCst);
    }
    let (log_reason, tooltip) = match enqueue_result {
        RealtimeAudioEnqueue::Full => (
            "audio queue could not keep up",
            "Realtime audio delayed — recording continues",
        ),
        RealtimeAudioEnqueue::Disconnected => (
            "ASR worker disconnected",
            "Realtime connection interrupted — recording continues",
        ),
        RealtimeAudioEnqueue::Sent => return,
    };
    eprintln!(
        "voice-input realtime ASR: {log_reason} for session {session_id}; preserving full audio for recovery"
    );
    let _ = state.update(|snapshot| {
        if snapshot.phase == Phase::Recording {
            snapshot.tooltip = tooltip.into();
        }
    });
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
            automatic_finish_requested,
            realtime_overloaded,
            asr_abort_flag,
            audio_buffer,
            capture_ready,
            asr_ready,
            asr_control_tx,
            asr_packetizer,
            waveform_analyzer,
            waveform,
        } = context;
        let max_samples = max_recording_samples(&config);
        let mut bytes = [0u8; 512];

        loop {
            if stop_flag.load(Ordering::SeqCst) || cancel_flag.load(Ordering::SeqCst) {
                break;
            }
            let read = match stdout.read(&mut bytes) {
                Ok(0) if stop_flag.load(Ordering::SeqCst) || cancel_flag.load(Ordering::SeqCst) => {
                    break;
                }
                Ok(0) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    request_session_finish(&automatic_finish_requested, "audio stream ended");
                    break;
                }
                Ok(read) => read,
                Err(_)
                    if stop_flag.load(Ordering::SeqCst) || cancel_flag.load(Ordering::SeqCst) =>
                {
                    break;
                }
                Err(error) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    request_session_finish(&automatic_finish_requested, "audio stream failed");
                    return Err(error).context("failed reading from pw-record");
                }
            };

            let mut chunk = Vec::with_capacity(read / 2);
            for pair in bytes[..read].chunks_exact(2) {
                chunk.push(i16::from_le_bytes([pair[0], pair[1]]));
            }

            let (accepted_samples, total_samples) = {
                let mut buffer = audio_buffer.lock().expect("audio buffer mutex poisoned");
                let accepted = append_recording_audio(&mut buffer, &chunk, max_samples);
                (accepted, buffer.len())
            };
            if accepted_samples == 0 {
                stop_flag.store(true, Ordering::SeqCst);
                request_automatic_finish(
                    &automatic_finish_requested,
                    config.audio.max_duration_secs,
                );
                break;
            }
            let chunk = &chunk[..accepted_samples];

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

            let frames = waveform_analyzer
                .lock()
                .expect("waveform analyzer mutex poisoned")
                // Keep local capture and visualization independent of realtime
                // network backpressure.
                .push(chunk, true);
            for frame in frames {
                waveform.try_publish(session_id, frame);
            }

            if !realtime_overloaded.load(Ordering::SeqCst)
                && let (Some(tx), Some(packetizer)) =
                    (asr_control_tx.as_ref(), asr_packetizer.as_ref())
            {
                let packets = packetizer
                    .lock()
                    .expect("ASR packetizer mutex poisoned")
                    .push(chunk);
                for packet in packets {
                    let result = try_enqueue_realtime_audio(tx, packet);
                    if result != RealtimeAudioEnqueue::Sent {
                        mark_realtime_overloaded(
                            &realtime_overloaded,
                            asr_abort_flag.as_deref(),
                            &state,
                            session_id,
                            result,
                        );
                        break;
                    }
                }
            }

            if accepted_samples < bytes.len() / 2 || total_samples >= max_samples {
                stop_flag.store(true, Ordering::SeqCst);
                request_automatic_finish(
                    &automatic_finish_requested,
                    config.audio.max_duration_secs,
                );
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
    realtime_overloaded: Arc<AtomicBool>,
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
            realtime_overloaded,
            event_rx,
        } = context;
        let mut final_transcript = None;
        let mut realtime_reconstructing = false;

        while let Ok(event) = event_rx.recv() {
            match event {
                backend::AsrEvent::Ready => {
                    asr_ready.store(true, Ordering::SeqCst);
                    if !realtime_reconstructing {
                        refresh_recording_readiness(
                            &state,
                            &config,
                            session_id,
                            capture_ready.load(Ordering::SeqCst),
                            asr_ready.load(Ordering::SeqCst),
                        )?;
                    }
                }
                backend::AsrEvent::SpeechStarted => {
                    speech_detected.store(true, Ordering::SeqCst);
                    voice_active.store(true, Ordering::Relaxed);
                }
                backend::AsrEvent::SpeechStopped => {
                    voice_active.store(false, Ordering::Relaxed);
                }
                backend::AsrEvent::RealtimeRestarting => {
                    // Reconstruction is only requested after Server VAD has
                    // confirmed an active speech segment. Keep stop-time
                    // full-audio recovery eligible if Stop races the
                    // replacement before Qwen emits a new speech event.
                    speech_detected.store(true, Ordering::SeqCst);
                    realtime_reconstructing = true;
                    state.update(|snapshot| {
                        if matches!(snapshot.phase, Phase::Arming | Phase::Recording) {
                            snapshot.tooltip = "Realtime reconnecting — recording continues".into();
                        }
                    })?;
                }
                backend::AsrEvent::RealtimeRestarted => {
                    realtime_reconstructing = false;
                    asr_ready.store(true, Ordering::SeqCst);
                    refresh_recording_readiness(
                        &state,
                        &config,
                        session_id,
                        capture_ready.load(Ordering::SeqCst),
                        true,
                    )?;
                    state.update(|snapshot| {
                        if matches!(snapshot.phase, Phase::Arming | Phase::Recording) {
                            snapshot.tooltip = "Replaying buffered audio…".into();
                        }
                    })?;
                }
                backend::AsrEvent::RealtimeTranscriptDelayed => {
                    speech_detected.store(true, Ordering::SeqCst);
                    realtime_overloaded.store(true, Ordering::SeqCst);
                    state.update(|snapshot| {
                        if matches!(snapshot.phase, Phase::Arming | Phase::Recording) {
                            snapshot.tooltip =
                                "Realtime transcript delayed — recording continues".into();
                        }
                    })?;
                }
                backend::AsrEvent::Partial {
                    committed,
                    unstable,
                } => {
                    realtime_reconstructing = false;
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
                            snapshot.tooltip = if realtime_overloaded.load(Ordering::SeqCst) {
                                "Realtime transcript delayed — recording continues".into()
                            } else if snapshot.phase == Phase::Arming && transcript.is_empty() {
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
                    realtime_reconstructing = false;
                    if !text.trim().is_empty() {
                        speech_detected.store(true, Ordering::SeqCst);
                    }
                    *partial_transcript
                        .lock()
                        .expect("partial transcript mutex poisoned") = text.clone();
                    state.update(|snapshot| {
                        if snapshot.phase != Phase::Idle {
                            snapshot.transcript = text.clone();
                            snapshot.tooltip = if realtime_overloaded.load(Ordering::SeqCst) {
                                "Realtime transcript delayed — recording continues".into()
                            } else {
                                text.clone()
                            };
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
                    speech_detected.store(true, Ordering::SeqCst);
                    realtime_overloaded.store(true, Ordering::SeqCst);
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
    fn transcribe_alibaba_full_audio(&self, audio: &[i16]) -> Result<Option<String>> {
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
        match backend::transcribe(&local_config, temp_file.path()) {
            Err(error) if backend::is_empty_transcript_error(&error) => Ok(String::new()),
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn control_commands_are_bounded_and_require_utf8() {
        let mut valid = Cursor::new(b"toggle".to_vec());
        assert_eq!(read_control_command(&mut valid).unwrap(), "toggle");

        let mut oversized = Cursor::new(vec![b'x'; CONTROL_COMMAND_MAX_BYTES as usize + 1]);
        assert!(read_control_command(&mut oversized).is_err());

        let mut invalid_utf8 = Cursor::new(vec![0xff]);
        assert!(read_control_command(&mut invalid_utf8).is_err());
    }

    #[test]
    fn queued_recording_controls_become_stale_after_idle_generation_advances() {
        let generation = AtomicU64::new(4);
        let queued_toggle = ControlReceipt {
            idle_generation: generation.load(Ordering::SeqCst),
            accepted_at: Instant::now(),
        };
        let queued_start = queued_toggle;

        assert_eq!(advance_idle_generation(&generation), 5);
        assert!(!control_receipt_is_current(
            queued_toggle,
            generation.load(Ordering::SeqCst)
        ));
        assert!(!control_receipt_is_current(
            queued_start,
            generation.load(Ordering::SeqCst)
        ));
    }

    #[test]
    fn control_received_after_idle_boundary_uses_current_generation() {
        let generation = AtomicU64::new(7);
        let receipt = ControlReceipt {
            idle_generation: generation.load(Ordering::SeqCst),
            accepted_at: Instant::now(),
        };

        assert!(control_receipt_is_current(
            receipt,
            generation.load(Ordering::SeqCst)
        ));
    }

    #[test]
    fn capture_drain_only_applies_to_normal_manual_stop() {
        assert!(should_drain_capture(false, false));
        assert!(!should_drain_capture(true, false));
        assert!(!should_drain_capture(false, true));
    }

    #[test]
    fn focused_window_and_agent_capture_have_independent_policies() {
        assert!(should_capture_focused_window(false, true));
        assert!(!should_capture_focused_window(true, true));
        assert!(!should_capture_focused_window(false, false));

        assert!(should_capture_agent_context(false, true, true));
        assert!(!should_capture_agent_context(true, true, true));
        assert!(!should_capture_agent_context(false, false, true));
        assert!(!should_capture_agent_context(false, true, false));
    }

    #[test]
    fn recording_sample_limit_uses_rate_and_duration() {
        let mut config = Config::default();
        config.audio.sample_rate = 16_000;
        config.audio.max_duration_secs = 90;
        assert_eq!(max_recording_samples(&config), 1_440_000);
    }

    #[test]
    fn recording_audio_is_capped_without_dropping_the_accepted_prefix() {
        let mut buffer = vec![1; 8];
        assert_eq!(append_recording_audio(&mut buffer, &[2, 3, 4, 5], 10), 2);
        assert_eq!(buffer, vec![1, 1, 1, 1, 1, 1, 1, 1, 2, 3]);
        assert_eq!(append_recording_audio(&mut buffer, &[6, 7], 10), 0);
        assert_eq!(buffer.len(), 10);
    }

    #[test]
    fn realtime_audio_enqueue_distinguishes_backpressure_and_disconnect() {
        let (sender, receiver) = mpsc::sync_channel(1);
        assert_eq!(
            try_enqueue_realtime_audio(&sender, vec![1, 2]),
            RealtimeAudioEnqueue::Sent
        );
        assert_eq!(
            try_enqueue_realtime_audio(&sender, vec![3, 4]),
            RealtimeAudioEnqueue::Full
        );
        drop(receiver);
        assert_eq!(
            try_enqueue_realtime_audio(&sender, vec![5, 6]),
            RealtimeAudioEnqueue::Disconnected
        );
    }
}
