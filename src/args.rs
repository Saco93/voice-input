use anyhow::{Result, anyhow, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Daemon,
    Record(RecordAction),
    Hud(HudCommand),
    Status(StatusOptions),
    Config(ConfigOptions),
    Settings,
    Setup(SetupCommand),
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
    BottomCenter,
    BottomLeft,
    BottomRight,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupCommand {
    Model,
    Waybar,
    Hyprland,
    Systemd,
    Backend(Vec<String>),
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
        "daemon" => Ok(Command::Daemon),
        "record" => parse_record(args.collect()),
        "hud" => parse_hud(args.collect()),
        "status" => parse_status(args.collect()),
        "config" => parse_config(args.collect()),
        "settings" => Ok(Command::Settings),
        "setup" => parse_setup(args.collect()),
        "llm" => parse_llm(args.collect()),
        other => bail!("unknown command `{other}`"),
    }
}

fn parse_record(args: Vec<String>) -> Result<Command> {
    let Some(action) = args.first() else {
        bail!("record requires start|stop|toggle|cancel");
    };

    let action = match action.as_str() {
        "start" => RecordAction::Start,
        "stop" => RecordAction::Stop,
        "toggle" => RecordAction::Toggle,
        "cancel" => RecordAction::Cancel,
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
            let position = match args.get(1).map(String::as_str) {
                Some("bottom-center") | Some("center") => HudPositionCommand::BottomCenter,
                Some("bottom-left") | Some("left") => HudPositionCommand::BottomLeft,
                Some("bottom-right") | Some("right") => HudPositionCommand::BottomRight,
                Some(other) => bail!("unknown hud position `{other}`"),
                None => bail!("hud position requires bottom-center|bottom-left|bottom-right"),
            };
            Ok(Command::Hud(HudCommand::Position(position)))
        }
        "center" => Ok(Command::Hud(HudCommand::Center)),
        "reset" => Ok(Command::Hud(HudCommand::Reset)),
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

fn parse_setup(args: Vec<String>) -> Result<Command> {
    let Some(subcommand) = args.first() else {
        return Ok(Command::Setup(SetupCommand::Waybar));
    };

    let command = match subcommand.as_str() {
        "model" => SetupCommand::Model,
        "waybar" => SetupCommand::Waybar,
        "hyprland" => SetupCommand::Hyprland,
        "systemd" => SetupCommand::Systemd,
        "gpu" | "onnx" => SetupCommand::Backend(args),
        other => bail!("unknown setup command `{other}`"),
    };

    Ok(Command::Setup(command))
}

fn parse_llm(args: Vec<String>) -> Result<Command> {
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
  voice-input record <start|stop|toggle|cancel>
  voice-input hud move <left|right|up|down> [amount]
  voice-input hud position <bottom-center|bottom-left|bottom-right>
  voice-input hud <center|reset>
  voice-input status [--follow] [--extended] [--format text|json]
  voice-input config [--format text|json]
  voice-input settings
  voice-input setup <model|waybar|hyprland|systemd|gpu|onnx>
  voice-input llm test

COMPATIBILITY:
  `voice-input record start` / `voice-input record stop` are intended for Hyprland `bind` / `bindr`.
  `voice-input record toggle` remains available as the fallback path for compositor setups where
  press-and-release is not robust.
"#
}
