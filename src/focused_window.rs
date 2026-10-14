use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const QUERY_TIMEOUT_SECS: &str = "1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RefinementCategory {
    #[default]
    Default,
    WeChat,
}

impl RefinementCategory {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::WeChat => "instant-messaging",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FocusedWindowSnapshot {
    #[serde(default)]
    class: String,
    #[serde(default, rename = "initialClass")]
    initial_class: String,
    pid: u32,
}

impl FocusedWindowSnapshot {
    pub(crate) fn refinement_category(&self) -> RefinementCategory {
        const INSTANT_MESSAGING_CLASSES: [&str; 6] = [
            "wechat",
            "Feishu",
            "feishu",
            "signal",
            "org.telegram.desktop",
            "TelegramDesktop",
        ];
        if INSTANT_MESSAGING_CLASSES
            .iter()
            .any(|class| self.class == *class || self.initial_class == *class)
        {
            RefinementCategory::WeChat
        } else {
            RefinementCategory::Default
        }
    }

    pub(crate) fn is_kitty(&self) -> bool {
        // Launchers can override the window class; use the actual executable.
        std::fs::read_link(format!("/proc/{}/exe", self.pid))
            .is_ok_and(|executable| executable.file_name().is_some_and(|name| name == "kitty"))
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn control_hint(&self) -> String {
        if self.refinement_category() == RefinementCategory::WeChat {
            "focus=instant-messaging".into()
        } else if self.is_kitty() {
            format!("focus=kitty:{}", self.pid)
        } else {
            "focus=other".into()
        }
    }
}

pub(crate) fn parse_control_hint(args: &[&str]) -> Result<Option<FocusedWindowSnapshot>> {
    let Some(value) = args.iter().find_map(|arg| arg.strip_prefix("focus=")) else {
        return Ok(None);
    };
    let snapshot = if matches!(value, "instant-messaging" | "wechat") {
        FocusedWindowSnapshot {
            class: "wechat".into(),
            initial_class: "wechat".into(),
            pid: 0,
        }
    } else if let Some(pid) = value.strip_prefix("kitty:") {
        FocusedWindowSnapshot {
            class: "kitty".into(),
            initial_class: "kitty".into(),
            pid: pid.parse().context("focused Kitty PID is invalid")?,
        }
    } else if value == "other" {
        FocusedWindowSnapshot {
            class: String::new(),
            initial_class: String::new(),
            pid: 0,
        }
    } else {
        bail!("unknown focused-window hint `{value}`");
    };
    Ok(Some(snapshot))
}

pub(crate) fn capture() -> Result<FocusedWindowSnapshot> {
    let output = Command::new("timeout")
        .args([QUERY_TIMEOUT_SECS, "hyprctl", "activewindow", "-j"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to query Hyprland active window")?;
    if !output.status.success() {
        bail!("Hyprland active-window query failed");
    }
    serde_json::from_slice(&output.stdout).context("failed to parse Hyprland active window")
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        io::{Read, Write},
        process::{Child, Command, Stdio},
    };

    use super::{FocusedWindowSnapshot, RefinementCategory, parse_control_hint};

    const PROCESS_TEST_MODE: &str = "VOICE_INPUT_FOCUSED_WINDOW_PROCESS_TEST";

    fn parse(payload: &str) -> FocusedWindowSnapshot {
        serde_json::from_str(payload).expect("focused window should parse")
    }

    struct TestProcess {
        child: Child,
        _directory: tempfile::TempDir,
    }

    impl TestProcess {
        fn new(executable_name: &str) -> Self {
            let test_executable = env::current_exe().unwrap();
            let directory = tempfile::tempdir_in(test_executable.parent().unwrap()).unwrap();
            let executable = directory.path().join(executable_name);
            // A hard link preserves this name in /proc/<pid>/exe without
            // opening an executable for writing while parallel tests spawn.
            std::fs::hard_link(test_executable, &executable).unwrap();
            let child = Command::new(executable)
                .args([
                    "--exact",
                    "focused_window::tests::process_test_child",
                    "--nocapture",
                ])
                .env(PROCESS_TEST_MODE, "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut process = Self {
                child,
                _directory: directory,
            };
            // Wait for the child test to run before inspecting /proc.
            let mut ready = [0; 6];
            process
                .child
                .stderr
                .as_mut()
                .unwrap()
                .read_exact(&mut ready)
                .unwrap();
            assert_eq!(&ready, b"ready\n");
            process
        }

        fn window(&self, class: &str) -> FocusedWindowSnapshot {
            parse(&format!(
                r#"{{"class":"{class}","initialClass":"{class}","pid":{}}}"#,
                self.child.id()
            ))
        }
    }

    impl Drop for TestProcess {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn process_test_child() {
        if env::var_os(PROCESS_TEST_MODE).is_none() {
            return;
        }
        std::io::stderr().write_all(b"ready\n").unwrap();
        // Keep the executable alive until the parent fixture is dropped.
        let _ = std::io::stdin().read_exact(&mut [0]);
    }

    #[test]
    fn kitty_process_identity_survives_custom_classes_and_control_hints() {
        let process = TestProcess::new("kitty");
        for class in ["kitty", "org.omarchy.agent"] {
            let window = process.window(class);
            let hint = window.control_hint();
            assert_eq!(hint, format!("focus=kitty:{}", process.child.id()));
            let restored = parse_control_hint(&[&hint]).unwrap().unwrap();
            assert_eq!(restored.pid(), process.child.id());
            assert_eq!(restored.control_hint(), hint);
        }
    }

    #[test]
    fn non_kitty_process_is_not_trusted_by_window_class() {
        let process = TestProcess::new("ghostty");
        for class in ["kitty", "org.omarchy.agent"] {
            assert_eq!(process.window(class).control_hint(), "focus=other");
        }
        let hint = format!("focus=kitty:{}", process.child.id());
        let restored = parse_control_hint(&[&hint]).unwrap().unwrap();
        assert_eq!(restored.control_hint(), "focus=other");
    }

    #[test]
    fn missing_process_is_not_trusted_by_window_class() {
        for class in ["kitty", "org.omarchy.agent"] {
            let window = parse(&format!(r#"{{"class":"{class}","pid":{}}}"#, u32::MAX));
            assert_eq!(window.control_hint(), "focus=other");
        }
    }

    #[test]
    fn exact_wechat_classes_select_conversational_refinement() {
        let window = parse(r#"{"class":"wechat","initialClass":"wechat","pid":42}"#);
        assert_eq!(window.refinement_category(), RefinementCategory::WeChat);
    }

    #[test]
    fn control_hints_preserve_wechat_and_kitty_identity() {
        let wechat = parse_control_hint(&["focus=wechat"])
            .unwrap()
            .expect("WeChat hint");
        assert_eq!(wechat.refinement_category(), RefinementCategory::WeChat);

        let kitty = parse_control_hint(&["focus=kitty:1234"])
            .unwrap()
            .expect("Kitty hint");
        assert_eq!(kitty.class, "kitty");
        assert_eq!(kitty.pid(), 1234);
        assert!(parse_control_hint(&["focus=kitty:nope"]).is_err());
    }

    #[test]
    fn installed_messaging_classes_share_conversational_refinement() {
        for class in [
            "wechat",
            "Feishu",
            "feishu",
            "signal",
            "org.telegram.desktop",
            "TelegramDesktop",
        ] {
            let payload = format!(r#"{{"class":"{class}","initialClass":"other","pid":42}}"#);
            assert_eq!(
                parse(&payload).refinement_category(),
                RefinementCategory::WeChat
            );
        }
    }

    #[test]
    fn near_matches_keep_default_refinement() {
        for payload in [
            r#"{"class":"Wechat","initialClass":"other","pid":42}"#,
            r#"{"class":"wechat-dev","initialClass":"other","pid":42}"#,
            r#"{"class":"electron","initialClass":"chat","pid":42}"#,
            r#"{"class":"other","initialClass":"other","pid":42}"#,
        ] {
            assert_eq!(
                parse(payload).refinement_category(),
                RefinementCategory::Default
            );
        }
    }
}
