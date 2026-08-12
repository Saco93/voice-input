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
    config::{AsrProvider, Config, HudConfig, HudPosition, NativeFinalPassMode},
    diagnostics::{
        FailureKind, FinalPassDecision, FinalPassKind, FinalPassReason, OverallOutcome,
        SelectedResult, StageStatus,
    },
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
pub(crate) const ADAPTIVE_NATIVE_DURATION_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullAudioPass {
    AlibabaCompatible,
    QwenAudio3Native,
}

impl FullAudioPass {
    fn diagnostics_kind(self) -> FinalPassKind {
        match self {
            Self::AlibabaCompatible => FinalPassKind::AlibabaCompatible,
            Self::QwenAudio3Native => FinalPassKind::QwenAudio3Native,
        }
    }

    fn selected_result(self) -> SelectedResult {
        match self {
            Self::AlibabaCompatible => SelectedResult::AlibabaCompatibleFinal,
            Self::QwenAudio3Native => SelectedResult::QwenAudio3Native,
        }
    }
}

fn selected_full_audio_pass(config: &Config) -> Option<FullAudioPass> {
    match config.asr.provider {
        AsrProvider::AlibabaQwenRealtime if config.asr.alibaba.final_pass_enabled => {
            Some(FullAudioPass::AlibabaCompatible)
        }
        AsrProvider::AlibabaQwenAudio3
            if config.asr.alibaba_audio3.native_final_pass_mode
                != NativeFinalPassMode::StreamingOnly =>
        {
            Some(FullAudioPass::QwenAudio3Native)
        }
        AsrProvider::LocalCli
        | AsrProvider::AlibabaQwenRealtime
        | AsrProvider::AlibabaQwenAudio3 => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateState {
    Usable,
    Empty,
    Failed,
    /// A usable realtime result whose worker also reported a failure.
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultDecision {
    Selected(SelectedResult),
    FallbackNeeded,
    AuthoritativeEmpty,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NativeFinalPassPolicyInput {
    mode: NativeFinalPassMode,
    cancelled: bool,
    has_audio: bool,
    streaming: CandidateState,
    worker_interrupted: bool,
    overloaded: bool,
    saw_finished: bool,
    captured_duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NativeFinalPassPolicyDecision {
    invoke: bool,
    reason: FinalPassReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FullAudioPassPlanInput {
    cancelled: bool,
    has_audio: bool,
    streaming: CandidateState,
    worker_interrupted: bool,
    overloaded: bool,
    saw_finished: bool,
    captured_duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FullAudioPassPlan {
    pass: Option<FullAudioPass>,
    audio3_decision: Option<NativeFinalPassPolicyDecision>,
}

struct FullAudioPassInvocation {
    state: Option<(FullAudioPass, CandidateState)>,
    text: Option<String>,
    error: Option<anyhow::Error>,
}

/// Text-free adaptive policy. Only bounded state crosses this boundary.
fn decide_native_final_pass(input: NativeFinalPassPolicyInput) -> NativeFinalPassPolicyDecision {
    let skip = |reason| NativeFinalPassPolicyDecision {
        invoke: false,
        reason,
    };
    let invoke = |reason| NativeFinalPassPolicyDecision {
        invoke: true,
        reason,
    };

    if input.cancelled {
        return skip(FinalPassReason::Cancelled);
    }
    if !input.has_audio {
        return skip(FinalPassReason::NoAudio);
    }
    match input.mode {
        NativeFinalPassMode::StreamingOnly => return skip(FinalPassReason::StreamingOnly),
        NativeFinalPassMode::Always => return invoke(FinalPassReason::Always),
        NativeFinalPassMode::Adaptive => {}
    }
    if input.overloaded {
        invoke(FinalPassReason::Overloaded)
    } else if input.worker_interrupted {
        invoke(FinalPassReason::Interrupted)
    } else if input.streaming == CandidateState::Empty {
        invoke(FinalPassReason::Empty)
    } else if matches!(
        input.streaming,
        CandidateState::Failed | CandidateState::Degraded
    ) {
        invoke(FinalPassReason::Degraded)
    } else if !input.saw_finished {
        invoke(FinalPassReason::MissingCompletion)
    } else if input.captured_duration_ms >= ADAPTIVE_NATIVE_DURATION_MS {
        invoke(FinalPassReason::Duration)
    } else {
        skip(FinalPassReason::HealthyStream)
    }
}

fn plan_full_audio_pass(config: &Config, input: FullAudioPassPlanInput) -> FullAudioPassPlan {
    if config.asr.provider == AsrProvider::AlibabaQwenAudio3 {
        let decision = decide_native_final_pass(NativeFinalPassPolicyInput {
            mode: config.asr.alibaba_audio3.native_final_pass_mode,
            cancelled: input.cancelled,
            has_audio: input.has_audio,
            streaming: input.streaming,
            worker_interrupted: input.worker_interrupted,
            overloaded: input.overloaded,
            saw_finished: input.saw_finished,
            captured_duration_ms: input.captured_duration_ms,
        });
        return FullAudioPassPlan {
            pass: decision.invoke.then_some(FullAudioPass::QwenAudio3Native),
            audio3_decision: Some(decision),
        };
    }

    FullAudioPassPlan {
        pass: (!input.cancelled && input.has_audio)
            .then(|| selected_full_audio_pass(config))
            .flatten(),
        audio3_decision: None,
    }
}

/// Executes the production-selected pass while keeping invocation injectable.
/// A suppressed plan never invokes the closure or starts a provider request.
fn execute_full_audio_pass(
    pass: Option<FullAudioPass>,
    invoke: impl FnOnce(FullAudioPass) -> Result<Option<String>>,
) -> FullAudioPassInvocation {
    let Some(pass) = pass else {
        return FullAudioPassInvocation {
            state: None,
            text: None,
            error: None,
        };
    };

    match invoke(pass) {
        Ok(Some(text)) if !text.trim().is_empty() => FullAudioPassInvocation {
            state: Some((pass, CandidateState::Usable)),
            text: Some(text),
            error: None,
        },
        Ok(_) => FullAudioPassInvocation {
            state: Some((pass, CandidateState::Empty)),
            text: None,
            error: None,
        },
        Err(error) => FullAudioPassInvocation {
            state: Some((pass, CandidateState::Failed)),
            text: None,
            error: Some(error),
        },
    }
}

fn captured_audio_duration_ms(sample_count: usize, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return u64::MAX;
    }
    (sample_count as u128)
        .saturating_mul(1_000)
        .checked_div(u128::from(sample_rate))
        .unwrap_or(u128::from(u64::MAX))
        .min(u128::from(u64::MAX)) as u64
}

/// Text-free result policy. Transcript values stay in the caller and only
/// candidate states cross this boundary.
fn decide_result_source(
    provider: AsrProvider,
    final_pass: Option<(FullAudioPass, CandidateState)>,
    streaming: CandidateState,
    fallback_enabled: bool,
    overloaded: bool,
) -> ResultDecision {
    if provider == AsrProvider::LocalCli {
        return match streaming {
            CandidateState::Usable | CandidateState::Degraded => {
                ResultDecision::Selected(SelectedResult::LocalPrimary)
            }
            CandidateState::Empty => ResultDecision::AuthoritativeEmpty,
            CandidateState::Failed => ResultDecision::Failed,
        };
    }

    let streaming_usable =
        !overloaded && matches!(streaming, CandidateState::Usable | CandidateState::Degraded);
    if let Some((pass, final_state)) = final_pass {
        return match final_state {
            CandidateState::Usable | CandidateState::Degraded => {
                ResultDecision::Selected(pass.selected_result())
            }
            CandidateState::Empty => {
                if streaming_usable {
                    ResultDecision::Selected(SelectedResult::Streaming)
                } else {
                    ResultDecision::AuthoritativeEmpty
                }
            }
            CandidateState::Failed => {
                if streaming_usable {
                    ResultDecision::Selected(SelectedResult::Streaming)
                } else if fallback_enabled {
                    ResultDecision::FallbackNeeded
                } else {
                    ResultDecision::Failed
                }
            }
        };
    }

    if streaming_usable {
        ResultDecision::Selected(SelectedResult::Streaming)
    } else if fallback_enabled {
        ResultDecision::FallbackNeeded
    } else {
        ResultDecision::Failed
    }
}

fn final_pass_kind(config: &Config) -> FinalPassKind {
    selected_full_audio_pass(config)
        .map(FullAudioPass::diagnostics_kind)
        .unwrap_or(FinalPassKind::None)
}

fn diagnostics_for_session(config: &Config, session_id: u64) -> crate::diagnostics::Diagnostics {
    let mut diagnostics = crate::diagnostics::Diagnostics::inactive();
    diagnostics.start_session(
        session_id,
        config.asr.provider.into(),
        final_pass_kind(config),
        config.asr.fallback_to_local,
    );
    if config.asr.provider == AsrProvider::AlibabaQwenAudio3 {
        diagnostics.configure_audio3_native_final_pass(
            session_id,
            config.asr.alibaba_audio3.native_final_pass_mode,
        );
    }
    diagnostics
}

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn classify_failure_text(message: &str) -> FailureKind {
    let message = message.to_ascii_lowercase();
    if message.contains("timeout") || message.contains("timed out") {
        FailureKind::Timeout
    } else if message.contains("rate limit")
        || message.contains("too many requests")
        || message.contains("http 429")
    {
        FailureKind::RateLimited
    } else if message.contains("unauthorized")
        || message.contains("authentication")
        || message.contains("api key")
        || message.contains("http 401")
    {
        FailureKind::Authentication
    } else if message.contains("forbidden")
        || message.contains("permission")
        || message.contains("http 403")
    {
        FailureKind::PermissionDenied
    } else if message.contains("connect")
        || message.contains("socket")
        || message.contains("websocket")
    {
        FailureKind::Connection
    } else if message.contains("protocol") {
        FailureKind::Protocol
    } else if message.contains("json")
        || message.contains("response")
        || message.contains("transcript")
    {
        FailureKind::InvalidResponse
    } else if message.contains("configuration") || message.contains("configured") {
        FailureKind::Configuration
    } else if message.contains("asr backend") || message.contains("local backend") {
        FailureKind::LocalBackend
    } else if message.contains("worker") || message.contains("thread") {
        FailureKind::Worker
    } else if message.contains("file") || message.contains("read") || message.contains("write") {
        FailureKind::Io
    } else {
        FailureKind::Service
    }
}

fn classify_failure(error: &anyhow::Error) -> FailureKind {
    for cause in error.chain() {
        let kind = classify_failure_text(&cause.to_string());
        if kind != FailureKind::Service {
            return kind;
        }
    }
    FailureKind::Service
}

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
        let failure_kind = classify_failure(error);
        let message = error.to_string();
        let _ = daemon.state.update(|snapshot| {
            let in_progress_session_id = snapshot
                .diagnostics
                .session
                .as_ref()
                .filter(|session| session.asr_outcome == OverallOutcome::InProgress)
                .map(|session| session.session_id);
            if let Some(session_id) = in_progress_session_id {
                snapshot.diagnostics.update_session(session_id, |session| {
                    if matches!(
                        session.streaming.status,
                        StageStatus::Pending | StageStatus::InProgress
                    ) {
                        session.streaming.status = StageStatus::Failed;
                        session.streaming.failure_kind = Some(failure_kind);
                    }
                });
                snapshot.diagnostics.finish_session(
                    session_id,
                    OverallOutcome::Failed,
                    SelectedResult::None,
                    0,
                );
            }
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

/// Keeps fallible persistence structurally behind capture shutdown. In
/// particular, an update error cannot bypass detaching shared capture or
/// stopping and joining dedicated capture.
fn update_after_capture_shutdown<T>(
    shutdown: impl FnOnce() -> Result<()>,
    update: impl FnOnce() -> Result<T>,
) -> Result<T> {
    shutdown()?;
    update()
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

    let agent_handle = Some(thread::spawn(move || {
        if let Some(elapsed) = agent_context::warm_terminology_segmenter() {
            eprintln!(
                "voice-input agent context: initialized local segmenter in {} ms",
                elapsed.as_millis()
            );
        }
        match agent_context::resolve_focused_session(snapshot) {
            Ok(Some(locator)) => Some(locator),
            Ok(None) => {
                eprintln!("voice-input agent context: captured process has no valid session");
                None
            }
            Err(_) => {
                eprintln!("voice-input agent context: captured session discovery failed");
                None
            }
        }
    }));
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
    session_id: u64,
    finalization_started_at: Arc<Mutex<Option<Instant>>>,
    stop_flag: Arc<AtomicBool>,
    cancel_flag: Arc<AtomicBool>,
    realtime_overloaded: Arc<AtomicBool>,
    audio_buffer: Arc<Mutex<Vec<i16>>>,
    output_target_hint: Option<output::OutputTargetHint>,
    asr_packetizer: Option<Arc<Mutex<AsrPacketizer>>>,
    speech_detected: Arc<AtomicBool>,
    capture_gate: Arc<Mutex<()>>,
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
        event_handle: thread::JoinHandle<Result<RealtimeEventOutcome>>,
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
    capture_gate: Arc<Mutex<()>>,
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
        let asr_started_at = Instant::now();
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
        let capture_gate = Arc::new(Mutex::new(()));
        self.waveform.try_reset(session_id);
        let finalization_started_at = Arc::new(Mutex::new(None));
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
                diagnostics: diagnostics_for_session(&self.config, session_id),
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
                    asr_started_at,
                    finalization_started_at: finalization_started_at.clone(),
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
                capture_gate: capture_gate.clone(),
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
            session_id,
            finalization_started_at,
            stop_flag,
            cancel_flag,
            realtime_overloaded,
            audio_buffer,
            output_target_hint,
            asr_packetizer,
            speech_detected,
            capture_gate,
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
        let session_id = session.session_id;
        let finalization_started = Instant::now();
        *session
            .finalization_started_at
            .lock()
            .expect("finalization timer mutex poisoned") = Some(finalization_started);
        let output_target_hint = session.output_target_hint;
        let realtime_overloaded_flag = session.realtime_overloaded.clone();
        let mut realtime_overloaded = realtime_overloaded_flag.load(Ordering::SeqCst);

        let stopped_at_ms = unix_time_ms();
        let capture_drain_deadline =
            should_drain_capture(cancel, session.stop_flag.load(Ordering::SeqCst))
                .then(|| Instant::now() + CAPTURE_STOP_DRAIN);
        let refinement_context =
            capture_refinement_context_at_stop(&self.config, cancel, focused_window_hint);
        // PipeWire capture runs ahead of this control thread. Keep accepting a
        // short post-key interval so samples already buffered in the audio
        // stack, including a final syllable, reach both full-audio and realtime
        // ASR before Stop. The displayed duration still ends at the key press.
        if let Some(remaining) = capture_drain_deadline
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
        {
            thread::sleep(remaining);
        }
        update_after_capture_shutdown(
            || {
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
                        let capture_guard = session
                            .capture_gate
                            .lock()
                            .expect("capture gate mutex poisoned");
                        drop(capture_guard);
                    }
                }
                Ok(())
            },
            || {
                self.state
                    .update(|snapshot| snapshot.stop_recording_clock_at(stopped_at_ms))
            },
        )?;

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

        let (raw_transcript, selected_result) = match session.asr_runtime {
            SessionAsrRuntime::Local { partial_handle } => {
                join_session_handle(partial_handle, "partial transcriber")?;

                if cancel || audio.is_empty() {
                    (String::new(), SelectedResult::None)
                } else {
                    self.state.update(|snapshot| {
                        snapshot.phase = Phase::Transcribing;
                        snapshot.class = "transcribing".into();
                        snapshot.icon = "󰔟".into();
                        snapshot.tooltip = "Transcribing…".into();
                        snapshot.bars = PROCESSING_WAVEFORM;
                        snapshot.diagnostics.update_session(session_id, |session| {
                            session.local_primary.status = StageStatus::InProgress;
                            session.local_primary.failure_kind = None;
                        });
                    })?;
                    let started_at = Instant::now();
                    let result = self.transcribe_local_audio(&audio);
                    let latency_ms = elapsed_ms(started_at);
                    let failure_kind = result.as_ref().err().map(classify_failure);
                    let _ = self.state.update(|snapshot| {
                        snapshot.diagnostics.update_session(session_id, |session| {
                            session.local_primary.latency_ms = Some(latency_ms);
                            match &result {
                                Ok(text) if text.trim().is_empty() => {
                                    session.local_primary.status = StageStatus::Empty;
                                    session.local_primary.failure_kind = None;
                                }
                                Ok(_) => {
                                    session.local_primary.status = StageStatus::Completed;
                                    session.local_primary.failure_kind = None;
                                }
                                Err(_) => {
                                    session.local_primary.status = StageStatus::Failed;
                                    session.local_primary.failure_kind = failure_kind;
                                }
                            }
                        });
                    });
                    let text = match result {
                        Ok(text) => text,
                        Err(error) => {
                            self.finish_diagnostics(
                                session_id,
                                OverallOutcome::Failed,
                                SelectedResult::None,
                                finalization_started,
                            );
                            return Err(error);
                        }
                    };
                    let decision = decide_result_source(
                        AsrProvider::LocalCli,
                        None,
                        if text.trim().is_empty() {
                            CandidateState::Empty
                        } else {
                            CandidateState::Usable
                        },
                        false,
                        false,
                    );
                    let selected = match decision {
                        ResultDecision::Selected(selected) => selected,
                        ResultDecision::AuthoritativeEmpty => SelectedResult::None,
                        ResultDecision::FallbackNeeded | ResultDecision::Failed => {
                            unreachable!("local primary policy cannot request fallback")
                        }
                    };
                    (text, selected)
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
                        let _ = control_tx.send(backend::AsrControl::append_pcm16(packet));
                    }
                    let _ = control_tx.send(backend::AsrControl::Finish);
                }
                let backend_result = join_session_handle(backend_handle, "realtime ASR worker");
                let event_result = join_value_handle(event_handle, "realtime ASR event pump");
                let worker_interrupted = backend_result.is_err() || event_result.is_err();
                let saw_finished = event_result
                    .as_ref()
                    .is_ok_and(|outcome| outcome.saw_finished);
                realtime_overloaded |= realtime_overloaded_flag.load(Ordering::SeqCst);

                if cancel {
                    let join_failure_kind = backend_result
                        .as_ref()
                        .err()
                        .or_else(|| event_result.as_ref().err())
                        .map(classify_failure);
                    let _ = self.state.update(|snapshot| {
                        snapshot.diagnostics.update_session(session_id, |session| {
                            if self.config.asr.provider == AsrProvider::AlibabaQwenAudio3 {
                                session.final_pass.decision = FinalPassDecision::Skipped;
                                session.final_pass.reason = Some(FinalPassReason::Cancelled);
                            }
                            if session.streaming.status != StageStatus::Failed {
                                if let Some(failure_kind) = join_failure_kind {
                                    session.streaming.status = StageStatus::Failed;
                                    session.streaming.failure_kind = Some(failure_kind);
                                } else {
                                    session.streaming.status = StageStatus::Cancelled;
                                    session.streaming.failure_kind = None;
                                }
                            }
                            session.streaming.finalize_latency_ms =
                                Some(elapsed_ms(finalization_started));
                        });
                    });
                    (String::new(), SelectedResult::None)
                } else if audio.is_empty() {
                    let _ = self.state.update(|snapshot| {
                        snapshot.diagnostics.update_session(session_id, |session| {
                            if self.config.asr.provider == AsrProvider::AlibabaQwenAudio3 {
                                session.final_pass.decision = FinalPassDecision::Skipped;
                                session.final_pass.reason = Some(FinalPassReason::NoAudio);
                            }
                        });
                    });
                    (String::new(), SelectedResult::None)
                } else {
                    let (streaming_state, remote_transcript, stream_error) = if realtime_overloaded
                    {
                        let _ = self.state.update(|snapshot| {
                            snapshot.tooltip = "Recovering complete audio…".into();
                        });
                        (
                            CandidateState::Failed,
                            None,
                            Some(anyhow!("realtime audio delivery overloaded")),
                        )
                    } else {
                        match (backend_result, event_result) {
                            (Ok(()), Ok(outcome))
                                if outcome
                                    .transcript
                                    .as_ref()
                                    .is_some_and(|text| !text.trim().is_empty()) =>
                            {
                                let text = outcome.transcript.expect("checked transcript");
                                (
                                    CandidateState::Usable,
                                    Some(backend::apply_script_conversion(
                                        self.config.asr.language,
                                        &text,
                                    )?),
                                    None,
                                )
                            }
                            (Ok(()), Ok(_)) => (CandidateState::Empty, None, None),
                            (Err(error), Ok(outcome))
                                if outcome
                                    .transcript
                                    .as_ref()
                                    .is_some_and(|text| !text.trim().is_empty()) =>
                            {
                                let text = outcome.transcript.expect("checked transcript");
                                (
                                    CandidateState::Degraded,
                                    Some(backend::apply_script_conversion(
                                        self.config.asr.language,
                                        &text,
                                    )?),
                                    Some(error),
                                )
                            }
                            (Err(error), Ok(_)) | (Ok(()), Err(error)) => {
                                (CandidateState::Failed, None, Some(error))
                            }
                            (Err(error), Err(_)) => (CandidateState::Failed, None, Some(error)),
                        }
                    };
                    let streaming_failure_kind = stream_error.as_ref().map(classify_failure);
                    let _ = self.state.update(|snapshot| {
                        snapshot.diagnostics.update_session(session_id, |session| {
                            let preserve_implicit_degradation = streaming_state
                                == CandidateState::Usable
                                && session.streaming.status == StageStatus::Degraded;
                            if !preserve_implicit_degradation {
                                session.streaming.status = match streaming_state {
                                    CandidateState::Usable => StageStatus::Completed,
                                    CandidateState::Degraded => StageStatus::Degraded,
                                    CandidateState::Empty => StageStatus::Empty,
                                    CandidateState::Failed => StageStatus::Failed,
                                };
                            }
                            if !preserve_implicit_degradation {
                                session.streaming.failure_kind = if realtime_overloaded {
                                    session
                                        .streaming
                                        .failure_kind
                                        .or(Some(FailureKind::Overloaded))
                                } else {
                                    streaming_failure_kind
                                };
                            }
                            session.streaming.finalize_latency_ms =
                                Some(elapsed_ms(finalization_started));
                        });
                    });

                    let full_audio_plan = plan_full_audio_pass(
                        &self.config,
                        FullAudioPassPlanInput {
                            cancelled: false,
                            has_audio: true,
                            streaming: streaming_state,
                            worker_interrupted,
                            overloaded: realtime_overloaded,
                            saw_finished,
                            captured_duration_ms: captured_audio_duration_ms(
                                audio.len(),
                                self.config.audio.sample_rate,
                            ),
                        },
                    );
                    if let Some(policy) = full_audio_plan.audio3_decision {
                        // The policy record is diagnostics-only and must not alter
                        // request or capture behavior if persistence is unavailable.
                        let _ = self.state.update(|snapshot| {
                            snapshot.diagnostics.update_session(session_id, |session| {
                                session.final_pass.decision = if policy.invoke {
                                    FinalPassDecision::Invoked
                                } else {
                                    FinalPassDecision::Skipped
                                };
                                session.final_pass.reason = Some(policy.reason);
                                if !policy.invoke
                                    && session.final_pass.status == StageStatus::Pending
                                {
                                    session.final_pass.status = StageStatus::Skipped;
                                }
                            });
                        });
                    }
                    let full_audio_pass = full_audio_plan.pass;

                    let full_audio_started_at = if full_audio_pass.is_some() {
                        // This runtime/UI transition is required: do not start a
                        // potentially billable request unless it was persisted.
                        self.state.update(|snapshot| {
                            snapshot.phase = Phase::Transcribing;
                            snapshot.class = "transcribing".into();
                            snapshot.icon = "󰔟".into();
                            snapshot.tooltip = "Retranscribing full audio…".into();
                            snapshot.bars = PROCESSING_WAVEFORM;
                            snapshot.diagnostics.update_session(session_id, |session| {
                                session.final_pass.status = StageStatus::InProgress;
                                session.final_pass.failure_kind = None;
                            });
                        })?;
                        Some(Instant::now())
                    } else {
                        None
                    };
                    let invocation = execute_full_audio_pass(full_audio_pass, |pass| {
                        self.transcribe_full_audio(&audio, pass)
                    });
                    let final_state = invocation.state;
                    let final_text = invocation.text;
                    let final_error = invocation.error;
                    if let (Some((_, state)), Some(started_at)) =
                        (final_state, full_audio_started_at)
                    {
                        let failure_kind = final_error.as_ref().map(classify_failure);
                        let _ = self.state.update(|snapshot| {
                            snapshot.diagnostics.update_session(session_id, |session| {
                                session.final_pass.status = match state {
                                    CandidateState::Usable | CandidateState::Degraded => {
                                        StageStatus::Completed
                                    }
                                    CandidateState::Empty => StageStatus::Empty,
                                    CandidateState::Failed => StageStatus::Failed,
                                };
                                session.final_pass.latency_ms = Some(elapsed_ms(started_at));
                                session.final_pass.failure_kind = failure_kind;
                            });
                        });
                    }

                    let decision = decide_result_source(
                        self.config.asr.provider,
                        final_state,
                        streaming_state,
                        self.config.asr.fallback_to_local,
                        realtime_overloaded,
                    );
                    match decision {
                        ResultDecision::Selected(selected)
                            if selected == SelectedResult::Streaming =>
                        {
                            if final_error.is_some() {
                                let _ = self.state.update(|snapshot| {
                                    if matches!(snapshot.phase, Phase::Transcribing) {
                                        snapshot.tooltip =
                                            "Full-audio ASR failed, using realtime transcript…"
                                                .into();
                                    }
                                });
                            }
                            (
                                remote_transcript.expect("usable streaming candidate"),
                                selected,
                            )
                        }
                        ResultDecision::Selected(selected) => {
                            (final_text.expect("usable full-audio candidate"), selected)
                        }
                        ResultDecision::FallbackNeeded => {
                            let recovery_tooltip = if full_audio_pass.is_some() {
                                "Full-audio ASR failed, falling back locally…"
                            } else if streaming_state == CandidateState::Failed {
                                "Realtime ASR failed, falling back locally…"
                            } else {
                                "No realtime transcript, falling back locally…"
                            };
                            let _ = self.state.update(|snapshot| {
                                if matches!(snapshot.phase, Phase::Transcribing) {
                                    snapshot.tooltip = recovery_tooltip.into();
                                }
                            });
                            let text = match self.transcribe_local_fallback(session_id, &audio) {
                                Ok(text) => text,
                                Err(error) => {
                                    self.finish_diagnostics(
                                        session_id,
                                        OverallOutcome::Failed,
                                        SelectedResult::None,
                                        finalization_started,
                                    );
                                    return Err(error);
                                }
                            };
                            let selected = if text.trim().is_empty() {
                                SelectedResult::None
                            } else {
                                SelectedResult::LocalFallback
                            };
                            (text, selected)
                        }
                        ResultDecision::AuthoritativeEmpty => (String::new(), SelectedResult::None),
                        ResultDecision::Failed => {
                            self.finish_diagnostics(
                                session_id,
                                OverallOutcome::Failed,
                                SelectedResult::None,
                                finalization_started,
                            );
                            if let Some(error) = final_error.or(stream_error) {
                                return Err(error);
                            }
                            bail!("Alibaba realtime ASR returned no final transcript");
                        }
                    }
                }
            }
        };

        if cancel || raw_transcript.trim().is_empty() {
            let outcome = if cancel {
                OverallOutcome::Cancelled
            } else {
                OverallOutcome::Empty
            };
            self.finish_diagnostics(
                session_id,
                outcome,
                SelectedResult::None,
                finalization_started,
            );
            self.state.update(|snapshot| {
                let diagnostics = snapshot.diagnostics.clone();
                *snapshot =
                    Snapshot::idle_preserving_completed_diagnostics(&self.config, diagnostics);
                if !cancel && audio.is_empty() {
                    snapshot.tooltip = "Voice Input idle\nNo audio captured".into();
                }
            })?;
            return Ok(());
        }

        self.finish_diagnostics(
            session_id,
            OverallOutcome::Completed,
            selected_result,
            finalization_started,
        );

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
                "voice-input refinement: using {} agent context (source_chars={} terminology_count={} terminology_chars={} extraction_us={})",
                reference.agent.label(),
                reference.source_char_count,
                reference.terminology.len(),
                reference.terminology_char_count,
                reference.extraction_elapsed.as_micros()
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
            let diagnostics = snapshot.diagnostics.clone();
            *snapshot = Snapshot::idle_preserving_completed_diagnostics(&self.config, diagnostics);
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

fn snapshot_matches_session(snapshot: &Snapshot, session_id: u64) -> bool {
    snapshot
        .diagnostics
        .session
        .as_ref()
        .is_some_and(|session| session.session_id == session_id)
}

fn refresh_recording_readiness(
    state: &StateHandle,
    config: &Config,
    session_id: u64,
    capture_ready: bool,
    asr_ready: bool,
) -> Result<()> {
    state.update(|snapshot| {
        if !snapshot_matches_session(snapshot, session_id) {
            return;
        }
        if !matches!(snapshot.phase, Phase::Arming | Phase::Recording) {
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
            let _capture_guard = active
                .capture_gate
                .lock()
                .expect("capture gate mutex poisoned");
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
    match sender.try_send(backend::AsrControl::append_pcm16(packet)) {
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
        if !snapshot_matches_session(snapshot, session_id) {
            return;
        }
        snapshot.diagnostics.update_session(session_id, |session| {
            session.streaming.status = StageStatus::Failed;
            session.streaming.failure_kind = Some(match enqueue_result {
                RealtimeAudioEnqueue::Full => FailureKind::Overloaded,
                RealtimeAudioEnqueue::Disconnected => FailureKind::Worker,
                RealtimeAudioEnqueue::Sent => return,
            });
        });
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

#[derive(Debug, PartialEq, Eq)]
struct RealtimeEventOutcome {
    transcript: Option<String>,
    saw_finished: bool,
}

#[derive(Clone, Copy)]
enum StreamingResultKind {
    Partial,
    SegmentFinal,
    Final,
}

#[derive(Clone, Copy)]
struct StreamingTelemetryUpdate {
    kind: StreamingResultKind,
    latency_ms: u64,
    nonempty: bool,
}

impl StreamingTelemetryUpdate {
    fn new(kind: StreamingResultKind, displayed_text: &str, latency_ms: u64) -> Self {
        Self {
            kind,
            latency_ms,
            nonempty: !displayed_text.trim().is_empty(),
        }
    }

    fn apply(self, stage: &mut crate::diagnostics::StreamingStage) {
        stage.last_result_latency_ms = Some(self.latency_ms);
        match self.kind {
            StreamingResultKind::Partial => {
                stage
                    .first_partial_latency_ms
                    .get_or_insert(self.latency_ms);
                stage.partial_event_count = stage.partial_event_count.saturating_add(1);
                if self.nonempty {
                    stage
                        .first_nonempty_partial_latency_ms
                        .get_or_insert(self.latency_ms);
                    stage.nonempty_partial_event_count =
                        stage.nonempty_partial_event_count.saturating_add(1);
                }
            }
            StreamingResultKind::SegmentFinal => {
                stage.segment_final_event_count = stage.segment_final_event_count.saturating_add(1);
            }
            StreamingResultKind::Final => {}
        }
    }
}

fn apply_timestamp_diagnostics(
    stage: &mut crate::diagnostics::StreamingStage,
    delta: backend::TimestampDiagnosticsDelta,
) {
    stage.timestamp_bearing_result_count = stage
        .timestamp_bearing_result_count
        .saturating_add(delta.timestamp_bearing_result_count);
    stage.accepted_timed_unit_count = stage
        .accepted_timed_unit_count
        .saturating_add(delta.accepted_timed_unit_count);
    stage.result_with_rejected_timestamp_metadata_count = stage
        .result_with_rejected_timestamp_metadata_count
        .saturating_add(delta.result_with_rejected_timestamp_metadata_count);
    stage.truncated_timed_unit_count = stage
        .truncated_timed_unit_count
        .saturating_add(delta.truncated_timed_unit_count);
    if delta.latest_valid_audio_end_ms.is_some() {
        stage.latest_valid_audio_end_ms = delta.latest_valid_audio_end_ms;
    }
}

fn record_timestamp_diagnostics(update_diagnostics: impl FnOnce() -> Result<()>) {
    let _ = update_diagnostics();
}

fn record_finished_telemetry(update_diagnostics: impl FnOnce() -> Result<()>) -> bool {
    let _ = update_diagnostics();
    true
}

fn reset_authoritative_transcript(
    final_transcript: &mut Option<String>,
    partial_transcript: &Mutex<String>,
    snapshot: &mut Snapshot,
) {
    *final_transcript = None;
    partial_transcript
        .lock()
        .expect("partial transcript mutex poisoned")
        .clear();
    snapshot.transcript.clear();
    snapshot.raw_transcript = None;
    snapshot.refined_transcript = None;
    snapshot.text.clear();

    // These values describe one authoritative provider attempt. Preserve only
    // session-scoped reconnect/audio-delivery fields across reconstruction.
    if let Some(session) = snapshot.diagnostics.session.as_mut() {
        let streaming = &mut session.streaming;
        streaming.first_partial_latency_ms = None;
        streaming.first_nonempty_partial_latency_ms = None;
        streaming.last_result_latency_ms = None;
        streaming.partial_event_count = 0;
        streaming.nonempty_partial_event_count = 0;
        streaming.segment_final_event_count = 0;
        streaming.timestamp_bearing_result_count = 0;
        streaming.accepted_timed_unit_count = 0;
        streaming.result_with_rejected_timestamp_metadata_count = 0;
        streaming.truncated_timed_unit_count = 0;
        streaming.latest_valid_audio_end_ms = None;
    }
}

fn finalize_realtime_events(
    saw_finished: bool,
    final_transcript: Option<String>,
    update_diagnostics: impl FnOnce(StageStatus, Option<FailureKind>) -> Result<()>,
) -> RealtimeEventOutcome {
    let has_usable_transcript = final_transcript
        .as_ref()
        .is_some_and(|text| !text.trim().is_empty());
    let (status, failure_kind) = match (saw_finished, has_usable_transcript) {
        (true, true) => (StageStatus::Completed, None),
        (true, false) => (StageStatus::Empty, None),
        (false, true) => (StageStatus::Degraded, Some(FailureKind::Worker)),
        (false, false) => (StageStatus::Failed, Some(FailureKind::Worker)),
    };
    let _ = update_diagnostics(status, failure_kind);
    RealtimeEventOutcome {
        transcript: final_transcript,
        saw_finished,
    }
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
    asr_started_at: Instant,
    finalization_started_at: Arc<Mutex<Option<Instant>>>,
    event_rx: mpsc::Receiver<backend::AsrEvent>,
}

fn spawn_realtime_event_thread(
    context: RealtimeEventThreadContext,
) -> thread::JoinHandle<Result<RealtimeEventOutcome>> {
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
            asr_started_at,
            finalization_started_at,
            event_rx,
        } = context;
        let mut final_transcript = None;
        let mut realtime_reconstructing = false;
        let mut saw_finished = false;
        let mut logged_first_partial = false;
        let mut logged_first_nonempty_partial = false;

        while let Ok(event) = event_rx.recv() {
            match event {
                backend::AsrEvent::Ready => {
                    asr_ready.store(true, Ordering::SeqCst);
                    let latency_ms = elapsed_ms(asr_started_at);
                    eprintln!(
                        "voice-input realtime ASR timing: session {session_id} event=ready elapsed_ms={latency_ms}"
                    );
                    let _ = state.update(|snapshot| {
                        if snapshot_matches_session(snapshot, session_id) {
                            snapshot.diagnostics.update_session(session_id, |session| {
                                if session.streaming.status != StageStatus::Failed {
                                    session.streaming.status = StageStatus::InProgress;
                                    session.streaming.failure_kind = None;
                                }
                                session.streaming.ready_latency_ms = Some(latency_ms);
                            });
                        }
                    });
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
                    speech_detected.store(true, Ordering::SeqCst);
                    realtime_reconstructing = true;
                    state.update(|snapshot| {
                        if snapshot_matches_session(snapshot, session_id)
                            && matches!(snapshot.phase, Phase::Arming | Phase::Recording)
                        {
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
                        if snapshot_matches_session(snapshot, session_id)
                            && matches!(snapshot.phase, Phase::Arming | Phase::Recording)
                        {
                            snapshot.tooltip = "Replaying buffered audio…".into();
                        }
                    })?;
                }
                backend::AsrEvent::TranscriptReset => {
                    realtime_reconstructing = true;
                    state.update(|snapshot| {
                        if !snapshot_matches_session(snapshot, session_id) {
                            return;
                        }
                        reset_authoritative_transcript(
                            &mut final_transcript,
                            &partial_transcript,
                            snapshot,
                        );
                        logged_first_partial = false;
                        logged_first_nonempty_partial = false;
                        if matches!(
                            snapshot.phase,
                            Phase::Arming | Phase::Recording | Phase::Transcribing
                        ) {
                            snapshot.tooltip = "Realtime reconnecting — recording continues".into();
                        }
                    })?;
                }
                backend::AsrEvent::RealtimeTranscriptDelayed => {
                    speech_detected.store(true, Ordering::SeqCst);
                    realtime_overloaded.store(true, Ordering::SeqCst);
                    state.update(|snapshot| {
                        if !snapshot_matches_session(snapshot, session_id) {
                            return;
                        }
                        snapshot.diagnostics.update_session(session_id, |session| {
                            session.streaming.status = StageStatus::Failed;
                            session.streaming.failure_kind = Some(FailureKind::Overloaded);
                        });
                        if matches!(snapshot.phase, Phase::Arming | Phase::Recording) {
                            snapshot.tooltip =
                                "Realtime transcript delayed — recording continues".into();
                        }
                    })?;
                }
                backend::AsrEvent::AudioDeliveryCompleted {
                    packet_count,
                    sample_count,
                    max_queue_delay_ms,
                    last_queue_delay_ms,
                } => {
                    let latency_ms = elapsed_ms(asr_started_at);
                    let audio_duration_ms = sample_count
                        .saturating_mul(1_000)
                        .checked_div(u64::from(config.audio.sample_rate))
                        .unwrap_or(0);
                    eprintln!(
                        "voice-input realtime ASR timing: session {session_id} event=finish-sent elapsed_ms={latency_ms} audio_duration_ms={audio_duration_ms} packets={packet_count} max_queue_delay_ms={max_queue_delay_ms} last_queue_delay_ms={last_queue_delay_ms}"
                    );
                    let _ = state.update(|snapshot| {
                        if snapshot_matches_session(snapshot, session_id) {
                            snapshot.diagnostics.update_session(session_id, |session| {
                                session.streaming.audio_packet_count = packet_count;
                                session.streaming.audio_sent_duration_ms = Some(audio_duration_ms);
                                session.streaming.max_audio_queue_delay_ms =
                                    Some(max_queue_delay_ms);
                                session.streaming.last_audio_queue_delay_ms =
                                    Some(last_queue_delay_ms);
                                session.streaming.finish_sent_latency_ms = Some(latency_ms);
                            });
                        }
                    });
                }
                backend::AsrEvent::StreamingReconnect {
                    attempted,
                    succeeded,
                    replay_packet_count,
                    replay_sample_count,
                    terminal_failure_kind,
                } => {
                    let _ = state.update(|snapshot| {
                        if snapshot_matches_session(snapshot, session_id) {
                            snapshot.diagnostics.update_session(session_id, |session| {
                                session.streaming.reconnect_attempted_count = attempted;
                                session.streaming.reconnect_succeeded_count = succeeded;
                                session.streaming.replay_packet_count = replay_packet_count;
                                session.streaming.replay_sample_count = replay_sample_count;
                                session.streaming.reconnect_terminal_failure_kind =
                                    terminal_failure_kind;
                            });
                        }
                    });
                }
                backend::AsrEvent::TimestampDiagnostics { delta } => {
                    record_timestamp_diagnostics(|| {
                        state.update(|snapshot| {
                            if snapshot_matches_session(snapshot, session_id) {
                                snapshot.diagnostics.update_session(session_id, |session| {
                                    apply_timestamp_diagnostics(&mut session.streaming, delta);
                                });
                            }
                        })
                    });
                }
                backend::AsrEvent::Partial {
                    committed,
                    unstable,
                } => {
                    realtime_reconstructing = false;
                    let latency_ms = elapsed_ms(asr_started_at);
                    let transcript = format!("{committed}{unstable}");
                    let telemetry = StreamingTelemetryUpdate::new(
                        StreamingResultKind::Partial,
                        &transcript,
                        latency_ms,
                    );
                    let nonempty = telemetry.nonempty;
                    if !logged_first_partial {
                        logged_first_partial = true;
                        eprintln!(
                            "voice-input realtime ASR timing: session {session_id} event=first-partial elapsed_ms={latency_ms} nonempty={nonempty}"
                        );
                    }
                    if nonempty && !logged_first_nonempty_partial {
                        logged_first_nonempty_partial = true;
                        eprintln!(
                            "voice-input realtime ASR timing: session {session_id} event=first-nonempty-partial elapsed_ms={latency_ms}"
                        );
                    }
                    if nonempty {
                        speech_detected.store(true, Ordering::SeqCst);
                    }
                    *partial_transcript
                        .lock()
                        .expect("partial transcript mutex poisoned") = transcript.clone();
                    state.update(|snapshot| {
                        if !snapshot_matches_session(snapshot, session_id) {
                            return;
                        }
                        snapshot.diagnostics.update_session(session_id, |session| {
                            telemetry.apply(&mut session.streaming);
                        });
                        if matches!(
                            snapshot.phase,
                            Phase::Arming | Phase::Recording | Phase::Transcribing
                        ) {
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
                    let latency_ms = elapsed_ms(asr_started_at);
                    let telemetry = StreamingTelemetryUpdate::new(
                        StreamingResultKind::SegmentFinal,
                        &text,
                        latency_ms,
                    );
                    if telemetry.nonempty {
                        speech_detected.store(true, Ordering::SeqCst);
                    }
                    *partial_transcript
                        .lock()
                        .expect("partial transcript mutex poisoned") = text.clone();
                    state.update(|snapshot| {
                        if !snapshot_matches_session(snapshot, session_id) {
                            return;
                        }
                        snapshot.diagnostics.update_session(session_id, |session| {
                            telemetry.apply(&mut session.streaming);
                        });
                        if matches!(
                            snapshot.phase,
                            Phase::Arming | Phase::Recording | Phase::Transcribing
                        ) {
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
                    let latency_ms = elapsed_ms(asr_started_at);
                    let telemetry = StreamingTelemetryUpdate::new(
                        StreamingResultKind::Final,
                        &text,
                        latency_ms,
                    );
                    if telemetry.nonempty {
                        speech_detected.store(true, Ordering::SeqCst);
                    }
                    *partial_transcript
                        .lock()
                        .expect("partial transcript mutex poisoned") = text.clone();
                    final_transcript = Some(text.clone());
                    state.update(|snapshot| {
                        if !snapshot_matches_session(snapshot, session_id) {
                            return;
                        }
                        snapshot.diagnostics.update_session(session_id, |session| {
                            telemetry.apply(&mut session.streaming);
                        });
                        if matches!(
                            snapshot.phase,
                            Phase::Arming | Phase::Recording | Phase::Transcribing
                        ) {
                            snapshot.transcript = text.clone();
                            snapshot.tooltip = text.clone();
                        }
                    })?;
                }
                backend::AsrEvent::Finished => {
                    let latency_ms = elapsed_ms(asr_started_at);
                    eprintln!(
                        "voice-input realtime ASR timing: session {session_id} event=task-finished elapsed_ms={latency_ms}"
                    );
                    saw_finished = record_finished_telemetry(|| {
                        state.update(|snapshot| {
                            if snapshot_matches_session(snapshot, session_id) {
                                snapshot.diagnostics.update_session(session_id, |session| {
                                    session.streaming.task_finished_latency_ms = Some(latency_ms);
                                });
                            }
                        })
                    });
                    break;
                }
                backend::AsrEvent::TaskFailed {
                    kind,
                    provider_error_code,
                } => {
                    let latency_ms = elapsed_ms(asr_started_at);
                    let code = provider_error_code
                        .as_ref()
                        .map(|code| code.as_str())
                        .unwrap_or("unavailable");
                    eprintln!(
                        "voice-input realtime ASR timing: session {session_id} event=task-failed elapsed_ms={latency_ms} failure={} provider_code={code}",
                        kind.as_str()
                    );
                    speech_detected.store(true, Ordering::SeqCst);
                    realtime_overloaded.store(true, Ordering::SeqCst);
                    state.update(|snapshot| {
                        if snapshot_matches_session(snapshot, session_id) {
                            snapshot.diagnostics.update_session(session_id, |session| {
                                session.streaming.status = StageStatus::Failed;
                                if session.streaming.failure_kind != Some(FailureKind::Overloaded) {
                                    session.streaming.failure_kind = Some(kind);
                                }
                                session.streaming.task_failed_latency_ms = Some(latency_ms);
                                session.streaming.provider_error_code = provider_error_code.clone();
                                session.streaming.finalize_latency_ms = finalization_started_at
                                    .lock()
                                    .expect("finalization timer mutex poisoned")
                                    .map(elapsed_ms);
                            });
                        }
                    })?;
                    return Err(anyhow!("realtime ASR failed ({})", kind.as_str()));
                }
                backend::AsrEvent::Error { kind } => {
                    speech_detected.store(true, Ordering::SeqCst);
                    realtime_overloaded.store(true, Ordering::SeqCst);
                    state.update(|snapshot| {
                        if snapshot_matches_session(snapshot, session_id) {
                            snapshot.diagnostics.update_session(session_id, |session| {
                                session.streaming.status = StageStatus::Failed;
                                if session.streaming.failure_kind != Some(FailureKind::Overloaded) {
                                    session.streaming.failure_kind = Some(kind);
                                }
                                session.streaming.finalize_latency_ms = finalization_started_at
                                    .lock()
                                    .expect("finalization timer mutex poisoned")
                                    .map(elapsed_ms);
                            });
                        }
                    })?;
                    return Err(anyhow!("realtime ASR failed ({})", kind.as_str()));
                }
            }
        }

        let finalize_latency_ms = finalization_started_at
            .lock()
            .expect("finalization timer mutex poisoned")
            .map(elapsed_ms);
        let outcome = finalize_realtime_events(
            saw_finished,
            final_transcript,
            |status, terminal_failure_kind| {
                state.update(|snapshot| {
                    if snapshot_matches_session(snapshot, session_id) {
                        snapshot.diagnostics.update_session(session_id, |session| {
                            if session.streaming.failure_kind != Some(FailureKind::Overloaded) {
                                session.streaming.status = status;
                                session.streaming.failure_kind = terminal_failure_kind;
                            }
                            session.streaming.finalize_latency_ms = finalize_latency_ms;
                        });
                    }
                })
            },
        );

        Ok(outcome)
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
                    if snapshot_matches_session(snapshot, session_id)
                        && snapshot.phase == Phase::Recording
                    {
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
    fn finish_diagnostics(
        &self,
        session_id: u64,
        outcome: OverallOutcome,
        selected_result: SelectedResult,
        finalization_started: Instant,
    ) {
        let _ = self.state.update(|snapshot| {
            snapshot.diagnostics.finish_session(
                session_id,
                outcome,
                selected_result,
                elapsed_ms(finalization_started),
            );
        });
    }

    fn transcribe_local_fallback(&self, session_id: u64, audio: &[i16]) -> Result<String> {
        let started_at = Instant::now();
        let _ = self.state.update(|snapshot| {
            snapshot.diagnostics.update_session(session_id, |session| {
                session.local_fallback.status = StageStatus::InProgress;
                session.local_fallback.failure_kind = None;
            });
        });
        let result = self.transcribe_local_audio(audio);
        let latency_ms = elapsed_ms(started_at);
        let failure_kind = result.as_ref().err().map(classify_failure);
        let _ = self.state.update(|snapshot| {
            snapshot.diagnostics.update_session(session_id, |session| {
                session.local_fallback.latency_ms = Some(latency_ms);
                match &result {
                    Ok(text) if text.trim().is_empty() => {
                        session.local_fallback.status = StageStatus::Empty;
                        session.local_fallback.failure_kind = None;
                    }
                    Ok(_) => {
                        session.local_fallback.status = StageStatus::Completed;
                        session.local_fallback.failure_kind = None;
                    }
                    Err(_) => {
                        session.local_fallback.status = StageStatus::Failed;
                        session.local_fallback.failure_kind = failure_kind;
                    }
                }
            });
        });
        result
    }

    fn transcribe_full_audio(&self, audio: &[i16], pass: FullAudioPass) -> Result<Option<String>> {
        let temp_file = tempfile::NamedTempFile::new()
            .context("failed to create WAV temp file for full-audio retranscription")?;
        wav::write_pcm16_wav(temp_file.path(), self.config.audio.sample_rate, audio)?;
        match pass {
            FullAudioPass::AlibabaCompatible => {
                backend::transcribe_alibaba_full_audio(&self.config, temp_file.path())
            }
            FullAudioPass::QwenAudio3Native => {
                backend::transcribe_qwen_audio3_full_audio(&self.config, temp_file.path())
            }
        }
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
    use std::{io::Cursor, sync::atomic::AtomicUsize};

    use super::*;

    fn healthy_full_audio_plan_input() -> FullAudioPassPlanInput {
        FullAudioPassPlanInput {
            cancelled: false,
            has_audio: true,
            streaming: CandidateState::Usable,
            worker_interrupted: false,
            overloaded: false,
            saw_finished: true,
            captured_duration_ms: ADAPTIVE_NATIVE_DURATION_MS - 1,
        }
    }

    #[test]
    fn full_audio_pass_selection_keeps_providers_independent() {
        let mut config = Config::default();
        assert_eq!(selected_full_audio_pass(&config), None);

        config.asr.provider = AsrProvider::AlibabaQwenRealtime;
        config.asr.alibaba.final_pass_enabled = true;
        assert_eq!(
            selected_full_audio_pass(&config),
            Some(FullAudioPass::AlibabaCompatible)
        );

        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        assert_eq!(selected_full_audio_pass(&config), None);
        config.asr.alibaba_audio3.native_final_pass_mode = NativeFinalPassMode::Always;
        assert_eq!(
            selected_full_audio_pass(&config),
            Some(FullAudioPass::QwenAudio3Native)
        );

        config.asr.provider = AsrProvider::AlibabaQwenRealtime;
        config.asr.alibaba.final_pass_enabled = false;
        assert_eq!(selected_full_audio_pass(&config), None);
    }

    #[test]
    fn sanitized_backend_errors_map_to_bounded_failure_categories() {
        assert_eq!(
            classify_failure_text("ASR backend failed with exit status: 23"),
            FailureKind::LocalBackend
        );
        assert_eq!(
            classify_failure_text("Qwen-Audio-3 native ASR returned HTTP 401"),
            FailureKind::Authentication
        );
        assert_eq!(
            classify_failure_text("Alibaba final-pass ASR returned HTTP 429"),
            FailureKind::RateLimited
        );
    }

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
    fn state_failure_cannot_bypass_logical_capture_shutdown() {
        let capture_attached = AtomicBool::new(true);
        let remote_delivery_running = AtomicBool::new(true);
        let update_attempted = AtomicBool::new(false);

        let result = update_after_capture_shutdown(
            || {
                capture_attached.store(false, Ordering::SeqCst);
                remote_delivery_running.store(false, Ordering::SeqCst);
                Ok(())
            },
            || {
                update_attempted.store(true, Ordering::SeqCst);
                assert!(!capture_attached.load(Ordering::SeqCst));
                assert!(!remote_delivery_running.load(Ordering::SeqCst));
                Err::<(), _>(anyhow!("injected state persistence failure"))
            },
        );

        assert!(result.is_err());
        assert!(update_attempted.load(Ordering::SeqCst));
        assert!(!capture_attached.load(Ordering::SeqCst));
        assert!(!remote_delivery_running.load(Ordering::SeqCst));
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

    #[test]
    fn recording_diagnostics_use_actual_session_configuration_and_replace_old_data() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.native_final_pass_mode = NativeFinalPassMode::Always;
        config.asr.fallback_to_local = true;

        let mut diagnostics = diagnostics_for_session(&config, 40);
        diagnostics.finish_session(40, OverallOutcome::Failed, SelectedResult::None, 7);
        diagnostics = diagnostics_for_session(&config, 41);

        let session = diagnostics.session.unwrap();
        assert_eq!(session.session_id, 41);
        assert_eq!(
            session.provider,
            crate::diagnostics::Provider::AlibabaQwenAudio3
        );
        assert_eq!(session.final_pass.kind, FinalPassKind::QwenAudio3Native);
        assert_eq!(session.local_fallback.status, StageStatus::Pending);
        assert_eq!(session.asr_outcome, OverallOutcome::InProgress);
    }

    #[test]
    fn stale_worker_snapshot_update_is_rejected_by_session_guard() {
        let mut snapshot = Snapshot::idle(&Config::default());
        snapshot.diagnostics = diagnostics_for_session(&Config::default(), 52);
        snapshot.transcript = "new session".into();

        if snapshot_matches_session(&snapshot, 51) {
            snapshot.transcript = "stale worker".into();
        }

        assert_eq!(snapshot.transcript, "new session");
        assert!(snapshot_matches_session(&snapshot, 52));
    }

    #[test]
    fn committed_only_partial_is_nonempty_in_streaming_telemetry() {
        let mut stage = crate::diagnostics::StreamingStage::default();
        let update =
            StreamingTelemetryUpdate::new(StreamingResultKind::Partial, "committed text", 41);
        assert!(update.nonempty);
        update.apply(&mut stage);

        assert_eq!(stage.first_partial_latency_ms, Some(41));
        assert_eq!(stage.first_nonempty_partial_latency_ms, Some(41));
        assert_eq!(stage.last_result_latency_ms, Some(41));
        assert_eq!(stage.partial_event_count, 1);
        assert_eq!(stage.nonempty_partial_event_count, 1);
    }

    #[test]
    fn final_only_sequence_records_nonempty_last_result_telemetry() {
        let mut stage = crate::diagnostics::StreamingStage::default();
        let update =
            StreamingTelemetryUpdate::new(StreamingResultKind::Final, "terminal result", 73);
        assert!(update.nonempty);
        update.apply(&mut stage);

        assert_eq!(stage.last_result_latency_ms, Some(73));
        assert_eq!(stage.first_partial_latency_ms, None);
        assert_eq!(stage.first_nonempty_partial_latency_ms, None);
        assert_eq!(stage.partial_event_count, 0);
        assert_eq!(stage.nonempty_partial_event_count, 0);
    }

    #[test]
    fn timestamp_diagnostics_saturate_and_overwrite_latest_numeric_end() {
        let mut stage = crate::diagnostics::StreamingStage {
            timestamp_bearing_result_count: u64::MAX,
            accepted_timed_unit_count: u64::MAX - 1,
            result_with_rejected_timestamp_metadata_count: 4,
            truncated_timed_unit_count: 5,
            latest_valid_audio_end_ms: Some(900),
            ..crate::diagnostics::StreamingStage::default()
        };
        apply_timestamp_diagnostics(
            &mut stage,
            backend::TimestampDiagnosticsDelta {
                timestamp_bearing_result_count: 1,
                accepted_timed_unit_count: 3,
                result_with_rejected_timestamp_metadata_count: 1,
                truncated_timed_unit_count: 7,
                latest_valid_audio_end_ms: Some(800),
            },
        );

        assert_eq!(stage.timestamp_bearing_result_count, u64::MAX);
        assert_eq!(stage.accepted_timed_unit_count, u64::MAX);
        assert_eq!(stage.result_with_rejected_timestamp_metadata_count, 5);
        assert_eq!(stage.truncated_timed_unit_count, 12);
        assert_eq!(stage.latest_valid_audio_end_ms, Some(800));
    }

    #[test]
    fn timestamp_diagnostics_persistence_failure_is_best_effort() {
        let transcript_outcome = "unchanged transcript";
        record_timestamp_diagnostics(|| {
            Err(anyhow!(
                "injected timestamp diagnostics persistence failure"
            ))
        });
        assert_eq!(transcript_outcome, "unchanged transcript");
    }

    #[test]
    fn transcript_reset_clears_daemon_final_partial_and_hud_state() {
        let config = Config::default();
        let partial = Mutex::new("stale partial".to_string());
        let mut final_transcript = Some("stale final".to_string());
        let mut snapshot = Snapshot::idle(&config);
        snapshot.diagnostics = diagnostics_for_session(&config, 1);
        let streaming = &mut snapshot.diagnostics.session.as_mut().unwrap().streaming;
        streaming.first_partial_latency_ms = Some(10);
        streaming.last_result_latency_ms = Some(20);
        streaming.partial_event_count = 3;
        streaming.segment_final_event_count = 2;
        streaming.timestamp_bearing_result_count = 4;
        streaming.latest_valid_audio_end_ms = Some(500);
        streaming.reconnect_attempted_count = 1;
        snapshot.transcript = "stale HUD transcript".into();
        snapshot.raw_transcript = Some("stale raw".into());
        snapshot.refined_transcript = Some("stale refined".into());
        snapshot.text = "stale display text".into();

        reset_authoritative_transcript(&mut final_transcript, &partial, &mut snapshot);

        assert!(final_transcript.is_none());
        assert!(partial.lock().unwrap().is_empty());
        assert!(snapshot.transcript.is_empty());
        assert!(snapshot.raw_transcript.is_none());
        assert!(snapshot.refined_transcript.is_none());
        assert!(snapshot.text.is_empty());
        let streaming = &snapshot.diagnostics.session.as_ref().unwrap().streaming;
        assert_eq!(streaming.first_partial_latency_ms, None);
        assert_eq!(streaming.last_result_latency_ms, None);
        assert_eq!(streaming.partial_event_count, 0);
        assert_eq!(streaming.segment_final_event_count, 0);
        assert_eq!(streaming.timestamp_bearing_result_count, 0);
        assert_eq!(streaming.latest_valid_audio_end_ms, None);
        assert_eq!(streaming.reconnect_attempted_count, 1);
    }

    #[test]
    fn old_alibaba_final_then_channel_close_keeps_usable_transcript() {
        let observed = std::cell::Cell::new(None);
        let transcript = finalize_realtime_events(
            false,
            Some("usable final".into()),
            |status, failure_kind| {
                observed.set(Some((status, failure_kind)));
                Err(anyhow!("injected diagnostics persistence failure"))
            },
        );

        assert_eq!(transcript.transcript.as_deref(), Some("usable final"));
        assert!(!transcript.saw_finished);
        assert_eq!(
            observed.get(),
            Some((StageStatus::Degraded, Some(FailureKind::Worker)))
        );
    }

    #[test]
    fn finished_telemetry_failure_preserves_terminal_transcript_outcome() {
        let saw_finished = record_finished_telemetry(|| {
            Err(anyhow!(
                "injected task-finished telemetry persistence failure"
            ))
        });
        let outcome =
            finalize_realtime_events(saw_finished, Some("successful final".into()), |_, _| {
                Err(anyhow!("injected terminal telemetry persistence failure"))
            });

        assert!(outcome.saw_finished);
        assert_eq!(outcome.transcript.as_deref(), Some("successful final"));
    }

    #[test]
    fn adaptive_native_policy_exhaustively_covers_modes_states_and_duration_boundary() {
        let modes = [
            NativeFinalPassMode::StreamingOnly,
            NativeFinalPassMode::Adaptive,
            NativeFinalPassMode::Always,
        ];
        let streaming_states = [
            CandidateState::Usable,
            CandidateState::Empty,
            CandidateState::Failed,
            CandidateState::Degraded,
        ];
        let durations = [
            ADAPTIVE_NATIVE_DURATION_MS - 1,
            ADAPTIVE_NATIVE_DURATION_MS,
            ADAPTIVE_NATIVE_DURATION_MS + 1,
        ];

        for mode in modes {
            for streaming in streaming_states {
                for cancelled in [false, true] {
                    for has_audio in [false, true] {
                        for worker_interrupted in [false, true] {
                            for overloaded in [false, true] {
                                for saw_finished in [false, true] {
                                    for captured_duration_ms in durations {
                                        let actual =
                                            decide_native_final_pass(NativeFinalPassPolicyInput {
                                                mode,
                                                cancelled,
                                                has_audio,
                                                streaming,
                                                worker_interrupted,
                                                overloaded,
                                                saw_finished,
                                                captured_duration_ms,
                                            });
                                        let expected = if cancelled {
                                            NativeFinalPassPolicyDecision {
                                                invoke: false,
                                                reason: FinalPassReason::Cancelled,
                                            }
                                        } else if !has_audio {
                                            NativeFinalPassPolicyDecision {
                                                invoke: false,
                                                reason: FinalPassReason::NoAudio,
                                            }
                                        } else if mode == NativeFinalPassMode::StreamingOnly {
                                            NativeFinalPassPolicyDecision {
                                                invoke: false,
                                                reason: FinalPassReason::StreamingOnly,
                                            }
                                        } else if mode == NativeFinalPassMode::Always {
                                            NativeFinalPassPolicyDecision {
                                                invoke: true,
                                                reason: FinalPassReason::Always,
                                            }
                                        } else if overloaded {
                                            NativeFinalPassPolicyDecision {
                                                invoke: true,
                                                reason: FinalPassReason::Overloaded,
                                            }
                                        } else if worker_interrupted {
                                            NativeFinalPassPolicyDecision {
                                                invoke: true,
                                                reason: FinalPassReason::Interrupted,
                                            }
                                        } else if streaming == CandidateState::Empty {
                                            NativeFinalPassPolicyDecision {
                                                invoke: true,
                                                reason: FinalPassReason::Empty,
                                            }
                                        } else if matches!(
                                            streaming,
                                            CandidateState::Failed | CandidateState::Degraded
                                        ) {
                                            NativeFinalPassPolicyDecision {
                                                invoke: true,
                                                reason: FinalPassReason::Degraded,
                                            }
                                        } else if !saw_finished {
                                            NativeFinalPassPolicyDecision {
                                                invoke: true,
                                                reason: FinalPassReason::MissingCompletion,
                                            }
                                        } else if captured_duration_ms
                                            >= ADAPTIVE_NATIVE_DURATION_MS
                                        {
                                            NativeFinalPassPolicyDecision {
                                                invoke: true,
                                                reason: FinalPassReason::Duration,
                                            }
                                        } else {
                                            NativeFinalPassPolicyDecision {
                                                invoke: false,
                                                reason: FinalPassReason::HealthyStream,
                                            }
                                        };
                                        assert_eq!(
                                            actual, expected,
                                            "input: mode={mode:?}, streaming={streaming:?}, cancelled={cancelled}, has_audio={has_audio}, worker_interrupted={worker_interrupted}, overloaded={overloaded}, saw_finished={saw_finished}, duration={captured_duration_ms}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn full_audio_plans_drive_exact_adaptive_invocation_counts() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.native_final_pass_mode = NativeFinalPassMode::Adaptive;

        let calls = AtomicUsize::new(0);
        let healthy_plan = plan_full_audio_pass(&config, healthy_full_audio_plan_input());
        assert_eq!(healthy_plan.pass, None);
        assert_eq!(
            healthy_plan.audio3_decision,
            Some(NativeFinalPassPolicyDecision {
                invoke: false,
                reason: FinalPassReason::HealthyStream,
            })
        );
        let invocation = execute_full_audio_pass(healthy_plan.pass, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some("unexpected".into()))
        });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(invocation.state, None);

        let mut missing_completion = healthy_full_audio_plan_input();
        missing_completion.saw_finished = false;
        let mut degraded = healthy_full_audio_plan_input();
        degraded.streaming = CandidateState::Degraded;
        let mut interrupted = healthy_full_audio_plan_input();
        interrupted.worker_interrupted = true;
        let mut overloaded = healthy_full_audio_plan_input();
        overloaded.overloaded = true;
        let mut empty = healthy_full_audio_plan_input();
        empty.streaming = CandidateState::Empty;
        let mut duration_boundary = healthy_full_audio_plan_input();
        duration_boundary.captured_duration_ms = ADAPTIVE_NATIVE_DURATION_MS;

        for (input, expected_reason) in [
            (missing_completion, FinalPassReason::MissingCompletion),
            (degraded, FinalPassReason::Degraded),
            (interrupted, FinalPassReason::Interrupted),
            (overloaded, FinalPassReason::Overloaded),
            (empty, FinalPassReason::Empty),
            (duration_boundary, FinalPassReason::Duration),
        ] {
            let calls = AtomicUsize::new(0);
            let plan = plan_full_audio_pass(&config, input);
            assert_eq!(plan.pass, Some(FullAudioPass::QwenAudio3Native));
            assert_eq!(
                plan.audio3_decision,
                Some(NativeFinalPassPolicyDecision {
                    invoke: true,
                    reason: expected_reason,
                })
            );
            let invocation = execute_full_audio_pass(plan.pass, |pass| {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(pass, FullAudioPass::QwenAudio3Native);
                Ok(Some("native result".into()))
            });
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                invocation.state,
                Some((FullAudioPass::QwenAudio3Native, CandidateState::Usable))
            );
        }
    }

    #[test]
    fn full_audio_plans_suppress_streaming_only_cancel_and_no_audio() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;

        let streaming_only = healthy_full_audio_plan_input();
        let mut cancelled = healthy_full_audio_plan_input();
        cancelled.cancelled = true;
        config.asr.alibaba_audio3.native_final_pass_mode = NativeFinalPassMode::Always;
        let mut no_audio = healthy_full_audio_plan_input();
        no_audio.has_audio = false;

        for (mode, input, expected_reason) in [
            (
                NativeFinalPassMode::StreamingOnly,
                streaming_only,
                FinalPassReason::StreamingOnly,
            ),
            (
                NativeFinalPassMode::Always,
                cancelled,
                FinalPassReason::Cancelled,
            ),
            (
                NativeFinalPassMode::Always,
                no_audio,
                FinalPassReason::NoAudio,
            ),
        ] {
            config.asr.alibaba_audio3.native_final_pass_mode = mode;
            let calls = AtomicUsize::new(0);
            let plan = plan_full_audio_pass(&config, input);
            assert_eq!(plan.pass, None);
            assert_eq!(plan.audio3_decision.unwrap().reason, expected_reason);
            let invocation = execute_full_audio_pass(plan.pass, |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some("unexpected".into()))
            });
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(invocation.state, None);
        }
    }

    #[test]
    fn legacy_alibaba_pass_invokes_once_independent_of_audio3_mode() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenRealtime;
        config.asr.alibaba.final_pass_enabled = true;

        for mode in [
            NativeFinalPassMode::StreamingOnly,
            NativeFinalPassMode::Adaptive,
            NativeFinalPassMode::Always,
        ] {
            config.asr.alibaba_audio3.native_final_pass_mode = mode;
            let calls = AtomicUsize::new(0);
            let plan = plan_full_audio_pass(&config, healthy_full_audio_plan_input());
            assert_eq!(plan.pass, Some(FullAudioPass::AlibabaCompatible));
            assert_eq!(plan.audio3_decision, None);
            let invocation = execute_full_audio_pass(plan.pass, |pass| {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(pass, FullAudioPass::AlibabaCompatible);
                Ok(Some("legacy result".into()))
            });
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                invocation.state,
                Some((FullAudioPass::AlibabaCompatible, CandidateState::Usable))
            );
        }
    }

    #[test]
    fn injected_full_audio_failures_preserve_streaming_or_request_fallback_exactly() {
        for message in ["native timeout", "native service failure"] {
            let calls = AtomicUsize::new(0);
            let invocation =
                execute_full_audio_pass(Some(FullAudioPass::QwenAudio3Native), |pass| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(pass, FullAudioPass::QwenAudio3Native);
                    Err(anyhow!(message))
                });
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                invocation.state,
                Some((FullAudioPass::QwenAudio3Native, CandidateState::Failed))
            );
            assert!(invocation.text.is_none());
            assert!(invocation.error.is_some());

            assert_eq!(
                decide_result_source(
                    AsrProvider::AlibabaQwenAudio3,
                    invocation.state,
                    CandidateState::Usable,
                    true,
                    false,
                ),
                ResultDecision::Selected(SelectedResult::Streaming)
            );
            assert_eq!(
                decide_result_source(
                    AsrProvider::AlibabaQwenAudio3,
                    invocation.state,
                    CandidateState::Empty,
                    true,
                    false,
                ),
                ResultDecision::FallbackNeeded
            );
            assert_eq!(
                decide_result_source(
                    AsrProvider::AlibabaQwenAudio3,
                    invocation.state,
                    CandidateState::Empty,
                    false,
                    false,
                ),
                ResultDecision::Failed
            );
        }
    }

    #[test]
    fn captured_audio_duration_uses_exact_thirty_second_boundary() {
        assert_eq!(captured_audio_duration_ms(479_999, 16_000), 29_999);
        assert_eq!(captured_audio_duration_ms(480_000, 16_000), 30_000);
        assert_eq!(captured_audio_duration_ms(480_001, 16_000), 30_000);
    }

    #[test]
    fn healthy_short_adaptive_diagnostics_skip_pending_final_stage() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.native_final_pass_mode = NativeFinalPassMode::Adaptive;
        let mut diagnostics = diagnostics_for_session(&config, 61);
        let policy = decide_native_final_pass(NativeFinalPassPolicyInput {
            mode: NativeFinalPassMode::Adaptive,
            cancelled: false,
            has_audio: true,
            streaming: CandidateState::Usable,
            worker_interrupted: false,
            overloaded: false,
            saw_finished: true,
            captured_duration_ms: ADAPTIVE_NATIVE_DURATION_MS - 1,
        });
        diagnostics.update_session(61, |session| {
            session.final_pass.decision = if policy.invoke {
                FinalPassDecision::Invoked
            } else {
                FinalPassDecision::Skipped
            };
            session.final_pass.reason = Some(policy.reason);
            if !policy.invoke {
                session.final_pass.status = StageStatus::Skipped;
            }
        });
        diagnostics.finish_session(61, OverallOutcome::Completed, SelectedResult::Streaming, 3);

        let final_pass = diagnostics.session.unwrap().final_pass;
        assert_eq!(final_pass.status, StageStatus::Skipped);
        assert_eq!(final_pass.decision, FinalPassDecision::Skipped);
        assert_eq!(final_pass.reason, Some(FinalPassReason::HealthyStream));
    }

    #[test]
    fn native_final_policy_is_table_driven_and_fallback_exact() {
        let final_states = [
            CandidateState::Usable,
            CandidateState::Empty,
            CandidateState::Failed,
        ];
        let streaming_states = [
            CandidateState::Usable,
            CandidateState::Empty,
            CandidateState::Failed,
            CandidateState::Degraded,
        ];

        for final_state in final_states {
            for streaming_state in streaming_states {
                for fallback_enabled in [false, true] {
                    let decision = decide_result_source(
                        AsrProvider::AlibabaQwenAudio3,
                        Some((FullAudioPass::QwenAudio3Native, final_state)),
                        streaming_state,
                        fallback_enabled,
                        false,
                    );
                    let streaming_usable = matches!(
                        streaming_state,
                        CandidateState::Usable | CandidateState::Degraded
                    );
                    let expected = match final_state {
                        CandidateState::Usable => {
                            ResultDecision::Selected(SelectedResult::QwenAudio3Native)
                        }
                        CandidateState::Empty if streaming_usable => {
                            ResultDecision::Selected(SelectedResult::Streaming)
                        }
                        CandidateState::Empty => ResultDecision::AuthoritativeEmpty,
                        CandidateState::Failed if streaming_usable => {
                            ResultDecision::Selected(SelectedResult::Streaming)
                        }
                        CandidateState::Failed if fallback_enabled => {
                            ResultDecision::FallbackNeeded
                        }
                        CandidateState::Failed => ResultDecision::Failed,
                        CandidateState::Degraded => unreachable!(),
                    };
                    assert_eq!(decision, expected);
                    assert_eq!(
                        matches!(decision, ResultDecision::FallbackNeeded),
                        final_state == CandidateState::Failed
                            && !streaming_usable
                            && fallback_enabled
                    );
                }
            }
        }
    }

    #[test]
    fn policy_covers_overload_compatible_pass_disabled_pass_and_local_primary() {
        for (final_state, fallback_enabled, expected) in [
            (
                CandidateState::Usable,
                false,
                ResultDecision::Selected(SelectedResult::QwenAudio3Native),
            ),
            (
                CandidateState::Empty,
                true,
                ResultDecision::AuthoritativeEmpty,
            ),
            (CandidateState::Failed, true, ResultDecision::FallbackNeeded),
            (CandidateState::Failed, false, ResultDecision::Failed),
        ] {
            assert_eq!(
                decide_result_source(
                    AsrProvider::AlibabaQwenAudio3,
                    Some((FullAudioPass::QwenAudio3Native, final_state)),
                    CandidateState::Usable,
                    fallback_enabled,
                    true,
                ),
                expected
            );
        }
        for (final_state, streaming, expected) in [
            (
                CandidateState::Usable,
                CandidateState::Failed,
                ResultDecision::Selected(SelectedResult::AlibabaCompatibleFinal),
            ),
            (
                CandidateState::Failed,
                CandidateState::Degraded,
                ResultDecision::Selected(SelectedResult::Streaming),
            ),
            (
                CandidateState::Empty,
                CandidateState::Failed,
                ResultDecision::AuthoritativeEmpty,
            ),
        ] {
            assert_eq!(
                decide_result_source(
                    AsrProvider::AlibabaQwenRealtime,
                    Some((FullAudioPass::AlibabaCompatible, final_state)),
                    streaming,
                    false,
                    false,
                ),
                expected
            );
        }
        for streaming in [CandidateState::Empty, CandidateState::Failed] {
            assert_eq!(
                decide_result_source(AsrProvider::AlibabaQwenAudio3, None, streaming, true, false,),
                ResultDecision::FallbackNeeded
            );
            assert_eq!(
                decide_result_source(
                    AsrProvider::AlibabaQwenAudio3,
                    None,
                    streaming,
                    false,
                    false,
                ),
                ResultDecision::Failed
            );
        }
        assert_eq!(
            decide_result_source(
                AsrProvider::LocalCli,
                None,
                CandidateState::Usable,
                true,
                false,
            ),
            ResultDecision::Selected(SelectedResult::LocalPrimary)
        );
    }

    #[test]
    fn degraded_streaming_can_be_the_selected_result() {
        let mut diagnostics = crate::diagnostics::Diagnostics::inactive();
        diagnostics.start_session(
            26,
            crate::diagnostics::Provider::AlibabaQwenRealtime,
            FinalPassKind::None,
            false,
        );
        diagnostics.update_session(26, |session| {
            session.streaming.status = StageStatus::Degraded;
            session.streaming.failure_kind = Some(FailureKind::Worker);
        });
        diagnostics.finish_session(26, OverallOutcome::Completed, SelectedResult::Streaming, 9);

        let session = diagnostics.session.unwrap();
        assert_eq!(session.asr_outcome, OverallOutcome::Completed);
        assert_eq!(session.streaming.status, StageStatus::Degraded);
        assert_eq!(session.streaming.failure_kind, Some(FailureKind::Worker));
        assert_eq!(session.selected_result, SelectedResult::Streaming);
    }

    #[test]
    fn representative_audio3_streaming_then_native_diagnostics_are_bounded() {
        let mut diagnostics = crate::diagnostics::Diagnostics::inactive();
        diagnostics.start_session(
            27,
            crate::diagnostics::Provider::AlibabaQwenAudio3,
            FinalPassKind::QwenAudio3Native,
            true,
        );
        diagnostics.update_session(27, |session| {
            session.streaming.status = StageStatus::Completed;
            session.streaming.ready_latency_ms = Some(5);
            session.streaming.finalize_latency_ms = Some(8);
            session.final_pass.status = StageStatus::Completed;
            session.final_pass.latency_ms = Some(13);
        });
        diagnostics.finish_session(
            27,
            OverallOutcome::Completed,
            SelectedResult::QwenAudio3Native,
            21,
        );

        let session = diagnostics.session.unwrap();
        assert_eq!(session.asr_outcome, OverallOutcome::Completed);
        assert_eq!(session.selected_result, SelectedResult::QwenAudio3Native);
        assert_eq!(session.local_fallback.status, StageStatus::Skipped);
        let json = serde_json::to_string(&session).unwrap();
        assert!(!json.contains("transcript"));
        assert!(!json.contains("endpoint"));
        assert!(!json.contains("model"));
    }

    #[test]
    fn cancellation_no_audio_and_error_have_terminal_outcomes() {
        for (session_id, outcome) in [
            (31, OverallOutcome::Cancelled),
            (32, OverallOutcome::Empty),
            (33, OverallOutcome::Failed),
        ] {
            let mut diagnostics = crate::diagnostics::Diagnostics::inactive();
            diagnostics.start_session(
                session_id,
                crate::diagnostics::Provider::AlibabaQwenAudio3,
                FinalPassKind::QwenAudio3Native,
                true,
            );
            diagnostics.finish_session(session_id, outcome, SelectedResult::None, 4);
            let session = diagnostics.session.unwrap();
            assert_eq!(session.asr_outcome, outcome);
            assert_eq!(session.selected_result, SelectedResult::None);
            assert_eq!(session.total_asr_latency_ms, Some(4));
        }
    }
}
