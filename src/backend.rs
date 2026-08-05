mod local_cli;
mod qwen_audio3;
mod qwen_batch;
mod qwen_realtime;
mod text;

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::Instant,
};

use anyhow::Result;

use crate::{
    config::{AsrProvider, Config},
    diagnostics::{FailureKind, ProviderErrorCode},
};

pub(crate) use qwen_audio3::transcribe_full_audio as transcribe_qwen_audio3_full_audio;
pub use qwen_batch::transcribe_full_audio as transcribe_alibaba_full_audio;
pub use text::apply_script_conversion;

#[derive(Debug, Clone, Copy)]
pub struct AudioSpec {
    pub sample_rate_hz: u32,
}

// At the 16 kHz capture rate, 128 packets bound queued PCM to roughly 16
// seconds (512 KiB) and still accommodate the maximum 10-second pre-roll.
pub const ASR_CONTROL_QUEUE_CAPACITY: usize = 128;

#[derive(Debug)]
pub enum AsrControl {
    AppendPcm16 {
        samples: Vec<i16>,
        enqueued_at: Instant,
    },
    Finish,
}

impl AsrControl {
    pub fn append_pcm16(samples: Vec<i16>) -> Self {
        Self::AppendPcm16 {
            samples,
            enqueued_at: Instant::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AsrEvent {
    Ready,
    SpeechStarted,
    SpeechStopped,
    RealtimeRestarting,
    RealtimeRestarted,
    RealtimeTranscriptDelayed,
    AudioDeliveryCompleted {
        packet_count: u64,
        sample_count: u64,
        max_queue_delay_ms: u64,
        last_queue_delay_ms: u64,
    },
    Partial {
        committed: String,
        unstable: String,
    },
    SegmentFinal {
        text: String,
    },
    Final {
        text: String,
    },
    Finished,
    Error {
        kind: FailureKind,
    },
    TaskFailed {
        kind: FailureKind,
        provider_error_code: Option<ProviderErrorCode>,
    },
}

pub struct AsrSessionHandle {
    pub control_tx: SyncSender<AsrControl>,
    pub abort_flag: Arc<AtomicBool>,
    pub event_rx: Receiver<AsrEvent>,
    pub join: JoinHandle<Result<()>>,
}

pub trait AsrBackend: Send + Sync {
    fn spawn_session(&self, config: &Config, spec: AudioSpec) -> Result<AsrSessionHandle>;

    fn transcribe_file(&self, config: &Config, wav_path: &Path) -> Result<String>;
}

pub fn build(config: &Config) -> Box<dyn AsrBackend> {
    match config.asr.provider {
        AsrProvider::LocalCli => Box::new(local_cli::LocalCliBackend::new()),
        AsrProvider::AlibabaQwenRealtime => Box::new(qwen_realtime::QwenRealtimeBackend::new()),
        AsrProvider::AlibabaQwenAudio3 => Box::new(qwen_audio3::QwenAudio3Backend::new()),
    }
}

pub fn transcribe(config: &Config, wav_path: &Path) -> Result<String> {
    build(config).transcribe_file(config, wav_path)
}

pub fn is_empty_transcript_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<local_cli::EmptyTranscriptError>())
}
