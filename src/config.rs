use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use url::Url;

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub state_file: String,
    pub hotkey: HotkeyConfig,
    pub audio: AudioConfig,
    pub asr: AsrConfig,
    pub output: OutputConfig,
    pub ime: ImeConfig,
    pub llm: LlmConfig,
    pub hud: HudConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub accelerator: String,
    pub mode: HotkeyMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HotkeyMode {
    Hold,
    Toggle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    pub device: String,
    pub sample_rate: u32,
    pub max_duration_secs: u64,
    pub partial_interval_ms: u64,
    pub pre_roll_enabled: bool,
    pub pre_roll_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AsrConfig {
    pub provider: AsrProvider,
    pub backend_command: String,
    pub engine: String,
    pub model: String,
    pub language: Language,
    pub connect_timeout_ms: u64,
    pub finalize_timeout_ms: u64,
    pub fallback_to_local: bool,
    pub alibaba: AlibabaRealtimeConfig,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AsrProvider {
    #[default]
    LocalCli,
    AlibabaQwenRealtime,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AlibabaTurnMode {
    #[default]
    ServerVad,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlibabaRealtimeConfig {
    pub endpoint: String,
    #[serde(default, skip_serializing)]
    pub api_key: String,
    pub model: String,
    pub turn_mode: AlibabaTurnMode,
    pub vad_threshold: f32,
    pub silence_duration_ms: u32,
    pub final_pass_enabled: bool,
    pub final_pass_base_url: String,
    pub final_pass_model: String,
    pub final_pass_timeout_ms: u64,
    pub final_pass_enable_itn: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    English,
    SimplifiedChinese,
    TraditionalChinese,
    Japanese,
    Korean,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub mode: OutputMode,
    pub fallback_to_clipboard: bool,
    pub type_delay_ms: u64,
    pub pre_type_delay_ms: u64,
    pub paste_keys: String,
    pub prefer_paste_for_xwayland: bool,
    pub xwayland_paste_keys: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMode {
    Type,
    Clipboard,
    Paste,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImeConfig {
    pub manage_fcitx5: bool,
    pub force_ascii_before_output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    pub api_base_url: String,
    #[serde(default, skip_serializing)]
    pub api_key: String,
    pub model: String,
    pub timeout_ms: u64,
    pub provider_sort: String,
    pub agent_context_enabled: bool,
    pub agent_context_max_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HudConfig {
    pub enabled: bool,
    pub margin_bottom: i32,
    pub height: i32,
    pub position: HudPosition,
    pub offset_x: i32,
    pub offset_y: i32,
    pub nudge_step: i32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum HudPosition {
    #[default]
    #[serde(rename = "bottom-center")]
    Center,
    #[serde(rename = "bottom-left")]
    Left,
    #[serde(rename = "bottom-right")]
    Right,
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    state_file: Option<String>,
    engine: Option<String>,
    hotkey: Option<RawHotkeyConfig>,
    audio: Option<RawAudioConfig>,
    whisper: Option<RawWhisperConfig>,
    asr: Option<AsrConfig>,
    output: Option<RawOutputConfig>,
    ime: Option<ImeConfig>,
    llm: Option<LlmConfig>,
    hud: Option<HudConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct RawHotkeyConfig {
    accelerator: Option<String>,
    mode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAudioConfig {
    device: Option<String>,
    sample_rate: Option<u32>,
    max_duration_secs: Option<u64>,
    partial_interval_ms: Option<u64>,
    pre_roll_enabled: Option<bool>,
    pre_roll_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawWhisperConfig {
    model: Option<String>,
    language: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawOutputConfig {
    mode: Option<OutputMode>,
    fallback_to_clipboard: Option<bool>,
    type_delay_ms: Option<u64>,
    pre_type_delay_ms: Option<u64>,
    paste_keys: Option<String>,
    prefer_paste_for_xwayland: Option<bool>,
    xwayland_paste_keys: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub revision: String,
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

#[derive(Debug)]
pub struct RevisionConflict;

impl fmt::Display for RevisionConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("configuration changed since it was loaded")
    }
}

impl Error for RevisionConflict {}

#[derive(Debug, Clone)]
pub struct ValidationError {
    pub fields: BTreeMap<String, String>,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("configuration validation failed")
    }
}

impl Error for ValidationError {}

impl Default for Config {
    fn default() -> Self {
        Self {
            state_file: "auto".into(),
            hotkey: HotkeyConfig {
                accelerator: "SUPER CTRL, X".into(),
                mode: HotkeyMode::Hold,
            },
            audio: AudioConfig {
                device: "default".into(),
                sample_rate: 16_000,
                max_duration_secs: 90,
                partial_interval_ms: 1_500,
                pre_roll_enabled: false,
                pre_roll_ms: 500,
            },
            asr: AsrConfig {
                provider: AsrProvider::LocalCli,
                backend_command: "/usr/bin/voxtype".into(),
                engine: "sensevoice".into(),
                model: String::new(),
                language: Language::SimplifiedChinese,
                connect_timeout_ms: 5_000,
                finalize_timeout_ms: 8_000,
                fallback_to_local: true,
                alibaba: AlibabaRealtimeConfig {
                    endpoint: "wss://dashscope.aliyuncs.com/api-ws/v1/realtime".into(),
                    api_key: String::new(),
                    model: "qwen3-asr-flash-realtime-2026-02-10".into(),
                    turn_mode: AlibabaTurnMode::ServerVad,
                    vad_threshold: 0.2,
                    silence_duration_ms: 400,
                    final_pass_enabled: false,
                    final_pass_base_url: String::new(),
                    final_pass_model: "qwen3-asr-flash-2026-02-10".into(),
                    final_pass_timeout_ms: 20_000,
                    final_pass_enable_itn: false,
                },
            },
            output: OutputConfig {
                mode: OutputMode::Type,
                fallback_to_clipboard: true,
                type_delay_ms: 0,
                pre_type_delay_ms: 140,
                paste_keys: "shift+Insert".into(),
                prefer_paste_for_xwayland: true,
                xwayland_paste_keys: "shift+Insert".into(),
            },
            ime: ImeConfig {
                manage_fcitx5: true,
                force_ascii_before_output: true,
            },
            llm: LlmConfig {
                enabled: false,
                api_base_url: "https://api.openai.com/v1".into(),
                api_key: String::new(),
                model: String::new(),
                timeout_ms: 15_000,
                provider_sort: String::new(),
                agent_context_enabled: false,
                agent_context_max_chars: 6_000,
            },
            hud: HudConfig {
                enabled: true,
                margin_bottom: 72,
                height: 56,
                position: HudPosition::Center,
                offset_x: 0,
                offset_y: 0,
                nudge_step: 24,
            },
        }
    }
}

impl Default for AsrConfig {
    fn default() -> Self {
        Config::default().asr
    }
}
impl Default for AlibabaRealtimeConfig {
    fn default() -> Self {
        Config::default().asr.alibaba
    }
}
impl Default for LlmConfig {
    fn default() -> Self {
        Config::default().llm
    }
}
impl Default for HudConfig {
    fn default() -> Self {
        Config::default().hud
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        Ok(ConfigStore::new(paths::config_path()?).load()?.config)
    }

    pub fn save(&self) -> Result<PathBuf> {
        let store = ConfigStore::new(paths::config_path()?);
        store.save(self, None)?;
        Ok(store.path().to_path_buf())
    }

    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        let mut fields = BTreeMap::new();
        validate_text(&mut fields, "state_file", &self.state_file, 4_096, true);
        validate_text(
            &mut fields,
            "hotkey.accelerator",
            &self.hotkey.accelerator,
            256,
            false,
        );
        validate_text(
            &mut fields,
            "audio.device",
            &self.audio.device,
            1_024,
            false,
        );
        range(
            &mut fields,
            "audio.sample_rate",
            self.audio.sample_rate,
            8_000,
            192_000,
        );
        range(
            &mut fields,
            "audio.max_duration_secs",
            self.audio.max_duration_secs,
            1,
            3_600,
        );
        range(
            &mut fields,
            "audio.partial_interval_ms",
            self.audio.partial_interval_ms,
            50,
            60_000,
        );
        range(
            &mut fields,
            "audio.pre_roll_ms",
            self.audio.pre_roll_ms,
            0,
            10_000,
        );
        if self.audio.pre_roll_ms > self.audio.max_duration_secs.saturating_mul(1_000) {
            fields.insert(
                "audio.pre_roll_ms".into(),
                "must not exceed maximum recording duration".into(),
            );
        }

        validate_text(
            &mut fields,
            "asr.backend_command",
            &self.asr.backend_command,
            4_096,
            self.asr.provider != AsrProvider::LocalCli && !self.asr.fallback_to_local,
        );
        // An empty engine intentionally delegates engine selection to the local backend.
        validate_text(&mut fields, "asr.engine", &self.asr.engine, 256, true);
        validate_text(&mut fields, "asr.model", &self.asr.model, 512, true);
        range(
            &mut fields,
            "asr.connect_timeout_ms",
            self.asr.connect_timeout_ms,
            100,
            120_000,
        );
        range(
            &mut fields,
            "asr.finalize_timeout_ms",
            self.asr.finalize_timeout_ms,
            100,
            120_000,
        );
        validate_url(
            &mut fields,
            "asr.alibaba.endpoint",
            &self.asr.alibaba.endpoint,
            &["ws", "wss"],
            self.asr.provider != AsrProvider::AlibabaQwenRealtime,
        );
        validate_text(
            &mut fields,
            "asr.alibaba.model",
            &self.asr.alibaba.model,
            512,
            self.asr.provider != AsrProvider::AlibabaQwenRealtime,
        );
        if !self.asr.alibaba.vad_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.asr.alibaba.vad_threshold)
        {
            fields.insert(
                "asr.alibaba.vad_threshold".into(),
                "must be finite and between 0 and 1".into(),
            );
        }
        range(
            &mut fields,
            "asr.alibaba.silence_duration_ms",
            self.asr.alibaba.silence_duration_ms,
            50,
            10_000,
        );
        // Empty means derive Alibaba's compatible HTTP endpoint from the realtime host.
        validate_url(
            &mut fields,
            "asr.alibaba.final_pass_base_url",
            &self.asr.alibaba.final_pass_base_url,
            &["http", "https"],
            true,
        );
        validate_text(
            &mut fields,
            "asr.alibaba.final_pass_model",
            &self.asr.alibaba.final_pass_model,
            512,
            !self.asr.alibaba.final_pass_enabled,
        );
        range(
            &mut fields,
            "asr.alibaba.final_pass_timeout_ms",
            self.asr.alibaba.final_pass_timeout_ms,
            100,
            120_000,
        );

        range(
            &mut fields,
            "output.type_delay_ms",
            self.output.type_delay_ms,
            0,
            10_000,
        );
        range(
            &mut fields,
            "output.pre_type_delay_ms",
            self.output.pre_type_delay_ms,
            0,
            10_000,
        );
        // Type mode also needs a paste chord for long text and clipboard fallback.
        validate_text(
            &mut fields,
            "output.paste_keys",
            &self.output.paste_keys,
            256,
            matches!(self.output.mode, OutputMode::Clipboard),
        );
        // Empty intentionally falls back to the Wayland paste chord.
        validate_text(
            &mut fields,
            "output.xwayland_paste_keys",
            &self.output.xwayland_paste_keys,
            256,
            true,
        );

        validate_url(
            &mut fields,
            "llm.api_base_url",
            &self.llm.api_base_url,
            &["http", "https"],
            !self.llm.enabled,
        );
        validate_text(
            &mut fields,
            "llm.model",
            &self.llm.model,
            512,
            !self.llm.enabled,
        );
        range(
            &mut fields,
            "llm.timeout_ms",
            self.llm.timeout_ms,
            100,
            120_000,
        );
        validate_text(
            &mut fields,
            "llm.provider_sort",
            &self.llm.provider_sort,
            128,
            true,
        );
        range(
            &mut fields,
            "llm.agent_context_max_chars",
            self.llm.agent_context_max_chars,
            128,
            100_000,
        );

        range(
            &mut fields,
            "hud.margin_bottom",
            self.hud.margin_bottom,
            -10_000,
            10_000,
        );
        range(&mut fields, "hud.height", self.hud.height, 16, 1_000);
        range(
            &mut fields,
            "hud.offset_x",
            self.hud.offset_x,
            -10_000,
            10_000,
        );
        range(
            &mut fields,
            "hud.offset_y",
            self.hud.offset_y,
            -10_000,
            10_000,
        );
        range(&mut fields, "hud.nudge_step", self.hud.nudge_step, 1, 1_000);

        if fields.is_empty() {
            Ok(())
        } else {
            Err(ValidationError { fields })
        }
    }

    pub fn state_path(&self) -> Result<Option<PathBuf>> {
        match self.state_file.as_str() {
            "disabled" => Ok(None),
            "auto" => Ok(Some(paths::runtime_dir()?.join("state.json"))),
            custom => Ok(Some(PathBuf::from(custom))),
        }
    }

    fn from_raw(raw: RawConfig) -> Self {
        let mut config = Self::default();
        if let Some(value) = raw.state_file {
            config.state_file = value;
        }
        if let Some(hotkey) = raw.hotkey {
            if let Some(value) = hotkey.accelerator {
                config.hotkey.accelerator = value;
            }
            if let Some(value) = hotkey.mode {
                config.hotkey.mode = if value == "toggle" {
                    HotkeyMode::Toggle
                } else {
                    HotkeyMode::Hold
                };
            }
        }
        if let Some(audio) = raw.audio {
            if let Some(value) = audio.device {
                config.audio.device = value;
            }
            if let Some(value) = audio.sample_rate {
                config.audio.sample_rate = value;
            }
            if let Some(value) = audio.max_duration_secs {
                config.audio.max_duration_secs = value;
            }
            if let Some(value) = audio.partial_interval_ms {
                config.audio.partial_interval_ms = value;
            }
            if let Some(value) = audio.pre_roll_enabled {
                config.audio.pre_roll_enabled = value;
            }
            if let Some(value) = audio.pre_roll_ms {
                config.audio.pre_roll_ms = value;
            }
        }
        if let Some(asr) = raw.asr {
            config.asr = asr;
        } else {
            if raw.whisper.is_some() {
                config.asr.engine = "whisper".into();
            }
            if let Some(value) = raw.engine {
                config.asr.engine = value;
            }
            if let Some(whisper) = raw.whisper {
                if let Some(value) = whisper.model {
                    config.asr.model = value;
                }
                if let Some(value) = whisper.language {
                    config.asr.language = Language::from_legacy_code(&value);
                }
            }
        }
        if let Some(output) = raw.output {
            if let Some(value) = output.mode {
                config.output.mode = value;
            }
            if let Some(value) = output.fallback_to_clipboard {
                config.output.fallback_to_clipboard = value;
            }
            if let Some(value) = output.type_delay_ms {
                config.output.type_delay_ms = value;
            }
            if let Some(value) = output.pre_type_delay_ms {
                config.output.pre_type_delay_ms = value;
            }
            if let Some(value) = output.paste_keys {
                config.output.paste_keys = value;
            }
            if let Some(value) = output.prefer_paste_for_xwayland {
                config.output.prefer_paste_for_xwayland = value;
            }
            if let Some(value) = output.xwayland_paste_keys {
                config.output.xwayland_paste_keys = value;
            }
        }
        if let Some(value) = raw.ime {
            config.ime = value;
        }
        if let Some(value) = raw.llm {
            config.llm = value;
        }
        if let Some(value) = raw.hud {
            config.hud = value;
        }
        config
    }
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<LoadedConfig> {
        if !self.path.exists() {
            return Ok(LoadedConfig {
                config: Config::default(),
                revision: missing_revision(),
            });
        }
        let source = fs::read(&self.path)
            .with_context(|| format!("failed to read config at {}", self.path.display()))?;
        let raw: RawConfig =
            toml::from_str(std::str::from_utf8(&source).context("config TOML is not valid UTF-8")?)
                .context("failed to parse config TOML")?;
        Ok(LoadedConfig {
            config: Config::from_raw(raw),
            revision: revision(&source),
        })
    }

    pub fn save(&self, config: &Config, expected_revision: Option<&str>) -> Result<String> {
        config.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config path has no parent directory"))?;
        make_private_directory(parent)?;
        let lock = ConfigLock::acquire(&self.path)?;
        let actual_revision = self.current_revision()?;
        if let Some(expected) = expected_revision
            && expected != actual_revision
        {
            return Err(RevisionConflict.into());
        }

        let data = toml::to_string_pretty(config).context("failed to serialize config")?;
        let mut temporary = NamedTempFile::new_in(parent)
            .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
        set_private_file(temporary.as_file())?;
        temporary
            .write_all(data.as_bytes())
            .context("failed to write temporary config")?;
        temporary
            .as_file_mut()
            .sync_all()
            .context("failed to sync temporary config")?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to replace {}", self.path.display()))?;
        set_private_path(&self.path)?;
        drop(lock);
        Ok(revision(data.as_bytes()))
    }

    fn current_revision(&self) -> Result<String> {
        match fs::read(&self.path) {
            Ok(source) => Ok(revision(&source)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(missing_revision()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read config at {}", self.path.display())),
        }
    }
}

struct ConfigLock(fs::File);

impl ConfigLock {
    fn acquire(config_path: &Path) -> Result<Self> {
        use std::fs::OpenOptions;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;

        let name = config_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("config.toml");
        let lock_path = config_path.with_file_name(format!(".{name}.lock"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(&lock_path)
            .with_context(|| format!("failed to open {}", lock_path.display()))?;
        set_private_file(&file)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: flock only borrows this valid file descriptor for the call.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error()).context("failed to lock config");
            }
        }
        Ok(Self(file))
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the descriptor remains valid until after this Drop implementation.
            let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

fn revision(source: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in source {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn missing_revision() -> String {
    "missing:fnv1a64:cbf29ce484222325".into()
}

fn validate_text(
    fields: &mut BTreeMap<String, String>,
    field: &str,
    value: &str,
    max_len: usize,
    allow_empty: bool,
) {
    if !allow_empty && value.trim().is_empty() {
        fields.insert(field.into(), "is required".into());
    } else if value.len() > max_len {
        fields.insert(field.into(), format!("must be at most {max_len} bytes"));
    } else if value.chars().any(char::is_control) {
        fields.insert(field.into(), "must not contain control characters".into());
    }
}

fn validate_url(
    fields: &mut BTreeMap<String, String>,
    field: &str,
    value: &str,
    schemes: &[&str],
    allow_empty: bool,
) {
    validate_text(fields, field, value, 2_048, allow_empty);
    if value.is_empty() && allow_empty {
        return;
    }
    let Ok(url) = Url::parse(value) else {
        fields.insert(field.into(), "must be a valid URL".into());
        return;
    };
    if !schemes.contains(&url.scheme()) {
        fields.insert(field.into(), format!("must use {}", schemes.join(" or ")));
    } else if !url.username().is_empty() || url.password().is_some() {
        fields.insert(field.into(), "must not contain embedded credentials".into());
    } else if url.host_str().is_none() {
        fields.insert(field.into(), "must include a host".into());
    }
}

fn range<T>(fields: &mut BTreeMap<String, String>, field: &str, value: T, min: T, max: T)
where
    T: PartialOrd + fmt::Display,
{
    if value < min || value > max {
        fields.insert(field.into(), format!("must be between {min} and {max}"));
    }
}

fn make_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(())
}

fn set_private_file(file: &fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .context("failed to secure config file")?;
    }
    Ok(())
}

fn set_private_path(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(())
}

impl AsrConfig {
    pub fn active_engine_label(&self) -> String {
        match self.provider {
            AsrProvider::LocalCli => self.engine.clone(),
            AsrProvider::AlibabaQwenRealtime => {
                if self.alibaba.final_pass_enabled {
                    "qwen-realtime + qwen-flash".into()
                } else {
                    "qwen-realtime".into()
                }
            }
        }
    }

    pub fn active_model_label(&self) -> String {
        match self.provider {
            AsrProvider::LocalCli => self.model.clone(),
            AsrProvider::AlibabaQwenRealtime => {
                if self.alibaba.final_pass_enabled {
                    format!(
                        "{} -> {}",
                        self.alibaba.model, self.alibaba.final_pass_model
                    )
                } else {
                    self.alibaba.model.clone()
                }
            }
        }
    }
}

impl Language {
    pub fn asr_code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::SimplifiedChinese | Language::TraditionalChinese => "zh",
            Language::Japanese => "ja",
            Language::Korean => "ko",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::SimplifiedChinese => "Simplified Chinese",
            Language::TraditionalChinese => "Traditional Chinese",
            Language::Japanese => "Japanese",
            Language::Korean => "Korean",
        }
    }
    pub fn opencc_profile(self) -> Option<&'static str> {
        match self {
            Language::SimplifiedChinese => Some("t2s"),
            Language::TraditionalChinese => Some("s2t"),
            _ => None,
        }
    }
    fn from_legacy_code(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "en" => Language::English,
            "ja" => Language::Japanese,
            "ko" => Language::Korean,
            "zh-tw" | "zh_hant" | "zh-hant" => Language::TraditionalChinese,
            _ => Language::SimplifiedChinese,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::{Config, ConfigStore, HudPosition, RevisionConflict};

    #[test]
    fn validation_reports_field_map() {
        let mut config = Config::default();
        config.audio.sample_rate = 1;
        config.asr.alibaba.vad_threshold = f32::NAN;
        config.llm.enabled = true;
        config.llm.api_base_url = "https://user:secret@example.com/v1".into();
        config.llm.model.clear();
        let error = config.validate().expect_err("invalid config");
        assert!(error.fields.contains_key("audio.sample_rate"));
        assert!(error.fields.contains_key("asr.alibaba.vad_threshold"));
        assert!(error.fields.contains_key("llm.api_base_url"));
        assert!(error.fields.contains_key("llm.model"));
    }

    #[test]
    fn optional_derived_endpoints_and_backend_defaults_validate() {
        let mut config = Config::default();
        config.asr.provider = super::AsrProvider::AlibabaQwenRealtime;
        config.asr.alibaba.final_pass_enabled = true;
        config.asr.alibaba.final_pass_base_url.clear();
        config.asr.engine.clear();
        config.output.xwayland_paste_keys.clear();
        config
            .validate()
            .expect("derived final-pass URL and documented empty fallbacks are valid");

        config.asr.backend_command.clear();
        let error = config
            .validate()
            .expect_err("local fallback still requires its backend executable");
        assert!(error.fields.contains_key("asr.backend_command"));
    }

    #[test]
    fn store_is_atomic_private_and_detects_revision_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("private");
        let path = directory.join("config.toml");
        let store = ConfigStore::new(&path);
        let loaded = store.load().unwrap();
        let first_revision = store
            .save(&loaded.config, Some(&loaded.revision))
            .expect("initial save");
        assert_eq!(store.load().unwrap().revision, first_revision);
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        fs::write(&path, "state_file = 'disabled'\n").unwrap();
        let error = store
            .save(&Config::default(), Some(&first_revision))
            .expect_err("stale revision must fail");
        assert!(error.downcast_ref::<RevisionConflict>().is_some());
    }

    #[test]
    fn malformed_toml_is_not_replaced_by_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        fs::write(&path, "[broken").unwrap();
        assert!(ConfigStore::new(path).load().is_err());
    }

    #[test]
    fn hud_position_serialization_preserves_config_values() {
        for (position, value) in [
            (HudPosition::Center, "bottom-center"),
            (HudPosition::Left, "bottom-left"),
            (HudPosition::Right, "bottom-right"),
        ] {
            let mut config = Config::default();
            config.hud.position = position;
            let serialized = toml::to_string(&config).unwrap();
            assert!(serialized.contains(format!("position = \"{value}\"").as_str()));
            assert_eq!(
                toml::from_str::<Config>(&serialized).unwrap().hud.position,
                position
            );
        }
    }

    #[test]
    fn serialization_omits_legacy_secrets() {
        let mut config = Config::default();
        config.asr.alibaba.api_key = "alibaba-secret".into();
        config.llm.api_key = "llm-secret".into();
        let toml = toml::to_string(&config).unwrap();
        let json = serde_json::to_string(&config).unwrap();
        assert!(!toml.contains("secret"));
        assert!(!toml.contains("api_key"));
        assert!(!json.contains("secret"));
        assert!(!json.contains("api_key"));
    }
}
