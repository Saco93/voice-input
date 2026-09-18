#![cfg(target_os = "linux")]

use std::{
    fs,
    io::{Read, Write},
    net::Shutdown,
    os::unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

const TEXT: &str = "完整保留：你好，世界。 café\n第二行包含 <b>原文</b> 与 \"引号\"。";
const TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        // Keep Unix socket paths short even if the caller's TMPDIR is long.
        let fixture = Self {
            directory: tempfile::Builder::new()
                .prefix("history-recording-")
                .tempdir_in("/tmp")
                .unwrap(),
        };
        for directory in ["bin", "runtime", "config/voice-input", "home", "tmp"] {
            fs::create_dir_all(fixture.path(directory)).unwrap();
        }
        fs::set_permissions(fixture.path("runtime"), fs::Permissions::from_mode(0o700)).unwrap();

        // PATH contains only these common tools and our fixtures, never the
        // caller's audio, desktop, clipboard, credential, or provider commands.
        for tool in ["cat", "sleep", "grep"] {
            let source = ["/usr/bin", "/bin"]
                .into_iter()
                .map(|directory| Path::new(directory).join(tool))
                .find(|path| path.is_file())
                .unwrap_or_else(|| panic!("required core utility missing: {tool}"));
            std::os::unix::fs::symlink(source, fixture.path(&format!("bin/{tool}"))).unwrap();
        }
        fs::write(fixture.path("transcript.txt"), TEXT).unwrap();
        // 400 ms: enough to make capture ready (320 ms), below the local
        // partial-transcription threshold (500 ms). Only Stop runs the backend.
        let pcm: Vec<u8> = (0..6_400)
            .flat_map(|sample| {
                if sample % 2 == 0 {
                    1_000_i16
                } else {
                    -1_000_i16
                }
                .to_le_bytes()
            })
            .collect();
        fs::write(fixture.path("audio.pcm"), pcm).unwrap();
        fixture.script(
            "pw-record",
            "cat \"$FIXTURE/audio.pcm\"\nprintf ready > \"$FIXTURE/pcm-sent\"\n# Hold stdout open until Stop kills this PID; also self-limit on failure.\nexec sleep 20\n",
        );
        fixture.script(
            "local-backend",
            "for argument do wav=$argument; done\n[ -s \"$wav\" ]\ncat \"$wav\" > \"$FIXTURE/captured.wav\"\nprintf 'called\\n' >> \"$FIXTURE/backend-calls\"\ncat \"$FIXTURE/transcript.txt\"\n",
        );
        // The local CLI parser currently extracts one stdout line. Stub its
        // external script converter too, so the completed backend result is
        // genuinely multiline without changing production text parsing.
        fixture.script(
            "opencc",
            "cat > \"$FIXTURE/conversion-input\"\ncat \"$FIXTURE/transcript.txt\"\n",
        );
        fixture.script(
            "wl-copy",
            "if [ \"${1-}\" = --clear ]; then exit 0; fi\ngrep -F -- \"$EXPECTED_JSON_TEXT\" \"$HISTORY\" > /dev/null\ncat \"$HISTORY\" > \"$FIXTURE/history-at-paste.json\"\ncat > \"$FIXTURE/attempted.txt\"\nif [ ! -f \"$FIXTURE/allow-paste\" ]; then\n  printf 'failed\\n' > \"$FIXTURE/delivery-failed\"\n  exit 17\nfi\n",
        );
        fixture.script("wl-paste", "exit 1\n");
        fixture.script("systemctl", "exit 0\n");
        fixture.script(
            "hyprctl",
            "case \"$1\" in\n  activewindow) printf '{\"xwayland\":false}\\n' ;;\n  dispatch) printf 'pasted\\n' > \"$FIXTURE/paste-dispatched\" ;;\n  *) exit 99 ;;\nesac\n",
        );
        for tool in [
            "xclip",
            "xdotool",
            "wtype",
            "ydotool",
            "fcitx5-remote",
            "systemd-creds",
            "notify-send",
            "wpctl",
            "pactl",
        ] {
            fixture.script(
                tool,
                "printf '%s\\n' \"$0 $*\" >> \"$FIXTURE/unexpected-command\"\nexit 99\n",
            );
        }
        fs::write(
            fixture.path("config/voice-input/config.toml"),
            r#"state_file = "auto"
[audio]
pre_roll_enabled = false
pre_roll_ms = 0
sample_rate = 16000
max_duration_secs = 30
partial_interval_ms = 50
[asr]
provider = "local-cli"
backend_command = "local-backend"
language = "simplified-chinese"
fallback_to_local = false
[llm]
enabled = false
agent_context_enabled = false
[ime]
manage_fcitx5 = false
force_ascii_before_output = false
[output]
mode = "paste"
pre_type_delay_ms = 0
[hud]
enabled = false
"#,
        )
        .unwrap();
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.path(&format!("bin/{name}"));
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn history(&self) -> Vec<u8> {
        fs::read(self.path("runtime/voice-input/history.json")).unwrap()
    }

    fn spawn(&self, log_name: &str) -> Daemon {
        let log_path = self.path(log_name);
        let log = fs::File::create(&log_path).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_voice-input"))
            .arg("daemon")
            .env_clear() // Includes real display, session, D-Bus and provider credentials.
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .env("TMPDIR", self.path("tmp"))
            .env("PATH", self.path("bin"))
            .env("FIXTURE", self.directory.path())
            .env("HISTORY", self.path("runtime/voice-input/history.json"))
            .env("EXPECTED_JSON_TEXT", serde_json::to_string(TEXT).unwrap())
            .current_dir(self.directory.path())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut daemon = Daemon {
            child: Some(child),
            socket: self.path("runtime/voice-input/control.sock"),
            state: self.path("runtime/voice-input/state.json"),
            log: log_path,
        };
        daemon.wait_for("control socket", |daemon| {
            UnixStream::connect(&daemon.socket).is_ok()
        });
        daemon
    }
}

struct Daemon {
    child: Option<Child>,
    socket: PathBuf,
    state: PathBuf,
    log: PathBuf,
}

impl Daemon {
    fn logs(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn wait_for(&mut self, description: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "daemon exited waiting for {description}: {}",
                self.logs()
            );
            if condition(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}: {}\nstate: {}",
                self.logs(),
                fs::read_to_string(&self.state).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn control(&self, command: &str) -> String {
        let mut stream = UnixStream::connect(&self.socket).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(TIMEOUT)).unwrap();
        stream.write_all(command.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream
            .take(64 * 1024)
            .read_to_string(&mut response)
            .unwrap_or_else(|error| panic!("{command}: {error}; {}", self.logs()));
        response
    }

    fn start_recording(&mut self) {
        let sent = self.log.parent().unwrap().join("pcm-sent");
        let _ = fs::remove_file(&sent);
        assert_eq!(self.control("start wayland"), "ok\n", "{}", self.logs());
        // Test capture itself, not the independently updated HUD phase. Stop's
        // normal drain interval lets the reader consume the remaining pipe bytes.
        self.wait_for("synthetic PCM capture", |_| sent.exists());
    }

    fn terminate(&mut self) -> bool {
        let Some(mut child) = self.child.take() else {
            return true;
        };
        // The daemon owns this group, including its synthetic recorder. Cleanup
        // runs during unwinding too; no wait() can hang a failing CI test.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                _ => return false,
            }
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if !self.terminate() {
            eprintln!("failed to reap isolated history-test daemon within timeout");
        }
    }
}

#[test]
fn completed_text_precedes_failed_delivery_and_survives_daemon_restart() {
    let fixture = Fixture::new();
    let mut daemon = fixture.spawn("first-daemon.log");
    daemon.start_recording();
    let response = daemon.control("stop");
    assert!(response.starts_with("error:"), "{response}");
    assert!(response.contains("wl-copy failed"), "{response}");
    assert!(fixture.path("delivery-failed").exists());
    assert!(!fixture.path("paste-dispatched").exists());
    assert_eq!(
        fs::read_to_string(fixture.path("attempted.txt")).unwrap(),
        TEXT
    );

    let persisted = fixture.history();
    let entries: Value = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert_eq!(entries[0]["text"], TEXT);
    assert!(entries[0]["completed_at_ms"].as_u64().unwrap() > 0);
    let id = entries[0]["id"].as_u64().unwrap();
    assert!(id > 0);
    assert_eq!(
        fs::read(fixture.path("history-at-paste.json")).unwrap(),
        persisted,
        "complete history must already exist when wl-copy attempts delivery"
    );
    let wav = fs::read(fixture.path("captured.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[44..], fs::read(fixture.path("audio.pcm")).unwrap());
    assert!(daemon.terminate());

    // Restart the actual binary with the same isolated runtime directory.
    let mut restarted = fixture.spawn("restarted-daemon.log");
    assert_eq!(fixture.history(), persisted);
    fs::write(fixture.path("allow-paste"), b"").unwrap();
    fs::remove_file(fixture.path("attempted.txt")).unwrap();
    assert_eq!(restarted.control(&format!("history-paste {id}")), "ok\n");
    assert_eq!(
        fs::read_to_string(fixture.path("attempted.txt")).unwrap(),
        TEXT
    );
    assert!(fixture.path("paste-dispatched").exists());
    assert!(restarted.logs().contains("hint=none"));
    assert_eq!(
        fixture.history(),
        persisted,
        "replay must not consume history"
    );

    restarted.start_recording();
    assert_eq!(restarted.control("cancel"), "ok\n");
    assert_eq!(
        fixture.history(),
        persisted,
        "cancellation must not add history"
    );
    assert_eq!(
        fs::read_to_string(fixture.path("backend-calls")).unwrap(),
        "called\n"
    );
    assert!(!fixture.path("unexpected-command").exists());
    assert!(restarted.terminate());
}
