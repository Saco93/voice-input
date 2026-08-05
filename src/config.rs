use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use url::{Host, Url};

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
    pub alibaba_audio3: AlibabaAudio3Config,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AsrProvider {
    #[default]
    LocalCli,
    AlibabaQwenRealtime,
    AlibabaQwenAudio3,
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

#[derive(Debug, Clone, Serialize)]
pub struct AlibabaAudio3Config {
    pub experimental_enabled: bool,
    pub endpoint: String,
    #[serde(default, skip_serializing)]
    pub api_key: String,
    pub model: String,
    pub language_hints_enabled: bool,
    pub heartbeat_enabled: bool,
    pub recognition_preset: Audio3RecognitionPreset,
    pub max_sentence_silence_ms: u32,
    pub semantic_punctuation_enabled: bool,
    pub multi_threshold_mode_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_noise_threshold: Option<f32>,
    pub vocabulary: Vec<Audio3VocabularyTerm>,
    pub native_endpoint: String,
    pub native_model: String,
    pub native_final_pass_mode: NativeFinalPassMode,
    pub native_timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Audio3RecognitionPreset {
    #[default]
    Standard,
    LowLatencyDictation,
    LongForm,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveAudio3RecognitionControls {
    pub max_sentence_silence_ms: u32,
    pub semantic_punctuation_enabled: bool,
    pub multi_threshold_mode_enabled: bool,
    pub speech_noise_threshold: Option<f32>,
}

impl AlibabaAudio3Config {
    pub fn effective_recognition_controls(&self) -> EffectiveAudio3RecognitionControls {
        match self.recognition_preset {
            Audio3RecognitionPreset::Standard => EffectiveAudio3RecognitionControls {
                max_sentence_silence_ms: 800,
                semantic_punctuation_enabled: false,
                multi_threshold_mode_enabled: false,
                speech_noise_threshold: None,
            },
            Audio3RecognitionPreset::LowLatencyDictation => EffectiveAudio3RecognitionControls {
                max_sentence_silence_ms: 400,
                semantic_punctuation_enabled: false,
                multi_threshold_mode_enabled: true,
                speech_noise_threshold: None,
            },
            Audio3RecognitionPreset::LongForm => EffectiveAudio3RecognitionControls {
                max_sentence_silence_ms: 1_300,
                semantic_punctuation_enabled: true,
                multi_threshold_mode_enabled: false,
                speech_noise_threshold: None,
            },
            Audio3RecognitionPreset::Custom => EffectiveAudio3RecognitionControls {
                max_sentence_silence_ms: self.max_sentence_silence_ms,
                semantic_punctuation_enabled: self.semantic_punctuation_enabled,
                multi_threshold_mode_enabled: self.multi_threshold_mode_enabled,
                speech_noise_threshold: self.speech_noise_threshold,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NativeFinalPassMode {
    #[default]
    StreamingOnly,
    Adaptive,
    Always,
}

impl NativeFinalPassMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StreamingOnly => "streaming-only",
            Self::Adaptive => "adaptive",
            Self::Always => "always",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawAlibabaAudio3Config {
    experimental_enabled: bool,
    endpoint: String,
    api_key: String,
    model: String,
    language_hints_enabled: bool,
    heartbeat_enabled: bool,
    recognition_preset: Option<Audio3RecognitionPreset>,
    max_sentence_silence_ms: u32,
    semantic_punctuation_enabled: bool,
    multi_threshold_mode_enabled: Option<bool>,
    speech_noise_threshold: Option<f32>,
    vocabulary: Vec<Audio3VocabularyTerm>,
    native_endpoint: String,
    native_model: String,
    native_final_pass_mode: Option<NativeFinalPassMode>,
    native_final_pass_enabled: Option<bool>,
    native_timeout_ms: u64,
}

impl Default for RawAlibabaAudio3Config {
    fn default() -> Self {
        let defaults = AlibabaAudio3Config::default();
        Self {
            experimental_enabled: defaults.experimental_enabled,
            endpoint: defaults.endpoint,
            api_key: defaults.api_key,
            model: defaults.model,
            language_hints_enabled: defaults.language_hints_enabled,
            heartbeat_enabled: defaults.heartbeat_enabled,
            recognition_preset: None,
            max_sentence_silence_ms: defaults.max_sentence_silence_ms,
            semantic_punctuation_enabled: defaults.semantic_punctuation_enabled,
            multi_threshold_mode_enabled: None,
            speech_noise_threshold: None,
            vocabulary: defaults.vocabulary,
            native_endpoint: defaults.native_endpoint,
            native_model: defaults.native_model,
            native_final_pass_mode: None,
            native_final_pass_enabled: None,
            native_timeout_ms: defaults.native_timeout_ms,
        }
    }
}

impl<'de> Deserialize<'de> for AlibabaAudio3Config {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawAlibabaAudio3Config::deserialize(deserializer)?;
        let legacy_mode = raw.native_final_pass_enabled.map(|enabled| {
            if enabled {
                NativeFinalPassMode::Always
            } else {
                NativeFinalPassMode::StreamingOnly
            }
        });
        let native_final_pass_mode = match (raw.native_final_pass_mode, legacy_mode) {
            (Some(mode), Some(legacy)) if mode != legacy => {
                return Err(serde::de::Error::custom(
                    "ambiguous Audio3 native final-pass configuration: mode conflicts with legacy setting",
                ));
            }
            (Some(mode), _) => mode,
            (None, Some(mode)) => mode,
            (None, None) => NativeFinalPassMode::StreamingOnly,
        };
        let multi_threshold_mode_enabled = raw.multi_threshold_mode_enabled.unwrap_or(false);
        let recognition_preset = raw.recognition_preset.unwrap_or_else(|| {
            if raw.max_sentence_silence_ms == 800
                && !raw.semantic_punctuation_enabled
                && !multi_threshold_mode_enabled
                && raw.speech_noise_threshold.is_none()
            {
                Audio3RecognitionPreset::Standard
            } else {
                Audio3RecognitionPreset::Custom
            }
        });
        Ok(Self {
            experimental_enabled: raw.experimental_enabled,
            endpoint: raw.endpoint,
            api_key: raw.api_key,
            model: raw.model,
            language_hints_enabled: raw.language_hints_enabled,
            heartbeat_enabled: raw.heartbeat_enabled,
            recognition_preset,
            max_sentence_silence_ms: raw.max_sentence_silence_ms,
            semantic_punctuation_enabled: raw.semantic_punctuation_enabled,
            multi_threshold_mode_enabled,
            speech_noise_threshold: raw.speech_noise_threshold,
            vocabulary: raw.vocabulary,
            native_endpoint: raw.native_endpoint,
            native_model: raw.native_model,
            native_final_pass_mode,
            native_timeout_ms: raw.native_timeout_ms,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Audio3VocabularyTerm {
    pub term: String,
    pub weight: i64,
}

pub const MAX_AUDIO3_VOCABULARY_TERMS: usize = 2_000;
pub const MAX_AUDIO3_VOCABULARY_BYTES: usize = 256 * 1024;

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
                accelerator: ", F9".into(),
                mode: HotkeyMode::Toggle,
            },
            audio: AudioConfig {
                device: "default".into(),
                sample_rate: 16_000,
                max_duration_secs: 300,
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
                alibaba_audio3: AlibabaAudio3Config {
                    experimental_enabled: false,
                    endpoint: "wss://dashscope.aliyuncs.com/api-ws/v1/inference".into(),
                    api_key: String::new(),
                    model: "qwen-audio-3.0-asr-flash-streaming".into(),
                    language_hints_enabled: false,
                    heartbeat_enabled: false,
                    recognition_preset: Audio3RecognitionPreset::Standard,
                    max_sentence_silence_ms: 800,
                    semantic_punctuation_enabled: false,
                    multi_threshold_mode_enabled: false,
                    speech_noise_threshold: None,
                    vocabulary: Vec::new(),
                    native_endpoint: "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation".into(),
                    native_model: "qwen-audio-3.0-asr-flash".into(),
                    native_final_pass_mode: NativeFinalPassMode::StreamingOnly,
                    native_timeout_ms: 20_000,
                },
            },
            output: OutputConfig {
                mode: OutputMode::Paste,
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
impl Default for AlibabaAudio3Config {
    fn default() -> Self {
        Config::default().asr.alibaba_audio3
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

        if self
            .asr
            .alibaba_audio3
            .speech_noise_threshold
            .is_some_and(|threshold| !threshold.is_finite())
        {
            fields.insert(
                "asr.alibaba_audio3.speech_noise_threshold".into(),
                "must be finite".into(),
            );
        }

        if self.asr.provider == AsrProvider::AlibabaQwenAudio3 {
            if !self.asr.alibaba_audio3.experimental_enabled {
                fields.insert(
                    "asr.alibaba_audio3.experimental_enabled".into(),
                    "must be true when the experimental provider is selected".into(),
                );
            }
            validate_url(
                &mut fields,
                "asr.alibaba_audio3.endpoint",
                &self.asr.alibaba_audio3.endpoint,
                &["ws", "wss"],
                false,
            );
            validate_text(
                &mut fields,
                "asr.alibaba_audio3.model",
                &self.asr.alibaba_audio3.model,
                512,
                false,
            );
            validate_url(
                &mut fields,
                "asr.alibaba_audio3.native_endpoint",
                &self.asr.alibaba_audio3.native_endpoint,
                &["http", "https"],
                false,
            );
            validate_text(
                &mut fields,
                "asr.alibaba_audio3.native_model",
                &self.asr.alibaba_audio3.native_model,
                512,
                false,
            );
            let recognition = self.asr.alibaba_audio3.effective_recognition_controls();
            range(
                &mut fields,
                "asr.alibaba_audio3.max_sentence_silence_ms",
                recognition.max_sentence_silence_ms,
                200,
                6_000,
            );
            if let Some(threshold) = recognition.speech_noise_threshold
                && threshold.is_finite()
                && !(-1.0..=1.0).contains(&threshold)
            {
                fields.insert(
                    "asr.alibaba_audio3.speech_noise_threshold".into(),
                    "must be between -1 and 1".into(),
                );
            }
            if self.asr.alibaba_audio3.recognition_preset == Audio3RecognitionPreset::Custom
                && recognition.semantic_punctuation_enabled
                && recognition.multi_threshold_mode_enabled
            {
                fields.insert(
                    "asr.alibaba_audio3.multi_threshold_mode_enabled".into(),
                    "cannot be combined with semantic punctuation in the custom preset".into(),
                );
            }
            if let Some(message) = validate_audio3_vocabulary(&self.asr.alibaba_audio3.vocabulary) {
                fields.insert("asr.alibaba_audio3.vocabulary".into(), message);
            }
            if self.asr.alibaba_audio3.native_final_pass_mode != NativeFinalPassMode::StreamingOnly
            {
                range(
                    &mut fields,
                    "asr.alibaba_audio3.native_timeout_ms",
                    self.asr.alibaba_audio3.native_timeout_ms,
                    100,
                    120_000,
                );
            }
        }

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
        // Text delivery always uses a clipboard paste shortcut. Legacy output
        // modes remain parseable so existing configurations continue to load.
        validate_text(
            &mut fields,
            "output.paste_keys",
            &self.output.paste_keys,
            256,
            false,
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
        let config = Config::from_raw(raw);
        config
            .validate()
            .context("configuration contains invalid values")?;
        Ok(LoadedConfig {
            config,
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

fn validate_audio3_vocabulary(vocabulary: &[Audio3VocabularyTerm]) -> Option<String> {
    if vocabulary.len() > MAX_AUDIO3_VOCABULARY_TERMS {
        return Some(format!(
            "must contain at most {MAX_AUDIO3_VOCABULARY_TERMS} entries"
        ));
    }
    let configured_bytes = vocabulary.iter().fold(0_usize, |total, entry| {
        total.saturating_add(entry.term.len())
    });
    if configured_bytes > MAX_AUDIO3_VOCABULARY_BYTES {
        return Some(format!(
            "must contain at most {MAX_AUDIO3_VOCABULARY_BYTES} configured term bytes"
        ));
    }

    let mut seen = HashSet::with_capacity(vocabulary.len());
    let mut weight_50_count = 0_usize;
    for (index, entry) in vocabulary.iter().enumerate() {
        let entry_number = index + 1;
        if entry.term.chars().any(char::is_control) {
            return Some(format!(
                "entry {entry_number} must not contain control characters"
            ));
        }
        let term = entry.term.trim();
        if term.is_empty() {
            return Some(format!("entry {entry_number} must not be empty"));
        }
        if term.is_ascii() {
            if term.split_whitespace().count() > 7 {
                return Some(format!(
                    "entry {entry_number} must contain at most 7 whitespace-separated segments"
                ));
            }
        } else if term.chars().count() > 15 {
            return Some(format!(
                "entry {entry_number} must contain at most 15 Unicode characters"
            ));
        }
        if !matches!(entry.weight, 1..=5 | 50) {
            return Some(format!(
                "entry {entry_number} weight must be between 1 and 5 or exactly 50"
            ));
        }
        if !seen.insert(term) {
            return Some(format!(
                "entry {entry_number} duplicates an earlier entry after trimming"
            ));
        }
        if entry.weight == 50 {
            weight_50_count += 1;
        }
    }
    if weight_50_count > 50 {
        return Some("must contain at most 50 entries with weight 50".into());
    }
    None
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
    } else if matches!(url.scheme(), "http" | "ws") && !url_host_is_loopback(&url) {
        fields.insert(
            field.into(),
            "must use transport encryption unless the host is loopback".into(),
        );
    } else if !url.username().is_empty() || url.password().is_some() {
        fields.insert(field.into(), "must not contain embedded credentials".into());
    } else if url.host_str().is_none() {
        fields.insert(field.into(), "must include a host".into());
    }
}

fn url_host_is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
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
            AsrProvider::AlibabaQwenAudio3 => "qwen-audio3 (experimental)".into(),
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
            AsrProvider::AlibabaQwenAudio3 => {
                if self.alibaba_audio3.native_final_pass_mode != NativeFinalPassMode::StreamingOnly
                {
                    format!(
                        "{} -> {}",
                        self.alibaba_audio3.model, self.alibaba_audio3.native_model
                    )
                } else {
                    self.alibaba_audio3.model.clone()
                }
            }
        }
    }
}

impl Language {
    pub fn audio3_language_hints(self) -> &'static [&'static str] {
        match self {
            Language::English => &["en"],
            Language::SimplifiedChinese | Language::TraditionalChinese => &["zh", "en"],
            Language::Japanese => &["ja", "en"],
            Language::Korean => &["ko", "en"],
        }
    }

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

    use super::{
        AsrProvider, Audio3RecognitionPreset, Audio3VocabularyTerm, Config, ConfigStore,
        EffectiveAudio3RecognitionControls, HudPosition, Language, MAX_AUDIO3_VOCABULARY_BYTES,
        NativeFinalPassMode, RevisionConflict,
    };

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
    fn plaintext_provider_urls_are_limited_to_loopback_hosts() {
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.model = "test-model".into();
        config.llm.api_base_url = "http://provider.example/v1".into();
        assert!(
            config
                .validate()
                .expect_err("remote plaintext provider URL must be rejected")
                .fields
                .contains_key("llm.api_base_url")
        );

        config.llm.api_base_url = "http://127.0.0.1:8080/v1".into();
        config
            .validate()
            .expect("loopback HTTP remains available for local development");
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
    fn manually_edited_invalid_config_is_rejected_on_load() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        fs::write(&path, "[audio]\nsample_rate = 1\n").unwrap();
        let error = ConfigStore::new(path)
            .load()
            .expect_err("validation must also apply to configuration loaded from disk");
        assert!(error.to_string().contains("invalid values"));
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
    fn audio3_defaults_are_additive_and_experimental() {
        let config = Config::default();
        assert!(!config.asr.alibaba_audio3.experimental_enabled);
        assert_eq!(
            config.asr.alibaba_audio3.endpoint,
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );
        assert_eq!(
            config.asr.alibaba_audio3.model,
            "qwen-audio-3.0-asr-flash-streaming"
        );
        assert!(!config.asr.alibaba_audio3.language_hints_enabled);
        assert!(!config.asr.alibaba_audio3.heartbeat_enabled);
        assert_eq!(
            config.asr.alibaba_audio3.recognition_preset,
            Audio3RecognitionPreset::Standard
        );
        assert_eq!(config.asr.alibaba_audio3.max_sentence_silence_ms, 800);
        assert!(!config.asr.alibaba_audio3.semantic_punctuation_enabled);
        assert!(!config.asr.alibaba_audio3.multi_threshold_mode_enabled);
        assert_eq!(config.asr.alibaba_audio3.speech_noise_threshold, None);
        assert_eq!(
            config.asr.alibaba_audio3.effective_recognition_controls(),
            EffectiveAudio3RecognitionControls {
                max_sentence_silence_ms: 800,
                semantic_punctuation_enabled: false,
                multi_threshold_mode_enabled: false,
                speech_noise_threshold: None,
            }
        );
        assert!(config.asr.alibaba_audio3.vocabulary.is_empty());
        assert_eq!(
            config.asr.alibaba_audio3.native_endpoint,
            "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation"
        );
        assert_eq!(
            config.asr.alibaba_audio3.native_model,
            "qwen-audio-3.0-asr-flash"
        );
        assert_eq!(
            config.asr.alibaba_audio3.native_final_pass_mode,
            NativeFinalPassMode::StreamingOnly
        );
        assert_eq!(config.asr.alibaba_audio3.native_timeout_ms, 20_000);
    }

    #[test]
    fn audio3_selection_requires_gate_and_validates_only_when_selected() {
        let mut config = Config::default();
        config.asr.alibaba_audio3.endpoint = "not a URL".into();
        config.asr.alibaba_audio3.model.clear();
        config
            .validate()
            .expect("inactive experimental configuration remains compatible");

        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        let error = config
            .validate()
            .expect_err("experimental gate is required");
        assert!(
            error
                .fields
                .contains_key("asr.alibaba_audio3.experimental_enabled")
        );
        assert!(error.fields.contains_key("asr.alibaba_audio3.endpoint"));
        assert!(error.fields.contains_key("asr.alibaba_audio3.model"));

        config.asr.alibaba_audio3.experimental_enabled = true;
        config.asr.alibaba_audio3.endpoint =
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference".into();
        config.asr.alibaba_audio3.model = "qwen-audio-3.0-asr-flash-streaming".into();
        config.validate().expect("gated provider configuration");

        config.asr.alibaba_audio3.recognition_preset = Audio3RecognitionPreset::Custom;
        for invalid in [199, 6_001] {
            config.asr.alibaba_audio3.max_sentence_silence_ms = invalid;
            let error = config
                .validate()
                .expect_err("sentence silence must follow the provider range");
            assert!(
                error
                    .fields
                    .contains_key("asr.alibaba_audio3.max_sentence_silence_ms")
            );
        }
        config.asr.alibaba_audio3.max_sentence_silence_ms = 200;
        config.validate().expect("minimum sentence silence");
        config.asr.alibaba_audio3.max_sentence_silence_ms = 6_000;
        config.validate().expect("maximum sentence silence");
        assert!(
            toml::to_string(&config)
                .unwrap()
                .contains("provider = \"alibaba-qwen-audio3\"")
        );
    }

    #[test]
    fn audio3_custom_threshold_and_control_interactions_are_validated_when_active() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.experimental_enabled = true;
        config.asr.alibaba_audio3.recognition_preset = Audio3RecognitionPreset::Custom;

        for valid in [-1.0, 0.0, 1.0] {
            config.asr.alibaba_audio3.speech_noise_threshold = Some(valid);
            config.validate().expect("threshold boundary is valid");
        }
        for invalid in [-1.01, 1.01] {
            config.asr.alibaba_audio3.speech_noise_threshold = Some(invalid);
            let error = config.validate().expect_err("invalid threshold range");
            assert_eq!(
                error.fields["asr.alibaba_audio3.speech_noise_threshold"],
                "must be between -1 and 1"
            );
        }

        config.asr.alibaba_audio3.speech_noise_threshold = None;
        config.asr.alibaba_audio3.semantic_punctuation_enabled = true;
        config.asr.alibaba_audio3.multi_threshold_mode_enabled = true;
        let error = config
            .validate()
            .expect_err("custom semantic and multi-threshold controls conflict");
        let message = &error.fields["asr.alibaba_audio3.multi_threshold_mode_enabled"];
        assert!(!message.contains("true"));
        assert!(!message.contains("false"));

        config.asr.alibaba_audio3.recognition_preset = Audio3RecognitionPreset::LongForm;
        config.asr.alibaba_audio3.max_sentence_silence_ms = 1;
        config.asr.alibaba_audio3.speech_noise_threshold = Some(1.5);
        config
            .validate()
            .expect("explicit preset overrides finite dormant custom controls");
    }

    #[test]
    fn nonfinite_audio3_threshold_is_rejected_universally_with_one_field_error() {
        for (provider, preset, threshold) in [
            (
                AsrProvider::LocalCli,
                Audio3RecognitionPreset::Standard,
                f32::NAN,
            ),
            (
                AsrProvider::AlibabaQwenAudio3,
                Audio3RecognitionPreset::Standard,
                f32::INFINITY,
            ),
            (
                AsrProvider::AlibabaQwenAudio3,
                Audio3RecognitionPreset::Custom,
                f32::NEG_INFINITY,
            ),
        ] {
            let mut config = Config::default();
            config.asr.provider = provider;
            config.asr.alibaba_audio3.experimental_enabled = true;
            config.asr.alibaba_audio3.recognition_preset = preset;
            config.asr.alibaba_audio3.speech_noise_threshold = Some(threshold);

            let error = config
                .validate()
                .expect_err("a configured nonfinite threshold cannot round-trip through JSON");
            assert_eq!(
                error.fields,
                std::collections::BTreeMap::from([(
                    "asr.alibaba_audio3.speech_noise_threshold".into(),
                    "must be finite".into(),
                )])
            );
        }
    }

    #[test]
    fn inactive_provider_isolates_finite_audio3_range_and_combination_values() {
        let mut config = Config::default();
        config.asr.alibaba_audio3.recognition_preset = Audio3RecognitionPreset::Custom;
        config.asr.alibaba_audio3.max_sentence_silence_ms = 1;
        config.asr.alibaba_audio3.semantic_punctuation_enabled = true;
        config.asr.alibaba_audio3.multi_threshold_mode_enabled = true;
        config.asr.alibaba_audio3.speech_noise_threshold = Some(1.5);
        config
            .validate()
            .expect("inactive Audio3 effective range and combination validation is isolated");
    }

    #[test]
    fn audio3_active_model_label_includes_enabled_native_final_pass() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        assert_eq!(
            config.asr.active_model_label(),
            "qwen-audio-3.0-asr-flash-streaming"
        );

        config.asr.alibaba_audio3.native_final_pass_mode = NativeFinalPassMode::Always;
        assert_eq!(
            config.asr.active_model_label(),
            "qwen-audio-3.0-asr-flash-streaming -> qwen-audio-3.0-asr-flash"
        );
    }

    #[test]
    fn audio3_native_timeout_is_validated_only_for_selected_enabled_final_pass() {
        let mut config = Config::default();
        config.asr.alibaba_audio3.native_timeout_ms = 0;
        config
            .validate()
            .expect("inactive Audio3 native timeout is ignored");

        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.experimental_enabled = true;
        config
            .validate()
            .expect("disabled Audio3 final pass ignores its timeout");

        config.asr.alibaba_audio3.native_final_pass_mode = NativeFinalPassMode::Adaptive;
        let error = config
            .validate()
            .expect_err("enabled Audio3 final pass validates its timeout");
        assert!(
            error
                .fields
                .contains_key("asr.alibaba_audio3.native_timeout_ms")
        );
    }

    #[test]
    fn old_asr_config_deserializes_with_unchanged_defaults() {
        let old = r#"
provider = "alibaba-qwen-realtime"
backend_command = "/usr/bin/voxtype"
engine = "sensevoice"
model = ""
language = "simplified-chinese"
connect_timeout_ms = 5000
finalize_timeout_ms = 8000
fallback_to_local = true

[alibaba]
endpoint = "wss://dashscope.aliyuncs.com/api-ws/v1/realtime"
model = "legacy-model"
"#;
        let asr: super::AsrConfig = toml::from_str(old).expect("old ASR config");
        assert_eq!(asr.provider, AsrProvider::AlibabaQwenRealtime);
        assert_eq!(asr.alibaba.model, "legacy-model");
        assert!(!asr.alibaba_audio3.experimental_enabled);
        assert_eq!(
            asr.alibaba_audio3.model,
            "qwen-audio-3.0-asr-flash-streaming"
        );
        assert!(!asr.alibaba_audio3.language_hints_enabled);
        assert!(!asr.alibaba_audio3.heartbeat_enabled);
        assert_eq!(
            asr.alibaba_audio3.recognition_preset,
            Audio3RecognitionPreset::Standard
        );
        assert!(!asr.alibaba_audio3.multi_threshold_mode_enabled);
        assert_eq!(asr.alibaba_audio3.speech_noise_threshold, None);
        assert!(asr.alibaba_audio3.vocabulary.is_empty());
        assert_eq!(
            asr.alibaba_audio3.native_final_pass_mode,
            NativeFinalPassMode::StreamingOnly
        );
        assert_eq!(asr.alibaba_audio3.native_timeout_ms, 20_000);
    }

    #[test]
    fn audio3_config_without_native_gate_or_timeout_uses_new_defaults() {
        let audio3: super::AlibabaAudio3Config = toml::from_str(
            r#"
experimental_enabled = true
endpoint = "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
model = "streaming-model"
native_endpoint = "https://dashscope.aliyuncs.com/native"
native_model = "native-model"
"#,
        )
        .expect("pre-milestone Audio3 config");
        assert!(!audio3.language_hints_enabled);
        assert!(!audio3.heartbeat_enabled);
        assert_eq!(audio3.recognition_preset, Audio3RecognitionPreset::Standard);
        assert!(!audio3.multi_threshold_mode_enabled);
        assert_eq!(audio3.speech_noise_threshold, None);
        assert!(audio3.vocabulary.is_empty());
        assert_eq!(
            audio3.native_final_pass_mode,
            NativeFinalPassMode::StreamingOnly
        );
        assert_eq!(audio3.native_timeout_ms, 20_000);
    }

    #[test]
    fn config_store_loads_pre_milestone_audio3_controls_as_disabled_and_empty() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            r#"
state_file = "auto"

[asr]
provider = "alibaba-qwen-audio3"
backend_command = "/usr/bin/voxtype"
engine = "sensevoice"
model = ""
language = "simplified-chinese"
connect_timeout_ms = 5000
finalize_timeout_ms = 8000
fallback_to_local = true

[asr.alibaba_audio3]
experimental_enabled = true
endpoint = "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
model = "qwen-audio-3.0-asr-flash-streaming"
native_endpoint = "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation"
native_model = "qwen-audio-3.0-asr-flash"
native_final_pass_enabled = false
native_timeout_ms = 20000
"#,
        )
        .unwrap();

        let audio3 = ConfigStore::new(path)
            .load()
            .expect("representative pre-milestone config")
            .config
            .asr
            .alibaba_audio3;
        assert!(!audio3.language_hints_enabled);
        assert!(!audio3.heartbeat_enabled);
        assert_eq!(audio3.recognition_preset, Audio3RecognitionPreset::Standard);
        assert!(!audio3.multi_threshold_mode_enabled);
        assert_eq!(audio3.speech_noise_threshold, None);
        assert!(audio3.vocabulary.is_empty());
    }

    #[test]
    fn audio3_recognition_presets_resolve_exact_candidate_controls() {
        let mut audio3 = super::AlibabaAudio3Config::default();
        for (preset, expected) in [
            (
                Audio3RecognitionPreset::Standard,
                EffectiveAudio3RecognitionControls {
                    max_sentence_silence_ms: 800,
                    semantic_punctuation_enabled: false,
                    multi_threshold_mode_enabled: false,
                    speech_noise_threshold: None,
                },
            ),
            (
                Audio3RecognitionPreset::LowLatencyDictation,
                EffectiveAudio3RecognitionControls {
                    max_sentence_silence_ms: 400,
                    semantic_punctuation_enabled: false,
                    multi_threshold_mode_enabled: true,
                    speech_noise_threshold: None,
                },
            ),
            (
                Audio3RecognitionPreset::LongForm,
                EffectiveAudio3RecognitionControls {
                    max_sentence_silence_ms: 1_300,
                    semantic_punctuation_enabled: true,
                    multi_threshold_mode_enabled: false,
                    speech_noise_threshold: None,
                },
            ),
        ] {
            audio3.recognition_preset = preset;
            assert_eq!(audio3.effective_recognition_controls(), expected);
        }

        audio3.recognition_preset = Audio3RecognitionPreset::Custom;
        audio3.max_sentence_silence_ms = 725;
        audio3.semantic_punctuation_enabled = false;
        audio3.multi_threshold_mode_enabled = true;
        audio3.speech_noise_threshold = Some(-0.25);
        assert_eq!(
            audio3.effective_recognition_controls(),
            EffectiveAudio3RecognitionControls {
                max_sentence_silence_ms: 725,
                semantic_punctuation_enabled: false,
                multi_threshold_mode_enabled: true,
                speech_noise_threshold: Some(-0.25),
            }
        );
    }

    #[test]
    fn pre_m2_audio3_recognition_controls_migrate_by_raw_values() {
        let baseline: super::AlibabaAudio3Config = serde_json::from_value(serde_json::json!({
            "max_sentence_silence_ms": 800,
            "semantic_punctuation_enabled": false
        }))
        .unwrap();
        assert_eq!(
            baseline.recognition_preset,
            Audio3RecognitionPreset::Standard
        );

        for value in [
            serde_json::json!({"max_sentence_silence_ms": 801}),
            serde_json::json!({"semantic_punctuation_enabled": true}),
            serde_json::json!({"multi_threshold_mode_enabled": true}),
            serde_json::json!({"speech_noise_threshold": 0.0}),
        ] {
            let migrated: super::AlibabaAudio3Config = serde_json::from_value(value).unwrap();
            assert_eq!(migrated.recognition_preset, Audio3RecognitionPreset::Custom);
        }
    }

    #[test]
    fn named_audio3_preset_preserves_finite_dormant_raw_controls_in_config_json() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.experimental_enabled = true;
        config.asr.alibaba_audio3.recognition_preset = Audio3RecognitionPreset::Standard;
        config.asr.alibaba_audio3.max_sentence_silence_ms = 333;
        config.asr.alibaba_audio3.semantic_punctuation_enabled = true;
        config.asr.alibaba_audio3.multi_threshold_mode_enabled = true;
        config.asr.alibaba_audio3.speech_noise_threshold = Some(1.5);
        config
            .validate()
            .expect("finite dormant out-of-range controls are preserved");
        assert_eq!(
            config.asr.alibaba_audio3.effective_recognition_controls(),
            EffectiveAudio3RecognitionControls {
                max_sentence_silence_ms: 800,
                semantic_punctuation_enabled: false,
                multi_threshold_mode_enabled: false,
                speech_noise_threshold: None,
            }
        );

        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(
            serialized["asr"]["alibaba_audio3"]["speech_noise_threshold"],
            1.5
        );
        let round_trip: Config = serde_json::from_value(serialized).unwrap();
        round_trip
            .validate()
            .expect("round-tripped config is valid");
        assert_eq!(
            round_trip.asr.alibaba_audio3.recognition_preset,
            Audio3RecognitionPreset::Standard
        );
        assert_eq!(round_trip.asr.alibaba_audio3.max_sentence_silence_ms, 333);
        assert!(round_trip.asr.alibaba_audio3.semantic_punctuation_enabled);
        assert!(round_trip.asr.alibaba_audio3.multi_threshold_mode_enabled);
        assert_eq!(
            round_trip.asr.alibaba_audio3.speech_noise_threshold,
            Some(1.5)
        );

        let default_serialized =
            serde_json::to_value(super::AlibabaAudio3Config::default()).unwrap();
        assert!(default_serialized.get("speech_noise_threshold").is_none());
        assert!(
            serde_json::from_value::<super::AlibabaAudio3Config>(serde_json::json!({
                "recognition_preset": "not-a-preset"
            }))
            .is_err()
        );
    }

    #[test]
    fn audio3_native_final_pass_direct_serde_migrates_and_rejects_ambiguity() {
        for (value, expected) in [
            (serde_json::json!({}), NativeFinalPassMode::StreamingOnly),
            (
                serde_json::json!({"native_final_pass_enabled": false}),
                NativeFinalPassMode::StreamingOnly,
            ),
            (
                serde_json::json!({"native_final_pass_enabled": true}),
                NativeFinalPassMode::Always,
            ),
            (
                serde_json::json!({"native_final_pass_mode": "adaptive"}),
                NativeFinalPassMode::Adaptive,
            ),
            (
                serde_json::json!({
                    "native_final_pass_mode": "streaming-only",
                    "native_final_pass_enabled": false
                }),
                NativeFinalPassMode::StreamingOnly,
            ),
            (
                serde_json::json!({
                    "native_final_pass_mode": "always",
                    "native_final_pass_enabled": true
                }),
                NativeFinalPassMode::Always,
            ),
        ] {
            let config: super::AlibabaAudio3Config = serde_json::from_value(value).unwrap();
            assert_eq!(config.native_final_pass_mode, expected);
            let serialized = serde_json::to_value(config).unwrap();
            assert_eq!(
                serialized["native_final_pass_mode"],
                serde_json::to_value(expected).unwrap()
            );
            assert!(serialized.get("native_final_pass_enabled").is_none());
        }

        for value in [
            serde_json::json!({
                "native_final_pass_mode": "always",
                "native_final_pass_enabled": false
            }),
            serde_json::json!({
                "native_final_pass_mode": "adaptive",
                "native_final_pass_enabled": true
            }),
        ] {
            let error = serde_json::from_value::<super::AlibabaAudio3Config>(value)
                .expect_err("conflicting settings must fail")
                .to_string();
            assert!(error.contains("ambiguous Audio3 native final-pass configuration"));
            assert!(!error.contains("true"));
            assert!(!error.contains("false"));
            assert!(!error.contains("adaptive"));
            assert!(!error.contains("always"));
        }
    }

    #[test]
    fn config_store_migrates_legacy_audio3_native_final_pass_boolean() {
        for (legacy, expected) in [
            (false, NativeFinalPassMode::StreamingOnly),
            (true, NativeFinalPassMode::Always),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("config.toml");
            let source = toml::to_string_pretty(&Config::default()).unwrap().replace(
                "native_final_pass_mode = \"streaming-only\"",
                &format!("native_final_pass_enabled = {legacy}"),
            );
            fs::write(&path, source).unwrap();
            let loaded = ConfigStore::new(&path).load().unwrap().config;
            assert_eq!(loaded.asr.alibaba_audio3.native_final_pass_mode, expected);

            ConfigStore::new(&path).save(&loaded, None).unwrap();
            let saved = fs::read_to_string(path).unwrap();
            assert!(saved.contains(&format!(
                "native_final_pass_mode = \"{}\"",
                match expected {
                    NativeFinalPassMode::StreamingOnly => "streaming-only",
                    NativeFinalPassMode::Adaptive => "adaptive",
                    NativeFinalPassMode::Always => "always",
                }
            )));
            assert!(!saved.contains("native_final_pass_enabled"));
        }
    }

    #[test]
    fn config_store_accepts_consistent_dual_audio3_fields_and_rejects_conflicts() {
        for (mode, legacy, accepted) in [
            ("streaming-only", false, true),
            ("always", true, true),
            ("adaptive", false, false),
            ("always", false, false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("config.toml");
            let source = toml::to_string_pretty(&Config::default()).unwrap().replace(
                "native_final_pass_mode = \"streaming-only\"",
                &format!(
                    "native_final_pass_mode = \"{mode}\"\nnative_final_pass_enabled = {legacy}"
                ),
            );
            fs::write(&path, source).unwrap();
            let result = ConfigStore::new(path).load();
            if accepted {
                assert!(result.is_ok(), "consistent dual fields must load");
            } else {
                let message = format!(
                    "{:#}",
                    result.expect_err("conflicting dual fields must fail")
                );
                assert!(message.contains("ambiguous Audio3 native final-pass configuration"));
                assert!(!message.contains(mode));
                assert!(!message.contains(&legacy.to_string()));
            }
        }
    }

    #[test]
    fn audio3_language_hints_are_deterministic_and_preserve_english_mixing() {
        for (language, expected) in [
            (Language::English, &["en"][..]),
            (Language::SimplifiedChinese, &["zh", "en"][..]),
            (Language::TraditionalChinese, &["zh", "en"][..]),
            (Language::Japanese, &["ja", "en"][..]),
            (Language::Korean, &["ko", "en"][..]),
        ] {
            assert_eq!(language.audio3_language_hints(), expected);
            assert!(language.audio3_language_hints().len() <= 4);
        }
    }

    #[test]
    fn audio3_vocabulary_serializes_as_typed_entries_and_trims_only_for_identity() {
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.experimental_enabled = true;
        config.asr.alibaba_audio3.vocabulary = vec![
            Audio3VocabularyTerm {
                term: " Voice Input ".into(),
                weight: 5,
            },
            Audio3VocabularyTerm {
                term: "语音输入".into(),
                weight: 50,
            },
        ];
        config.validate().expect("valid dynamic vocabulary");

        let serialized = toml::to_string(&config).unwrap();
        assert!(serialized.contains("[[asr.alibaba_audio3.vocabulary]]"));
        let round_trip: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(
            round_trip.asr.alibaba_audio3.vocabulary,
            config.asr.alibaba_audio3.vocabulary
        );
    }

    #[test]
    fn audio3_vocabulary_enforces_term_weight_duplicate_and_super_hotword_limits() {
        let valid = |term: &str, weight| Audio3VocabularyTerm {
            term: term.into(),
            weight,
        };
        for (vocabulary, expected) in [
            (vec![valid("   ", 1)], "entry 1 must not be empty"),
            (
                vec![valid("private\tterm", 1)],
                "entry 1 must not contain control characters",
            ),
            (
                vec![valid("one two three four five six seven eight", 1)],
                "entry 1 must contain at most 7 whitespace-separated segments",
            ),
            (
                vec![valid("一二三四五六七八九十一二三四五六", 1)],
                "entry 1 must contain at most 15 Unicode characters",
            ),
            (
                vec![valid("valid", 6)],
                "entry 1 weight must be between 1 and 5 or exactly 50",
            ),
            (
                vec![valid("same", 1), valid(" same ", 2)],
                "entry 2 duplicates an earlier entry after trimming",
            ),
        ] {
            let mut config = Config::default();
            config.asr.provider = AsrProvider::AlibabaQwenAudio3;
            config.asr.alibaba_audio3.experimental_enabled = true;
            config.asr.alibaba_audio3.vocabulary = vocabulary;
            let error = config.validate().expect_err("invalid vocabulary");
            assert_eq!(error.fields["asr.alibaba_audio3.vocabulary"], expected);
        }

        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.experimental_enabled = true;
        config.asr.alibaba_audio3.vocabulary = (0..51)
            .map(|index| valid(&format!("term{index}"), 50))
            .collect();
        assert_eq!(
            config.validate().unwrap_err().fields["asr.alibaba_audio3.vocabulary"],
            "must contain at most 50 entries with weight 50"
        );

        config.asr.alibaba_audio3.vocabulary = (0..2_000)
            .map(|index| valid(&format!("term{index}"), 1))
            .collect();
        config.validate().expect("2,000 unique terms are valid");
        config
            .asr
            .alibaba_audio3
            .vocabulary
            .push(valid("one-too-many", 1));
        assert_eq!(
            config.validate().unwrap_err().fields["asr.alibaba_audio3.vocabulary"],
            "must contain at most 2000 entries"
        );
    }

    #[test]
    fn audio3_vocabulary_is_bounded_and_validation_errors_do_not_expose_terms() {
        const SENTINEL: &str = "private-vocabulary-sentinel";
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.experimental_enabled = true;
        config.asr.alibaba_audio3.vocabulary = vec![Audio3VocabularyTerm {
            term: format!("{SENTINEL}{}", "x".repeat(MAX_AUDIO3_VOCABULARY_BYTES)),
            weight: 1,
        }];
        let error = config.validate().expect_err("oversized vocabulary");
        let formatted = format!("{error}: {:?}", error.fields);
        assert!(formatted.contains("configured term bytes"));
        assert!(!formatted.contains(SENTINEL));

        config.asr.provider = AsrProvider::LocalCli;
        config
            .validate()
            .expect("legacy providers ignore Audio3-only vocabulary controls");
        config.asr.provider = AsrProvider::AlibabaQwenRealtime;
        config
            .validate()
            .expect("Alibaba realtime ignores Audio3-only vocabulary controls");
    }

    #[test]
    fn serialization_omits_legacy_secrets() {
        let mut config = Config::default();
        config.asr.alibaba.api_key = "alibaba-secret".into();
        config.asr.alibaba_audio3.api_key = "audio3-secret".into();
        config.llm.api_key = "llm-secret".into();
        let toml = toml::to_string(&config).unwrap();
        let json = serde_json::to_string(&config).unwrap();
        assert!(!toml.contains("secret"));
        assert!(!toml.contains("api_key"));
        assert!(!json.contains("secret"));
        assert!(!json.contains("api_key"));
    }
}
