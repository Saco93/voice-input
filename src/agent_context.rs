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
const MAX_CONTEXT_TURNS: usize = 5;
const MAX_AUDIO3_TURN_CHARS: usize = 400;
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
    audio3_messages: Vec<Value>,
    candidate_count: usize,
    pub source_char_count: usize,
    pub extraction_elapsed: Duration,
}

pub struct SelectedTerminology {
    pub terms: Vec<String>,
    pub char_count: usize,
}

pub struct Audio3SessionContext {
    pub messages: Vec<Value>,
}

impl AgentTerminologySnapshot {
    pub fn select_for_refinement(&self) -> SelectedTerminology {
        // These are the union of the exact terms sent in the ASR turns. The
        // five shared 400-character turn budgets already bound this list.
        SelectedTerminology {
            terms: self.terms.iter().map(|term| term.text.clone()).collect(),
            char_count: self
                .terms
                .iter()
                .map(|term| term.text.chars().count())
                .sum(),
        }
    }

    pub fn select_for_audio3(&self) -> Option<Audio3SessionContext> {
        (!self.audio3_messages.is_empty()).then(|| Audio3SessionContext {
            messages: self.audio3_messages.clone(),
        })
    }

    pub fn candidate_count(&self) -> usize {
        self.candidate_count
    }

    /// Restores exact spellings for high-confidence technical variants using
    /// only this operation's dynamic terminology snapshot. No terms persist
    /// across Voice Input sessions.
    pub fn normalize_technical_terms(&self, text: &str) -> String {
        let canonical_terms = self
            .terms
            .iter()
            .filter(|term| term.normalization_eligible)
            .map(|term| term.text.clone())
            .collect::<Vec<_>>();
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
        let candidates = terms
            .iter()
            .enumerate()
            .map(|(candidate_order, term)| TerminologyTerm {
                text: (*term).to_string(),
                frequency: 1,
                candidate_order,
                normalization_eligible: true,
            })
            .collect();
        Arc::new(
            snapshot_from_candidates(
                agent,
                vec![(candidates, Vec::new())],
                terms.iter().map(|term| term.chars().count()).sum(),
            )
            .unwrap(),
        )
    }

    #[cfg(test)]
    pub(crate) fn from_turns(agent: AgentKind, turns: &[(&str, &str)]) -> Arc<Self> {
        let source = turns
            .iter()
            .map(|(user, assistant)| ConversationTurn {
                user: (*user).into(),
                assistant: (*assistant).into(),
            })
            .collect::<Vec<_>>();
        Arc::new(build_snapshot(agent, &source, MAX_AGENT_CONTEXT_CHARS).unwrap())
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
    // Freeze both the focused agent session and its recent conversation turns
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

fn load_source(
    locator: &AgentSessionLocator,
) -> Result<Option<(AgentKind, Vec<ConversationTurn>)>> {
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

    let turns = match locator.kind {
        // Only the extension knows the active branch after a Pi tree switch.
        // Never substitute the physical JSONL tail for an empty publication.
        AgentKind::Pi => current_pi_published_turns(locator)?,
        AgentKind::Codex => recent_codex_turns(&locator.session_path, &locator.session_id)?,
    };
    Ok(turns
        .filter(|turns| !turns.is_empty())
        .map(|turns| (locator.kind, turns)))
}

#[derive(Clone, Deserialize, Default, PartialEq, Eq)]
struct ConversationTurn {
    user: String,
    #[serde(default)]
    assistant: String,
}

fn build_snapshot(
    agent: AgentKind,
    source: &[ConversationTurn],
    max_chars: usize,
) -> Option<AgentTerminologySnapshot> {
    let source = &source[source.len().saturating_sub(MAX_CONTEXT_TURNS)..];
    let message_count = source
        .iter()
        .flat_map(|turn| [&turn.user, &turn.assistant])
        .filter(|text| !text.trim().is_empty())
        .count();
    if message_count == 0 {
        return None;
    }
    // Keep the configured source budget for the entire snapshot, distributed
    // across messages so an oversized answer cannot erase other turns/roles.
    let message_budget =
        max_chars.clamp(MIN_AGENT_CONTEXT_CHARS, MAX_AGENT_CONTEXT_CHARS) / message_count;
    let started = Instant::now();
    let mut source_char_count = 0;
    let mut candidates = Vec::new();
    for turn in source {
        let user = sanitize_reference(&turn.user, message_budget);
        let assistant = sanitize_reference(&turn.assistant, message_budget);
        source_char_count += user.chars().count() + assistant.chars().count();
        candidates.push((extract_terminology(&user), extract_terminology(&assistant)));
    }
    let mut snapshot = snapshot_from_candidates(agent, candidates, source_char_count)?;
    snapshot.extraction_elapsed = started.elapsed();
    Some(snapshot)
}

fn snapshot_from_candidates(
    agent: AgentKind,
    candidates: Vec<(Vec<TerminologyTerm>, Vec<TerminologyTerm>)>,
    source_char_count: usize,
) -> Option<AgentTerminologySnapshot> {
    let mut terms: Vec<TerminologyTerm> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut audio3_messages = Vec::new();
    let mut candidate_count = 0;
    for (user, assistant) in candidates
        .into_iter()
        .rev()
        .take(MAX_CONTEXT_TURNS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        candidate_count += user.len() + assistant.len();
        let mut selected = [Vec::new(), Vec::new()];
        let mut char_count = 0;
        // Alternate between the two rare-first lists. Both roles get space;
        // either role may use the remainder when the other's list runs out.
        for index in 0..user.len().max(assistant.len()) {
            for (role, candidates) in [&user, &assistant].into_iter().enumerate() {
                let Some(term) = candidates.get(index) else {
                    continue;
                };
                let cost = term.text.chars().count() + usize::from(!selected[role].is_empty());
                if char_count + cost <= MAX_AUDIO3_TURN_CHARS {
                    char_count += cost;
                    selected[role].push(term);
                }
            }
        }
        if selected.iter().all(Vec::is_empty) {
            continue;
        }
        // An empty user glossary still anchors an assistant-only glossary to
        // its own turn (e.g. the user said only "好"). Never reassign its role.
        for (role, role_terms) in selected.into_iter().enumerate() {
            if role == 1 && role_terms.is_empty() {
                continue;
            }
            let text = role_terms
                .iter()
                .map(|term| term.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            audio3_messages.push(serde_json::json!({
                "role": if role == 0 { "user" } else { "assistant" },
                "content": [{
                    "type": if role == 0 { "input_text" } else { "text" },
                    "text": text,
                }],
            }));
            for term in role_terms {
                let key = term.text.to_lowercase();
                if let Some(&index) = seen.get(&key) {
                    terms[index].normalization_eligible |= term.normalization_eligible;
                } else {
                    seen.insert(key, terms.len());
                    terms.push(term.clone());
                }
            }
        }
    }
    if terms.is_empty() {
        return None;
    }
    Some(AgentTerminologySnapshot {
        agent,
        terms,
        audio3_messages,
        candidate_count,
        source_char_count,
        extraction_elapsed: Duration::ZERO,
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
    Ok(focused_kitty_agent_from_payload(&payload))
}

fn focused_kitty_agent_from_payload(payload: &Value) -> Option<FocusedAgentProcess> {
    let os_windows = payload.as_array()?;
    for os_window in os_windows {
        if !os_window["is_focused"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(tabs) = os_window["tabs"].as_array() else {
            continue;
        };
        for tab in tabs {
            if !tab["is_active"].as_bool().unwrap_or(false) {
                continue;
            }
            let Some(windows) = tab["windows"].as_array() else {
                continue;
            };
            for window in windows {
                if !window["is_active"].as_bool().unwrap_or(false) {
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
                    return Some(FocusedAgentProcess { kind, pid });
                }
            }
        }
    }

    None
}

#[derive(Deserialize)]
struct PiRegistry {
    version: u32,
    pid: u32,
    process_start_ticks: u64,
    session_id: String,
    session_file: PathBuf,
    #[serde(default)]
    recent_turns: Option<Vec<ConversationTurn>>,
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

fn current_pi_published_turns(
    locator: &AgentSessionLocator,
) -> Result<Option<Vec<ConversationTurn>>> {
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
    let Some(mut turns) = registry.recent_turns else {
        eprintln!("voice-input agent context: reload Pi to publish recent conversation turns");
        return Ok(None);
    };
    turns.drain(..turns.len().saturating_sub(MAX_CONTEXT_TURNS));
    Ok(Some(turns))
}

fn recent_codex_turns(path: &Path, session_id: &str) -> Result<Option<Vec<ConversationTurn>>> {
    let header = first_json_line(path)?;
    if header["payload"]["id"].as_str() != Some(session_id) {
        return Ok(None);
    }
    let values = tail_json_lines(path, MAX_SESSION_SCAN_BYTES)?;
    Ok(Some(codex_turns(&values)))
}

fn codex_turns(values: &[Value]) -> Vec<ConversationTurn> {
    let mut turns: Vec<ConversationTurn> = Vec::new();
    let mut current: Option<usize> = None;
    let mut task_open = false;
    let mut task_id = None;
    let mut explicit_user = false;
    for value in values {
        let payload = &value["payload"];
        if value["type"] == "event_msg" && payload["type"] == "task_started" {
            // A task boundary prevents a tail-orphan answer from being paired
            // with the previous user's request.
            current = None;
            task_open = true;
            task_id = payload["turn_id"].as_str();
            explicit_user = false;
            continue;
        }
        if value["type"] == "event_msg" && payload["type"] == "thread_rolled_back" {
            if let Some(count) = payload["num_turns"].as_u64() {
                turns.truncate(
                    turns
                        .len()
                        .saturating_sub(count.min(usize::MAX as u64) as usize),
                );
            }
            current = None;
            task_open = false;
            task_id = None;
            explicit_user = false;
            continue;
        }
        let event_user = if value["type"] == "event_msg" && payload["type"] == "user_message" {
            payload["message"].as_str().map(ToOwned::to_owned)
        } else if value["type"] == "event_msg"
            && payload["type"] == "item_completed"
            && payload["item"]["type"] == "UserMessage"
        {
            Some(extract_text_blocks(&payload["item"]["content"]))
        } else {
            None
        };
        let is_explicit = event_user.is_some();
        let user = event_user.or_else(|| {
            if value["type"] == "response_item"
                && payload["type"] == "message"
                && payload["role"] == "user"
                && !explicit_user
            {
                codex_user_text(payload)
            } else {
                None
            }
        });
        if let Some(user) = user {
            if current.is_none() || (!task_open && (!is_explicit || explicit_user)) {
                turns.push(ConversationTurn::default());
                current = Some(turns.len() - 1);
            }
            // Codex persists both a response_item and a user event for the
            // same input. The explicit event replaces the fallback; it does
            // not consume an extra context round.
            turns[current.expect("user starts a turn")].user = user;
            explicit_user |= is_explicit;
            continue;
        }
        if value["type"] == "response_item"
            && payload["type"] == "message"
            && payload["role"] == "assistant"
            && payload["phase"] == "final_answer"
            && let Some(index) = current
        {
            let text = extract_output_text_blocks(&payload["content"]);
            if !text.trim().is_empty() {
                let assistant = &mut turns[index].assistant;
                if !assistant.is_empty() {
                    assistant.push('\n');
                }
                assistant.push_str(&text);
            }
        }
        if value["type"] == "event_msg" && payload["type"] == "task_complete" {
            if task_id
                .zip(payload["turn_id"].as_str())
                .is_some_and(|(active, completed)| active != completed)
            {
                continue;
            }
            if let Some(index) = current
                && turns[index].assistant.is_empty()
                && let Some(text) = payload["last_agent_message"].as_str()
            {
                turns[index].assistant = text.to_string();
            }
            current = None;
            task_open = false;
            task_id = None;
            explicit_user = false;
        }
    }
    turns.drain(..turns.len().saturating_sub(MAX_CONTEXT_TURNS));
    turns
}

fn codex_user_text(payload: &Value) -> Option<String> {
    let content = payload["content"].as_array()?;
    let kinds =
        payload["internal_chat_message_metadata_passthrough"]["content_item_kinds"].as_array();
    let text = content
        .iter()
        .enumerate()
        .filter(|(index, block)| {
            block["type"] == "input_text"
                && kinds.is_none_or(|kinds| {
                    kinds.get(*index).and_then(Value::as_str) == Some("user.text")
                })
        })
        .filter_map(|(_, block)| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = text.trim();
    // Legacy sessions predate per-block provenance. Their bootstrap messages
    // have these explicit wrappers; they are not user conversation rounds.
    if trimmed.is_empty()
        || (kinds.is_none()
            && [
                "# AGENTS.md instructions for ",
                "<environment_context>",
                "<permissions instructions>",
            ]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix)))
    {
        return None;
    }
    Some(text)
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
        AgentKind, AgentSessionLocator, AgentTerminologySnapshot, ConversationTurn,
        MAX_AUDIO3_TURN_CHARS, MAX_CONTEXT_TURNS, PiRegistry, build_snapshot, cap_text,
        current_pi_published_turns, extract_terminology, focused_kitty_agent_from_payload,
        recent_codex_turns, sanitize_reference, snapshot_from_candidates,
        start_terminology_capture,
    };

    #[test]
    fn parses_pi_registry_with_active_branch_turns() {
        let registry: PiRegistry = serde_json::from_value(json!({
            "version": 2,
            "pid": 123,
            "process_start_ticks": 456,
            "session_id": "session-1",
            "session_file": "/tmp/session.jsonl",
            "recent_turns": [{"user":"UserModel", "assistant":"AssistantModel"}]
        }))
        .unwrap();
        let turns = registry.recent_turns.unwrap();
        assert_eq!(turns[0].user, "UserModel");
        assert_eq!(turns[0].assistant, "AssistantModel");
    }

    #[test]
    fn pi_publication_preserves_active_turns_and_empty_branch_without_file_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let session_path = directory.path().join("session.jsonl");
        std::fs::write(&session_path, "{\"unrelated_branch\":\"DoNotRead\"}\n").unwrap();
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
        let mut payload = json!({
            "version": 2, "pid": 123, "process_start_ticks": 456,
            "session_id": "session-1", "session_file": session_path,
            "recent_turns": (0..6).map(|index| json!({
                "user": format!("User{index}"), "assistant": format!("Assistant{index}"),
            })).collect::<Vec<_>>(),
        });
        std::fs::write(&registry_path, payload.to_string()).unwrap();
        let turns = current_pi_published_turns(&locator).unwrap().unwrap();
        assert_eq!(turns.len(), 5);
        assert_eq!(turns[0].user, "User1");
        assert_eq!(turns[4].assistant, "Assistant5");
        let frozen = build_snapshot(AgentKind::Pi, &turns, 6_000).unwrap();

        payload["recent_turns"] = json!([]);
        std::fs::write(&registry_path, payload.to_string()).unwrap();
        assert!(
            current_pi_published_turns(&locator)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert!(
            frozen
                .select_for_refinement()
                .terms
                .contains(&"Assistant5".into())
        );

        payload.as_object_mut().unwrap().remove("recent_turns");
        payload["latest_completed_assistant_message"] = json!("LegacyDoNotUse");
        std::fs::write(&registry_path, payload.to_string()).unwrap();
        assert!(current_pi_published_turns(&locator).unwrap().is_none());

        payload["recent_turns"] = json!([{ "user": "StaleSession" }]);
        payload["session_id"] = json!("different-session");
        std::fs::write(&registry_path, payload.to_string()).unwrap();
        assert!(current_pi_published_turns(&locator).unwrap().is_none());
    }

    #[test]
    fn reads_codex_turn_with_user_and_final_answer_only() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for value in [
            json!({"type":"session_meta","payload":{"id":"codex-1","source":"cli","thread_source":"user"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"UserModel"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"working"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"done"}]}}),
        ] {
            writeln!(file, "{value}").unwrap();
        }
        let turns = recent_codex_turns(file.path(), "codex-1").unwrap().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user, "UserModel");
        assert_eq!(turns[0].assistant, "done");
    }

    #[test]
    fn codex_turns_deduplicate_user_events_ignore_injections_and_apply_rollback() {
        let mut values = vec![json!({"type":"response_item","payload":{
            "type":"message", "role":"assistant", "phase":"final_answer",
            "content":[{"type":"output_text","text":"OrphanDoNotUse"}]
        }})];
        for index in 0..6 {
            values.push(json!({"type":"event_msg","payload":{"type":"task_started","turn_id":format!("turn-{index}")}}));
            values.push(json!({"type":"response_item","payload":{
                "type":"message", "role":"user",
                "content":[{"type":"input_text","text":"InjectedDoNotUse"},
                           {"type":"input_text","text":format!("UserModel{index}")}],
                "internal_chat_message_metadata_passthrough":{
                    "content_item_kinds":["agents_md.instructions","user.text"]
                }
            }}));
            values.push(if index % 2 == 0 {
                json!({"type":"event_msg","payload":{"type":"user_message","message":format!("UserModel{index}")}})
            } else {
                json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":format!("turn-{index}"),
                    "item":{"type":"UserMessage","content":[{"type":"text","text":format!("UserModel{index}")}]}}})
            });
            values.push(json!({"type":"response_item","payload":{
                "type":"message","role":"assistant","phase":"commentary",
                "content":[{"type":"output_text","text":"CommentaryDoNotUse"}]
            }}));
            if index != 2 {
                values.push(json!({"type":"response_item","payload":{
                    "type":"message","role":"assistant","phase":"final_answer",
                    "content":[{"type":"output_text","text":format!("AssistantModel{index}")}]
                }}));
            }
            values.push(json!({"type":"event_msg","payload":{"type":"task_complete",
                "turn_id":format!("turn-{index}"),"last_agent_message":format!("FallbackModel{index}")}}));
        }
        let turns = super::codex_turns(&values);
        assert_eq!(turns.len(), 5);
        assert_eq!(turns[0].user, "UserModel1");
        assert_eq!(turns[1].assistant, "FallbackModel2");
        assert_eq!(turns[4].assistant, "AssistantModel5");

        values.push(
            json!({"type":"event_msg","payload":{"type":"thread_rolled_back","num_turns":1}}),
        );
        values
            .push(json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"latest"}}));
        values.push(
            json!({"type":"event_msg","payload":{"type":"user_message","message":"LatestUser"}}),
        );
        let turns = super::codex_turns(&values);
        assert_eq!(turns.len(), 5);
        assert_eq!(turns[0].user, "UserModel1");
        assert_eq!(turns[4].user, "LatestUser");
        assert!(turns[4].assistant.is_empty());
        assert!(turns.iter().all(|turn| !turn.assistant.contains("DoNotUse") && !turn.user.contains("DoNotUse")));
    }

    #[test]
    fn codex_response_only_messages_preserve_rounds_and_filter_bootstrap() {
        let values = vec![
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /tmp\nBootstrap"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"FirstUser"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"FirstAssistant"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"SecondUser"}]}}),
            json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"SecondAssistant"}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"no-user"}}),
            json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"OrphanDoNotUse"}}),
        ];
        let turns = super::codex_turns(&values);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user, "FirstUser");
        assert_eq!(turns[0].assistant, "FirstAssistant");
        assert_eq!(turns[1].user, "SecondUser");
        assert_eq!(turns[1].assistant, "SecondAssistant");
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
        let snapshot = snapshot_from_candidates(AgentKind::Pi, vec![(terms, vec![])], 64).unwrap();
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
    fn kitty_agent_comes_from_focused_os_window_active_tab_and_pane() {
        let payload = json!([
            {
                "is_focused": false,
                "tabs": [{
                    "is_active": true,
                    "windows": [{
                        "is_active": true,
                        "foreground_processes": [{"pid": 101, "cmdline": ["pi"]}]
                    }]
                }]
            },
            {
                "is_focused": true,
                "tabs": [
                    {
                        "is_active": false,
                        "windows": [{
                            "is_active": true,
                            "is_focused": true,
                            "foreground_processes": [{"pid": 202, "cmdline": ["pi"]}]
                        }]
                    },
                    {
                        "is_active": true,
                        "windows": [
                            {
                                "is_active": false,
                                "foreground_processes": [{"pid": 303, "cmdline": ["pi"]}]
                            },
                            {
                                "is_active": true,
                                "foreground_processes": [{
                                    "pid": 404,
                                    "cmdline": ["/opt/pi/bin/pi", "--session", "active"]
                                }]
                            }
                        ]
                    }
                ]
            }
        ]);

        let agent = focused_kitty_agent_from_payload(&payload).unwrap();
        assert_eq!(agent.kind, AgentKind::Pi);
        assert_eq!(agent.pid, 404);
    }

    #[test]
    fn kitty_agent_lookup_fails_closed_without_active_hierarchy() {
        let payload = json!([{
            "is_focused": true,
            "tabs": [{
                "is_active": false,
                "windows": [{
                    "is_active": true,
                    "is_focused": true,
                    "foreground_processes": [{"pid": 202, "cmdline": ["pi"]}]
                }]
            }]
        }]);

        assert!(focused_kitty_agent_from_payload(&payload).is_none());
    }

    #[test]
    fn ordinary_window_does_not_start_terminology_capture() {
        let window: crate::focused_window::FocusedWindowSnapshot =
            serde_json::from_value(json!({"class":"firefox","pid":42})).unwrap();
        assert!(start_terminology_capture(window, 6_000).unwrap().is_none());
    }

    #[test]
    fn terminology_frequency_is_ascending_with_stable_candidate_ties() {
        let snapshot = AgentTerminologySnapshot::from_turns(
            AgentKind::Pi,
            &[(
                "RareModel CommonTerm CommonTerm Qwen-Audio-3 CommonTerm AnotherRare",
                "",
            )],
        );
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
        let text = context.messages[0]["content"][0]["text"].as_str().unwrap();
        assert!(text.chars().count() <= MAX_AUDIO3_TURN_CHARS);
        assert!(
            text.split('\n')
                .all(|selected| terms.iter().any(|term| term == selected))
        );
    }

    #[test]
    fn snapshot_keeps_five_role_preserving_bounded_glossaries_and_refine_union() {
        let source = (0..6)
            .map(|index| ConversationTurn {
                user: format!(
                    "UserModel{index} SharedModel\nAPI_KEY={}HiddenSecret",
                    "x".repeat(1_000)
                ),
                assistant: format!(
                    "AssistantModel{index} SharedModel {}",
                    "ExtraModel ".repeat(200)
                ),
            })
            .collect::<Vec<_>>();
        let snapshot = build_snapshot(AgentKind::Pi, &source, 6_000).unwrap();
        assert!(snapshot.source_char_count <= 6_000);
        let context = snapshot.select_for_audio3().unwrap();
        assert_eq!(context.messages.len(), MAX_CONTEXT_TURNS * 2);
        let mut asr_terms = std::collections::HashSet::new();
        for (index, pair) in context.messages.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(pair[0]["role"], "user");
            assert_eq!(pair[0]["content"][0]["type"], "input_text");
            assert_eq!(pair[1]["role"], "assistant");
            assert_eq!(pair[1]["content"][0]["type"], "text");
            let user = pair[0]["content"][0]["text"].as_str().unwrap();
            let assistant = pair[1]["content"][0]["text"].as_str().unwrap();
            assert!(user.contains(&format!("UserModel{}", index + 1)));
            assert!(assistant.contains(&format!("AssistantModel{}", index + 1)));
            assert!(user.chars().count() + assistant.chars().count() <= MAX_AUDIO3_TURN_CHARS);
            for term in user.lines().chain(assistant.lines()) {
                assert!(!term.contains("HiddenSecret") && !term.contains("REDACTED"));
                asr_terms.insert(term.to_lowercase());
            }
        }
        let refined = snapshot.select_for_refinement();
        let refine_terms = refined
            .terms
            .iter()
            .map(|term| term.to_lowercase())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(asr_terms, refine_terms);
        assert_eq!(refine_terms.len(), refined.terms.len());
        assert!(!refined.terms.contains(&"UserModel0".into()));
    }

    #[test]
    fn both_roles_share_400_characters_without_splitting_unicode_terms() {
        let candidates = (0..80)
            .map(|index| super::TerminologyTerm {
                text: format!("术语{index}{}", "甲".repeat(15)),
                frequency: 1,
                candidate_order: index,
                normalization_eligible: false,
            })
            .collect::<Vec<_>>();
        let snapshot = snapshot_from_candidates(
            AgentKind::Codex,
            vec![(candidates[..40].to_vec(), candidates[40..].to_vec())],
            3_000,
        )
        .unwrap();
        let messages = snapshot.select_for_audio3().unwrap().messages;
        let user = messages[0]["content"][0]["text"].as_str().unwrap();
        let assistant = messages[1]["content"][0]["text"].as_str().unwrap();
        assert!(!user.is_empty() && !assistant.is_empty());
        assert!(user.chars().count() + assistant.chars().count() <= MAX_AUDIO3_TURN_CHARS);
        assert!(user.lines().all(|term| {
            candidates[..40]
                .iter()
                .any(|candidate| candidate.text == term)
        }));
        assert!(assistant.lines().all(|term| {
            candidates[40..]
                .iter()
                .any(|candidate| candidate.text == term)
        }));
    }

    #[test]
    fn empty_roles_do_not_reassign_assistant_terms_or_backfill_older_turns() {
        let source = vec![
            ConversationTurn {
                user: "好".into(),
                assistant: "AssistantModel".into(),
            },
            ConversationTurn {
                user: "PendingUser".into(),
                assistant: String::new(),
            },
        ];
        let snapshot = build_snapshot(AgentKind::Pi, &source, 6_000).unwrap();
        let messages = snapshot.select_for_audio3().unwrap().messages;
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "");
        assert_eq!(messages[1]["role"], "assistant");
        assert!(
            messages[1]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("AssistantModel")
        );
        assert_eq!(messages[2]["role"], "user");

        let mut source = vec![ConversationTurn {
            user: "TooOld".into(),
            assistant: String::new(),
        }];
        source.extend((0..5).map(|_| ConversationTurn {
            user: "好".into(),
            assistant: String::new(),
        }));
        assert!(build_snapshot(AgentKind::Pi, &source, 6_000).is_none());
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

        assert!(terminology.len() > 96);
        let snapshot = snapshot_from_candidates(
            AgentKind::Pi,
            vec![(terminology, vec![])],
            sanitized.chars().count(),
        )
        .unwrap();
        let refinement = snapshot.select_for_refinement();
        assert!(refinement.char_count <= MAX_AUDIO3_TURN_CHARS);
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
        assert!(
            audio3.messages[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= MAX_AUDIO3_TURN_CHARS
        );
    }
}
