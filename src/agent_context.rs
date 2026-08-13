use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use jieba_rs::Jieba;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    config::{MAX_AGENT_CONTEXT_CHARS, MIN_AGENT_CONTEXT_CHARS},
    focused_window::FocusedWindowSnapshot,
};

const MAX_SESSION_SCAN_BYTES: u64 = 8 * 1024 * 1024;
const KITTY_QUERY_TIMEOUT_SECS: &str = "1";
const MAX_REFINEMENT_TERMINOLOGY_COUNT: usize = 96;
const MAX_REFINEMENT_TERMINOLOGY_CHARS: usize = 1_500;
const MAX_AUDIO3_SESSION_CONTEXT_CHARS: usize = 400;
const MAX_SNAPSHOT_TERMINOLOGY_COUNT: usize = 4_096;
const MAX_SNAPSHOT_TERMINOLOGY_CHARS: usize = 48_000;
const MAX_TERM_CHARS: usize = 96;
static JIEBA: OnceLock<Jieba> = OnceLock::new();

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
    pi_registry_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminologyTerm {
    text: String,
    frequency: usize,
    candidate_order: usize,
    normalization_eligible: bool,
}

/// One immutable, start-time terminology snapshot shared by Audio3 and Refine.
///
/// Deliberately does not implement `Debug`: term text must not be exposed by
/// routine logs or diagnostics.
pub struct AgentTerminologySnapshot {
    pub agent: AgentKind,
    terms: Vec<TerminologyTerm>,
    pub source_char_count: usize,
    pub extraction_elapsed: Duration,
}

pub struct SelectedTerminology {
    pub terms: Vec<String>,
    pub char_count: usize,
}

pub struct Audio3SessionContext {
    pub text: String,
}

impl AgentTerminologySnapshot {
    pub fn select_for_refinement(&self) -> SelectedTerminology {
        let mut terms = Vec::new();
        let mut char_count = 0_usize;
        for term in &self.terms {
            if terms.len() >= MAX_REFINEMENT_TERMINOLOGY_COUNT {
                break;
            }
            let term_chars = term.text.chars().count();
            if char_count.saturating_add(term_chars) > MAX_REFINEMENT_TERMINOLOGY_CHARS {
                continue;
            }
            char_count += term_chars;
            terms.push(term.text.clone());
        }
        SelectedTerminology { terms, char_count }
    }

    pub fn select_for_audio3(&self) -> Option<Audio3SessionContext> {
        let mut selected = Vec::new();
        let mut char_count = 0_usize;
        for term in &self.terms {
            let separator_chars = usize::from(!selected.is_empty());
            let term_chars = term.text.chars().count();
            if char_count
                .saturating_add(separator_chars)
                .saturating_add(term_chars)
                > MAX_AUDIO3_SESSION_CONTEXT_CHARS
            {
                continue;
            }
            char_count += separator_chars + term_chars;
            selected.push(term.text.as_str());
        }
        if selected.is_empty() {
            return None;
        }
        Some(Audio3SessionContext {
            text: selected.join("\n"),
        })
    }

    pub fn candidate_count(&self) -> usize {
        self.terms.len()
    }

    /// Restores exact spellings for high-confidence technical variants using
    /// only this operation's dynamic terminology snapshot. No terms persist
    /// across Voice Input sessions.
    pub fn normalize_technical_terms(&self, text: &str) -> String {
        let mut selected_count = 0_usize;
        let mut selected_chars = 0_usize;
        let mut canonical_terms = Vec::new();
        for term in &self.terms {
            if selected_count >= MAX_REFINEMENT_TERMINOLOGY_COUNT {
                break;
            }
            let term_chars = term.text.chars().count();
            if selected_chars.saturating_add(term_chars) > MAX_REFINEMENT_TERMINOLOGY_CHARS {
                continue;
            }
            selected_count += 1;
            selected_chars += term_chars;
            if term.normalization_eligible {
                canonical_terms.push(term.text.clone());
            }
        }
        normalize_dynamic_technical_terms(text, &canonical_terms)
    }

    #[cfg(test)]
    pub(crate) fn frequencies(&self) -> Vec<(&str, usize)> {
        self.terms
            .iter()
            .map(|term| (term.text.as_str(), term.frequency))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn from_terms(agent: AgentKind, terms: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            agent,
            terms: terms
                .iter()
                .enumerate()
                .map(|(candidate_order, term)| TerminologyTerm {
                    text: (*term).to_string(),
                    frequency: 1,
                    candidate_order,
                    normalization_eligible: true,
                })
                .collect(),
            source_char_count: terms.iter().map(|term| term.chars().count()).sum(),
            extraction_elapsed: Duration::ZERO,
        })
    }
}

#[derive(Clone)]
pub struct AgentTerminologyCapture {
    shared: Arc<TerminologyCaptureState>,
}

struct TerminologyCaptureState {
    result: Mutex<Option<Option<Arc<AgentTerminologySnapshot>>>>,
    ready: Condvar,
}

impl AgentTerminologyCapture {
    fn pending() -> Self {
        Self {
            shared: Arc::new(TerminologyCaptureState {
                result: Mutex::new(None),
                ready: Condvar::new(),
            }),
        }
    }

    fn complete(&self, result: Option<Arc<AgentTerminologySnapshot>>) {
        let mut slot = self
            .shared
            .result
            .lock()
            .expect("agent terminology capture mutex poisoned");
        if slot.is_none() {
            *slot = Some(result);
            self.shared.ready.notify_all();
        }
    }

    pub fn wait_with_abort(
        &self,
        abort_flag: &AtomicBool,
        timeout: Duration,
    ) -> Option<Arc<AgentTerminologySnapshot>> {
        let deadline = Instant::now().checked_add(timeout)?;
        let mut slot = self
            .shared
            .result
            .lock()
            .expect("agent terminology capture mutex poisoned");
        loop {
            if let Some(result) = slot.as_ref() {
                return result.clone();
            }
            if abort_flag.load(Ordering::SeqCst) {
                return None;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let (next_slot, _) = self
                .shared
                .ready
                .wait_timeout(slot, remaining.min(Duration::from_millis(10)))
                .expect("agent terminology capture mutex poisoned");
            slot = next_slot;
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn completed(snapshot: Option<Arc<AgentTerminologySnapshot>>) -> Self {
        let capture = Self::pending();
        capture.complete(snapshot);
        capture
    }
}

pub struct FocusedAgentSnapshot {
    kind: AgentKind,
    pid: u32,
}

impl FocusedAgentSnapshot {
    pub fn agent(&self) -> AgentKind {
        self.kind
    }
}

pub fn capture_focused_agent(
    window: &FocusedWindowSnapshot,
) -> Result<Option<FocusedAgentSnapshot>> {
    if !window.class().eq_ignore_ascii_case("kitty") {
        return Ok(None);
    }

    focused_kitty_agent(window.pid()).map(|process| {
        process.map(|process| FocusedAgentSnapshot {
            kind: process.kind,
            pid: process.pid,
        })
    })
}

pub fn resolve_focused_session(
    snapshot: FocusedAgentSnapshot,
) -> Result<Option<AgentSessionLocator>> {
    match snapshot.kind {
        AgentKind::Pi => resolve_pi_session(snapshot.pid),
        AgentKind::Codex => resolve_codex_session(snapshot.pid),
    }
}

pub fn warm_terminology_segmenter() -> Option<Duration> {
    if JIEBA.get().is_some() {
        return None;
    }
    let started = Instant::now();
    JIEBA.get_or_init(Jieba::new);
    Some(started.elapsed())
}

pub fn start_terminology_capture(
    window: FocusedWindowSnapshot,
    max_chars: usize,
) -> Result<Option<AgentTerminologyCapture>> {
    if !window.class().eq_ignore_ascii_case("kitty") {
        return Ok(None);
    }
    // Freeze both the focused agent session and its latest completed source
    // before launching the segmentation worker. A later Kitty tab switch or
    // assistant response cannot change this Voice Input operation's snapshot.
    let Some(focused_agent) = capture_focused_agent(&window)? else {
        return Ok(None);
    };
    let Some(locator) = resolve_focused_session(focused_agent)? else {
        return Ok(None);
    };
    let Some((agent, source)) = load_source(&locator)? else {
        return Ok(None);
    };

    let capture = AgentTerminologyCapture::pending();
    let worker_capture = capture.clone();
    let spawn_result = thread::Builder::new()
        .name("voice-input-agent-terminology".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(elapsed) = warm_terminology_segmenter() {
                    eprintln!(
                        "voice-input agent context: initialized local segmenter in {} ms",
                        elapsed.as_millis()
                    );
                }
                build_snapshot(agent, &source, max_chars)
            }));
            match result {
                Ok(snapshot) => worker_capture.complete(snapshot.map(Arc::new)),
                Err(_) => {
                    eprintln!("voice-input agent context: start-time capture failed");
                    worker_capture.complete(None);
                }
            }
        });
    if spawn_result.is_err() {
        capture.complete(None);
        return Err(anyhow!("failed to start agent terminology worker"));
    }
    Ok(Some(capture))
}

fn load_source(locator: &AgentSessionLocator) -> Result<Option<(AgentKind, String)>> {
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
        AgentKind::Pi => {
            let published_reference = current_pi_published_reference(locator)?;
            latest_pi_reference(
                &locator.session_path,
                &locator.session_id,
                published_reference.as_deref(),
            )?
        }
        AgentKind::Codex => latest_codex_assistant(&locator.session_path, &locator.session_id)?,
    };
    Ok(text.map(|text| (locator.kind, text)))
}

fn build_snapshot(
    agent: AgentKind,
    source: &str,
    max_chars: usize,
) -> Option<AgentTerminologySnapshot> {
    let text = sanitize_reference(
        source,
        max_chars.clamp(MIN_AGENT_CONTEXT_CHARS, MAX_AGENT_CONTEXT_CHARS),
    );
    if text.trim().is_empty() {
        return None;
    }

    let started = Instant::now();
    let terms = extract_terminology(&text);
    let extraction_elapsed = started.elapsed();
    if terms.is_empty() {
        return None;
    }

    Some(AgentTerminologySnapshot {
        agent,
        terms,
        source_char_count: text.chars().count(),
        extraction_elapsed,
    })
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
    #[serde(default)]
    latest_completed_assistant_message: Option<String>,
}

fn resolve_pi_session(pid: u32) -> Result<Option<AgentSessionLocator>> {
    let runtime = runtime_dir()?;
    let registry_name = format!("pi-{pid}.json");
    let registry_path = runtime
        .join("voice-input/agent-sessions")
        .join(&registry_name);
    let legacy_registry_path = runtime.join("voxtype/agent-sessions").join(&registry_name);
    let (source, active_registry_path) = match fs::read(&registry_path) {
        Ok(source) => (source, registry_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::read(&legacy_registry_path) {
                Ok(source) => (source, legacy_registry_path),
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
    if !matches!(registry.version, 1 | 2)
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
        pi_registry_path: Some(active_registry_path),
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
        pi_registry_path: None,
    }))
}

fn current_pi_published_reference(locator: &AgentSessionLocator) -> Result<Option<String>> {
    let Some(registry_path) = &locator.pi_registry_path else {
        return Ok(None);
    };
    let source = match fs::read(registry_path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to refresh Pi session registry"),
    };
    let registry: PiRegistry = match serde_json::from_slice(&source) {
        Ok(registry) => registry,
        Err(_) => return Ok(None),
    };
    if registry.version != 2
        || registry.pid != locator.pid
        || registry.process_start_ticks != locator.process_start_ticks
        || registry.session_id != locator.session_id
    {
        return Ok(None);
    }
    let session_path = match registry.session_file.canonicalize() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if session_path != locator.session_path {
        return Ok(None);
    }
    Ok(registry.latest_completed_assistant_message)
}

fn latest_pi_reference(
    path: &Path,
    session_id: &str,
    published_reference: Option<&str>,
) -> Result<Option<String>> {
    if let Some(text) = published_reference {
        return Ok(Some(text.to_string()));
    }
    latest_pi_assistant(path, session_id)
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
        if value["type"] == "event_msg"
            && value["payload"]["type"] == "task_complete"
            && let Some(text) = value["payload"]["last_agent_message"].as_str()
            && !text.trim().is_empty()
        {
            return Ok(Some(text.to_string()));
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
    // Redact complete lines before capping. Capping first could split a
    // sensitive line, retain its value in the tail, and discard the marker
    // that would have caused the whole line to be removed.
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

fn extract_terminology(value: &str) -> Vec<TerminologyTerm> {
    let jieba = JIEBA.get_or_init(Jieba::new);
    let mut seen = HashSet::new();
    let mut terminology = Vec::new();
    let mut total_chars = 0_usize;

    // Jieba intentionally separates punctuation, which would split model IDs,
    // paths, flags, and code identifiers. Preserve those high-value technical
    // forms first, then add ordinary segmented words below.
    for term in value.split(|character: char| !is_technical_character(character)) {
        let structured = term
            .chars()
            .any(|character| matches!(character, '-' | '_' | '/' | '.' | ':' | '+' | '#' | '@'));
        let mixed_case = term.chars().any(|character| character.is_ascii_uppercase())
            && term
                .chars()
                .skip(1)
                .any(|character| character.is_ascii_lowercase());
        let has_digit = term.chars().any(|character| character.is_ascii_digit());
        if structured || mixed_case || has_digit {
            push_term(term, &mut seen, &mut terminology, &mut total_chars);
        }
    }

    for token in jieba.cut(value, true) {
        let term = token.word.trim_matches(|character: char| {
            character.is_whitespace() || is_term_boundary(character)
        });
        push_term(term, &mut seen, &mut terminology, &mut total_chars);
        if terminology.len() >= MAX_SNAPSHOT_TERMINOLOGY_COUNT
            || total_chars >= MAX_SNAPSHOT_TERMINOLOGY_CHARS
        {
            break;
        }
    }

    let lowercase_source = value.to_lowercase();
    for term in &mut terminology {
        term.frequency = lowercase_source
            .match_indices(&term.text.to_lowercase())
            .count()
            .max(1);
        term.normalization_eligible = is_normalizable_technical_term(&term.text)
            && has_independent_source_occurrence(value, &term.text);
    }
    terminology.sort_by_key(|term| (term.frequency, term.candidate_order));
    terminology
}

fn has_independent_source_occurrence(source: &str, term: &str) -> bool {
    let source_lower = source.to_ascii_lowercase();
    let term_lower = term.to_ascii_lowercase();
    let term_starts_with_separator = term.chars().next().is_some_and(is_normalization_separator);
    let term_ends_with_separator = term
        .chars()
        .next_back()
        .is_some_and(is_normalization_separator);
    source_lower
        .match_indices(&term_lower)
        .any(|(start, matched)| {
            let end = start + matched.len();
            let previous = source[..start].chars().next_back();
            let next = source[end..].chars().next();
            !previous.is_some_and(|character| {
                is_source_term_continuation(character)
                    || (!term_starts_with_separator && is_joining_separator(character))
            }) && !next.is_some_and(|character| {
                is_source_term_continuation(character)
                    || (!term_ends_with_separator && is_joining_separator(character))
            })
        })
}

fn is_source_term_continuation(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '#' | '+' | '/' | '@' | ':')
}

fn normalize_dynamic_technical_terms(text: &str, terms: &[String]) -> String {
    let mut canonical_by_key: HashMap<String, Option<String>> = HashMap::new();
    for term in terms {
        if !is_normalizable_technical_term(term) {
            continue;
        }
        let key = normalization_key(term);
        if key.is_empty() {
            continue;
        }
        canonical_by_key
            .entry(key)
            .and_modify(|canonical| {
                if canonical.as_deref() != Some(term.as_str()) {
                    *canonical = None;
                }
            })
            .or_insert_with(|| Some(term.clone()));
    }

    let mut canonicals = canonical_by_key
        .into_iter()
        .filter_map(|(key, canonical)| canonical.map(|canonical| (key, canonical)))
        .collect::<Vec<_>>();
    canonicals.sort_by(|(left_key, left), (right_key, right)| {
        right_key
            .len()
            .cmp(&left_key.len())
            .then_with(|| right.len().cmp(&left.len()))
            .then_with(|| left.cmp(right))
    });

    let characters = text.char_indices().collect::<Vec<_>>();
    let mut output = String::with_capacity(text.len());
    let mut character_index = 0_usize;
    let mut byte_index = 0_usize;
    while character_index < characters.len() {
        let start_byte = characters[character_index].0;
        let mut best: Option<(usize, usize, &str)> = None;
        for (_, canonical) in &canonicals {
            let Some((end_character, end_byte)) =
                match_canonical_variant(text, &characters, character_index, canonical)
            else {
                continue;
            };
            if !has_technical_boundaries(text, start_byte, end_byte) {
                continue;
            }
            let span = end_byte.saturating_sub(start_byte);
            if best.is_none_or(|(best_span, _, _)| span > best_span) {
                best = Some((span, end_character, canonical.as_str()));
            }
        }

        if let Some((_, end_character, canonical)) = best {
            output.push_str(&text[byte_index..start_byte]);
            output.push_str(canonical);
            byte_index = if end_character < characters.len() {
                characters[end_character].0
            } else {
                text.len()
            };
            character_index = end_character;
        } else {
            character_index += 1;
        }
    }
    output.push_str(&text[byte_index..]);
    output
}

fn is_normalizable_technical_term(term: &str) -> bool {
    let alphanumeric_count = term
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .count();
    let has_ascii_letter = term
        .chars()
        .any(|character| character.is_ascii_alphabetic());
    let symbolic_language =
        term.chars().any(|character| matches!(character, '#' | '+')) && has_ascii_letter;
    if !term.is_ascii() || !has_ascii_letter || (alphanumeric_count < 2 && !symbolic_language) {
        return false;
    }
    let has_separator = term
        .chars()
        .any(|character| matches!(character, '-' | '_' | '.'));
    let has_digit = term.chars().any(|character| character.is_ascii_digit());
    let letters = term
        .chars()
        .filter(|character| character.is_ascii_alphabetic())
        .collect::<String>();
    let acronym = letters.len() >= 2
        && letters
            .chars()
            .all(|character| character.is_ascii_uppercase());
    let mixed_case = letters
        .chars()
        .skip(1)
        .any(|character| character.is_ascii_uppercase())
        && letters
            .chars()
            .any(|character| character.is_ascii_lowercase());
    has_separator || has_digit || acronym || mixed_case || term.contains('#') || term.contains('+')
}

fn normalization_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| !is_normalization_separator(*character))
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_normalization_separator(character: char) -> bool {
    is_spacing_separator(character) || is_joining_separator(character)
}

fn is_spacing_separator(character: char) -> bool {
    matches!(character, ' ' | '\t')
}

fn is_joining_separator(character: char) -> bool {
    matches!(
        character,
        '-' | '_'
            | '.'
            | '\u{2010}' // HYPHEN
            | '\u{2011}' // NON-BREAKING HYPHEN
            | '\u{2212}' // MINUS SIGN
            | '\u{ff0d}' // FULLWIDTH HYPHEN-MINUS
    )
}

fn match_canonical_variant(
    text: &str,
    source: &[(usize, char)],
    start: usize,
    canonical: &str,
) -> Option<(usize, usize)> {
    if is_normalization_separator(source[start].1) {
        return None;
    }
    let canonical = canonical.chars().collect::<Vec<_>>();
    let mut source_index = start;
    let mut canonical_index = 0_usize;
    while canonical_index < canonical.len() {
        if is_normalization_separator(canonical[canonical_index]) {
            while canonical_index < canonical.len()
                && is_normalization_separator(canonical[canonical_index])
            {
                canonical_index += 1;
            }
            // One canonical separator group may map to exactly one space,
            // tab, hyphen, underscore, or dot. This permits `LSP client` for
            // `lsp-client` without matching across sentences or punctuation
            // runs such as `LSP...client`.
            if source_index >= source.len() || !is_normalization_separator(source[source_index].1) {
                return None;
            }
            source_index += 1;
            continue;
        }
        if source_index >= source.len()
            || !source[source_index]
                .1
                .eq_ignore_ascii_case(&canonical[canonical_index])
        {
            return None;
        }
        source_index += 1;
        canonical_index += 1;
    }
    let end_byte = if source_index < source.len() {
        source[source_index].0
    } else {
        text.len()
    };
    Some((source_index, end_byte))
}

fn has_technical_boundaries(text: &str, start: usize, end: usize) -> bool {
    let previous = text[..start].chars().next_back();
    let next = text[end..].chars().next();
    !previous.is_some_and(is_technical_word_character)
        && !next.is_some_and(is_technical_word_character)
}

fn is_technical_word_character(character: char) -> bool {
    is_source_term_continuation(character) || is_joining_separator(character)
}

fn push_term(
    term: &str,
    seen: &mut HashSet<String>,
    terminology: &mut Vec<TerminologyTerm>,
    total_chars: &mut usize,
) {
    let char_count = term.chars().count();
    if !term_is_useful(term, char_count)
        || terminology.len() >= MAX_SNAPSHOT_TERMINOLOGY_COUNT
        || total_chars.saturating_add(char_count) > MAX_SNAPSHOT_TERMINOLOGY_CHARS
    {
        return;
    }
    let deduplication_key = term.to_lowercase();
    if seen.insert(deduplication_key) {
        *total_chars += char_count;
        terminology.push(TerminologyTerm {
            text: term.to_string(),
            frequency: 0,
            candidate_order: terminology.len(),
            normalization_eligible: false,
        });
    }
}

fn is_technical_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '-' | '_' | '/' | '.' | ':' | '+' | '#' | '@')
}

fn is_term_boundary(character: char) -> bool {
    matches!(
        character,
        '`' | '"'
            | '\''
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | ','
            | '，'
            | ';'
            | '；'
            | ':'
            | '：'
            | '!'
            | '！'
            | '?'
            | '？'
            | '。'
            | '、'
            | '…'
            | '“'
            | '”'
            | '‘'
            | '’'
    )
}

fn term_is_useful(term: &str, char_count: usize) -> bool {
    if char_count == 0
        || char_count > MAX_TERM_CHARS
        || term.to_ascii_uppercase().contains("REDACTED")
        || term_looks_sensitive(term)
    {
        return false;
    }
    let has_cjk = term.chars().any(is_cjk);
    let has_ascii_alphanumeric = term
        .chars()
        .any(|character| character.is_ascii_alphanumeric());
    if !has_cjk && !has_ascii_alphanumeric {
        return false;
    }
    if has_cjk {
        return char_count >= 2 && !is_stopword(term);
    }
    if char_count < 2 || is_stopword(term) {
        return false;
    }
    let lower = term.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return false;
    }
    // Long unstructured ASCII values are more likely to be identifiers,
    // hashes, or credentials than useful spoken terminology. Structured
    // commands, paths, model IDs, and code identifiers remain eligible.
    let structured = term
        .chars()
        .any(|character| matches!(character, '-' | '_' | '/' | '.' | ':' | '+' | '#' | '@'));
    char_count <= 48 || structured
}

fn term_looks_sensitive(term: &str) -> bool {
    let lower = term.to_ascii_lowercase();
    let jwt_like = term.len() > 80 && term.matches('.').count() == 2;
    let known_secret = term.len() > 20
        && ["sk-", "sk_", "ghp_", "github_pat_", "xoxb-", "xoxp-"]
            .iter()
            .any(|prefix| lower.starts_with(prefix));
    let aws_access_key = term.len() == 20
        && term.is_ascii()
        && (term.starts_with("AKIA") || term.starts_with("ASIA"))
        && term
            .chars()
            .all(|character| character.is_ascii_alphanumeric());
    let uri_userinfo = term.contains("://")
        && term.split_once("://").is_some_and(|(_, authority)| {
            authority
                .split('/')
                .next()
                .is_some_and(|value| value.contains('@'))
        });
    let long_unstructured_ascii = term.len() > 48
        && term.is_ascii()
        && term
            .chars()
            .all(|character| character.is_ascii_alphanumeric());
    jwt_like || known_secret || aws_access_key || uri_userinfo || long_unstructured_ascii
}

fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{ac00}'..='\u{d7af}'
    )
}

fn is_stopword(term: &str) -> bool {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "from", "this", "that", "into", "only", "when", "then", "use",
        "using", "used", "should", "must", "will", "can", "could", "would", "also", "not", "are",
        "was", "were", "have", "has", "had", "its", "you", "your", "user", "message", "text",
        "current", "existing", "new", "one", "two", "first", "second", "all", "any", "如果",
        "可以", "需要", "使用", "进行", "实现", "当前", "这个", "那个", "以及", "然后", "同时",
        "一个", "一些", "已经", "没有", "不会", "应该", "必须", "我们", "你们", "他们", "用户",
        "文本", "消息", "内容", "相关", "通过", "对于", "因为", "所以", "但是", "或者",
    ];
    STOPWORDS
        .iter()
        .any(|stopword| term.eq_ignore_ascii_case(stopword))
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

    use super::{
        AgentKind, AgentSessionLocator, AgentTerminologySnapshot, MAX_AUDIO3_SESSION_CONTEXT_CHARS,
        MAX_REFINEMENT_TERMINOLOGY_CHARS, MAX_REFINEMENT_TERMINOLOGY_COUNT, PiRegistry, cap_text,
        current_pi_published_reference, extract_terminology, latest_codex_assistant,
        latest_pi_assistant, latest_pi_reference, sanitize_reference, start_terminology_capture,
    };

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
    fn parses_pi_registry_with_published_reference() {
        let registry: PiRegistry = serde_json::from_value(json!({
            "version": 2,
            "pid": 123,
            "process_start_ticks": 456,
            "session_id": "session-1",
            "session_file": "/tmp/session.jsonl",
            "latest_completed_assistant_message": "latest active-branch answer"
        }))
        .unwrap();
        assert_eq!(
            registry.latest_completed_assistant_message.as_deref(),
            Some("latest active-branch answer")
        );
    }

    #[test]
    fn refreshes_pi_reference_after_session_capture() {
        let directory = tempfile::tempdir().unwrap();
        let session_path = directory.path().join("session.jsonl");
        std::fs::write(&session_path, "{}\n").unwrap();
        let session_path = session_path.canonicalize().unwrap();
        let metadata = std::fs::metadata(&session_path).unwrap();
        let registry_path = directory.path().join("registry.json");
        let locator = AgentSessionLocator {
            kind: AgentKind::Pi,
            pid: 123,
            process_start_ticks: 456,
            session_id: "session-1".into(),
            session_path: session_path.clone(),
            device: std::os::unix::fs::MetadataExt::dev(&metadata),
            inode: std::os::unix::fs::MetadataExt::ino(&metadata),
            pi_registry_path: Some(registry_path.clone()),
        };
        std::fs::write(
            &registry_path,
            json!({
                "version": 2,
                "pid": 123,
                "process_start_ticks": 456,
                "session_id": "session-1",
                "session_file": session_path,
                "latest_completed_assistant_message": "new answer"
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            current_pi_published_reference(&locator).unwrap(),
            Some("new answer".into())
        );
    }

    #[test]
    fn prefers_pi_reference_published_from_active_branch() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            latest_pi_reference(file.path(), "session-1", Some("published latest")).unwrap(),
            Some("published latest".into())
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
            "safe\nAPI_KEY=secret\n{}\nsecret: private-tail-value",
            "test-token-shaped-placeholder".repeat(20)
        );
        let output = sanitize_reference(&value, 120);
        assert!(output.contains("safe"));
        assert!(!output.contains("API_KEY=secret"));
        assert!(!output.contains("private-tail-value"));
        assert!(output.contains("[REDACTED SENSITIVE LINE]"));
        // Redaction markers can expand the already bounded source slightly.
        assert!(output.chars().count() <= 160);
    }

    #[test]
    fn redaction_happens_before_cap_can_split_a_sensitive_line() {
        let sensitive = format!("API_KEY={}tail-secret", "x".repeat(500));
        let value = format!("safe-head\n{sensitive}\nsafe-tail");
        let output = sanitize_reference(&value, 80);
        assert!(output.contains("safe-head"));
        assert!(output.contains("safe-tail"));
        assert!(!output.contains("tail-secret"));
        assert!(output.contains("REDACTED"));
    }

    #[test]
    fn cap_preserves_head_and_tail() {
        let output = cap_text(&"a".repeat(200), 60);
        assert!(output.starts_with(&"a".repeat(40)));
        assert!(output.ends_with(&"a".repeat(17)));
    }

    #[test]
    fn terminology_uses_local_segmentation_and_stable_deduplication() {
        let source = "实现 Qwen-Audio-3 Streaming reconnect 和语音识别。再次检查 qwen-audio-3、\
             AgentReference、src/backend/qwen_audio3/streaming.rs 与 cargo test --locked。";
        let benchmark_source = source.repeat(40);
        let cold_started = std::time::Instant::now();
        let cold_terms = extract_terminology(&benchmark_source);
        let cold_elapsed = cold_started.elapsed();
        let warm_started = std::time::Instant::now();
        let warm_terms = extract_terminology(&benchmark_source);
        eprintln!(
            "terminology benchmark source_chars={} cold_us={} warm_us={} terms={} term_chars={}",
            benchmark_source.chars().count(),
            cold_elapsed.as_micros(),
            warm_started.elapsed().as_micros(),
            cold_terms.len(),
            cold_terms
                .iter()
                .map(|term| term.text.chars().count())
                .sum::<usize>()
        );
        assert_eq!(cold_terms, warm_terms);
        let terminology = extract_terminology(source);

        assert!(terminology.iter().any(|term| term.text == "语音"));
        assert!(terminology.iter().any(|term| term.text == "识别"));
        assert!(terminology.iter().any(|term| term.text == "AgentReference"));
        assert!(terminology.iter().any(|term| term.text == "Streaming"));
        assert_eq!(
            terminology
                .iter()
                .filter(|term| term.text.eq_ignore_ascii_case("qwen"))
                .count(),
            1
        );
        assert!(!terminology.iter().any(|term| term.text == "实现"));
    }

    #[test]
    fn extracted_subterms_do_not_become_deterministic_canonical_spellings() {
        let terms =
            extract_terminology("CLAUDE_DEEPSEEK_MODEL=deepseek-v4-pro DeepSeek-V4-Pro-0813");
        assert!(
            terms
                .iter()
                .any(|term| term.text == "deepseek-v4-pro" && term.normalization_eligible)
        );
        assert!(
            terms
                .iter()
                .filter(|term| term.text == "DEEPSEEK")
                .all(|term| !term.normalization_eligible)
        );
        let snapshot = AgentTerminologySnapshot {
            agent: AgentKind::Pi,
            terms,
            source_char_count: 64,
            extraction_elapsed: std::time::Duration::ZERO,
        };
        assert_eq!(
            snapshot.normalize_technical_terms("Deepseek 和 DEEPSEEK‑v4‑pro"),
            "Deepseek 和 deepseek-v4-pro"
        );
        assert_eq!(
            snapshot.normalize_technical_terms("DEEPSEEK‑v4‑pro‑0813 CLAUDE_DEEPSEEK_MODEL"),
            "DeepSeek-V4-Pro-0813 CLAUDE_DEEPSEEK_MODEL"
        );
    }

    #[test]
    fn dynamic_technical_normalization_accepts_common_unicode_hyphens() {
        let snapshot = AgentTerminologySnapshot::from_terms(AgentKind::Pi, &["deepseek-v4-pro"]);
        for input in [
            "DEEPSEEK‑v4‑pro",   // U+2011
            "DEEPSEEK‐v4‐pro",   // U+2010
            "DEEPSEEK−v4−pro",   // U+2212
            "DEEPSEEK－v4－pro", // U+FF0D
        ] {
            assert_eq!(snapshot.normalize_technical_terms(input), "deepseek-v4-pro");
        }
        assert_eq!(
            snapshot.normalize_technical_terms("DEEPSEEK‑v4‑pro‑0813"),
            "DEEPSEEK‑v4‑pro‑0813"
        );
    }

    #[test]
    fn dynamic_technical_normalization_restores_exact_session_spellings() {
        let snapshot = AgentTerminologySnapshot::from_terms(
            AgentKind::Pi,
            &[
                "lsp-client",
                "debugging-code",
                "SKILL.md",
                "TypeScript",
                "LSP",
                "1.",
                "普通",
            ],
        );
        assert_eq!(
            snapshot.normalize_technical_terms(
                "LSP client, DEBUGGING_code, skill md, typescript, LSP and 普通。"
            ),
            "lsp-client, debugging-code, SKILL.md, TypeScript, LSP and 普通。"
        );
        assert_eq!(snapshot.normalize_technical_terms("第 1 项"), "第 1 项");
        assert_eq!(
            snapshot.normalize_technical_terms("LSP...client 和 LSP  client"),
            "LSP...client 和 LSP  client"
        );
    }

    #[test]
    fn dynamic_technical_normalization_requires_boundaries_and_rejects_conflicts() {
        let snapshot = AgentTerminologySnapshot::from_terms(
            AgentKind::Pi,
            &["lsp-client", "LSP_client", "C#", "qwen-audio-3.0"],
        );
        // The two LSP forms collapse to one ambiguous key, so neither wins.
        assert_eq!(
            snapshot
                .normalize_technical_terms("LSP client inside XLSP client; c# and QWEN audio 3 0!"),
            "LSP client inside XLSP client; C# and qwen-audio-3.0!"
        );
    }

    #[test]
    fn capture_result_is_shared_and_abort_wait_is_bounded() {
        let snapshot = super::AgentTerminologySnapshot::from_terms(
            AgentKind::Pi,
            &["RareModel", "Qwen-Audio-3"],
        );
        let capture = super::AgentTerminologyCapture::completed(Some(snapshot.clone()));
        let abort = std::sync::atomic::AtomicBool::new(false);
        let first = capture
            .wait_with_abort(&abort, std::time::Duration::from_secs(1))
            .unwrap();
        let second = capture
            .wait_with_abort(&abort, std::time::Duration::from_secs(1))
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&snapshot, &first));
        assert!(std::sync::Arc::ptr_eq(&first, &second));

        let pending = super::AgentTerminologyCapture::pending();
        let abort = std::sync::atomic::AtomicBool::new(true);
        let started = std::time::Instant::now();
        assert!(
            pending
                .wait_with_abort(&abort, std::time::Duration::from_secs(5))
                .is_none()
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn ordinary_window_does_not_start_terminology_capture() {
        let window: crate::focused_window::FocusedWindowSnapshot =
            serde_json::from_value(json!({"class":"firefox","pid":42})).unwrap();
        assert!(start_terminology_capture(window, 6_000).unwrap().is_none());
    }

    #[test]
    fn terminology_frequency_is_ascending_with_stable_candidate_ties() {
        let snapshot = super::AgentTerminologySnapshot {
            agent: AgentKind::Pi,
            terms: extract_terminology(
                "RareModel CommonTerm CommonTerm Qwen-Audio-3 CommonTerm AnotherRare",
            ),
            source_char_count: 72,
            extraction_elapsed: std::time::Duration::ZERO,
        };
        let frequencies = snapshot.frequencies();
        let rare_model = frequencies
            .iter()
            .position(|(term, frequency)| *term == "RareModel" && *frequency == 1)
            .unwrap();
        let common = frequencies
            .iter()
            .position(|(term, frequency)| *term == "CommonTerm" && *frequency == 3)
            .unwrap();
        assert!(rare_model < common);
        assert!(frequencies.windows(2).all(|pair| pair[0].1 <= pair[1].1));
        let rare_terms = frequencies
            .iter()
            .filter(|(_, frequency)| *frequency == 1)
            .map(|(term, _)| *term)
            .collect::<Vec<_>>();
        assert!(
            rare_terms
                .windows(2)
                .any(|pair| pair == ["RareModel", "Qwen-Audio-3"])
        );
    }

    #[test]
    fn terminology_excludes_short_structured_credentials() {
        let source = "AKIAIOSFODNN7EXAMPLE postgres://alice:password@example.com/db safe-model";
        let terms = extract_terminology(source);
        assert!(!terms.iter().any(|term| term.text.contains("AKIA")));
        assert!(!terms.iter().any(|term| term.text.contains("password@")));
        assert!(terms.iter().any(|term| term.text == "safe-model"));
    }

    #[test]
    fn audio3_selector_counts_newlines_and_never_splits_terms() {
        let terms = (0..20)
            .map(|index| format!("术语{index}{}", "甲".repeat(20)))
            .collect::<Vec<_>>();
        let references = terms.iter().map(String::as_str).collect::<Vec<_>>();
        let snapshot = super::AgentTerminologySnapshot::from_terms(AgentKind::Pi, &references);
        let context = snapshot.select_for_audio3().unwrap();
        assert!(context.text.chars().count() <= MAX_AUDIO3_SESSION_CONTEXT_CHARS);
        assert!(
            context
                .text
                .split('\n')
                .all(|selected| terms.iter().any(|term| term == selected))
        );
    }

    #[test]
    fn terminology_is_bounded_and_excludes_redacted_secrets() {
        let source = format!(
            "API_KEY=private-secret\n{} {}",
            "a".repeat(64),
            (0..300)
                .map(|index| format!("uniqueTerm{index}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let sanitized = sanitize_reference(&source, 12_000);
        let terminology = extract_terminology(&sanitized);

        assert!(terminology.len() > MAX_REFINEMENT_TERMINOLOGY_COUNT);
        let snapshot = super::AgentTerminologySnapshot {
            agent: AgentKind::Pi,
            terms: terminology,
            source_char_count: sanitized.chars().count(),
            extraction_elapsed: std::time::Duration::ZERO,
        };
        let refinement = snapshot.select_for_refinement();
        assert!(refinement.terms.len() <= MAX_REFINEMENT_TERMINOLOGY_COUNT);
        assert!(refinement.char_count <= MAX_REFINEMENT_TERMINOLOGY_CHARS);
        assert!(
            refinement
                .terms
                .iter()
                .all(|term| !term.contains("private-secret"))
        );
        assert!(
            refinement
                .terms
                .iter()
                .all(|term| !term.contains("REDACTED"))
        );
        assert!(refinement.terms.iter().all(|term| term != &"a".repeat(64)));
        let audio3 = snapshot.select_for_audio3().unwrap();
        assert!(audio3.text.chars().count() <= MAX_AUDIO3_SESSION_CONTEXT_CHARS);
    }
}
