use std::sync::OnceLock;
use std::{
    collections::HashMap,
    env, fs,
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::config::{Config, HotkeyMode, OutputMode};

const ACTIVE_WINDOW_TIMEOUT_MS: u64 = 500;
const INPUT_METHOD_TIMEOUT_MS: u64 = 800;
const CLIPBOARD_QUERY_TIMEOUT_MS: u64 = 1_000;
const CLIPBOARD_COPY_TIMEOUT_MS: u64 = 1_500;
const KEY_SIMULATION_TIMEOUT_MS: u64 = 1_500;
const TEXT_INJECTION_BASE_TIMEOUT_MS: u64 = 1_500;
const TEXT_INJECTION_PER_CHAR_TIMEOUT_MS: u64 = 12;
const TEXT_INJECTION_MAX_TIMEOUT_MS: u64 = 5_000;
const X11_CLIPBOARD_TIMEOUT_MS: u64 = 1_500;
const SESSION_ENV_TIMEOUT_MS: u64 = 800;
const OUTPUT_TARGET_RETRIES: usize = 6;
const OUTPUT_TARGET_RETRY_DELAY_MS: u64 = 50;
const DIRECT_TYPE_MAX_CHARS: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputTargetHint {
    Wayland,
    XWayland,
}

#[derive(Debug, Clone)]
pub struct EmitReport {
    pub target: String,
    pub mode: String,
    pub driver: String,
}

pub fn emit_text(
    config: &Config,
    text: &str,
    target_hint: Option<OutputTargetHint>,
) -> Result<EmitReport> {
    eprintln!("voice-input output: begin emit_text");
    let ime_guard = ImeGuard::prepare(config)?;
    let settle_delay_ms = effective_output_settle_delay_ms(config);
    let target = resolve_output_target(target_hint);
    let effective_mode = effective_output_mode_for_text(config, &target, text);
    eprintln!(
        "voice-input output: target={} mode={} hint={}",
        target.label(),
        effective_mode.label(),
        target_hint
            .map(|hint| match hint {
                OutputTargetHint::Wayland => "wayland",
                OutputTargetHint::XWayland => "xwayland",
            })
            .unwrap_or("none")
    );

    let primary_result = match effective_mode {
        EffectiveOutputMode::Type => {
            eprintln!("voice-input output: type_with_wtype");
            type_with_wtype(config, text, settle_delay_ms)
        }
        OutputMode::Clipboard => {
            eprintln!("voice-input output: clipboard copy");
            settle_before_output(settle_delay_ms);
            copy_to_clipboards(text, &target)
        }
        OutputMode::Paste => {
            eprintln!("voice-input output: paste via clipboard");
            settle_before_output(settle_delay_ms);
            paste_via_clipboard(config, text, &target)
        }
    };

    let result = match primary_result {
        Ok(()) => Ok(EmitReport {
            target: target.label().to_string(),
            mode: effective_mode.label().to_string(),
            driver: target.driver_for_mode(&effective_mode).to_string(),
        }),
        Err(error) if should_try_clipboard_fallback(config, &effective_mode) => {
            settle_before_output(settle_delay_ms);
            paste_via_clipboard(config, text, &target)
                .map(|()| EmitReport {
                    target: target.label().to_string(),
                    mode: "paste-fallback".into(),
                    driver: target.driver_for_mode(&OutputMode::Paste).to_string(),
                })
                .or(Err(error))
        }
        Err(error) => Err(error),
    };

    eprintln!("voice-input output: restoring ime");
    ime_guard.restore()?;
    eprintln!("voice-input output: end emit_text");
    result
}

pub fn detect_output_target_hint() -> Result<OutputTargetHint> {
    detect_output_target_with_retry().map(|target| match target {
        OutputTarget::Wayland => OutputTargetHint::Wayland,
        OutputTarget::XWayland { .. } => OutputTargetHint::XWayland,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutputTarget {
    Wayland,
    XWayland { address: Option<String> },
}

type EffectiveOutputMode = OutputMode;

trait EffectiveOutputModeLabel {
    fn label(&self) -> &'static str;
}

impl EffectiveOutputModeLabel for OutputMode {
    fn label(&self) -> &'static str {
        match self {
            OutputMode::Type => "type",
            OutputMode::Clipboard => "clipboard",
            OutputMode::Paste => "paste",
        }
    }
}

fn type_with_wtype(config: &Config, text: &str, settle_delay_ms: u64) -> Result<()> {
    let mut command = Command::new("wtype");
    command
        .arg("-s")
        .arg(settle_delay_ms.to_string())
        .arg("-d")
        .arg(config.output.type_delay_ms.to_string())
        .arg("--")
        .arg(text)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = spawn_and_wait(
        &mut command,
        text_injection_timeout_ms(text),
        "wtype text injection",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "wtype failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn text_injection_timeout_ms(text: &str) -> u64 {
    TEXT_INJECTION_BASE_TIMEOUT_MS
        .saturating_add(
            (text.chars().count() as u64).saturating_mul(TEXT_INJECTION_PER_CHAR_TIMEOUT_MS),
        )
        .min(TEXT_INJECTION_MAX_TIMEOUT_MS)
}

fn effective_output_settle_delay_ms(config: &Config) -> u64 {
    const TOGGLE_HOTKEY_SETTLE_MS: u64 = 500;

    let configured = config.output.pre_type_delay_ms;
    if matches!(config.hotkey.mode, HotkeyMode::Toggle)
        && accelerator_uses_modifiers(&config.hotkey.accelerator)
    {
        configured.max(TOGGLE_HOTKEY_SETTLE_MS)
    } else {
        configured
    }
}

fn accelerator_uses_modifiers(accelerator: &str) -> bool {
    let upper = accelerator.to_ascii_uppercase();
    ["SUPER", "CTRL", "ALT", "SHIFT"]
        .iter()
        .any(|modifier| upper.contains(modifier))
}

fn settle_before_output(delay_ms: u64) {
    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }
}

fn paste_via_clipboard(config: &Config, text: &str, target: &OutputTarget) -> Result<()> {
    eprintln!("voice-input output: capture clipboard backup");
    let backup = ClipboardBackup::capture(target)?;
    eprintln!("voice-input output: write clipboard payload");
    copy_to_clipboards(text, target)?;
    eprintln!(
        "voice-input output: send paste chord {}",
        selected_paste_keys(config, target)
    );
    press_key_chord(selected_paste_keys(config, target), target)?;
    thread::sleep(Duration::from_millis(220));
    eprintln!("voice-input output: restore clipboard backup");
    backup.restore()
}

fn selected_paste_keys<'a>(config: &'a Config, target: &OutputTarget) -> &'a str {
    if target.is_xwayland() && !config.output.xwayland_paste_keys.trim().is_empty() {
        &config.output.xwayland_paste_keys
    } else {
        &config.output.paste_keys
    }
}

fn effective_output_mode(config: &Config, target: &OutputTarget) -> EffectiveOutputMode {
    if target.is_xwayland()
        && matches!(config.output.mode, OutputMode::Type)
        && config.output.prefer_paste_for_xwayland
    {
        OutputMode::Paste
    } else {
        config.output.mode.clone()
    }
}

fn effective_output_mode_for_text(
    config: &Config,
    target: &OutputTarget,
    text: &str,
) -> EffectiveOutputMode {
    let configured = effective_output_mode(config, target);
    if matches!(configured, OutputMode::Type) && text.chars().count() > DIRECT_TYPE_MAX_CHARS {
        eprintln!(
            "voice-input output: using paste for long text ({} chars)",
            text.chars().count()
        );
        OutputMode::Paste
    } else {
        configured
    }
}

fn should_try_clipboard_fallback(config: &Config, attempted_mode: &OutputMode) -> bool {
    matches!(attempted_mode, OutputMode::Type) && config.output.fallback_to_clipboard
}

fn copy_to_clipboards(text: &str, target: &OutputTarget) -> Result<()> {
    if target.is_xwayland() {
        copy_to_x11_clipboard(text.as_bytes())?;
    } else {
        copy_to_wayland_clipboard(text)?;
    }
    Ok(())
}

fn copy_to_wayland_clipboard(text: &str) -> Result<()> {
    let mut command = Command::new("wl-copy");
    command
        .arg("--trim-newline")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        // wl-copy forks a clipboard-provider process by default. A piped
        // stderr remains open in that provider and makes wait_with_output()
        // wait forever even though the original child has exited.
        .stderr(Stdio::null());
    let output = spawn_with_input_and_wait(
        &mut command,
        text.as_bytes(),
        CLIPBOARD_COPY_TIMEOUT_MS,
        "wl-copy clipboard update",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "wl-copy failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn press_key_chord(chord: &str, target: &OutputTarget) -> Result<()> {
    if let OutputTarget::XWayland { .. } = target {
        return press_key_chord_via_xdotool(chord);
    }

    let mut parts: Vec<String> = chord
        .split('+')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect();

    if parts.is_empty() {
        return Err(anyhow!("paste key chord is empty"));
    }

    let key = parts.pop().unwrap_or_default();
    let mut command = Command::new("wtype");
    for modifier in &parts {
        command.arg("-M").arg(modifier);
    }
    command.arg("-k").arg(key);
    for modifier in parts.iter().rev() {
        command.arg("-m").arg(modifier);
    }

    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let output = spawn_and_wait(
        &mut command,
        KEY_SIMULATION_TIMEOUT_MS,
        "wtype paste chord injection",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "failed to simulate paste chord: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn press_key_chord_via_xdotool(chord: &str) -> Result<()> {
    ensure_x11_tools_available()?;
    eprintln!("voice-input output: xdotool key {}", chord);
    let mut command = Command::new("xdotool");
    command
        .arg("key")
        .arg("--clearmodifiers")
        .arg(chord)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = spawn_and_wait(
        &mut command,
        KEY_SIMULATION_TIMEOUT_MS,
        "xdotool XWayland paste injection",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "failed to dispatch XWayland paste chord: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn detect_output_target() -> Result<OutputTarget> {
    let active = query_active_window_via_socket().or_else(|_| query_active_window_via_hyprctl())?;

    Ok(if active.xwayland {
        OutputTarget::XWayland {
            address: active.address.filter(|value| !value.trim().is_empty()),
        }
    } else {
        OutputTarget::Wayland
    })
}

fn detect_output_target_with_retry() -> Result<OutputTarget> {
    let mut last_error = None;
    for attempt in 0..OUTPUT_TARGET_RETRIES {
        match detect_output_target() {
            Ok(target) => return Ok(target),
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < OUTPUT_TARGET_RETRIES {
                    thread::sleep(Duration::from_millis(OUTPUT_TARGET_RETRY_DELAY_MS));
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("failed to detect active output target")))
}

fn query_active_window_via_socket() -> Result<ActiveWindow> {
    let socket_path = hyprland_command_socket_path()?;
    let mut stream = UnixStream::connect(&socket_path)
        .with_context(|| format!("failed to connect to {}", socket_path.display()))?;
    let timeout = Some(Duration::from_millis(ACTIVE_WINDOW_TIMEOUT_MS.max(1)));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    stream
        .write_all(b"j/activewindow")
        .context("failed to query Hyprland active window over IPC")?;
    stream.shutdown(Shutdown::Write).ok();

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .context("failed to read Hyprland active window response")?;

    serde_json::from_str(&response).context("failed to parse Hyprland active window IPC JSON")
}

fn query_active_window_via_hyprctl() -> Result<ActiveWindow> {
    let mut command = hyprctl_command();
    command.arg("activewindow").arg("-j");
    let output = spawn_and_wait(
        &mut command,
        ACTIVE_WINDOW_TIMEOUT_MS,
        "Hyprland active window query",
    )?;

    if !output.status.success() {
        bail!(
            "hyprctl activewindow failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    serde_json::from_slice(&output.stdout).context("failed to parse Hyprland active window JSON")
}

#[derive(Debug, Deserialize)]
struct ActiveWindow {
    #[serde(default)]
    xwayland: bool,
    #[serde(default)]
    address: Option<String>,
}

impl OutputTarget {
    fn is_xwayland(&self) -> bool {
        matches!(self, Self::XWayland { .. })
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::XWayland { .. } => "xwayland",
        }
    }

    fn driver_for_mode(&self, mode: &OutputMode) -> &'static str {
        match (self, mode) {
            (_, OutputMode::Type) => "wtype",
            (Self::Wayland, OutputMode::Clipboard) => "wl-copy",
            (Self::Wayland, OutputMode::Paste) => "wl-copy+wtype",
            (Self::XWayland { .. }, OutputMode::Clipboard) => "xclip",
            (Self::XWayland { .. }, OutputMode::Paste) => "xclip+xdotool",
        }
    }
}

fn resolve_output_target(target_hint: Option<OutputTargetHint>) -> OutputTarget {
    let current = detect_output_target_with_retry().ok();

    if matches!(target_hint, Some(OutputTargetHint::XWayland))
        || matches!(current, Some(OutputTarget::XWayland { .. }))
    {
        return OutputTarget::XWayland { address: None };
    }

    if matches!(target_hint, Some(OutputTargetHint::Wayland))
        || matches!(current, Some(OutputTarget::Wayland))
    {
        return OutputTarget::Wayland;
    }

    OutputTarget::Wayland
}

fn ensure_x11_tools_available() -> Result<()> {
    let missing: Vec<&str> = ["xdotool", "xclip"]
        .into_iter()
        .filter(|binary| !command_available(binary))
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        bail!(
            "reliable XWayland paste requires {} to be installed",
            missing.join(" and ")
        )
    }
}

fn capture_x11_clipboard(target: &OutputTarget) -> Result<Option<Vec<u8>>> {
    if !target.is_xwayland() || !command_available("xclip") {
        return Ok(None);
    }

    eprintln!("voice-input output: capture x11 clipboard");
    let mut command = Command::new("xclip");
    command
        .arg("-selection")
        .arg("clipboard")
        .arg("-out")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = spawn_and_wait(
        &mut command,
        X11_CLIPBOARD_TIMEOUT_MS,
        "xclip clipboard capture",
    )?;

    if output.status.success() {
        Ok(Some(output.stdout))
    } else {
        Ok(None)
    }
}

fn copy_to_x11_clipboard(text: &[u8]) -> Result<()> {
    ensure_x11_tools_available()?;
    eprintln!("voice-input output: xclip set clipboard");

    let mut command = Command::new("xclip");
    command
        .arg("-selection")
        .arg("clipboard")
        .arg("-in")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        // xclip forks a long-lived clipboard provider. If stderr is piped,
        // that provider keeps the descriptor open after the launcher exits,
        // causing wait_with_output() to block indefinitely.
        .stderr(Stdio::null());
    let output = spawn_with_input_and_wait(
        &mut command,
        text,
        X11_CLIPBOARD_TIMEOUT_MS,
        "xclip clipboard update",
    )?;

    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "xclip failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn restore_x11_clipboard(text: &[u8]) -> Result<()> {
    copy_to_x11_clipboard(text)
}

fn command_available(binary: &str) -> bool {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(format!("command -v {binary} >/dev/null 2>&1"));
    command
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn hyprctl_command() -> Command {
    let mut command = Command::new("hyprctl");
    populate_session_environment(&mut command);
    command
}

fn hyprland_command_socket_path() -> Result<PathBuf> {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(dirs::runtime_dir)
        .ok_or_else(|| anyhow!("XDG runtime directory not found"))?;
    let hypr_dir = runtime_dir.join("hypr");

    if let Some(signature) = session_env_value("HYPRLAND_INSTANCE_SIGNATURE")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| discover_hyprland_signature(&hypr_dir))
    {
        let candidate = hypr_dir.join(signature).join(".socket.sock");
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    find_latest_hyprland_socket(&hypr_dir)
        .ok_or_else(|| anyhow!("failed to locate Hyprland command socket"))
}

fn populate_session_environment(command: &mut Command) {
    for key in [
        "HYPRLAND_INSTANCE_SIGNATURE",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XDG_RUNTIME_DIR",
    ] {
        if let Some(value) = session_env_value(key) {
            command.env(key, value);
        }
    }
}

fn session_env_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            session_environment()
                .get(key)
                .cloned()
                .filter(|value| !value.trim().is_empty())
        })
}

fn session_environment() -> &'static HashMap<String, String> {
    static SESSION_ENV: OnceLock<HashMap<String, String>> = OnceLock::new();
    SESSION_ENV.get_or_init(load_session_environment)
}

fn load_session_environment() -> HashMap<String, String> {
    let mut command = Command::new("systemctl");
    command.arg("--user").arg("show-environment");
    let output = match spawn_and_wait(
        &mut command,
        SESSION_ENV_TIMEOUT_MS,
        "systemd user environment query",
    ) {
        Ok(output) if output.status.success() => output,
        _ => return HashMap::new(),
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn discover_hyprland_signature(hypr_dir: &std::path::Path) -> Option<String> {
    let path = find_latest_hyprland_socket(hypr_dir)?;
    let parent = path.parent()?;
    let name = parent.file_name()?;
    Some(name.to_string_lossy().to_string())
}

fn find_latest_hyprland_socket(hypr_dir: &std::path::Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;

    for entry in fs::read_dir(hypr_dir).ok()? {
        let entry = entry.ok()?;
        let socket_path = entry.path().join(".socket.sock");
        if !socket_path.exists() {
            continue;
        }

        let modified = socket_path
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH);

        match &best {
            Some((best_modified, _)) if *best_modified >= modified => {}
            _ => best = Some((modified, socket_path)),
        }
    }

    best.map(|(_, path)| path)
}

struct ClipboardBackup {
    mime_type: Option<String>,
    temp_path: Option<PathBuf>,
    x11_text: Option<Vec<u8>>,
}

impl ClipboardBackup {
    fn capture(target: &OutputTarget) -> Result<Self> {
        let x11_text = capture_x11_clipboard(target).unwrap_or(None);
        if target.is_xwayland() {
            eprintln!(
                "voice-input output: xwayland backup capture complete (has_x11={})",
                x11_text.is_some()
            );
            return Ok(Self {
                mime_type: None,
                temp_path: None,
                x11_text,
            });
        }

        let mut list_types = Command::new("wl-paste");
        list_types.arg("--list-types");
        let type_output = match spawn_and_wait(
            &mut list_types,
            CLIPBOARD_QUERY_TIMEOUT_MS,
            "wl-paste clipboard type query",
        ) {
            Ok(output) => output,
            Err(_) => {
                return Ok(Self {
                    mime_type: None,
                    temp_path: None,
                    x11_text,
                });
            }
        };

        if !type_output.status.success() {
            return Ok(Self {
                mime_type: None,
                temp_path: None,
                x11_text,
            });
        }

        let mime_type = String::from_utf8_lossy(&type_output.stdout)
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_string());

        let Some(mime_type) = mime_type else {
            return Ok(Self {
                mime_type: None,
                temp_path: None,
                x11_text,
            });
        };

        let mut paste = Command::new("wl-paste");
        paste.arg("--type").arg(&mime_type);
        let output = match spawn_and_wait(
            &mut paste,
            CLIPBOARD_QUERY_TIMEOUT_MS,
            "wl-paste clipboard content capture",
        ) {
            Ok(output) => output,
            Err(_) => {
                return Ok(Self {
                    mime_type: None,
                    temp_path: None,
                    x11_text,
                });
            }
        };

        if !output.status.success() {
            return Ok(Self {
                mime_type: None,
                temp_path: None,
                x11_text,
            });
        }

        let file = tempfile::NamedTempFile::new().context("failed to create clipboard backup")?;
        fs::write(file.path(), &output.stdout).context("failed to persist clipboard backup")?;
        let (_, path) = file
            .keep()
            .context("failed to keep clipboard backup file")?;

        Ok(Self {
            mime_type: Some(mime_type),
            temp_path: Some(path),
            x11_text,
        })
    }

    fn restore(self) -> Result<()> {
        if let Some(x11_text) = self.x11_text.as_deref() {
            eprintln!("voice-input output: restore x11 clipboard");
            restore_x11_clipboard(x11_text)?;
        }

        let (Some(mime_type), Some(temp_path)) = (self.mime_type, self.temp_path) else {
            return Ok(());
        };

        let data = fs::read(&temp_path).context("failed to read clipboard backup")?;
        let mut command = Command::new("wl-copy");
        command
            .arg("--type")
            .arg(mime_type)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            // See copy_to_wayland_clipboard: wl-copy daemonizes, so captured
            // output pipes would be held open by the clipboard provider.
            .stderr(Stdio::null());
        let output = spawn_with_input_and_wait(
            &mut command,
            &data,
            CLIPBOARD_COPY_TIMEOUT_MS,
            "wl-copy clipboard restore",
        )?;
        fs::remove_file(&temp_path).ok();

        if output.status.success() {
            Ok(())
        } else {
            bail!(
                "failed to restore clipboard contents: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        }
    }
}

struct ImeGuard {
    should_restore: bool,
}

impl ImeGuard {
    fn prepare(config: &Config) -> Result<Self> {
        if !config.ime.manage_fcitx5 || !config.ime.force_ascii_before_output {
            return Ok(Self {
                should_restore: false,
            });
        }

        let mut command = Command::new("fcitx5-remote");
        let output = match spawn_and_wait(
            &mut command,
            INPUT_METHOD_TIMEOUT_MS,
            "fcitx5-remote state query",
        ) {
            Ok(output) => output,
            Err(_) => {
                return Ok(Self {
                    should_restore: false,
                });
            }
        };

        if !output.status.success() {
            return Ok(Self {
                should_restore: false,
            });
        }

        let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let should_restore = state == "2";
        if should_restore {
            let mut command = Command::new("fcitx5-remote");
            command.arg("-c");
            let _ = spawn_and_wait(&mut command, INPUT_METHOD_TIMEOUT_MS, "fcitx5 ASCII switch");
        }

        Ok(Self { should_restore })
    }

    fn restore(&self) -> Result<()> {
        if self.should_restore {
            let mut command = Command::new("fcitx5-remote");
            command.arg("-o");
            let output = spawn_and_wait(
                &mut command,
                INPUT_METHOD_TIMEOUT_MS,
                "fcitx5 state restore",
            )?;
            if !output.status.success() {
                bail!("fcitx5-remote -o returned non-zero status");
            }
        }
        Ok(())
    }
}

fn spawn_and_wait(command: &mut Command, timeout_ms: u64, label: &str) -> Result<Output> {
    let child = command
        .spawn()
        .with_context(|| format!("failed to launch {label}"))?;
    wait_with_output_timeout(child, timeout_ms, label)
}

fn spawn_with_input_and_wait(
    command: &mut Command,
    input: &[u8],
    timeout_ms: u64,
    label: &str,
) -> Result<Output> {
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch {label}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(input)
            .with_context(|| format!("failed to write stdin for {label}"))?;
    }
    drop(child.stdin.take());
    wait_with_output_timeout(child, timeout_ms, label)
}

fn wait_with_output_timeout(mut child: Child, timeout_ms: u64, label: &str) -> Result<Output> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));

    loop {
        if let Some(_status) = child
            .try_wait()
            .with_context(|| format!("failed to poll {label}"))?
        {
            return child
                .wait_with_output()
                .with_context(|| format!("failed waiting for {label}"));
        }

        if Instant::now() >= deadline {
            child.kill().ok();
            let _ = child.wait();
            bail!("{label} timed out after {} ms", timeout_ms.max(1));
        }

        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActiveWindow, EffectiveOutputMode, OutputTarget, effective_output_mode,
        effective_output_mode_for_text, text_injection_timeout_ms,
    };
    use crate::config::{Config, OutputMode};

    #[test]
    fn xwayland_prefers_paste_when_enabled() {
        let config = Config::default();
        assert!(matches!(
            effective_output_mode(&config, &OutputTarget::XWayland { address: None }),
            EffectiveOutputMode::Paste
        ));
    }

    #[test]
    fn wayland_keeps_direct_type_mode() {
        let config = Config::default();
        assert!(matches!(
            effective_output_mode(&config, &OutputTarget::Wayland),
            EffectiveOutputMode::Type
        ));
    }

    #[test]
    fn text_injection_timeout_scales_but_remains_bounded() {
        assert_eq!(text_injection_timeout_ms(""), 1_500);
        assert_eq!(text_injection_timeout_ms(&"a".repeat(100)), 2_700);
        assert_eq!(text_injection_timeout_ms(&"a".repeat(10_000)), 5_000);
    }

    #[test]
    fn wayland_uses_paste_for_long_text() {
        let config = Config::default();
        let text = "长".repeat(121);
        assert!(matches!(
            effective_output_mode_for_text(&config, &OutputTarget::Wayland, &text),
            EffectiveOutputMode::Paste
        ));
    }

    #[test]
    fn wayland_keeps_type_for_short_text() {
        let config = Config::default();
        let text = "short dictation";
        assert!(matches!(
            effective_output_mode_for_text(&config, &OutputTarget::Wayland, text),
            EffectiveOutputMode::Type
        ));
    }

    #[test]
    fn xwayland_prefers_user_mode_when_override_disabled() {
        let mut config = Config::default();
        config.output.prefer_paste_for_xwayland = false;
        config.output.mode = OutputMode::Type;
        assert!(matches!(
            effective_output_mode(&config, &OutputTarget::XWayland { address: None }),
            EffectiveOutputMode::Type
        ));
    }

    #[test]
    fn active_window_defaults_to_non_xwayland() {
        let active: ActiveWindow =
            serde_json::from_str("{}").expect("active window json should parse");
        assert!(!active.xwayland);
        assert!(active.address.is_none());
    }
}
