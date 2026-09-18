use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::Path,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

fn run(runtime: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_voice-input"))
        .args(arguments)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", runtime.join("unused-config"))
        .output()
        .expect("history command should run")
}

fn seed(runtime: &Path) -> std::path::PathBuf {
    let directory = runtime.join("voice-input");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("history.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!([
            {"id": 1, "completed_at_ms": 10, "text": "第一条\n完整文本"},
            {"id": 2, "completed_at_ms": 20, "text": "<b>plain text</b> 🦀"}
        ]))
        .unwrap(),
    )
    .unwrap();
    path
}

#[test]
fn history_shortcut_defaults_to_ctrl_f9_in_generated_and_packaged_bindings() {
    let runtime = tempfile::tempdir().unwrap();
    let output = run(runtime.path(), &["setup", "hyprland"]);
    assert!(output.status.success(), "{:?}", output);
    let generated = String::from_utf8(output.stdout).unwrap();
    let packaged = include_str!("../assets/omarchy-hyprland-snippet.conf");
    let expected = "bindd = CTRL, F9, Voice input history, exec, voice-input history";
    for snippet in [generated.as_str(), packaged] {
        let bindings: Vec<_> = snippet
            .lines()
            .filter(|line| line.ends_with("exec, voice-input history"))
            .collect();
        assert_eq!(bindings, vec![expected]);
    }
}

#[test]
fn history_list_needs_no_daemon_or_configuration_and_preserves_full_text() {
    let runtime = tempfile::tempdir().unwrap();
    let empty = run(runtime.path(), &["history", "list"]);
    assert!(empty.status.success(), "{:?}", empty);
    assert_eq!(
        serde_json::from_slice::<Value>(&empty.stdout).unwrap(),
        json!([])
    );

    let path = seed(runtime.path());
    let original = fs::read(&path).unwrap();
    let listed = run(runtime.path(), &["history", "list"]);
    assert!(listed.status.success(), "{:?}", listed);
    let entries: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(entries[0]["id"], 2);
    assert_eq!(entries[1]["id"], 1);
    assert_eq!(entries[1]["text"], "第一条\n完整文本");
    assert_eq!(fs::read(&path).unwrap(), original);
}

fn replay_response(response: &'static str) -> Output {
    let runtime = tempfile::tempdir().unwrap();
    let path = seed(runtime.path());
    let original = fs::read(&path).unwrap();
    let listener = UnixListener::bind(runtime.path().join("voice-input/control.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "CLI did not contact the daemon");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("failed to accept control request: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut command = String::new();
        stream.read_to_string(&mut command).unwrap();
        // No text, recorded-window identity, or old Wayland/XWayland hint is sent.
        assert_eq!(command, "history-paste 1");
        stream.write_all(response.as_bytes()).unwrap();
    });
    let output = run(runtime.path(), &["history", "paste", "1"]);
    server.join().unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        original,
        "replay must not consume history"
    );
    output
}

#[test]
fn history_paste_sends_only_the_selected_id_and_keeps_the_entry() {
    let output = replay_response("ok\n");
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(output.stdout, b"ok\n");
}

#[test]
fn history_paste_failure_is_reported_to_the_picker_without_losing_the_entry() {
    let output = replay_response("error: Voice Input is busy; try again\n");
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Voice Input is busy")
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn disconnected_daemon_does_not_report_successful_paste() {
    let output = replay_response("");
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("did not acknowledge")
    );
}

#[test]
fn history_rejects_invalid_actions_and_ids_before_contacting_the_daemon() {
    let runtime = tempfile::tempdir().unwrap();
    for arguments in [
        vec!["history", "paste", "0"],
        vec!["history", "paste", "-1"],
        vec!["history", "paste", "not-an-id"],
        vec!["history", "paste", "1", "extra"],
        vec!["history", "list", "extra"],
    ] {
        let output = run(runtime.path(), &arguments);
        assert!(!output.status.success(), "accepted {arguments:?}");
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("connect to daemon")
        );
    }
}
