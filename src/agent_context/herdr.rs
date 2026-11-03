//! Resolve the focused pane of the Herdr client in the active Kitty window.
//! Herdr owns the PTYs in a separate server, so Kitty cannot report agent PIDs.

use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    AgentKind, AgentSessionLocator, FocusedAgentSnapshot, executable_name,
    process_environment_value,
};

const QUERY_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const REQUEST_ID: &str = "voice-input";

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SessionRefKind {
    Id,
    Path,
}

#[derive(Deserialize)]
pub(super) struct SessionRef {
    agent: String,
    kind: SessionRefKind,
    value: String,
}

impl SessionRef {
    pub(super) fn matches(&self, locator: &AgentSessionLocator) -> bool {
        if agent_kind(&self.agent) != Some(locator.kind) || self.value.is_empty() {
            return false;
        }
        match self.kind {
            SessionRefKind::Id => self.value == locator.session_id,
            SessionRefKind::Path => Path::new(&self.value)
                .canonicalize()
                .is_ok_and(|path| path == locator.session_path),
        }
    }
}

#[derive(Deserialize)]
struct Pane {
    pane_id: String,
    focused: bool,
    agent: Option<String>,
    agent_session: Option<SessionRef>,
}

fn agent_kind(name: &str) -> Option<AgentKind> {
    match name {
        "pi" => Some(AgentKind::Pi),
        "codex" => Some(AgentKind::Codex),
        _ => None,
    }
}

pub(super) fn capture(pid: u32, args: &[&str]) -> Result<Option<FocusedAgentSnapshot>> {
    let Some(socket) = socket_path(args, |key| process_environment_value(pid, key)) else {
        return Ok(None);
    };
    capture_from_socket(&socket)
}

fn capture_from_socket(socket: &Path) -> Result<Option<FocusedAgentSnapshot>> {
    // Do not pass caller_pane_id (or use the daemon's HERDR_PANE_ID): dictation
    // follows the human's focused pane, not the pane that launched Voice Input.
    let response = query(socket, "pane.current", json!({}))?;
    let pane: Pane =
        serde_json::from_value(response["pane"].clone()).context("invalid Herdr focused pane")?;
    if !pane.focused || pane.pane_id.is_empty() {
        return Ok(None);
    }
    let Some(kind) = pane.agent.as_deref().and_then(agent_kind) else {
        return Ok(None);
    };
    let Some(session) = pane.agent_session else {
        return Ok(None);
    };
    if agent_kind(&session.agent) != Some(kind) || session.value.is_empty() {
        return Ok(None);
    }

    // Pin this pane, even if focus changes between the two requests.
    let response = query(
        socket,
        "pane.process_info",
        json!({"pane_id": pane.pane_id}),
    )?;
    let info = &response["process_info"];
    if info["pane_id"].as_str() != Some(&pane.pane_id) {
        return Ok(None);
    }
    let Some(processes) = info["foreground_processes"].as_array() else {
        return Ok(None);
    };
    let mut candidates = processes.iter().filter_map(|process| {
        let pid = u32::try_from(process["pid"].as_u64()?)
            .ok()
            .filter(|pid| *pid > 0)?;
        let executable = process["argv"].as_array()?.first()?.as_str()?;
        (executable_name(executable).and_then(agent_kind) == Some(kind)).then_some(pid)
    });
    let Some(pid) = candidates.next() else {
        return Ok(None);
    };
    if candidates.next().is_some() {
        return Ok(None);
    }
    Ok(Some(FocusedAgentSnapshot {
        kind,
        pid,
        herdr_session: Some(session),
    }))
}

fn socket_path(args: &[&str], env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let mut session = None;
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        match *arg {
            "--session" => session = Some(args.next()?.to_string()),
            "--handoff" => {}
            "session" if args.next() == Some(&"attach") => {
                session = Some(args.next()?.to_string());
            }
            arg if arg.starts_with("--session=") => session = Some(arg[10..].to_string()),
            // Remote attach and helper/server commands must never resolve to
            // an unrelated local server, even if a default socket exists.
            _ => return None,
        }
    }
    if session.is_none() {
        if let Some(socket) = env("HERDR_SOCKET_PATH").filter(|value| !value.is_empty()) {
            return Some(socket.into());
        }
        session = env("HERDR_SESSION").filter(|value| !value.is_empty());
    }
    let config_dir = if let Some(config) = env("HERDR_CONFIG_PATH") {
        PathBuf::from(config).parent()?.to_path_buf()
    } else if let Some(config_home) = env("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        PathBuf::from(config_home).join("herdr")
    } else {
        env("HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir)?
            .join(".config/herdr")
    };
    match session {
        Some(name) => {
            // Named sessions are a single directory component.
            if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                return None;
            }
            Some(config_dir.join("sessions").join(name).join("herdr.sock"))
        }
        None => Some(config_dir.join("herdr.sock")),
    }
}

fn query(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let mut stream = connect(socket)?;
    stream.set_write_timeout(Some(QUERY_TIMEOUT))?;
    let request = json!({"id": REQUEST_ID, "method": method, "params": params});
    writeln!(stream, "{request}").context("failed to query Herdr")?;

    let mut response = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("Herdr query timed out");
        }
        stream.set_read_timeout(Some(remaining))?;
        let mut chunk = [0; 4096];
        let count = stream
            .read(&mut chunk)
            .context("failed to read Herdr response")?;
        if count == 0 {
            bail!("Herdr disconnected before completing its response");
        }
        let newline = chunk[..count].iter().position(|byte| *byte == b'\n');
        let end = newline.unwrap_or(count);
        if response.len() + end > MAX_RESPONSE_BYTES {
            bail!("Herdr response exceeded its size limit");
        }
        response.extend_from_slice(&chunk[..end]);
        if newline.is_some() {
            break;
        }
    }
    let response: Value = serde_json::from_slice(&response).context("invalid Herdr response")?;
    if response["id"].as_str() != Some(REQUEST_ID) || response.get("error").is_some() {
        bail!("Herdr request failed or returned a mismatched response");
    }
    let expected_type = method.replace('.', "_");
    if response["result"]["type"].as_str() != Some(&expected_type) {
        bail!("unexpected Herdr response type");
    }
    Ok(response["result"].clone())
}

fn connect(path: &Path) -> Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is a C POD struct; zero initialization also terminates sun_path.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        bail!("invalid Herdr socket path");
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dest, source) in address.sun_path.iter_mut().zip(bytes) {
        *dest = *source as libc::c_char;
    }
    // Nonblocking connect avoids waiting indefinitely on a full server backlog.
    // SAFETY: socket has no pointer parameters and returns an owned descriptor.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to create Herdr socket");
    }
    // SAFETY: fd is a newly created descriptor and ownership is transferred exactly once.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    // SAFETY: address is initialized, len stays within it, and fd is a live socket.
    if unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            len,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("failed to connect to Herdr");
    }
    let stream = UnixStream::from(fd);
    stream
        .set_nonblocking(false)
        .map_err(|_| anyhow!("failed to configure Herdr socket"))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader},
        os::unix::{fs::MetadataExt, net::UnixListener},
        thread,
    };

    use super::*;
    use crate::agent_context::{load_source, process_start_ticks};

    fn mock_server(responses: Vec<Value>) -> (tempfile::TempDir, PathBuf, thread::JoinHandle<()>) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = thread::spawn(move || {
            for (index, result) in responses.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(&stream).read_line(&mut request).unwrap();
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_eq!(request["id"], REQUEST_ID);
                if index == 0 {
                    assert_eq!(request["method"], "pane.current");
                    assert_eq!(request["params"], json!({}));
                } else {
                    assert_eq!(request["method"], "pane.process_info");
                    assert_eq!(request["params"], json!({"pane_id": "w1:p2"}));
                }
                writeln!(stream, "{}", json!({"id": REQUEST_ID, "result": result})).unwrap();
            }
        });
        (directory, path, worker)
    }

    fn pane(agent: &str, kind: &str, value: &str) -> Value {
        json!({"type": "pane_current", "pane": {
            "pane_id": "w1:p2", "focused": true, "agent": agent,
            "agent_session": {"agent": agent, "kind": kind, "value": value}
        }})
    }

    fn processes(agent: &str) -> Value {
        json!({"type": "pane_process_info", "process_info": {
            "pane_id": "w1:p2", "foreground_processes": [
                {"pid": 1, "argv": ["bash"]},
                {"pid": std::process::id(), "argv": [format!("/usr/bin/{agent}")]}
            ]
        }})
    }

    #[test]
    fn focused_herdr_pi_uses_matching_active_branch_publication() {
        let directory = tempfile::tempdir().unwrap();
        let session_path = directory.path().join("pi.jsonl");
        fs::write(&session_path, "{\"unrelated_branch\":\"DoNotRead\"}\n").unwrap();
        let session_path = session_path.canonicalize().unwrap();
        let metadata = fs::metadata(&session_path).unwrap();
        let registry_path = directory.path().join("registry.json");
        let pid = std::process::id();
        let start_ticks = process_start_ticks(pid).unwrap();
        fs::write(
            &registry_path,
            json!({
                "version": 2, "pid": pid, "process_start_ticks": start_ticks,
                "session_id": "pi-session", "session_file": session_path,
                "recent_turns": [{"user": "FocusedUser", "assistant": "FocusedAssistant"}]
            })
            .to_string(),
        )
        .unwrap();
        let (_server_dir, socket, worker) = mock_server(vec![
            pane("pi", "path", session_path.to_str().unwrap()),
            processes("pi"),
        ]);
        let snapshot = capture_from_socket(&socket).unwrap().unwrap();
        worker.join().unwrap();
        assert_eq!(snapshot.kind, AgentKind::Pi);
        assert_eq!(snapshot.pid, pid);
        let mut locator = AgentSessionLocator {
            kind: snapshot.kind,
            pid,
            process_start_ticks: start_ticks,
            session_id: "pi-session".into(),
            session_path,
            device: metadata.dev(),
            inode: metadata.ino(),
            pi_registry_path: Some(registry_path),
        };
        let session = snapshot.herdr_session.unwrap();
        assert!(session.matches(&locator));
        let (_, turns) = load_source(&locator).unwrap().unwrap();
        assert_eq!(turns[0].user, "FocusedUser");
        assert_eq!(turns[0].assistant, "FocusedAssistant");
        locator.session_path = directory.path().join("other.jsonl");
        assert!(!session.matches(&locator));
    }

    #[test]
    fn focused_herdr_codex_uses_matching_session_id_and_history() {
        let directory = tempfile::tempdir().unwrap();
        let session_path = directory.path().join("codex.jsonl");
        fs::write(&session_path, [
            json!({"type":"session_meta", "payload":{"id":"codex-session"}}),
            json!({"type":"event_msg", "payload":{"type":"user_message", "message":"CodexUser"}}),
            json!({"type":"response_item", "payload":{"type":"message", "role":"assistant", "phase":"final_answer", "content":[{"type":"output_text", "text":"CodexAnswer"}]}}),
        ].iter().map(|value| format!("{value}\n")).collect::<String>()).unwrap();
        let metadata = fs::metadata(&session_path).unwrap();
        let (_server_dir, socket, worker) = mock_server(vec![
            pane("codex", "id", "codex-session"),
            processes("codex"),
        ]);
        let snapshot = capture_from_socket(&socket).unwrap().unwrap();
        worker.join().unwrap();
        let mut locator = AgentSessionLocator {
            kind: snapshot.kind,
            pid: snapshot.pid,
            process_start_ticks: process_start_ticks(snapshot.pid).unwrap(),
            session_id: "codex-session".into(),
            session_path,
            device: metadata.dev(),
            inode: metadata.ino(),
            pi_registry_path: None,
        };
        let session = snapshot.herdr_session.unwrap();
        assert!(session.matches(&locator));
        let (_, turns) = load_source(&locator).unwrap().unwrap();
        assert_eq!(turns[0].user, "CodexUser");
        assert_eq!(turns[0].assistant, "CodexAnswer");
        locator.session_id = "different-session".into();
        assert!(!session.matches(&locator));
    }

    #[test]
    fn kitty_routes_only_its_active_herdr_client_to_the_socket() {
        let (_directory, socket, worker) =
            mock_server(vec![pane("pi", "id", "focused-session"), processes("pi")]);
        let mut client = std::process::Command::new("sh")
            .args(["-c", "printf 'ready\\n'; read -r _"])
            .env("HERDR_SOCKET_PATH", &socket)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // spawn() can return before /proc exposes the child's new environment.
        // Keep the shell alive without another exec after this readiness signal.
        let mut ready = String::new();
        BufReader::new(client.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        assert_eq!(
            process_environment_value(client.id(), "HERDR_SOCKET_PATH"),
            Some(socket.to_str().unwrap().to_string())
        );
        let payload = json!([{"is_focused": true, "tabs": [{
            "is_active": true, "windows": [
                {"is_active": false, "foreground_processes": [{"pid": 1, "cmdline": ["pi"]}]},
                {"is_active": true, "foreground_processes": [{"pid": client.id(), "cmdline": ["herdr"]}]}
            ]
        }]}]);
        let result = crate::agent_context::focused_agent_from_kitty_payload(&payload);
        client.kill().unwrap();
        client.wait().unwrap();
        let snapshot = result.unwrap().unwrap();
        assert_eq!(snapshot.kind, AgentKind::Pi);
        assert_eq!(snapshot.pid, std::process::id());
        assert_eq!(snapshot.herdr_session.unwrap().value, "focused-session");
        worker.join().unwrap();
    }

    #[test]
    #[ignore = "requires a local Herdr client PID in VOICE_INPUT_TEST_HERDR_PID"]
    fn live_herdr_context() {
        let pid = std::env::var("VOICE_INPUT_TEST_HERDR_PID")
            .unwrap()
            .parse()
            .unwrap();
        let snapshot = capture(pid, &["herdr"]).unwrap().unwrap();
        let locator = crate::agent_context::resolve_focused_session(snapshot)
            .unwrap()
            .unwrap();
        let (agent, turns) = load_source(&locator).unwrap().unwrap();
        assert!(!turns.is_empty());
        eprintln!(
            "live Herdr: agent={}, recent_turns={}",
            agent.label(),
            turns.len()
        );
    }

    #[test]
    fn missing_identity_or_unfocused_pane_does_not_select_another_agent() {
        let mut cases = vec![
            pane("pi", "path", "/tmp/session"),
            pane("pi", "id", "session"),
        ];
        cases[0]["pane"]["focused"] = json!(false);
        cases[1]["pane"]["agent_session"] = Value::Null;
        cases.push(pane("unsupported", "id", "session"));
        for response in cases {
            let (_directory, socket, worker) = mock_server(vec![response]);
            assert!(capture_from_socket(&socket).unwrap().is_none());
            worker.join().unwrap();
        }
    }

    #[test]
    fn changed_pane_or_agent_process_is_rejected() {
        let mut wrong_pane = processes("pi");
        wrong_pane["process_info"]["pane_id"] = json!("w2:p1");
        let mut ambiguous = processes("pi");
        ambiguous["process_info"]["foreground_processes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"pid": 42, "argv": ["pi"]}));
        for response in [wrong_pane, processes("codex"), ambiguous] {
            let (_directory, socket, worker) =
                mock_server(vec![pane("pi", "id", "session"), response]);
            assert!(capture_from_socket(&socket).unwrap().is_none());
            worker.join().unwrap();
        }
    }

    #[test]
    fn socket_selection_uses_focused_client_environment_and_named_session() {
        let env = |key: &str| match key {
            "HOME" => Some("/home/test".into()),
            "HERDR_SOCKET_PATH" => Some("/tmp/custom.sock".into()),
            "HERDR_SESSION" => Some("environment".into()),
            _ => None,
        };
        assert_eq!(
            socket_path(&["herdr"], env),
            Some("/tmp/custom.sock".into())
        );
        assert_eq!(
            socket_path(&["herdr", "--session", "work"], env),
            Some("/home/test/.config/herdr/sessions/work/herdr.sock".into())
        );
        assert_eq!(
            socket_path(&["herdr", "session", "attach", "work"], env),
            socket_path(&["herdr", "--session", "work"], env)
        );
        assert_eq!(
            socket_path(&["herdr"], |key| match key {
                "XDG_CONFIG_HOME" => Some("/config".into()),
                "HERDR_SESSION" => Some("work".into()),
                _ => None,
            }),
            Some("/config/herdr/sessions/work/herdr.sock".into())
        );
        assert_eq!(
            socket_path(&["herdr"], |key| (key == "HERDR_CONFIG_PATH")
                .then(|| "/custom/config.toml".into())),
            Some("/custom/herdr.sock".into())
        );
        for args in [
            vec!["herdr", "--remote", "host"],
            vec!["herdr", "--machine", "host"],
            vec!["herdr", "server"],
            vec!["herdr", "--session", "../other"],
        ] {
            assert!(socket_path(&args, env).is_none());
        }
    }

    #[test]
    fn socket_rejects_invalid_mismatched_oversized_and_incomplete_responses() {
        for response in [
            b"invalid\n".to_vec(),
            b"{\"id\":\"other\",\"result\":{\"type\":\"pane_current\"}}\n".to_vec(),
            b"{\"id\":\"voice-input\",\"error\":{\"message\":\"failed\"}}\n".to_vec(),
            b"{\"id\":\"voice-input\",\"result\":{\"type\":\"other\"}}\n".to_vec(),
            vec![b'x'; MAX_RESPONSE_BYTES + 1],
            b"{\"id\":".to_vec(),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("herdr.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let worker = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(&stream).read_line(&mut request).unwrap();
                let _ = stream.write_all(&response);
            });
            assert!(query(&path, "pane.current", json!({})).is_err());
            worker.join().unwrap();
        }
    }

    #[test]
    fn unresponsive_socket_has_a_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(QUERY_TIMEOUT * 2);
        });
        let started = Instant::now();
        assert!(query(&path, "pane.current", json!({})).is_err());
        assert!(started.elapsed() < QUERY_TIMEOUT * 2);
        worker.join().unwrap();
    }
}
