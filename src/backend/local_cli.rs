use std::{
    io::Read,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::{Arc, atomic::AtomicBool, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    backend::{
        ASR_CONTROL_QUEUE_CAPACITY, AsrBackend, AsrEvent, AsrSessionHandle, AsrSessionOptions,
    },
    config::Config,
};

use super::text::{apply_script_conversion, extract_transcript};

const BACKEND_OUTPUT_MAX_BYTES: u64 = 2 * 1024 * 1024;
const BACKEND_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug)]
pub(crate) struct EmptyTranscriptError;

impl std::fmt::Display for EmptyTranscriptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ASR backend returned an empty transcript")
    }
}

impl std::error::Error for EmptyTranscriptError {}

pub struct LocalCliBackend;

impl LocalCliBackend {
    pub fn new() -> Self {
        Self
    }
}

impl AsrBackend for LocalCliBackend {
    fn spawn_session(
        &self,
        _config: &Config,
        _options: AsrSessionOptions,
    ) -> Result<AsrSessionHandle> {
        let (control_tx, control_rx) = mpsc::sync_channel(ASR_CONTROL_QUEUE_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel();

        let join = thread::spawn(move || {
            let _ = event_tx.send(AsrEvent::Ready);
            drop(control_rx);
            Ok(())
        });

        Ok(AsrSessionHandle {
            control_tx,
            abort_flag: Arc::new(AtomicBool::new(false)),
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

        let output = run_command_with_timeout(&mut command, local_backend_timeout(config))
            .with_context(|| format!("failed to run backend `{}`", config.asr.backend_command))?;

        if !output.status.success() {
            return Err(failed_backend_output(&output));
        }

        let transcript = extract_transcript(&String::from_utf8_lossy(&output.stdout));
        if transcript.is_empty() {
            return Err(EmptyTranscriptError.into());
        }

        apply_script_conversion(config.asr.language, &transcript)
    }
}

fn failed_backend_output(output: &Output) -> anyhow::Error {
    // stderr can contain recognized speech or provider diagnostics. Keep only
    // the bounded process status, which is sufficient to categorize failure.
    anyhow!("ASR backend failed with {}", output.status)
}

fn local_backend_timeout(config: &Config) -> Duration {
    let finalize_seconds = config.asr.finalize_timeout_ms.div_ceil(1_000);
    Duration::from_secs(
        config
            .audio
            .max_duration_secs
            .max(finalize_seconds)
            .clamp(30, 3_600),
    )
}

fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> Result<Output> {
    // Put the backend in its own process group so a timeout also terminates
    // descendants that inherited stdout or stderr. Otherwise pipe readers can
    // remain blocked after the direct child has been killed.
    command.process_group(0);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start backend process")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("backend process did not provide stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("backend process did not provide stderr"))?;
    let stdout_handle = read_output_bounded(stdout);
    let stderr_handle = read_output_bounded(stderr);
    let started = Instant::now();

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(BACKEND_POLL_INTERVAL),
            Ok(None) => {
                terminate_process_group(&mut child);
                let _ = join_output_reader(stdout_handle, "stdout");
                let _ = join_output_reader(stderr_handle, "stderr");
                bail!("ASR backend timed out after {} seconds", timeout.as_secs());
            }
            Err(error) => {
                terminate_process_group(&mut child);
                let _ = join_output_reader(stdout_handle, "stdout");
                let _ = join_output_reader(stderr_handle, "stderr");
                return Err(error).context("failed to poll backend process");
            }
        }
    };

    let stdout = join_output_reader(stdout_handle, "stdout")?;
    let stderr = join_output_reader(stderr_handle, "stderr")?;
    if stdout.len() as u64 > BACKEND_OUTPUT_MAX_BYTES
        || stderr.len() as u64 > BACKEND_OUTPUT_MAX_BYTES
    {
        bail!("ASR backend output exceeds {BACKEND_OUTPUT_MAX_BYTES} bytes");
    }

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn terminate_process_group(child: &mut Child) {
    if let Ok(process_group) = i32::try_from(child.id()) {
        // SAFETY: process_group is the positive PID assigned to the child and
        // negating it asks kill(2) to signal only that child's process group.
        let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn read_output_bounded(
    reader: impl Read + Send + 'static,
) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        reader
            .take(BACKEND_OUTPUT_MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_output_reader(
    handle: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    label: &str,
) -> Result<Vec<u8>> {
    match handle.join() {
        Ok(result) => result.with_context(|| format!("failed to read backend {label}")),
        Err(_) => bail!("backend {label} reader panicked"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_process_timeout_terminates_a_hung_command() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2"]);
        let started = Instant::now();
        let error = run_command_with_timeout(&mut command, Duration::from_millis(50))
            .expect_err("sleep must exceed the deadline");
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn empty_transcript_marker_survives_error_context() {
        let error = anyhow::Error::new(EmptyTranscriptError).context("local ASR failed");
        assert!(crate::backend::is_empty_transcript_error(&error));
    }

    #[test]
    fn failed_backend_stderr_is_not_exposed() {
        const SENTINEL: &str = "private-local-stderr-sentinel";
        let output = Command::new("sh")
            .args(["-c", &format!("printf {SENTINEL} >&2; exit 23")])
            .output()
            .unwrap();
        let error = failed_backend_output(&output);

        assert!(error.to_string().contains("exit status: 23"));
        assert!(!format!("{error:#}").contains(SENTINEL));
    }

    #[test]
    fn backend_process_output_is_collected() {
        let mut command = Command::new("printf");
        command.arg("transcript");
        let output = run_command_with_timeout(&mut command, Duration::from_secs(1)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"transcript");
    }
}
