mod streaming;

use std::path::Path;

use anyhow::{Result, bail};

use crate::{
    backend::{AsrBackend, AsrSessionHandle, AudioSpec},
    config::Config,
};

pub struct QwenAudio3Backend;

impl QwenAudio3Backend {
    pub fn new() -> Self {
        Self
    }
}

impl AsrBackend for QwenAudio3Backend {
    fn spawn_session(&self, config: &Config, spec: AudioSpec) -> Result<AsrSessionHandle> {
        streaming::spawn_session(config, spec)
    }

    fn transcribe_file(&self, _config: &Config, _wav_path: &Path) -> Result<String> {
        bail!("experimental Qwen-Audio-3 backend does not support file transcription")
    }
}
