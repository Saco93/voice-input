mod local_cli;
mod qwen_batch;
mod qwen_realtime;
mod text;

use std::{
    path::Path,
    sync::mpsc::{Receiver, Sender},
    thread::JoinHandle,
};

use anyhow::Result;

use crate::config::{AsrProvider, Config};

pub use qwen_batch::transcribe_full_audio as transcribe_alibaba_full_audio;
pub use text::apply_script_conversion;

#[derive(Debug, Clone, Copy)]
pub struct AudioSpec {
    pub sample_rate_hz: u32,
}

#[derive(Debug)]
pub enum AsrControl {
    AppendPcm16(Vec<i16>),
    Finish,
    Cancel,
}

#[derive(Debug, Clone)]
pub enum AsrEvent {
    Ready,
    SpeechStarted,
    SpeechStopped,
    Partial { committed: String, unstable: String },
    SegmentFinal { text: String },
    Final { text: String },
    Error { message: String },
}

pub struct AsrSessionHandle {
    pub control_tx: Sender<AsrControl>,
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
    }
}

pub fn transcribe(config: &Config, wav_path: &Path) -> Result<String> {
    build(config).transcribe_file(config, wav_path)
}
