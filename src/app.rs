use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::{
        atomic::Ordering,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    args::{
        AsrCommand, AsrStreamTestOptions, AsrTestOptions, Command as ParsedCommand, ConfigOptions,
        HudCommand, HudMoveDirection, HudPositionCommand, LlmCommand, OutputFormat, SetupCommand,
    },
    backend::{self, AsrBackend, AsrControl, AsrEvent, AsrSessionHandle, AudioSpec},
    config::{AsrProvider, Config},
    daemon, focused_window, llm, output, paths, setup,
    state::Snapshot,
    wav,
};

pub fn run() -> Result<()> {
    match crate::args::parse()? {
        ParsedCommand::Daemon => {
            let mut config = Config::load()?;
            crate::credentials::apply_runtime_credentials(&mut config)?;
            daemon::run(config)
        }
        ParsedCommand::Record(action) => {
            let command = record_control_command(action);
            let response = daemon::send_control_command(&command)?;
            print!("{response}");
            Ok(())
        }
        ParsedCommand::Hud(command) => {
            let response = daemon::send_control_command(&hud_control_command(command))?;
            print!("{response}");
            Ok(())
        }
        ParsedCommand::Status(options) => run_status(options),
        ParsedCommand::Config(options) => print_config(options),
        ParsedCommand::Settings => open_settings(),
        ParsedCommand::SettingsBackend => crate::settings_backend::run_stdio(),
        ParsedCommand::Setup(command) => run_setup(command),
        ParsedCommand::Asr(AsrCommand::Test(options)) => run_asr_test(options),
        ParsedCommand::Asr(AsrCommand::StreamTest(options)) => run_asr_stream_test(options),
        ParsedCommand::Llm(LlmCommand::Test) => {
            let mut config = Config::load()?;
            crate::credentials::apply_runtime_credentials(&mut config)?;
            llm::test_connectivity(&config)?;
            println!("LLM connectivity OK");
            Ok(())
        }
        ParsedCommand::Help => {
            print!("{}", crate::args::help_text());
            Ok(())
        }
        ParsedCommand::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn record_control_command(action: crate::args::RecordAction) -> String {
    // Capture the destination before any output-target probing so toggle-off
    // describes the window focused as close as possible to the key press.
    let focused_window_hint = matches!(
        action,
        crate::args::RecordAction::Stop | crate::args::RecordAction::Toggle
    )
    .then(|| {
        focused_window::capture()
            .ok()
            .map(|window| window.control_hint())
    })
    .flatten();
    let base = match action {
        crate::args::RecordAction::Start => "start",
        crate::args::RecordAction::Stop => "stop",
        crate::args::RecordAction::Toggle => "toggle",
        crate::args::RecordAction::Cancel => "cancel",
        crate::args::RecordAction::Restart => "restart",
    };

    if matches!(
        action,
        crate::args::RecordAction::Start
            | crate::args::RecordAction::Toggle
            | crate::args::RecordAction::Restart
    ) && let Ok(target_hint) = output::detect_output_target_hint()
    {
        let label = match target_hint {
            output::OutputTargetHint::Wayland => "wayland",
            output::OutputTargetHint::XWayland => "xwayland",
        };
        let mut command = format!("{base} {label}");
        if let Some(hint) = focused_window_hint {
            command.push(' ');
            command.push_str(&hint);
        }
        return command;
    }

    focused_window_hint
        .map(|hint| format!("{base} {hint}"))
        .unwrap_or_else(|| base.into())
}

fn run_status(options: crate::args::StatusOptions) -> Result<()> {
    let config = Config::load()?;
    let state_path = config
        .state_path()?
        .unwrap_or(paths::runtime_dir()?.join("state.json"));
    let mut last_payload = String::new();

    loop {
        let snapshot = load_snapshot(&config, &state_path)?;
        let payload = match options.format {
            OutputFormat::Text => snapshot.class.clone(),
            OutputFormat::Json => snapshot.as_waybar_json(options.extended).to_string(),
        };

        if payload != last_payload {
            println!("{payload}");
            std::io::stdout().flush().ok();
            last_payload = payload;
        }

        if !options.follow {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn print_config(options: ConfigOptions) -> Result<()> {
    let config = Config::load()?;
    match options.format {
        OutputFormat::Text => {
            print!("{}", toml::to_string_pretty(&config)?);
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
    }
    Ok(())
}

fn open_settings() -> Result<()> {
    let settings_dir = paths::quickshell_settings_path()?;
    let activated = Command::new("/usr/bin/qs")
        .arg("--path")
        .arg(&settings_dir)
        .args(["ipc", "call", "voiceInputSettings", "activate"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if activated {
        return Ok(());
    }

    Command::new("/usr/bin/qs")
        .args(["--daemonize", "--no-duplicate", "--path"])
        .arg(settings_dir)
        .env("VOICE_INPUT_BIN", paths::current_executable()?)
        .env("VOICE_INPUT_FONT_PATH", paths::ui_font_path()?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to launch Quickshell settings UI")?;
    Ok(())
}

fn run_asr_test(options: AsrTestOptions) -> Result<()> {
    let mut config = load_audio3_test_config("asr test")?;
    let metadata = match fs::metadata(&options.file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("ASR test file does not exist: {}", options.file.display())
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect ASR test file `{}`",
                    options.file.display()
                )
            });
        }
    };
    if !metadata.is_file() {
        anyhow::bail!("ASR test path is not a file: {}", options.file.display());
    }

    apply_audio3_test_credentials(&mut config)?;

    match backend::transcribe_qwen_audio3_full_audio(&config, &options.file)? {
        Some(transcript) => println!("{transcript}"),
        None => println!("No transcript (empty audio)."),
    }
    Ok(())
}

fn run_asr_stream_test(options: AsrStreamTestOptions) -> Result<()> {
    let mut config = load_audio3_test_config("asr stream-test")?;
    let samples = wav::read_pcm16_wav(
        &options.file,
        config.audio.sample_rate,
        config.audio.max_duration_secs,
    )?;
    apply_audio3_test_credentials(&mut config)?;

    let asr = backend::build(&config);
    match stream_pcm_with_backend(asr.as_ref(), &config, &samples)? {
        Some(transcript) => println!("{transcript}"),
        None => println!("No transcript (empty audio)."),
    }
    Ok(())
}

fn load_audio3_test_config(command: &str) -> Result<Config> {
    let config = Config::load()?;
    if config.asr.provider != AsrProvider::AlibabaQwenAudio3 {
        bail!("{command} requires selected provider `alibaba-qwen-audio3`");
    }
    if !config.asr.alibaba_audio3.experimental_enabled {
        bail!("{command} requires `asr.alibaba_audio3.experimental_enabled = true`");
    }
    Ok(config)
}

fn apply_audio3_test_credentials(config: &mut Config) -> Result<()> {
    crate::credentials::apply_runtime_credentials(config)?;
    if config.asr.alibaba_audio3.api_key.trim().is_empty() {
        let api_key = crate::credentials::decrypt(crate::credentials::ALIBABA_CREDENTIAL_ID)
            .context("failed to load encrypted Alibaba credential")?;
        config.asr.alibaba.api_key = api_key.clone();
        config.asr.alibaba_audio3.api_key = api_key;
    }
    Ok(())
}

fn stream_pcm_with_backend(
    asr: &dyn AsrBackend,
    config: &Config,
    samples: &[i16],
) -> Result<Option<String>> {
    let session = asr.spawn_session(
        config,
        AudioSpec {
            sample_rate_hz: config.audio.sample_rate,
        },
    )?;
    let AsrSessionHandle {
        control_tx,
        abort_flag,
        event_rx,
        join,
    } = session;
    let chunk_samples = usize::try_from(config.audio.sample_rate)
        .context("audio sample rate does not fit this platform")?
        .div_ceil(10);

    // The adapter and its bounded control queue provide backpressure, so the
    // prerecorded input can be submitted without sleeping for its duration.
    let send_result = samples
        .chunks(chunk_samples)
        .try_for_each(|chunk| control_tx.send(AsrControl::AppendPcm16(chunk.to_vec())))
        .and_then(|()| control_tx.send(AsrControl::Finish));
    drop(control_tx);
    if send_result.is_err() {
        abort_flag.store(true, Ordering::SeqCst);
        let _ = join_asr_worker(join);
        bail!("Qwen-Audio-3 streaming ASR stopped while accepting audio");
    }

    let outcome = collect_stream_events(
        &event_rx,
        Duration::from_millis(config.asr.finalize_timeout_ms),
    );
    if !matches!(outcome, StreamOutcome::Finished(_)) {
        abort_flag.store(true, Ordering::SeqCst);
    }
    drop(event_rx);
    let worker_result = join_asr_worker(join);

    match outcome {
        StreamOutcome::Finished(transcript) => {
            worker_result?;
            Ok(transcript)
        }
        StreamOutcome::Error => bail!("Qwen-Audio-3 streaming ASR failed"),
        StreamOutcome::TimedOut => bail!("Qwen-Audio-3 streaming ASR finalization timed out"),
        StreamOutcome::Disconnected => {
            if worker_result.is_err() {
                bail!("Qwen-Audio-3 streaming ASR worker failed");
            }
            bail!("Qwen-Audio-3 streaming ASR ended without a completion event")
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StreamOutcome {
    Finished(Option<String>),
    Error,
    TimedOut,
    Disconnected,
}

fn collect_stream_events(event_rx: &Receiver<AsrEvent>, timeout: Duration) -> StreamOutcome {
    let deadline = Instant::now() + timeout;
    let mut transcript = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match event_rx.recv_timeout(remaining) {
            Ok(AsrEvent::Final { text }) if !text.trim().is_empty() => transcript = Some(text),
            Ok(AsrEvent::Finished) => return StreamOutcome::Finished(transcript),
            Ok(AsrEvent::Error { .. }) => return StreamOutcome::Error,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return StreamOutcome::TimedOut,
            Err(mpsc::RecvTimeoutError::Disconnected) => return StreamOutcome::Disconnected,
        }
    }
}

fn join_asr_worker(join: thread::JoinHandle<Result<()>>) -> Result<()> {
    join.join()
        .map_err(|_| anyhow!("Qwen-Audio-3 streaming ASR worker panicked"))?
}

fn run_setup(command: SetupCommand) -> Result<()> {
    match command {
        SetupCommand::Model => {
            let mut config = Config::load()?;
            setup::run_model_wizard(&mut config)
        }
        SetupCommand::Waybar => setup::print_waybar_snippet(),
        SetupCommand::Hyprland => setup::print_hyprland_snippet(),
        SetupCommand::Systemd => setup::install_systemd_unit(),
        SetupCommand::Backend(args) => setup::proxy_backend_setup(&args),
    }
}

fn load_snapshot(config: &Config, state_path: &std::path::Path) -> Result<Snapshot> {
    if !state_path.exists() {
        return Ok(Snapshot::idle(config));
    }
    let source = fs::read_to_string(state_path)
        .with_context(|| format!("failed to read {}", state_path.display()))?;
    let snapshot: Snapshot = serde_json::from_str(&source)
        .with_context(|| format!("failed to parse {}", state_path.display()))?;
    Ok(snapshot)
}

fn hud_control_command(command: HudCommand) -> String {
    match command {
        HudCommand::Move { direction, amount } => {
            let direction = match direction {
                HudMoveDirection::Left => "left",
                HudMoveDirection::Right => "right",
                HudMoveDirection::Up => "up",
                HudMoveDirection::Down => "down",
            };
            match amount {
                Some(value) => format!("hud move {direction} {value}"),
                None => format!("hud move {direction}"),
            }
        }
        HudCommand::Position(position) => format!(
            "hud position {}",
            match position {
                HudPositionCommand::Center => "bottom-center",
                HudPositionCommand::Left => "bottom-left",
                HudPositionCommand::Right => "bottom-right",
            }
        ),
        HudCommand::Center => "hud center".into(),
        HudCommand::Reset => "hud reset".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{Arc, Mutex, atomic::AtomicBool, mpsc},
        thread,
    };

    use anyhow::Result;

    use super::stream_pcm_with_backend;
    use crate::{
        backend::{
            ASR_CONTROL_QUEUE_CAPACITY, AsrBackend, AsrControl, AsrEvent, AsrSessionHandle,
            AudioSpec,
        },
        config::Config,
    };

    struct FakeStreamingBackend {
        controls: Arc<Mutex<Vec<usize>>>,
    }

    impl AsrBackend for FakeStreamingBackend {
        fn spawn_session(&self, _config: &Config, _spec: AudioSpec) -> Result<AsrSessionHandle> {
            let (control_tx, control_rx) = mpsc::sync_channel(ASR_CONTROL_QUEUE_CAPACITY);
            let (event_tx, event_rx) = mpsc::channel();
            let controls = self.controls.clone();
            let join = thread::spawn(move || {
                event_tx.send(AsrEvent::Ready).unwrap();
                while let Ok(control) = control_rx.recv() {
                    match control {
                        AsrControl::AppendPcm16(samples) => {
                            controls.lock().unwrap().push(samples.len());
                        }
                        AsrControl::Finish => {
                            controls.lock().unwrap().push(0);
                            event_tx
                                .send(AsrEvent::Final {
                                    text: "stream transcript".into(),
                                })
                                .unwrap();
                            event_tx.send(AsrEvent::Finished).unwrap();
                            return Ok(());
                        }
                    }
                }
                anyhow::bail!("missing finish control")
            });
            Ok(AsrSessionHandle {
                control_tx,
                abort_flag: Arc::new(AtomicBool::new(false)),
                event_rx,
                join,
            })
        }

        fn transcribe_file(&self, _config: &Config, _wav_path: &Path) -> Result<String> {
            unreachable!("stream test must not use native file transcription")
        }
    }

    #[test]
    fn stream_test_chunks_pcm_and_collects_final_transcript() {
        let controls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeStreamingBackend {
            controls: controls.clone(),
        };
        let config = Config::default();
        let samples = vec![7; 3_201];

        assert_eq!(
            stream_pcm_with_backend(&backend, &config, &samples).unwrap(),
            Some("stream transcript".into())
        );
        assert_eq!(*controls.lock().unwrap(), vec![1_600, 1_600, 1, 0]);
    }
}
