use std::{path::Path, process::Command, sync::mpsc, thread};

use anyhow::{Context, Result, bail};

use crate::{
    backend::{AsrBackend, AsrEvent, AsrSessionHandle, AudioSpec},
    config::Config,
};

use super::text::{apply_script_conversion, extract_transcript};

pub struct LocalCliBackend;

impl LocalCliBackend {
    pub fn new() -> Self {
        Self
    }
}

impl AsrBackend for LocalCliBackend {
    fn spawn_session(&self, _config: &Config, _spec: AudioSpec) -> Result<AsrSessionHandle> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let join = thread::spawn(move || {
            let _ = event_tx.send(AsrEvent::Ready);
            drop(control_rx);
            Ok(())
        });

        Ok(AsrSessionHandle {
            control_tx,
            event_rx,
            join,
        })
    }

    fn transcribe_file(&self, config: &Config, wav_path: &Path) -> Result<String> {
        let mut command = Command::new(&config.asr.backend_command);
        if !config.asr.engine.trim().is_empty() {
            command.arg("--engine").arg(&config.asr.engine);
        }
        if !config.asr.model.trim().is_empty() {
            command.arg("--model").arg(&config.asr.model);
        }

        command
            .arg("--language")
            .arg(config.asr.language.asr_code())
            .arg("transcribe")
            .arg(wav_path);

        let output = command
            .output()
            .with_context(|| format!("failed to run backend `{}`", config.asr.backend_command))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("ASR backend failed: {}", stderr.trim());
        }

        let transcript = extract_transcript(&String::from_utf8_lossy(&output.stdout));
        if transcript.is_empty() {
            bail!("ASR backend returned an empty transcript");
        }

        apply_script_conversion(config.asr.language, &transcript)
    }
}
