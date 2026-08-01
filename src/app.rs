use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};

use crate::{
    args::{
        Command as ParsedCommand, ConfigOptions, HudCommand, HudMoveDirection, HudPositionCommand,
        LlmCommand, OutputFormat, SetupCommand,
    },
    config::Config,
    daemon, focused_window, llm, output, paths, setup,
    state::Snapshot,
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
