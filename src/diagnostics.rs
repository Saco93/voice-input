use std::fmt::Write as _;

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{
    config::{AsrProvider, Config, NativeFinalPassMode},
    state::{Phase, Snapshot},
};

pub const DIAGNOSTICS_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Diagnostics {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub session: Option<SessionDiagnostics>,
}

impl Diagnostics {
    pub fn inactive() -> Self {
        Self::default()
    }

    pub(crate) fn start_session(
        &mut self,
        session_id: u64,
        provider: Provider,
        final_pass_kind: FinalPassKind,
        fallback_enabled: bool,
    ) {
        self.session = Some(SessionDiagnostics::new(
            session_id,
            provider,
            final_pass_kind,
            fallback_enabled,
        ));
    }

    /// Applies a session-local update only when it still belongs to the active
    /// diagnostics record. This is the boundary used by asynchronous workers.
    pub(crate) fn configure_audio3_native_final_pass(
        &mut self,
        session_id: u64,
        mode: NativeFinalPassMode,
    ) -> bool {
        self.update_session(session_id, |session| {
            session.final_pass.configured_mode = Some(mode);
            match mode {
                NativeFinalPassMode::StreamingOnly => {
                    session.final_pass.kind = FinalPassKind::None;
                    session.final_pass.status = StageStatus::Inactive;
                    session.final_pass.decision = FinalPassDecision::Skipped;
                    session.final_pass.reason = Some(FinalPassReason::StreamingOnly);
                }
                NativeFinalPassMode::Adaptive | NativeFinalPassMode::Always => {
                    session.final_pass.kind = FinalPassKind::QwenAudio3Native;
                    session.final_pass.status = StageStatus::Pending;
                    session.final_pass.decision = FinalPassDecision::Pending;
                    session.final_pass.reason = None;
                }
            }
        })
    }

    pub(crate) fn update_session(
        &mut self,
        session_id: u64,
        update: impl FnOnce(&mut SessionDiagnostics),
    ) -> bool {
        let Some(session) = self.session.as_mut().filter(|session| {
            session.session_id == session_id && session.asr_outcome == OverallOutcome::InProgress
        }) else {
            return false;
        };
        update(session);
        true
    }

    pub(crate) fn finish_session(
        &mut self,
        session_id: u64,
        outcome: OverallOutcome,
        selected_result: SelectedResult,
        total_asr_latency_ms: u64,
    ) -> bool {
        if outcome == OverallOutcome::InProgress {
            return false;
        }
        self.update_session(session_id, |session| {
            session.asr_outcome = outcome;
            session.selected_result = selected_result;
            session.total_asr_latency_ms = Some(total_asr_latency_ms);
            if session.final_pass.decision == FinalPassDecision::Pending {
                session.final_pass.decision = FinalPassDecision::Skipped;
                if session.final_pass.reason.is_none() {
                    session.final_pass.reason = Some(FinalPassReason::NotReached);
                }
            }
            for status in [
                &mut session.streaming.status,
                &mut session.final_pass.status,
                &mut session.local_primary.status,
                &mut session.local_fallback.status,
            ] {
                if *status == StageStatus::Pending {
                    *status = StageStatus::Skipped;
                }
            }
        })
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            schema_version: DIAGNOSTICS_SCHEMA_VERSION,
            session: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionDiagnostics {
    pub session_id: u64,
    pub provider: Provider,
    pub asr_outcome: OverallOutcome,
    pub streaming: StreamingStage,
    pub final_pass: FinalPassStage,
    pub local_primary: LocalPrimaryStage,
    pub local_fallback: LocalFallbackStage,
    pub selected_result: SelectedResult,
    pub total_asr_latency_ms: Option<u64>,
}

impl SessionDiagnostics {
    pub fn new(
        session_id: u64,
        provider: Provider,
        final_pass_kind: FinalPassKind,
        fallback_enabled: bool,
    ) -> Self {
        let streaming_status = if provider == Provider::LocalCli {
            StageStatus::Inactive
        } else {
            StageStatus::Pending
        };
        let final_pass_status = if final_pass_kind == FinalPassKind::None {
            StageStatus::Inactive
        } else {
            StageStatus::Pending
        };
        let (configured_mode, final_pass_decision, final_pass_reason) =
            match (provider, final_pass_kind) {
                (Provider::AlibabaQwenAudio3, FinalPassKind::None) => (
                    Some(NativeFinalPassMode::StreamingOnly),
                    FinalPassDecision::Skipped,
                    Some(FinalPassReason::StreamingOnly),
                ),
                (Provider::AlibabaQwenAudio3, FinalPassKind::QwenAudio3Native) => (
                    Some(NativeFinalPassMode::Always),
                    FinalPassDecision::Pending,
                    None,
                ),
                _ => (None, FinalPassDecision::NotApplicable, None),
            };
        let local_primary_status = if provider == Provider::LocalCli {
            StageStatus::Pending
        } else {
            StageStatus::Inactive
        };
        let local_fallback_status = if provider != Provider::LocalCli && fallback_enabled {
            StageStatus::Pending
        } else {
            StageStatus::Inactive
        };

        Self {
            session_id,
            provider,
            asr_outcome: OverallOutcome::InProgress,
            streaming: StreamingStage {
                status: streaming_status,
                ..StreamingStage::default()
            },
            final_pass: FinalPassStage {
                kind: final_pass_kind,
                status: final_pass_status,
                configured_mode,
                decision: final_pass_decision,
                reason: final_pass_reason,
                ..FinalPassStage::default()
            },
            local_primary: LocalPrimaryStage {
                status: local_primary_status,
                ..LocalPrimaryStage::default()
            },
            local_fallback: LocalFallbackStage {
                status: local_fallback_status,
                ..LocalFallbackStage::default()
            },
            selected_result: SelectedResult::Pending,
            total_asr_latency_ms: None,
        }
    }

    fn format_safe_text(&self, output: &mut String) {
        let _ = writeln!(
            output,
            "Session: {} (asr-outcome={}, provider={})",
            self.session_id,
            self.asr_outcome.as_str(),
            self.provider.as_str()
        );

        let _ = write!(output, "Streaming: {}", self.streaming.status.as_str());
        append_latency(output, "ready-latency-ms", self.streaming.ready_latency_ms);
        append_latency(
            output,
            "first-partial-latency-ms",
            self.streaming.first_partial_latency_ms,
        );
        append_latency(
            output,
            "first-nonempty-partial-latency-ms",
            self.streaming.first_nonempty_partial_latency_ms,
        );
        append_latency(
            output,
            "last-result-latency-ms",
            self.streaming.last_result_latency_ms,
        );
        if self.streaming.partial_event_count > 0 {
            let _ = write!(
                output,
                ", partial-events={}",
                self.streaming.partial_event_count
            );
        }
        if self.streaming.nonempty_partial_event_count > 0 {
            let _ = write!(
                output,
                ", nonempty-partial-events={}",
                self.streaming.nonempty_partial_event_count
            );
        }
        if self.streaming.segment_final_event_count > 0 {
            let _ = write!(
                output,
                ", segment-final-events={}",
                self.streaming.segment_final_event_count
            );
        }
        if self.streaming.audio_packet_count > 0 {
            let _ = write!(
                output,
                ", audio-packets={}",
                self.streaming.audio_packet_count
            );
        }
        append_latency(
            output,
            "audio-sent-duration-ms",
            self.streaming.audio_sent_duration_ms,
        );
        append_latency(
            output,
            "max-audio-queue-delay-ms",
            self.streaming.max_audio_queue_delay_ms,
        );
        append_latency(
            output,
            "last-audio-queue-delay-ms",
            self.streaming.last_audio_queue_delay_ms,
        );
        append_latency(
            output,
            "finish-sent-latency-ms",
            self.streaming.finish_sent_latency_ms,
        );
        append_latency(
            output,
            "task-finished-latency-ms",
            self.streaming.task_finished_latency_ms,
        );
        append_latency(
            output,
            "task-failed-latency-ms",
            self.streaming.task_failed_latency_ms,
        );
        append_latency(
            output,
            "finalize-latency-ms",
            self.streaming.finalize_latency_ms,
        );
        append_failure(output, self.streaming.failure_kind);
        if let Some(code) = &self.streaming.provider_error_code {
            let _ = write!(output, ", provider-error-code={}", code.as_str());
        }
        output.push('\n');

        let _ = write!(
            output,
            "Final pass: {} ({}, decision={})",
            self.final_pass.kind.as_str(),
            self.final_pass.status.as_str(),
            self.final_pass.decision.as_str()
        );
        if let Some(mode) = self.final_pass.configured_mode {
            let _ = write!(output, ", configured-mode={}", mode.as_str());
        }
        if let Some(reason) = self.final_pass.reason {
            let _ = write!(output, ", reason={}", reason.as_str());
        }
        append_latency(output, "latency-ms", self.final_pass.latency_ms);
        append_failure(output, self.final_pass.failure_kind);
        output.push('\n');

        let _ = write!(
            output,
            "Local primary: {}",
            self.local_primary.status.as_str()
        );
        append_latency(output, "latency-ms", self.local_primary.latency_ms);
        append_failure(output, self.local_primary.failure_kind);
        output.push('\n');

        let _ = write!(
            output,
            "Local fallback: {}",
            self.local_fallback.status.as_str()
        );
        append_latency(output, "latency-ms", self.local_fallback.latency_ms);
        append_failure(output, self.local_fallback.failure_kind);
        output.push('\n');

        let _ = writeln!(output, "Selected result: {}", self.selected_result.as_str());
        match self.total_asr_latency_ms {
            Some(latency) => {
                let _ = writeln!(output, "Total ASR latency: {latency} ms");
            }
            None => output.push_str("Total ASR latency: unavailable\n"),
        }
    }
}

impl Default for SessionDiagnostics {
    fn default() -> Self {
        Self::new(0, Provider::default(), FinalPassKind::None, false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct StreamingStage {
    pub status: StageStatus,
    pub ready_latency_ms: Option<u64>,
    pub first_partial_latency_ms: Option<u64>,
    pub first_nonempty_partial_latency_ms: Option<u64>,
    pub last_result_latency_ms: Option<u64>,
    pub partial_event_count: u64,
    pub nonempty_partial_event_count: u64,
    pub segment_final_event_count: u64,
    pub audio_packet_count: u64,
    pub audio_sent_duration_ms: Option<u64>,
    pub max_audio_queue_delay_ms: Option<u64>,
    pub last_audio_queue_delay_ms: Option<u64>,
    pub finish_sent_latency_ms: Option<u64>,
    pub task_finished_latency_ms: Option<u64>,
    pub task_failed_latency_ms: Option<u64>,
    pub finalize_latency_ms: Option<u64>,
    pub failure_kind: Option<FailureKind>,
    #[serde(default, deserialize_with = "deserialize_provider_error_code")]
    pub provider_error_code: Option<ProviderErrorCode>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PersistedProviderErrorCode {
    String(String),
    Other(de::IgnoredAny),
}

fn deserialize_provider_error_code<'de, D>(
    deserializer: D,
) -> Result<Option<ProviderErrorCode>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<PersistedProviderErrorCode>::deserialize(deserializer)?;
    Ok(match value {
        Some(PersistedProviderErrorCode::String(value)) => ProviderErrorCode::try_new(&value),
        Some(PersistedProviderErrorCode::Other(_)) | None => None,
    })
}

/// A provider error identifier that is safe to persist in support diagnostics.
///
/// Values are accepted only when the complete value is a short ASCII token.
/// Provider messages and malformed identifiers are discarded rather than
/// truncated or normalized, so private response content cannot be smuggled
/// into diagnostics through this field.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ProviderErrorCode(String);

impl ProviderErrorCode {
    pub(crate) fn try_new(value: &str) -> Option<Self> {
        (!value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
        .then(|| Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ProviderErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(&value).ok_or_else(|| de::Error::custom("invalid provider error code"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct FinalPassStage {
    pub kind: FinalPassKind,
    pub status: StageStatus,
    pub configured_mode: Option<NativeFinalPassMode>,
    pub decision: FinalPassDecision,
    pub reason: Option<FinalPassReason>,
    pub latency_ms: Option<u64>,
    pub failure_kind: Option<FailureKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct LocalPrimaryStage {
    pub status: StageStatus,
    pub latency_ms: Option<u64>,
    pub failure_kind: Option<FailureKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct LocalFallbackStage {
    pub status: StageStatus,
    pub latency_ms: Option<u64>,
    pub failure_kind: Option<FailureKind>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    LocalCli,
    AlibabaQwenRealtime,
    AlibabaQwenAudio3,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalCli => "local-cli",
            Self::AlibabaQwenRealtime => "alibaba-qwen-realtime",
            Self::AlibabaQwenAudio3 => "alibaba-qwen-audio3",
        }
    }
}

impl From<AsrProvider> for Provider {
    fn from(provider: AsrProvider) -> Self {
        match provider {
            AsrProvider::LocalCli => Self::LocalCli,
            AsrProvider::AlibabaQwenRealtime => Self::AlibabaQwenRealtime,
            AsrProvider::AlibabaQwenAudio3 => Self::AlibabaQwenAudio3,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OverallOutcome {
    #[default]
    InProgress,
    Completed,
    Empty,
    Cancelled,
    Failed,
}

impl OverallOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in-progress",
            Self::Completed => "completed",
            Self::Empty => "empty",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StageStatus {
    #[default]
    Inactive,
    Pending,
    InProgress,
    Completed,
    Degraded,
    Empty,
    Cancelled,
    Failed,
    Skipped,
}

impl StageStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Pending => "pending",
            Self::InProgress => "in-progress",
            Self::Completed => "completed",
            Self::Degraded => "degraded",
            Self::Empty => "empty",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FinalPassDecision {
    Pending,
    Invoked,
    Skipped,
    #[default]
    NotApplicable,
}

impl FinalPassDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Invoked => "invoked",
            Self::Skipped => "skipped",
            Self::NotApplicable => "not-applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FinalPassReason {
    StreamingOnly,
    Always,
    Empty,
    Degraded,
    Interrupted,
    Overloaded,
    MissingCompletion,
    Duration,
    HealthyStream,
    Cancelled,
    NoAudio,
    NotReached,
}

impl FinalPassReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::StreamingOnly => "streaming-only",
            Self::Always => "always",
            Self::Empty => "empty",
            Self::Degraded => "degraded",
            Self::Interrupted => "interrupted",
            Self::Overloaded => "overloaded",
            Self::MissingCompletion => "missing-completion",
            Self::Duration => "duration",
            Self::HealthyStream => "healthy-stream",
            Self::Cancelled => "cancelled",
            Self::NoAudio => "no-audio",
            Self::NotReached => "not-reached",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FinalPassKind {
    #[default]
    None,
    QwenAudio3Native,
    AlibabaCompatible,
}

impl FinalPassKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::QwenAudio3Native => "qwen-audio3-native",
            Self::AlibabaCompatible => "alibaba-compatible",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SelectedResult {
    #[default]
    Pending,
    Streaming,
    QwenAudio3Native,
    AlibabaCompatibleFinal,
    LocalPrimary,
    LocalFallback,
    None,
}

impl SelectedResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Streaming => "streaming",
            Self::QwenAudio3Native => "qwen-audio3-native",
            Self::AlibabaCompatibleFinal => "alibaba-compatible-final",
            Self::LocalPrimary => "local-primary",
            Self::LocalFallback => "local-fallback",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    Unavailable,
    Configuration,
    Authentication,
    PermissionDenied,
    Connection,
    Timeout,
    RateLimited,
    Overloaded,
    Protocol,
    InvalidResponse,
    Service,
    LocalBackend,
    Worker,
    Io,
    #[default]
    #[serde(other)]
    Unknown,
}

impl FailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Configuration => "configuration",
            Self::Authentication => "authentication",
            Self::PermissionDenied => "permission-denied",
            Self::Connection => "connection",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate-limited",
            Self::Overloaded => "overloaded",
            Self::Protocol => "protocol",
            Self::InvalidResponse => "invalid-response",
            Self::Service => "service",
            Self::LocalBackend => "local-backend",
            Self::Worker => "worker",
            Self::Io => "io",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SupportPayload {
    pub schema_version: u32,
    pub runtime: RuntimeSummary,
    pub config: SafeConfigSummary,
    pub session: Option<SessionDiagnostics>,
}

impl SupportPayload {
    pub fn new(config: &Config, snapshot: Option<&Snapshot>) -> Self {
        Self {
            schema_version: DIAGNOSTICS_SCHEMA_VERSION,
            runtime: RuntimeSummary::from_snapshot(snapshot),
            config: SafeConfigSummary::from_config(config),
            session: snapshot.and_then(|snapshot| snapshot.diagnostics.session.clone()),
        }
    }

    pub fn format_text(&self) -> String {
        let mut output = format!("Voice Input diagnostics (schema {})\n", self.schema_version);
        if self.runtime.available {
            let phase = self.runtime.phase.map(phase_name).unwrap_or("unavailable");
            match self.runtime.updated_at_ms {
                Some(updated_at_ms) => {
                    let _ = writeln!(
                        output,
                        "Runtime: available (phase={phase}, updated-at-ms={updated_at_ms})"
                    );
                }
                None => {
                    let _ = writeln!(output, "Runtime: available (phase={phase})");
                }
            }
        } else {
            output.push_str("Runtime: unavailable\n");
        }
        let _ = writeln!(
            output,
            "Configuration: provider={}, final-pass={}, fallback={}, audio3-native-mode={}, audio3-language-hints={}, audio3-heartbeat={}, audio3-max-sentence-silence-ms={}, audio3-semantic-punctuation={}, audio3-vocabulary-count={}",
            self.config.provider.as_str(),
            enabled_name(self.config.final_pass_enabled),
            enabled_name(self.config.fallback_enabled),
            self.config.audio3_native_final_pass_mode.as_str(),
            enabled_name(self.config.audio3_language_hints_enabled),
            enabled_name(self.config.audio3_heartbeat_enabled),
            self.config.audio3_max_sentence_silence_ms,
            enabled_name(self.config.audio3_semantic_punctuation_enabled),
            self.config.audio3_vocabulary_count
        );
        match &self.session {
            Some(session) => session.format_safe_text(&mut output),
            None => output.push_str("Session: none\n"),
        }
        output
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeSummary {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u128>,
}

impl RuntimeSummary {
    fn from_snapshot(snapshot: Option<&Snapshot>) -> Self {
        match snapshot {
            Some(snapshot) => Self {
                available: true,
                phase: Some(snapshot.phase),
                updated_at_ms: Some(snapshot.updated_at_ms),
            },
            None => Self {
                available: false,
                phase: None,
                updated_at_ms: None,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SafeConfigSummary {
    pub provider: Provider,
    pub final_pass_enabled: bool,
    pub fallback_enabled: bool,
    pub audio3_native_final_pass_mode: NativeFinalPassMode,
    pub audio3_language_hints_enabled: bool,
    pub audio3_heartbeat_enabled: bool,
    pub audio3_max_sentence_silence_ms: u32,
    pub audio3_semantic_punctuation_enabled: bool,
    pub audio3_vocabulary_count: usize,
}

impl SafeConfigSummary {
    fn from_config(config: &Config) -> Self {
        let final_pass_enabled = match config.asr.provider {
            AsrProvider::LocalCli => false,
            AsrProvider::AlibabaQwenRealtime => config.asr.alibaba.final_pass_enabled,
            AsrProvider::AlibabaQwenAudio3 => {
                config.asr.alibaba_audio3.native_final_pass_mode
                    != NativeFinalPassMode::StreamingOnly
            }
        };
        Self {
            provider: config.asr.provider.into(),
            final_pass_enabled,
            fallback_enabled: config.asr.fallback_to_local,
            audio3_native_final_pass_mode: config.asr.alibaba_audio3.native_final_pass_mode,
            audio3_language_hints_enabled: config.asr.alibaba_audio3.language_hints_enabled,
            audio3_heartbeat_enabled: config.asr.alibaba_audio3.heartbeat_enabled,
            audio3_max_sentence_silence_ms: config.asr.alibaba_audio3.max_sentence_silence_ms,
            audio3_semantic_punctuation_enabled: config
                .asr
                .alibaba_audio3
                .semantic_punctuation_enabled,
            audio3_vocabulary_count: config.asr.alibaba_audio3.vocabulary.len(),
        }
    }
}

fn schema_version() -> u32 {
    DIAGNOSTICS_SCHEMA_VERSION
}

fn append_latency(output: &mut String, label: &str, latency: Option<u64>) {
    if let Some(latency) = latency {
        let _ = write!(output, ", {label}={latency}");
    }
}

fn append_failure(output: &mut String, failure: Option<FailureKind>) {
    if let Some(failure) = failure {
        let _ = write!(output, ", failure={}", failure.as_str());
    }
}

fn enabled_name(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Arming => "arming",
        Phase::Recording => "recording",
        Phase::Transcribing => "transcribing",
        Phase::Refining => "refining",
        Phase::Outputting => "outputting",
        Phase::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn enums_use_kebab_case_and_unknown_failures_are_bounded() {
        assert_eq!(
            serde_json::to_value(OverallOutcome::InProgress).unwrap(),
            json!("in-progress")
        );
        assert_eq!(
            serde_json::to_value(FinalPassKind::QwenAudio3Native).unwrap(),
            json!("qwen-audio3-native")
        );
        assert_eq!(
            serde_json::to_value(SelectedResult::AlibabaCompatibleFinal).unwrap(),
            json!("alibaba-compatible-final")
        );
        assert_eq!(
            serde_json::to_value(SelectedResult::LocalPrimary).unwrap(),
            json!("local-primary")
        );
        assert_eq!(
            serde_json::to_value(FailureKind::Overloaded).unwrap(),
            json!("overloaded")
        );
        assert_eq!(
            serde_json::to_value(FinalPassDecision::NotApplicable).unwrap(),
            json!("not-applicable")
        );
        assert_eq!(
            serde_json::to_value(FinalPassReason::MissingCompletion).unwrap(),
            json!("missing-completion")
        );
        assert_eq!(
            serde_json::from_value::<FailureKind>(json!("future-private-error")).unwrap(),
            FailureKind::Unknown
        );
    }

    #[test]
    fn provider_error_codes_are_strictly_bounded_safe_tokens() {
        let maximum = "A".repeat(64);
        for valid in [
            "InvalidParameter",
            "Service-500",
            "quota.rate_limited",
            &maximum,
        ] {
            let code = ProviderErrorCode::try_new(valid).expect("valid provider code");
            assert_eq!(code.as_str(), valid);
            assert_eq!(serde_json::to_value(&code).unwrap(), json!(valid));
            assert_eq!(
                serde_json::from_value::<ProviderErrorCode>(json!(valid)).unwrap(),
                code
            );
        }

        for invalid in [
            "",
            "contains space",
            "path/value",
            "line\nbreak",
            "私密内容",
            &"A".repeat(65),
        ] {
            assert!(ProviderErrorCode::try_new(invalid).is_none());
            assert!(serde_json::from_value::<ProviderErrorCode>(json!(invalid)).is_err());
        }
    }

    #[test]
    fn malformed_persisted_provider_error_code_does_not_reject_snapshot() {
        let mut snapshot = crate::state::Snapshot::idle(&Config::default());
        snapshot.diagnostics.start_session(
            25,
            Provider::AlibabaQwenAudio3,
            FinalPassKind::None,
            false,
        );
        snapshot
            .diagnostics
            .update_session(25, |session| session.streaming.ready_latency_ms = Some(17));
        let mut persisted = serde_json::to_value(snapshot).unwrap();

        for malformed in [
            json!("contains space"),
            json!(65),
            json!({"code": "nested"}),
        ] {
            persisted["diagnostics"]["session"]["streaming"]["provider_error_code"] = malformed;
            let restored: crate::state::Snapshot =
                serde_json::from_value(persisted.clone()).expect("snapshot must remain readable");
            let streaming = &restored.diagnostics.session.unwrap().streaming;
            assert_eq!(streaming.provider_error_code, None);
            assert_eq!(streaming.ready_latency_ms, Some(17));
        }
    }

    #[test]
    fn streaming_event_diagnostics_are_aggregate_and_safe() {
        let mut session =
            SessionDiagnostics::new(24, Provider::AlibabaQwenAudio3, FinalPassKind::None, false);
        session.streaming.ready_latency_ms = Some(150);
        session.streaming.first_partial_latency_ms = Some(900);
        session.streaming.first_nonempty_partial_latency_ms = Some(1_200);
        session.streaming.last_result_latency_ms = Some(2_500);
        session.streaming.partial_event_count = 3;
        session.streaming.nonempty_partial_event_count = 2;
        session.streaming.segment_final_event_count = 1;
        session.streaming.audio_packet_count = 20;
        session.streaming.audio_sent_duration_ms = Some(2_560);
        session.streaming.max_audio_queue_delay_ms = Some(151);
        session.streaming.last_audio_queue_delay_ms = Some(4);
        session.streaming.finish_sent_latency_ms = Some(2_600);
        session.streaming.task_finished_latency_ms = Some(2_650);
        session.streaming.task_failed_latency_ms = Some(2_700);
        session.streaming.failure_kind = Some(FailureKind::Service);
        session.streaming.provider_error_code = ProviderErrorCode::try_new("ServiceUnavailable");

        let mut text = String::new();
        session.format_safe_text(&mut text);
        let json = serde_json::to_value(&session).unwrap();
        assert!(text.contains("first-partial-latency-ms=900"));
        assert!(text.contains("nonempty-partial-events=2"));
        assert!(text.contains("provider-error-code=ServiceUnavailable"));
        assert_eq!(
            json["streaming"]["first_nonempty_partial_latency_ms"],
            1_200
        );
        assert_eq!(
            json["streaming"]["provider_error_code"],
            "ServiceUnavailable"
        );
        assert!(!text.contains("transcript"));
        assert!(!json.to_string().contains("transcript"));
    }

    #[test]
    fn missing_fields_use_inactive_compatibility_defaults() {
        let diagnostics: Diagnostics = serde_json::from_value(json!({})).unwrap();
        assert_eq!(diagnostics, Diagnostics::inactive());

        let session: SessionDiagnostics = serde_json::from_value(json!({})).unwrap();
        assert_eq!(session.session_id, 0);
        assert_eq!(session.asr_outcome, OverallOutcome::InProgress);
        assert_eq!(session.streaming.status, StageStatus::Inactive);
        assert_eq!(session.final_pass.kind, FinalPassKind::None);
        assert_eq!(session.final_pass.configured_mode, None);
        assert_eq!(
            session.final_pass.decision,
            FinalPassDecision::NotApplicable
        );
        assert_eq!(session.final_pass.reason, None);
        assert_eq!(session.local_primary.status, StageStatus::Pending);
        assert_eq!(session.local_fallback.status, StageStatus::Inactive);
        assert_eq!(session.selected_result, SelectedResult::Pending);
    }

    #[test]
    fn new_session_activates_only_configured_stages() {
        let diagnostics = Diagnostics {
            schema_version: DIAGNOSTICS_SCHEMA_VERSION,
            session: Some(SessionDiagnostics::new(
                42,
                Provider::AlibabaQwenAudio3,
                FinalPassKind::QwenAudio3Native,
                true,
            )),
        };
        let session = diagnostics.session.unwrap();
        assert_eq!(session.streaming.status, StageStatus::Pending);
        assert_eq!(session.final_pass.status, StageStatus::Pending);
        assert_eq!(session.local_primary.status, StageStatus::Inactive);
        assert_eq!(session.local_fallback.status, StageStatus::Pending);
    }

    #[test]
    fn starting_a_session_replaces_the_previous_record() {
        let mut diagnostics = Diagnostics::inactive();
        diagnostics.start_session(
            3,
            Provider::AlibabaQwenRealtime,
            FinalPassKind::AlibabaCompatible,
            true,
        );
        diagnostics.update_session(3, |session| {
            session.asr_outcome = OverallOutcome::Failed;
        });

        diagnostics.start_session(
            4,
            Provider::AlibabaQwenAudio3,
            FinalPassKind::QwenAudio3Native,
            false,
        );

        let session = diagnostics.session.unwrap();
        assert_eq!(session.session_id, 4);
        assert_eq!(session.provider, Provider::AlibabaQwenAudio3);
        assert_eq!(session.asr_outcome, OverallOutcome::InProgress);
        assert_eq!(session.selected_result, SelectedResult::Pending);
    }

    #[test]
    fn stale_session_updates_are_rejected() {
        let mut diagnostics = Diagnostics::inactive();
        diagnostics.start_session(8, Provider::AlibabaQwenAudio3, FinalPassKind::None, false);

        assert!(!diagnostics.update_session(7, |session| {
            session.streaming.status = StageStatus::Failed;
        }));
        assert_eq!(
            diagnostics.session.as_ref().unwrap().streaming.status,
            StageStatus::Pending
        );
        assert!(diagnostics.update_session(8, |session| {
            session.streaming.status = StageStatus::InProgress;
        }));
    }

    #[test]
    fn same_id_updates_are_rejected_after_terminal_outcome() {
        let mut diagnostics = Diagnostics::inactive();
        diagnostics.start_session(8, Provider::AlibabaQwenAudio3, FinalPassKind::None, false);
        assert!(diagnostics.finish_session(
            8,
            OverallOutcome::Completed,
            SelectedResult::Streaming,
            17,
        ));

        assert!(!diagnostics.update_session(8, |session| {
            session.streaming.status = StageStatus::Failed;
            session.streaming.failure_kind = Some(FailureKind::Worker);
        }));
        assert!(!diagnostics.finish_session(8, OverallOutcome::Failed, SelectedResult::None, 99,));

        let session = diagnostics.session.unwrap();
        assert_eq!(session.asr_outcome, OverallOutcome::Completed);
        assert_eq!(session.streaming.status, StageStatus::Skipped);
        assert_eq!(session.streaming.failure_kind, None);
        assert_eq!(session.final_pass.decision, FinalPassDecision::Skipped);
        assert_eq!(
            session.final_pass.reason,
            Some(FinalPassReason::StreamingOnly)
        );
        assert_eq!(session.total_asr_latency_ms, Some(17));
    }

    #[test]
    fn terminal_session_normalizes_pending_final_pass_without_losing_specific_reason() {
        for (session_id, outcome, reason, expected_reason) in [
            (
                10,
                OverallOutcome::Failed,
                None,
                FinalPassReason::NotReached,
            ),
            (
                11,
                OverallOutcome::Cancelled,
                Some(FinalPassReason::Cancelled),
                FinalPassReason::Cancelled,
            ),
            (
                12,
                OverallOutcome::Empty,
                Some(FinalPassReason::NoAudio),
                FinalPassReason::NoAudio,
            ),
        ] {
            let mut diagnostics = Diagnostics::inactive();
            diagnostics.start_session(
                session_id,
                Provider::AlibabaQwenAudio3,
                FinalPassKind::QwenAudio3Native,
                false,
            );
            diagnostics.update_session(session_id, |session| {
                session.final_pass.reason = reason;
            });

            assert!(diagnostics.finish_session(session_id, outcome, SelectedResult::None, 5,));
            assert!(!diagnostics.update_session(session_id, |session| {
                session.final_pass.decision = FinalPassDecision::Invoked;
                session.final_pass.reason = Some(FinalPassReason::Always);
            }));
            assert!(!diagnostics.finish_session(
                session_id,
                OverallOutcome::Completed,
                SelectedResult::QwenAudio3Native,
                99,
            ));
            let session = diagnostics.session.unwrap();
            assert_eq!(session.asr_outcome, outcome);
            assert_eq!(session.total_asr_latency_ms, Some(5));
            assert_eq!(session.final_pass.status, StageStatus::Skipped);
            assert_eq!(session.final_pass.decision, FinalPassDecision::Skipped);
            assert_eq!(session.final_pass.reason, Some(expected_reason));
        }
    }

    #[test]
    fn invoked_final_pass_remains_invoked_when_session_finishes() {
        let mut diagnostics = Diagnostics::inactive();
        diagnostics.start_session(
            14,
            Provider::AlibabaQwenAudio3,
            FinalPassKind::QwenAudio3Native,
            false,
        );
        diagnostics.update_session(14, |session| {
            session.final_pass.status = StageStatus::Completed;
            session.final_pass.decision = FinalPassDecision::Invoked;
            session.final_pass.reason = Some(FinalPassReason::Duration);
        });

        assert!(diagnostics.finish_session(
            14,
            OverallOutcome::Completed,
            SelectedResult::QwenAudio3Native,
            8,
        ));
        let session = diagnostics.session.unwrap();
        assert_eq!(session.final_pass.status, StageStatus::Completed);
        assert_eq!(session.final_pass.decision, FinalPassDecision::Invoked);
        assert_eq!(session.final_pass.reason, Some(FinalPassReason::Duration));
    }

    #[test]
    fn normalized_terminal_final_pass_has_safe_text_and_json() {
        let mut diagnostics = Diagnostics::inactive();
        diagnostics.start_session(
            13,
            Provider::AlibabaQwenAudio3,
            FinalPassKind::QwenAudio3Native,
            false,
        );
        diagnostics.finish_session(13, OverallOutcome::Failed, SelectedResult::None, 9);
        let session = diagnostics.session.unwrap();

        let mut text = String::new();
        session.format_safe_text(&mut text);
        let json = serde_json::to_value(&session).unwrap();
        assert!(text.contains("decision=skipped"));
        assert!(text.contains("reason=not-reached"));
        assert!(!text.contains("decision=pending"));
        assert_eq!(json["final_pass"]["decision"], "skipped");
        assert_eq!(json["final_pass"]["reason"], "not-reached");
        assert!(!json.to_string().contains("transcript"));
    }

    #[test]
    fn local_primary_models_success_empty_and_bounded_failure() {
        for (session_id, status, failure_kind) in [
            (20, StageStatus::Completed, None),
            (21, StageStatus::Empty, None),
            (22, StageStatus::Failed, Some(FailureKind::LocalBackend)),
        ] {
            let mut diagnostics = Diagnostics::inactive();
            diagnostics.start_session(session_id, Provider::LocalCli, FinalPassKind::None, true);
            let session = diagnostics.session.as_ref().unwrap();
            assert_eq!(session.local_primary.status, StageStatus::Pending);
            assert_eq!(session.streaming.status, StageStatus::Inactive);
            assert_eq!(session.local_fallback.status, StageStatus::Inactive);

            assert!(diagnostics.update_session(session_id, |session| {
                session.local_primary.status = status;
                session.local_primary.latency_ms = Some(12);
                session.local_primary.failure_kind = failure_kind;
            }));
            assert!(diagnostics.finish_session(
                session_id,
                if status == StageStatus::Completed {
                    OverallOutcome::Completed
                } else if status == StageStatus::Empty {
                    OverallOutcome::Empty
                } else {
                    OverallOutcome::Failed
                },
                if status == StageStatus::Completed {
                    SelectedResult::LocalPrimary
                } else {
                    SelectedResult::None
                },
                12,
            ));

            let session = diagnostics.session.unwrap();
            assert_eq!(session.local_primary.status, status);
            assert_eq!(session.local_primary.latency_ms, Some(12));
            assert_eq!(session.local_primary.failure_kind, failure_kind);
        }
    }

    #[test]
    fn audio3_mode_and_safe_controls_are_serialized_without_private_values() {
        const TERM: &str = "private-diagnostics-vocabulary-sentinel";
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.native_final_pass_mode = NativeFinalPassMode::Adaptive;
        config.asr.alibaba_audio3.language_hints_enabled = true;
        config.asr.alibaba_audio3.heartbeat_enabled = true;
        config.asr.alibaba_audio3.vocabulary = vec![crate::config::Audio3VocabularyTerm {
            term: TERM.into(),
            weight: 50,
        }];

        let payload = SupportPayload::new(&config, None);
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["schema_version"], 3);
        assert_eq!(json["config"]["audio3_native_final_pass_mode"], "adaptive");
        assert_eq!(json["config"]["audio3_language_hints_enabled"], true);
        assert_eq!(json["config"]["audio3_heartbeat_enabled"], true);
        assert_eq!(json["config"]["audio3_max_sentence_silence_ms"], 800);
        assert_eq!(json["config"]["audio3_semantic_punctuation_enabled"], false);
        assert_eq!(json["config"]["audio3_vocabulary_count"], 1);
        assert!(!json.to_string().contains(TERM));

        let mut diagnostics = Diagnostics::inactive();
        diagnostics.start_session(
            23,
            Provider::AlibabaQwenAudio3,
            FinalPassKind::QwenAudio3Native,
            true,
        );
        assert!(diagnostics.configure_audio3_native_final_pass(23, NativeFinalPassMode::Adaptive));
        diagnostics.update_session(23, |session| {
            session.final_pass.decision = FinalPassDecision::Invoked;
            session.final_pass.reason = Some(FinalPassReason::MissingCompletion);
        });
        let session = diagnostics.session.unwrap();
        assert_eq!(
            session.final_pass.configured_mode,
            Some(NativeFinalPassMode::Adaptive)
        );
        assert_eq!(session.final_pass.decision, FinalPassDecision::Invoked);
        assert_eq!(
            session.final_pass.reason,
            Some(FinalPassReason::MissingCompletion)
        );
    }

    #[test]
    fn audio3_vocabulary_terms_never_enter_support_diagnostics() {
        const SENTINEL: &str = "private-diagnostics-vocabulary-sentinel";
        let mut config = Config::default();
        config.asr.provider = AsrProvider::AlibabaQwenAudio3;
        config.asr.alibaba_audio3.vocabulary = vec![crate::config::Audio3VocabularyTerm {
            term: SENTINEL.into(),
            weight: 50,
        }];

        let payload = SupportPayload::new(&config, None);
        assert!(!payload.format_text().contains(SENTINEL));
        assert!(!serde_json::to_string(&payload).unwrap().contains(SENTINEL));
    }

    #[test]
    fn terminal_session_skips_only_stages_that_were_never_invoked() {
        let mut diagnostics = Diagnostics::inactive();
        diagnostics.start_session(
            9,
            Provider::AlibabaQwenAudio3,
            FinalPassKind::QwenAudio3Native,
            true,
        );
        diagnostics.update_session(9, |session| {
            session.streaming.status = StageStatus::Completed;
            session.final_pass.status = StageStatus::Empty;
        });
        diagnostics.finish_session(9, OverallOutcome::Empty, SelectedResult::None, 21);

        let session = diagnostics.session.unwrap();
        assert_eq!(session.asr_outcome, OverallOutcome::Empty);
        assert_eq!(session.streaming.status, StageStatus::Completed);
        assert_eq!(session.final_pass.status, StageStatus::Empty);
        assert_eq!(session.local_primary.status, StageStatus::Inactive);
        assert_eq!(session.local_fallback.status, StageStatus::Skipped);
        assert_eq!(session.total_asr_latency_ms, Some(21));
    }
}
