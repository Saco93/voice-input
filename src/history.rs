use std::{
    fs,
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::paths;

/// Identifies one completed transcription, independently of recording sessions
/// and daemon restarts. History contains no window, clipboard, or audio data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub id: u64,
    pub completed_at_ms: u64,
    pub text: String,
}

pub fn list() -> Result<Vec<Entry>> {
    let mut entries = read_entries(&paths::history_path()?)?;
    entries.reverse();
    Ok(entries)
}

pub fn get(id: u64) -> Result<Entry> {
    find_entry(&paths::history_path()?, id)
}

pub fn record(text: &str) -> Result<()> {
    let completed_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis()
        .try_into()
        .context("transcription completion time is too large")?;
    append_entry(&paths::history_path()?, text, completed_at_ms)
}

fn read_entries(path: &Path) -> Result<Vec<Entry>> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).context("failed to parse transcription history")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).context("failed to read transcription history"),
    }
}

fn find_entry(path: &Path, id: u64) -> Result<Entry> {
    read_entries(path)?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| anyhow!("transcription history entry {id} was not found"))
}

// The daemon is the only writer and serializes recording finalization. Readers
// use the atomic snapshot, so opening the picker cannot observe a partial write.
fn append_entry(path: &Path, text: &str, completed_at_ms: u64) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }

    let mut entries = read_entries(path)?;
    let id = entries
        .iter()
        .map(|entry| entry.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| anyhow!("transcription history IDs are exhausted"))?;
    entries.push(Entry {
        id,
        completed_at_ms,
        text: text.to_owned(),
    });

    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("transcription history path has no parent directory"))?;
    fs::create_dir_all(directory).context("failed to create transcription history directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .context("failed to secure transcription history directory")?;
    }

    let mut temporary = NamedTempFile::new_in(directory)
        .context("failed to create temporary transcription history file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .context("failed to secure transcription history file")?;
    }
    serde_json::to_writer(&mut temporary, &entries)
        .context("failed to serialize transcription history")?;
    temporary
        .write_all(b"\n")
        .context("failed to finish transcription history file")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("failed to save transcription history")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_history_is_empty_and_blank_results_are_not_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        assert!(read_entries(&path).unwrap().is_empty());
        append_entry(&path, " \n\t", 10).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn records_survive_reopening_and_keep_complete_unicode_and_multiline_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let text = format!(
            "保留原文，不截断。\n{}\n",
            "长文本 🦀 <b>plain</b> ".repeat(1000)
        );
        append_entry(&path, &text, 10).unwrap();
        // No in-memory store is retained between writes, just as after restart.
        append_entry(&path, "second", 20).unwrap();
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, text);
        assert_eq!(entries[0].completed_at_ms, 10);
        assert_eq!(entries[0].id, 1);
        assert_eq!(entries[1].id, 2);
        assert_eq!(find_entry(&path, 1).unwrap(), entries[0]);
        assert!(find_entry(&path, 3).is_err());
    }

    #[test]
    fn repeated_transcriptions_remain_distinct_and_lookup_does_not_consume_them() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        append_entry(&path, "same", 10).unwrap();
        append_entry(&path, "same", 10).unwrap();
        assert_ne!(
            find_entry(&path, 1).unwrap().id,
            find_entry(&path, 2).unwrap().id
        );
        let before = fs::read(&path).unwrap();
        find_entry(&path, 1).unwrap();
        find_entry(&path, 1).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn invalid_history_is_not_silently_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        fs::write(&path, b"invalid history").unwrap();
        assert!(append_entry(&path, "new", 10).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid history");
    }

    #[cfg(unix)]
    #[test]
    fn history_directory_and_file_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("voice-input");
        let path = directory.join("history.json");
        append_entry(&path, "private", 10).unwrap();
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        append_entry(&path, "still private", 20).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
