use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Daemon,
    Record(RecordAction),
    Hud(HudCommand),
    Status(StatusOptions),
    Diagnostics(DiagnosticsOptions),
    Config(ConfigOptions),
    Settings,
    SettingsBackend,
    Setup(SetupCommand),
    Asr(AsrCommand),
    Llm(LlmCommand),
    Help,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordAction {
    Start,
    Stop,
    Toggle,
    Cancel,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HudCommand {
    Move {
        direction: HudMoveDirection,
        amount: Option<i32>,
    },
    Position(HudPositionCommand),
    Center,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudMoveDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudPositionCommand {
    Center,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusOptions {
    pub follow: bool,
    pub extended: bool,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigOptions {
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticsOptions {
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupCommand {
    Model,
    Waybar,
    Hyprland,
    Systemd,
    Backend(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrCommand {
    Test(AsrTestOptions),
    StreamTest(AsrStreamTestOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrTestOptions {
    pub file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrStreamTestOptions {
    pub file: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmCommand {
    Test,
}

pub fn parse() -> Result<Command> {
    let mut args = std::env::args().skip(1);
    let Some(first) = args.next() else {
        return Ok(Command::Daemon);
    };

    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "-V" | "--version" | "version" => Ok(Command::Version),
        "daemon" => require_no_args(args, "daemon").map(|()| Command::Daemon),
        "record" => parse_record(args.collect()),
        "hud" => parse_hud(args.collect()),
        "status" => parse_status(args.collect()),
        "diagnostics" => parse_diagnostics(args.collect()),
        "config" => parse_config(args.collect()),
        "settings" => require_no_args(args, "settings").map(|()| Command::Settings),
        "settings-backend" => parse_settings_backend(args.collect()),
        "setup" => parse_setup(args.collect()),
        "asr" => parse_asr(args.collect()),
        "llm" => parse_llm(args.collect()),
        other => bail!("unknown command `{other}`"),
    }
}

fn require_no_args(mut args: impl Iterator<Item = String>, command: &str) -> Result<()> {
    if let Some(extra) = args.next() {
        bail!("{command} does not accept argument `{extra}`");
    }
    Ok(())
}

fn require_arg_count(args: &[String], expected: usize, usage: &str) -> Result<()> {
    if args.len() != expected {
        bail!("expected `{usage}`");
    }
    Ok(())
}

fn parse_record(args: Vec<String>) -> Result<Command> {
    require_arg_count(&args, 1, "record <start|stop|toggle|cancel|restart>")?;
    let Some(action) = args.first() else {
        bail!("record requires start|stop|toggle|cancel|restart");
    };

    let action = match action.as_str() {
        "start" => RecordAction::Start,
        "stop" => RecordAction::Stop,
        "toggle" => RecordAction::Toggle,
        "cancel" => RecordAction::Cancel,
        "restart" => RecordAction::Restart,
        other => bail!("unknown record action `{other}`"),
    };

    Ok(Command::Record(action))
}

fn parse_hud(args: Vec<String>) -> Result<Command> {
    let Some(action) = args.first() else {
        bail!("hud requires move|position|center|reset");
    };

    match action.as_str() {
        "move" => {
            if !(2..=3).contains(&args.len()) {
                bail!("expected `hud move <left|right|up|down> [amount]`");
            }
            let direction = match args.get(1).map(String::as_str) {
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

            Ok(Command::Hud(HudCommand::Move { direction, amount }))
        }
        "position" => {
            require_arg_count(
                &args,
                2,
                "hud position <bottom-center|bottom-left|bottom-right>",
            )?;
            let position = match args.get(1).map(String::as_str) {
                Some("bottom-center") | Some("center") => HudPositionCommand::Center,
                Some("bottom-left") | Some("left") => HudPositionCommand::Left,
                Some("bottom-right") | Some("right") => HudPositionCommand::Right,
                Some(other) => bail!("unknown hud position `{other}`"),
                None => bail!("hud position requires bottom-center|bottom-left|bottom-right"),
            };
            Ok(Command::Hud(HudCommand::Position(position)))
        }
        "center" => {
            require_arg_count(&args, 1, "hud center")?;
            Ok(Command::Hud(HudCommand::Center))
        }
        "reset" => {
            require_arg_count(&args, 1, "hud reset")?;
            Ok(Command::Hud(HudCommand::Reset))
        }
        other => bail!("unknown hud command `{other}`"),
    }
}

fn parse_status(args: Vec<String>) -> Result<Command> {
    let mut follow = false;
    let mut extended = false;
    let mut format = OutputFormat::Text;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--follow" => follow = true,
            "--extended" => extended = true,
            "--format" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("--format requires `text` or `json`"))?;
                format = parse_format(&value)?;
            }
            other => bail!("unknown status option `{other}`"),
        }
    }

    Ok(Command::Status(StatusOptions {
        follow,
        extended,
        format,
    }))
}

fn parse_diagnostics(args: Vec<String>) -> Result<Command> {
    match args.as_slice() {
        [] => Ok(Command::Diagnostics(DiagnosticsOptions {
            format: OutputFormat::Text,
        })),
        [option, value] if option == "--format" => Ok(Command::Diagnostics(DiagnosticsOptions {
            format: parse_format(value)?,
        })),
        [option] if option == "--format" => bail!("--format requires `text` or `json`"),
        [unknown, ..] => bail!("unknown diagnostics option `{unknown}`"),
    }
}

fn parse_config(args: Vec<String>) -> Result<Command> {
    let mut format = OutputFormat::Text;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--format" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("--format requires `text` or `json`"))?;
                format = parse_format(&value)?;
            }
            other => bail!("unknown config option `{other}`"),
        }
    }

    Ok(Command::Config(ConfigOptions { format }))
}

fn parse_settings_backend(args: Vec<String>) -> Result<Command> {
    if args == ["--stdio"] {
        Ok(Command::SettingsBackend)
    } else {
        bail!("settings-backend requires exactly `--stdio`")
    }
}

fn parse_setup(args: Vec<String>) -> Result<Command> {
    let Some(subcommand) = args.first() else {
        return Ok(Command::Setup(SetupCommand::Waybar));
    };

    let command = match subcommand.as_str() {
        "model" | "waybar" | "hyprland" | "systemd" => {
            require_arg_count(&args, 1, "setup <model|waybar|hyprland|systemd>")?;
            match subcommand.as_str() {
                "model" => SetupCommand::Model,
                "waybar" => SetupCommand::Waybar,
                "hyprland" => SetupCommand::Hyprland,
                "systemd" => SetupCommand::Systemd,
                _ => unreachable!(),
            }
        }
        "gpu" | "onnx" => SetupCommand::Backend(args),
        other => bail!("unknown setup command `{other}`"),
    };

    Ok(Command::Setup(command))
}

fn parse_asr(args: Vec<String>) -> Result<Command> {
    let Some(subcommand) = args.first() else {
        bail!("asr requires the `test` or `stream-test` subcommand");
    };
    match subcommand.as_str() {
        "test" => {
            let file = parse_asr_file_option(&args[1..], subcommand)?;
            Ok(Command::Asr(AsrCommand::Test(AsrTestOptions { file })))
        }
        "stream-test" => {
            let file = parse_asr_file_option(&args[1..], subcommand)?;
            Ok(Command::Asr(AsrCommand::StreamTest(AsrStreamTestOptions {
                file,
            })))
        }
        _ => bail!("unknown asr command `{subcommand}`"),
    }
}

fn parse_asr_file_option(args: &[String], subcommand: &str) -> Result<PathBuf> {
    match args.first().map(String::as_str) {
        Some("--file") => {}
        Some(other) => {
            bail!("unknown asr {subcommand} argument `{other}`; expected `--file <wav-path>`")
        }
        None => bail!("asr {subcommand} requires `--file <wav-path>`"),
    }
    let file = args
        .get(1)
        .ok_or_else(|| anyhow!("--file requires a WAV path"))?;
    if let Some(extra) = args.get(2) {
        bail!("asr {subcommand} does not accept extra argument `{extra}`");
    }

    Ok(PathBuf::from(file))
}

fn parse_llm(args: Vec<String>) -> Result<Command> {
    require_arg_count(&args, 1, "llm test")?;
    let Some(subcommand) = args.first() else {
        bail!("llm requires a subcommand");
    };

    match subcommand.as_str() {
        "test" => Ok(Command::Llm(LlmCommand::Test)),
        other => bail!("unknown llm command `{other}`"),
    }
}

fn parse_format(value: &str) -> Result<OutputFormat> {
    match value {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        other => bail!("unknown format `{other}`"),
    }
}

pub fn help_text() -> &'static str {
    r#"Voice Input is an Omarchy-native voice input utility for Wayland.

USAGE:
  voice-input
  voice-input daemon
  voice-input record <start|stop|toggle|cancel|restart>
  voice-input hud move <left|right|up|down> [amount]
  voice-input hud position <bottom-center|bottom-left|bottom-right>
  voice-input hud <center|reset>
  voice-input status [--follow] [--extended] [--format text|json]
  voice-input diagnostics [--format text|json]
  voice-input config [--format text|json]
  voice-input settings
  voice-input setup <model|waybar|hyprland|systemd|gpu|onnx>
  voice-input asr test --file <wav-path>
  voice-input asr stream-test --file <wav-path>
  voice-input llm test

COMPATIBILITY:
  `voice-input record start` / `voice-input record stop` are intended for Hyprland `bind` / `bindr`.
  `voice-input record toggle` remains available as the fallback path for compositor setups where
  press-and-release is not robust.
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn fixed_arity_commands_reject_trailing_arguments() {
        assert!(parse_record(strings(&["toggle", "extra"])).is_err());
        assert!(parse_hud(strings(&["center", "extra"])).is_err());
        assert!(parse_hud(strings(&["position", "left", "extra"])).is_err());
        assert!(parse_llm(strings(&["test", "extra"])).is_err());
        assert!(parse_setup(strings(&["systemd", "extra"])).is_err());
        assert!(parse_asr(strings(&["test", "--file", "sample.wav", "extra"])).is_err());
        assert!(parse_asr(strings(&["stream-test", "--file", "sample.wav", "extra"])).is_err());
    }

    #[test]
    fn restart_is_a_single_record_action() {
        assert_eq!(
            parse_record(strings(&["restart"])).unwrap(),
            Command::Record(RecordAction::Restart)
        );
    }

    #[test]
    fn asr_test_requires_exact_file_option() {
        assert_eq!(
            parse_asr(strings(&["test", "--file", "/tmp/sample.wav"])).unwrap(),
            Command::Asr(AsrCommand::Test(AsrTestOptions {
                file: PathBuf::from("/tmp/sample.wav")
            }))
        );
        assert!(parse_asr(strings(&[])).is_err());
        assert!(parse_asr(strings(&["unknown"])).is_err());
        assert!(parse_asr(strings(&["test"])).is_err());
        assert!(parse_asr(strings(&["test", "sample.wav"])).is_err());
        assert!(parse_asr(strings(&["test", "--file"])).is_err());
        assert!(parse_asr(strings(&["test", "--other", "sample.wav"])).is_err());
    }

    #[test]
    fn asr_stream_test_requires_exact_file_option() {
        assert_eq!(
            parse_asr(strings(&["stream-test", "--file", "/tmp/sample.wav"])).unwrap(),
            Command::Asr(AsrCommand::StreamTest(AsrStreamTestOptions {
                file: PathBuf::from("/tmp/sample.wav")
            }))
        );
        assert!(parse_asr(strings(&["stream-test"])).is_err());
        assert!(parse_asr(strings(&["stream-test", "sample.wav"])).is_err());
        assert!(parse_asr(strings(&["stream-test", "--file"])).is_err());
        assert!(parse_asr(strings(&["stream-test", "--other", "sample.wav"])).is_err());
    }

    #[test]
    fn help_lists_asr_stream_test_usage() {
        assert!(help_text().contains("voice-input asr stream-test --file <wav-path>"));
    }

    #[test]
    fn diagnostics_parser_is_typed_and_strict() {
        assert_eq!(
            parse_diagnostics(strings(&[])).unwrap(),
            Command::Diagnostics(DiagnosticsOptions {
                format: OutputFormat::Text
            })
        );
        assert_eq!(
            parse_diagnostics(strings(&["--format", "json"])).unwrap(),
            Command::Diagnostics(DiagnosticsOptions {
                format: OutputFormat::Json
            })
        );
        assert!(parse_diagnostics(strings(&["--format"])).is_err());
        assert!(parse_diagnostics(strings(&["--format", "yaml"])).is_err());
        assert!(parse_diagnostics(strings(&["--format", "json", "extra"])).is_err());
        assert!(parse_diagnostics(strings(&["--extended"])).is_err());
    }

    #[test]
    fn help_lists_diagnostics_usage() {
        assert!(help_text().contains("voice-input diagnostics [--format text|json]"));
    }

    #[test]
    fn backend_setup_preserves_forwarded_arguments() {
        assert_eq!(
            parse_setup(strings(&["gpu", "--device", "cuda"])).unwrap(),
            Command::Setup(SetupCommand::Backend(strings(&["gpu", "--device", "cuda"])))
        );
    }
}
