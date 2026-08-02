mod native;
mod streaming;

use std::path::Path;

use anyhow::Result;

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

    fn transcribe_file(&self, config: &Config, wav_path: &Path) -> Result<String> {
        Ok(native::transcribe_full_audio(config, wav_path)?.unwrap_or_default())
    }
}

pub(crate) use native::transcribe_full_audio;
