use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::{
    config::{AsrProvider, Config},
    state::{Phase, Snapshot},
};

pub const DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;

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
            "finalize-latency-ms",
            self.streaming.finalize_latency_ms,
        );
        append_failure(output, self.streaming.failure_kind);
        output.push('\n');

        let _ = write!(
            output,
            "Final pass: {} ({})",
            self.final_pass.kind.as_str(),
            self.final_pass.status.as_str()
        );
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
    pub finalize_latency_ms: Option<u64>,
    pub failure_kind: Option<FailureKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct FinalPassStage {
    pub kind: FinalPassKind,
    pub status: StageStatus,
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
            "Configuration: provider={}, final-pass={}, fallback={}",
            self.config.provider.as_str(),
            enabled_name(self.config.final_pass_enabled),
            enabled_name(self.config.fallback_enabled)
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
}

impl SafeConfigSummary {
    fn from_config(config: &Config) -> Self {
        let final_pass_enabled = match config.asr.provider {
            AsrProvider::LocalCli => false,
            AsrProvider::AlibabaQwenRealtime => config.asr.alibaba.final_pass_enabled,
            AsrProvider::AlibabaQwenAudio3 => config.asr.alibaba_audio3.native_final_pass_enabled,
        };
        Self {
            provider: config.asr.provider.into(),
            final_pass_enabled,
            fallback_enabled: config.asr.fallback_to_local,
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
            serde_json::from_value::<FailureKind>(json!("future-private-error")).unwrap(),
            FailureKind::Unknown
        );
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
        assert_eq!(session.total_asr_latency_ms, Some(17));
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
