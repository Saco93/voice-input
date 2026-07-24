use std::process::Command;

use anyhow::{Context, Result};

use crate::config::Language;

pub fn extract_transcript(stdout: &str) -> String {
    let cleaned = strip_ansi(stdout);
    let mut candidates = Vec::new();

    for raw_line in cleaned.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(transcript) = transcript_from_line(line) {
            return transcript;
        }

        if is_backend_noise(line) {
            continue;
        }

        candidates.push(line.to_string());
    }

    candidates.pop().unwrap_or_default()
}

pub fn apply_script_conversion(language: Language, transcript: &str) -> Result<String> {
    let Some(profile) = language.opencc_profile() else {
        return Ok(transcript.to_string());
    };

    let output = Command::new("opencc")
        .arg("-c")
        .arg(profile)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(transcript.as_bytes())?;
            }
            child.wait_with_output()
        })
        .context("failed to run opencc for script conversion")?;

    if !output.status.success() {
        return Ok(transcript.to_string());
    }

    let converted = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if converted.is_empty() {
        Ok(transcript.to_string())
    } else {
        Ok(converted)
    }
}

fn transcript_from_line(line: &str) -> Option<String> {
    if let Some((_, suffix)) = line.rsplit_once("Transcribed:") {
        let suffix = suffix.trim();
        return Some(extract_quoted(suffix).unwrap_or_else(|| suffix.to_string()));
    }

    if line.contains("Transcription completed") {
        if let Some(transcript) = extract_quoted(line) {
            return Some(transcript);
        }
    }

    None
}

fn is_backend_noise(line: &str) -> bool {
    line.starts_with("Loading audio file:")
        || line.starts_with("Audio format:")
        || line.starts_with("Processing ")
        || line.starts_with("Model ")
        || line.starts_with("Using ")
        || line.starts_with("whisper_")
        || line.starts_with("main: ")
        || looks_like_timestamped_log(line)
}

fn looks_like_timestamped_log(line: &str) -> bool {
    let mut chars = line.chars();
    let starts_with_date =
        chars.by_ref().take(4).all(|ch| ch.is_ascii_digit()) && line.get(4..5) == Some("-");

    starts_with_date
        && (line.contains(" INFO ")
            || line.contains(" DEBUG ")
            || line.contains(" WARN ")
            || line.contains(" ERROR "))
}

fn extract_quoted(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let end = line.rfind('"')?;
    if end > start {
        Some(line[start + 1..end].to_string())
    } else {
        None
    }
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::extract_transcript;

    #[test]
    fn extracts_transcript_from_transcribed_prefix() {
        let stdout = r#"
Loading audio file: "/tmp/demo.wav"
Audio format: 16000 Hz, 1 channel(s), Int
2026-04-08T05:22:21.308511Z INFO Using backend whisper
Transcribed: "you"
"#;

        assert_eq!(extract_transcript(stdout), "you");
    }

    #[test]
    fn extracts_last_non_noise_line_when_backend_emits_plain_text() {
        let stdout = r#"
Loading audio file: "/tmp/demo.wav"
Processing 22528 samples (1.41s)...
hello world
"#;

        assert_eq!(extract_transcript(stdout), "hello world");
    }

    #[test]
    fn extracts_transcript_from_timestamped_completion_log() {
        let stdout = r#"
Loading audio file: "/tmp/tmpDvxvRi"
Audio format: 16000 Hz, 1 channel(s), Int
Processing 57344 samples (3.58s)...
2026-04-08T07:24:04.273641Z INFO Using local whisper transcription
2026-04-08T07:24:04.273653Z INFO Loading whisper model from /home/example/.local/share/voxtype/models/ggml-base.en.bin
2026-04-08T07:24:04.321945Z INFO Model loaded in 0.05s
2026-04-08T07:24:05.036914Z INFO Transcription completed in 0.71s: "hello from voice-input"
"#;

        assert_eq!(extract_transcript(stdout), "hello from voice-input");
    }

    #[test]
    fn never_returns_noise_blob_when_stdout_only_contains_backend_logs() {
        let stdout = r#"
Loading audio file: "/tmp/tmpDvxvRi"
Audio format: 16000 Hz, 1 channel(s), Int
Processing 57344 samples (3.58s)...
2026-04-08T07:24:04.273641Z INFO Using local whisper transcription
"#;

        assert_eq!(extract_transcript(stdout), "");
    }
}
