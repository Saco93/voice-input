use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    config::{Config, HudPosition},
    paths,
    waveform::WAVEFORM_BAR_COUNT,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Arming,
    Recording,
    Transcribing,
    Refining,
    Outputting,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub phase: Phase,
    pub class: String,
    pub icon: String,
    pub text: String,
    pub tooltip: String,
    pub transcript: String,
    pub bars: [f32; WAVEFORM_BAR_COUNT],
    pub language: String,
    pub engine: String,
    pub model: String,
    pub hud_position: HudPosition,
    pub hud_offset_x: i32,
    pub hud_offset_y: i32,
    #[serde(default)]
    pub raw_transcript: Option<String>,
    #[serde(default)]
    pub refined_transcript: Option<String>,
    #[serde(default)]
    pub refinement_status: Option<String>,
    #[serde(default)]
    pub refinement_changed: Option<bool>,
    #[serde(default)]
    pub output_target_hint: Option<String>,
    #[serde(default)]
    pub output_target_resolved: Option<String>,
    #[serde(default)]
    pub output_mode: Option<String>,
    #[serde(default)]
    pub output_driver: Option<String>,
    pub error: Option<String>,
    pub updated_at_ms: u128,
}

impl Snapshot {
    pub fn idle(config: &Config) -> Self {
        Self {
            phase: Phase::Idle,
            class: "idle".into(),
            icon: String::new(),
            text: String::new(),
            tooltip: format!(
                "Voice Input idle\nLanguage: {}\nEngine: {}",
                config.asr.language.label(),
                config.asr.active_engine_label()
            ),
            transcript: String::new(),
            bars: [0.0; WAVEFORM_BAR_COUNT],
            language: config.asr.language.label().into(),
            engine: config.asr.active_engine_label(),
            model: config.asr.active_model_label(),
            hud_position: config.hud.position,
            hud_offset_x: config.hud.offset_x,
            hud_offset_y: config.hud.offset_y,
            raw_transcript: None,
            refined_transcript: None,
            refinement_status: None,
            refinement_changed: None,
            output_target_hint: None,
            output_target_resolved: None,
            output_mode: None,
            output_driver: None,
            error: None,
            updated_at_ms: now_ms(),
        }
    }

    pub fn as_waybar_json(&self, extended: bool) -> Value {
        let mut payload = json!({
            "text": self.text,
            "class": self.class,
            "tooltip": self.tooltip,
            "icon": self.icon,
        });

        if extended {
            payload["phase"] = json!(self.phase);
            payload["language"] = json!(self.language);
            payload["engine"] = json!(self.engine);
            payload["model"] = json!(self.model);
            payload["transcript"] = json!(self.transcript);
            payload["hud_position"] = json!(self.hud_position);
            payload["hud_offset_x"] = json!(self.hud_offset_x);
            payload["hud_offset_y"] = json!(self.hud_offset_y);
            payload["raw_transcript"] = json!(self.raw_transcript);
            payload["refined_transcript"] = json!(self.refined_transcript);
            payload["refinement_status"] = json!(self.refinement_status);
            payload["refinement_changed"] = json!(self.refinement_changed);
            payload["output_target_hint"] = json!(self.output_target_hint);
            payload["output_target_resolved"] = json!(self.output_target_resolved);
            payload["output_mode"] = json!(self.output_mode);
            payload["output_driver"] = json!(self.output_driver);
        }

        payload
    }
}

#[derive(Clone)]
pub struct StateHandle {
    inner: Arc<StateInner>,
}

struct StateInner {
    config: Config,
    snapshot: Mutex<Snapshot>,
    update_lock: Mutex<()>,
    hud_process: Mutex<Option<Child>>,
}

impl StateHandle {
    pub fn new(config: Config) -> Result<Self> {
        fs::create_dir_all(paths::runtime_dir()?)
            .context("failed to create runtime directory for voice-input")?;
        let snapshot = Snapshot::idle(&config);
        let handle = Self {
            inner: Arc::new(StateInner {
                config,
                snapshot: Mutex::new(snapshot),
                update_lock: Mutex::new(()),
                hud_process: Mutex::new(None),
            }),
        };
        handle.persist()?;
        Ok(handle)
    }

    pub fn update<F>(&self, update: F) -> Result<()>
    where
        F: FnOnce(&mut Snapshot),
    {
        // Snapshot updates arrive concurrently from audio capture and realtime ASR.
        // Serialize mutation and persistence together so an older writer cannot
        // overwrite a newer transcript, and so two fs::write calls never overlap.
        let _update_guard = self
            .inner
            .update_lock
            .lock()
            .expect("state update mutex poisoned");
        {
            let mut snapshot = self.inner.snapshot.lock().expect("snapshot mutex poisoned");
            update(&mut snapshot);
            snapshot.updated_at_ms = now_ms();
        }
        self.persist()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner
            .snapshot
            .lock()
            .expect("snapshot mutex poisoned")
            .clone()
    }

    fn persist(&self) -> Result<()> {
        let snapshot = self.snapshot();

        let runtime_path = paths::runtime_dir()?.join("state.json");
        self.write_snapshot(&runtime_path, &snapshot)?;

        if let Some(custom_state_path) = self.inner.config.state_path()? {
            if custom_state_path != runtime_path {
                if let Some(parent) = custom_state_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                self.write_snapshot(&custom_state_path, &snapshot)?;
            }
        }

        // HUD failures should not take the daemon down; typing and Waybar state still matter.
        let _ = self.ensure_hud(&snapshot);

        Ok(())
    }

    fn write_snapshot(&self, path: &PathBuf, snapshot: &Snapshot) -> Result<()> {
        let payload =
            serde_json::to_vec_pretty(snapshot).context("failed to serialize runtime state")?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("state path has no valid file name"))?;
        let temporary_path = path.with_file_name(format!(".{file_name}.tmp"));

        fs::write(&temporary_path, payload)
            .with_context(|| format!("failed to write {}", temporary_path.display()))?;
        fs::rename(&temporary_path, path)
            .with_context(|| format!("failed to replace {}", path.display()))
    }

    fn ensure_hud(&self, _snapshot: &Snapshot) -> Result<()> {
        if !self.inner.config.hud.enabled || external_hud_enabled() {
            return Ok(());
        }
        if _snapshot.phase == Phase::Idle {
            return Ok(());
        }

        let mut process = self
            .inner
            .hud_process
            .lock()
            .expect("hud process mutex poisoned");
        let should_spawn = match process.as_mut() {
            Some(child) => child.try_wait().ok().flatten().is_some(),
            None => true,
        };

        if !should_spawn {
            return Ok(());
        }

        let script = paths::hud_script_path()?;
        let runtime_dir = paths::runtime_dir()?;
        let state_file = runtime_dir.join("state.json");
        let waveform_socket = runtime_dir.join("waveform.sock");
        let mut command = Command::new("python");
        command
            .arg(script)
            .arg("--state-file")
            .arg(state_file)
            .arg("--waveform-socket")
            .arg(waveform_socket)
            .arg("--height")
            .arg(self.inner.config.hud.height.to_string())
            .arg("--margin-bottom")
            .arg(self.inner.config.hud.margin_bottom.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Ok(preload_path) = layer_shell_preload_path() {
            let merged = std::env::var("LD_PRELOAD")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|existing| format!("{preload_path}:{existing}"))
                .unwrap_or(preload_path);
            command.env("LD_PRELOAD", merged);
        }

        let child = command.spawn().context("failed to spawn HUD helper")?;
        *process = Some(child);
        Ok(())
    }
}

fn external_hud_enabled() -> bool {
    std::env::var("VOICE_INPUT_EXTERNAL_HUD")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn layer_shell_preload_path() -> Result<String> {
    let candidates = [
        "/usr/lib/libgtk4-layer-shell.so",
        "/usr/lib/libgtk4-layer-shell.so.0",
    ];

    for candidate in candidates {
        if fs::metadata(candidate).is_ok() {
            return Ok(candidate.to_string());
        }
    }

    bail!("gtk4-layer-shell shared library not found")
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
