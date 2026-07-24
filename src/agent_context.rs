use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

const MAX_SESSION_SCAN_BYTES: u64 = 8 * 1024 * 1024;
const KITTY_QUERY_TIMEOUT_SECS: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Pi,
    Codex,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pi => "Pi",
            Self::Codex => "Codex",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentSessionLocator {
    kind: AgentKind,
    pid: u32,
    process_start_ticks: u64,
    session_id: String,
    session_path: PathBuf,
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone)]
pub struct AgentReference {
    pub agent: AgentKind,
    pub text: String,
}

pub fn capture_focused_session() -> Result<Option<AgentSessionLocator>> {
    let active = active_window()?;
    if !active.class.eq_ignore_ascii_case("kitty") {
        return Ok(None);
    }

    let Some(process) = focused_kitty_agent(active.pid)? else {
        return Ok(None);
    };

    match process.kind {
        AgentKind::Pi => resolve_pi_session(process.pid),
        AgentKind::Codex => resolve_codex_session(process.pid),
    }
}

pub fn load_reference(
    locator: &AgentSessionLocator,
    max_chars: usize,
) -> Result<Option<AgentReference>> {
    if process_start_ticks(locator.pid)? != locator.process_start_ticks {
        return Ok(None);
    }

    let metadata = fs::metadata(&locator.session_path).with_context(|| {
        format!(
            "failed to stat agent session {}",
            locator.session_path.display()
        )
    })?;
    if metadata.dev() != locator.device || metadata.ino() != locator.inode {
        return Ok(None);
    }

    let text = match locator.kind {
        AgentKind::Pi => latest_pi_assistant(&locator.session_path, &locator.session_id)?,
        AgentKind::Codex => latest_codex_assistant(&locator.session_path, &locator.session_id)?,
    };

    let Some(text) = text else {
        return Ok(None);
    };
    let text = sanitize_reference(&text, max_chars.clamp(500, 12_000));
    if text.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(AgentReference {
        agent: locator.kind,
        text,
    }))
}

#[derive(Deserialize)]
struct ActiveWindow {
    #[serde(default)]
    class: String,
    pid: u32,
}

fn active_window() -> Result<ActiveWindow> {
    let output = Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to query Hyprland active window")?;
    if !output.status.success() {
        bail!("Hyprland active-window query failed");
    }
    serde_json::from_slice(&output.stdout).context("failed to parse Hyprland active window")
}

struct FocusedAgentProcess {
    kind: AgentKind,
    pid: u32,
}

fn focused_kitty_agent(kitty_pid: u32) -> Result<Option<FocusedAgentProcess>> {
    let socket = format!("unix:/tmp/kitty-{kitty_pid}");
    let output = Command::new("timeout")
        .args([
            KITTY_QUERY_TIMEOUT_SECS,
            "kitty",
            "@",
            "--to",
            socket.as_str(),
            "ls",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to query focused Kitty window")?;
    if !output.status.success() {
        return Ok(None);
    }

    let payload: Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse Kitty remote-control response")?;
    let Some(os_windows) = payload.as_array() else {
        return Ok(None);
    };

    for os_window in os_windows {
        let Some(tabs) = os_window["tabs"].as_array() else {
            continue;
        };
        for tab in tabs {
            let Some(windows) = tab["windows"].as_array() else {
                continue;
            };
            for window in windows {
                if !window["is_focused"].as_bool().unwrap_or(false) {
                    continue;
                }
                let Some(processes) = window["foreground_processes"].as_array() else {
                    continue;
                };
                for process in processes.iter().rev() {
                    let Some(pid) = process["pid"]
                        .as_u64()
                        .and_then(|pid| u32::try_from(pid).ok())
                    else {
                        continue;
                    };
                    let executable = process["cmdline"]
                        .as_array()
                        .and_then(|args| args.first())
                        .and_then(Value::as_str)
                        .and_then(|arg| Path::new(arg).file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    let kind = match executable {
                        "pi" => AgentKind::Pi,
                        "codex" => AgentKind::Codex,
                        _ => continue,
                    };
                    return Ok(Some(FocusedAgentProcess { kind, pid }));
                }
            }
        }
    }

    Ok(None)
}

#[derive(Deserialize)]
struct PiRegistry {
    version: u32,
    pid: u32,
    process_start_ticks: u64,
    session_id: String,
    session_file: PathBuf,
}

fn resolve_pi_session(pid: u32) -> Result<Option<AgentSessionLocator>> {
    let runtime = runtime_dir()?;
    let registry_name = format!("pi-{pid}.json");
    let registry_path = runtime
        .join("voice-input/agent-sessions")
        .join(&registry_name);
    let legacy_registry_path = runtime.join("voxtype/agent-sessions").join(&registry_name);
    let source = match fs::read(&registry_path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::read(&legacy_registry_path) {
                Ok(source) => source,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(error).context("failed to read legacy Pi session registry");
                }
            }
        }
        Err(error) => return Err(error).context("failed to read Pi session registry"),
    };
    let registry: PiRegistry =
        serde_json::from_slice(&source).context("failed to parse Pi session registry")?;
    if registry.version != 1
        || registry.pid != pid
        || registry.process_start_ticks != process_start_ticks(pid)?
    {
        return Ok(None);
    }

    let canonical = registry
        .session_file
        .canonicalize()
        .context("failed to resolve Pi session file")?;
    let allowed_root = dirs::home_dir()
        .ok_or_else(|| anyhow!("home directory not found"))?
        .join(".pi/agent/sessions")
        .canonicalize()
        .context("failed to resolve Pi session directory")?;
    if !canonical.starts_with(&allowed_root) {
        return Ok(None);
    }

    let metadata = fs::metadata(&canonical)?;
    let header = first_json_line(&canonical)?;
    if header["type"] != "session" || header["id"].as_str() != Some(&registry.session_id) {
        return Ok(None);
    }

    Ok(Some(AgentSessionLocator {
        kind: AgentKind::Pi,
        pid,
        process_start_ticks: registry.process_start_ticks,
        session_id: registry.session_id,
        session_path: canonical,
        device: metadata.dev(),
        inode: metadata.ino(),
    }))
}

fn resolve_codex_session(pid: u32) -> Result<Option<AgentSessionLocator>> {
    let start_ticks = process_start_ticks(pid)?;
    let codex_home = process_environment_value(pid, "CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .ok_or_else(|| anyhow!("Codex home directory not found"))?;
    let allowed_root = codex_home
        .join("sessions")
        .canonicalize()
        .context("failed to resolve Codex session directory")?;

    let mut candidates = Vec::new();
    for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
        let entry = entry?;
        let target = match fs::read_link(entry.path()) {
            Ok(target) => target,
            Err(_) => continue,
        };
        if target.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let canonical = match target.canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !canonical.starts_with(&allowed_root) {
            continue;
        }
        let header = match first_json_line(&canonical) {
            Ok(header) => header,
            Err(_) => continue,
        };
        if header["type"] != "session_meta"
            || header["payload"]["source"].as_str() != Some("cli")
            || header["payload"]["thread_source"].as_str() != Some("user")
        {
            continue;
        }
        let Some(session_id) = header["payload"]["id"].as_str() else {
            continue;
        };
        candidates.push((canonical, session_id.to_string()));
    }

    if candidates.len() != 1 {
        return Ok(None);
    }
    let (session_path, session_id) = candidates.remove(0);
    let metadata = fs::metadata(&session_path)?;
    Ok(Some(AgentSessionLocator {
        kind: AgentKind::Codex,
        pid,
        process_start_ticks: start_ticks,
        session_id,
        session_path,
        device: metadata.dev(),
        inode: metadata.ino(),
    }))
}

fn latest_pi_assistant(path: &Path, session_id: &str) -> Result<Option<String>> {
    let values = tail_json_lines(path, MAX_SESSION_SCAN_BYTES)?;
    if values.is_empty() {
        return Ok(None);
    }
    let header = first_json_line(path)?;
    if header["id"].as_str() != Some(session_id) {
        return Ok(None);
    }

    let mut entries = HashMap::new();
    let mut leaf_id = None;
    for value in values {
        let Some(id) = value["id"].as_str() else {
            continue;
        };
        leaf_id = Some(id.to_string());
        entries.insert(id.to_string(), value);
    }

    let mut current = leaf_id;
    while let Some(id) = current {
        let Some(entry) = entries.get(&id) else {
            return Ok(None);
        };
        if entry["type"] == "message"
            && entry["message"]["role"] == "assistant"
            && entry["message"]["stopReason"].as_str() == Some("stop")
        {
            let text = extract_text_blocks(&entry["message"]["content"]);
            if !text.trim().is_empty() {
                return Ok(Some(text));
            }
        }
        current = entry["parentId"].as_str().map(ToOwned::to_owned);
    }

    Ok(None)
}

fn latest_codex_assistant(path: &Path, session_id: &str) -> Result<Option<String>> {
    let header = first_json_line(path)?;
    if header["payload"]["id"].as_str() != Some(session_id) {
        return Ok(None);
    }

    let values = tail_json_lines(path, MAX_SESSION_SCAN_BYTES)?;
    for value in values.iter().rev() {
        if value["type"] == "response_item"
            && value["payload"]["type"] == "message"
            && value["payload"]["role"] == "assistant"
            && value["payload"]["phase"] == "final_answer"
        {
            let text = extract_output_text_blocks(&value["payload"]["content"]);
            if !text.trim().is_empty() {
                return Ok(Some(text));
            }
        }
    }

    for value in values.iter().rev() {
        if value["type"] == "event_msg" && value["payload"]["type"] == "task_complete" {
            if let Some(text) = value["payload"]["last_agent_message"].as_str() {
                if !text.trim().is_empty() {
                    return Ok(Some(text.to_string()));
                }
            }
        }
    }

    Ok(None)
}

fn extract_text_blocks(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_output_text_blocks(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "output_text")
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_json_line(path: &Path) -> Result<Value> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref().take(128 * 1024).read_to_end(&mut bytes)?;
    let line = bytes
        .split(|byte| *byte == b'\n')
        .next()
        .ok_or_else(|| anyhow!("agent session has no header"))?;
    serde_json::from_slice(line).context("failed to parse agent session header")
}

fn tail_json_lines(path: &Path, max_bytes: u64) -> Result<Vec<Value>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start).min(max_bytes) as usize);
    file.read_to_end(&mut bytes)?;

    let mut lines = bytes.split(|byte| *byte == b'\n');
    if start > 0 {
        lines.next();
    }
    Ok(lines
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .collect())
}

fn sanitize_reference(value: &str, max_chars: usize) -> String {
    let mut redacted = Vec::new();
    for line in value.lines() {
        let lower = line.to_ascii_lowercase();
        if [
            "authorization:",
            "api_key",
            "api-key",
            "apikey",
            "password=",
            "password:",
            "secret=",
            "secret:",
            "access_token",
            "refresh_token",
            "private key-----",
            "cookie:",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            redacted.push("[REDACTED SENSITIVE LINE]".to_string());
        } else {
            redacted.push(redact_token_like_words(line));
        }
    }
    cap_text(&redacted.join("\n"), max_chars)
}

fn redact_token_like_words(line: &str) -> String {
    line.split_inclusive(char::is_whitespace)
        .map(|piece| {
            let token = piece.trim();
            let lower = token.to_ascii_lowercase();
            let jwt_like = token.len() > 80 && token.matches('.').count() == 2;
            let known_secret = token.len() > 20
                && ["sk-", "sk_", "ghp_", "github_pat_", "xoxb-", "xoxp-"]
                    .iter()
                    .any(|prefix| lower.starts_with(prefix));
            if jwt_like || known_secret {
                let trailing = piece.strip_prefix(token).unwrap_or_default();
                format!("[REDACTED]{trailing}")
            } else {
                piece.to_string()
            }
        })
        .collect()
}

fn cap_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let head_len = max_chars * 2 / 3;
    let tail_len = max_chars.saturating_sub(head_len + 3);
    let head = value.chars().take(head_len).collect::<String>();
    let tail = value
        .chars()
        .rev()
        .take(tail_len)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{head}\n…\n{tail}")
}

fn process_start_ticks(pid: u32) -> Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let end = stat
        .rfind(')')
        .ok_or_else(|| anyhow!("invalid process stat"))?;
    stat[end + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| anyhow!("process stat is missing start time"))?
        .parse()
        .context("invalid process start time")
}

fn process_environment_value(pid: u32, key: &str) -> Option<String> {
    let bytes = fs::read(format!("/proc/{pid}/environ")).ok()?;
    bytes.split(|byte| *byte == 0).find_map(|entry| {
        let separator = entry.iter().position(|byte| *byte == b'=')?;
        let (name, value_with_separator) = entry.split_at(separator);
        let value = value_with_separator.get(1..)?;
        (name == key.as_bytes()).then(|| String::from_utf8_lossy(value).to_string())
    })
}

fn runtime_dir() -> Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(dirs::runtime_dir)
        .ok_or_else(|| anyhow!("runtime directory not found"))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::{cap_text, latest_codex_assistant, latest_pi_assistant, sanitize_reference};

    #[test]
    fn reads_latest_pi_assistant_on_active_branch() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for value in [
            json!({"type":"session","version":3,"id":"session-1","cwd":"/tmp"}),
            json!({"type":"message","id":"u1","parentId":null,"message":{"role":"user","content":"one"}}),
            json!({"type":"message","id":"a1","parentId":"u1","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"first"}]}}),
            json!({"type":"message","id":"u2","parentId":"a1","message":{"role":"user","content":"two"}}),
            json!({"type":"message","id":"a2","parentId":"u2","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"latest"},{"type":"toolCall","name":"x"}]}}),
            json!({"type":"custom","id":"c1","parentId":"a2","data":{}}),
        ] {
            writeln!(file, "{value}").unwrap();
        }
        assert_eq!(
            latest_pi_assistant(file.path(), "session-1").unwrap(),
            Some("latest".into())
        );
    }

    #[test]
    fn reads_latest_codex_final_answer() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for value in [
            json!({"type":"session_meta","payload":{"id":"codex-1","source":"cli","thread_source":"user"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"working"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"done"}]}}),
        ] {
            writeln!(file, "{value}").unwrap();
        }
        assert_eq!(
            latest_codex_assistant(file.path(), "codex-1").unwrap(),
            Some("done".into())
        );
    }

    #[test]
    fn redacts_and_caps_reference() {
        let value = format!(
            "safe\nAPI_KEY=secret\n{}",
            "test-token-shaped-placeholder".repeat(20)
        );
        let output = sanitize_reference(&value, 120);
        assert!(output.contains("safe"));
        assert!(!output.contains("secret"));
        assert!(output.chars().count() <= 122);
    }

    #[test]
    fn cap_preserves_head_and_tail() {
        let output = cap_text(&"a".repeat(200), 60);
        assert!(output.starts_with(&"a".repeat(40)));
        assert!(output.ends_with(&"a".repeat(17)));
    }
}
