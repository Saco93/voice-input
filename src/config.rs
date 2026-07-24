use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AsrProvider {
    LocalCli,
    AlibabaQwenRealtime,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AlibabaTurnMode {
    ServerVad,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlibabaRealtimeConfig {
    pub endpoint: String,
    #[serde(skip_serializing)]
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
    #[serde(skip_serializing)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HudPosition {
    BottomCenter,
    BottomLeft,
    BottomRight,
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
                timeout_ms: 5_000,
                provider_sort: String::new(),
                agent_context_enabled: false,
                agent_context_max_chars: 6_000,
            },
            hud: HudConfig {
                enabled: true,
                margin_bottom: 72,
                height: 56,
                position: HudPosition::BottomCenter,
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

impl Default for AsrProvider {
    fn default() -> Self {
        Self::LocalCli
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

impl Default for HudPosition {
    fn default() -> Self {
        Self::BottomCenter
    }
}

impl Default for AlibabaTurnMode {
    fn default() -> Self {
        Self::ServerVad
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = paths::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let raw: RawConfig = toml::from_str(&source).context("failed to parse config TOML")?;
        Ok(Self::from_raw(raw))
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = paths::config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let data = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(&path, data).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
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

        if let Some(state_file) = raw.state_file {
            config.state_file = state_file;
        }

        if let Some(hotkey) = raw.hotkey {
            if let Some(accelerator) = hotkey.accelerator {
                config.hotkey.accelerator = accelerator;
            }
            if let Some(mode) = hotkey.mode {
                config.hotkey.mode = if mode == "toggle" {
                    HotkeyMode::Toggle
                } else {
                    HotkeyMode::Hold
                };
            }
        }

        if let Some(audio) = raw.audio {
            if let Some(device) = audio.device {
                config.audio.device = device;
            }
            if let Some(sample_rate) = audio.sample_rate {
                config.audio.sample_rate = sample_rate;
            }
            if let Some(max_duration_secs) = audio.max_duration_secs {
                config.audio.max_duration_secs = max_duration_secs;
            }
            if let Some(partial_interval_ms) = audio.partial_interval_ms {
                config.audio.partial_interval_ms = partial_interval_ms;
            }
            if let Some(pre_roll_enabled) = audio.pre_roll_enabled {
                config.audio.pre_roll_enabled = pre_roll_enabled;
            }
            if let Some(pre_roll_ms) = audio.pre_roll_ms {
                config.audio.pre_roll_ms = pre_roll_ms;
            }
        }

        if let Some(asr) = raw.asr {
            config.asr = asr;
        } else {
            if raw.whisper.is_some() {
                config.asr.engine = "whisper".into();
            }
            if let Some(engine) = raw.engine {
                config.asr.engine = engine;
            }
            if let Some(whisper) = raw.whisper {
                if let Some(model) = whisper.model {
                    config.asr.model = model;
                }
                if let Some(language) = whisper.language {
                    config.asr.language = Language::from_legacy_code(&language);
                }
            }
        }

        if let Some(output) = raw.output {
            if let Some(mode) = output.mode {
                config.output.mode = mode;
            }
            if let Some(fallback) = output.fallback_to_clipboard {
                config.output.fallback_to_clipboard = fallback;
            }
            if let Some(type_delay_ms) = output.type_delay_ms {
                config.output.type_delay_ms = type_delay_ms;
            }
            if let Some(pre_type_delay_ms) = output.pre_type_delay_ms {
                config.output.pre_type_delay_ms = pre_type_delay_ms;
            }
            if let Some(paste_keys) = output.paste_keys {
                config.output.paste_keys = paste_keys;
            }
            if let Some(prefer_paste_for_xwayland) = output.prefer_paste_for_xwayland {
                config.output.prefer_paste_for_xwayland = prefer_paste_for_xwayland;
            }
            if let Some(xwayland_paste_keys) = output.xwayland_paste_keys {
                config.output.xwayland_paste_keys = xwayland_paste_keys;
            }
        }

        if let Some(ime) = raw.ime {
            config.ime = ime;
        }

        if let Some(llm) = raw.llm {
            config.llm = llm;
        }

        if let Some(hud) = raw.hud {
            config.hud = hud;
        }

        config
    }
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
    use super::{AlibabaTurnMode, AsrProvider, Config, Language, RawConfig};

    #[test]
    fn legacy_asr_config_maps_to_local_cli_defaults() {
        let raw: RawConfig = toml::from_str(
            r#"
engine = "whisper"

[whisper]
model = "base.en"
language = "en"
"#,
        )
        .expect("legacy config parses");

        let config = Config::from_raw(raw);
        assert_eq!(config.asr.provider, AsrProvider::LocalCli);
        assert_eq!(config.asr.engine, "whisper");
        assert_eq!(config.asr.model, "base.en");
        assert_eq!(config.asr.language, Language::English);
        assert_eq!(
            config.asr.alibaba.model,
            "qwen3-asr-flash-realtime-2026-02-10"
        );
    }

    #[test]
    fn nested_alibaba_config_overrides_remote_fields() {
        let raw: RawConfig = toml::from_str(
            r#"
[asr]
provider = "alibaba-qwen-realtime"
language = "simplified-chinese"
fallback_to_local = false

[asr.alibaba]
endpoint = "wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime"
api_key = "test-placeholder"
model = "qwen3-asr-flash-realtime-2026-02-10"
turn_mode = "manual"
vad_threshold = -0.1
silence_duration_ms = 900
final_pass_enabled = true
final_pass_base_url = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
final_pass_model = "qwen3-asr-flash-2026-02-10"
final_pass_timeout_ms = 12000
final_pass_enable_itn = true
"#,
        )
        .expect("remote config parses");

        let config = Config::from_raw(raw);
        assert_eq!(config.asr.provider, AsrProvider::AlibabaQwenRealtime);
        assert!(!config.asr.fallback_to_local);
        assert_eq!(
            config.asr.alibaba.endpoint,
            "wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime"
        );
        assert_eq!(config.asr.alibaba.api_key, "test-placeholder");
        assert_eq!(config.asr.alibaba.turn_mode, AlibabaTurnMode::Manual);
        assert_eq!(config.asr.alibaba.vad_threshold, -0.1);
        assert_eq!(config.asr.alibaba.silence_duration_ms, 900);
        assert!(config.asr.alibaba.final_pass_enabled);
        assert_eq!(
            config.asr.alibaba.final_pass_base_url,
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(
            config.asr.alibaba.final_pass_model,
            "qwen3-asr-flash-2026-02-10"
        );
        assert_eq!(config.asr.alibaba.final_pass_timeout_ms, 12_000);
        assert!(config.asr.alibaba.final_pass_enable_itn);
    }
}
