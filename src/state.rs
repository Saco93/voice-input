use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
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
    #[serde(default = "default_hud_enabled")]
    pub hud_enabled: bool,
    #[serde(default = "default_hud_margin_bottom")]
    pub hud_margin_bottom: i32,
    #[serde(default = "default_hud_height")]
    pub hud_height: i32,
    pub hud_position: HudPosition,
    pub hud_offset_x: i32,
    pub hud_offset_y: i32,
    #[serde(default)]
    pub recording_started_at_ms: Option<u128>,
    #[serde(default)]
    pub recording_duration_ms: u64,
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
            hud_enabled: config.hud.enabled,
            hud_margin_bottom: config.hud.margin_bottom,
            hud_height: config.hud.height,
            hud_position: config.hud.position,
            hud_offset_x: config.hud.offset_x,
            hud_offset_y: config.hud.offset_y,
            recording_started_at_ms: None,
            recording_duration_ms: 0,
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
            payload["hud_enabled"] = json!(self.hud_enabled);
            payload["hud_margin_bottom"] = json!(self.hud_margin_bottom);
            payload["hud_height"] = json!(self.hud_height);
            payload["hud_position"] = json!(self.hud_position);
            payload["hud_offset_x"] = json!(self.hud_offset_x);
            payload["hud_offset_y"] = json!(self.hud_offset_y);
            payload["recording_started_at_ms"] = json!(self.recording_started_at_ms);
            payload["recording_duration_ms"] = json!(self.recording_duration_ms);
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

    pub fn start_recording_clock(&mut self) {
        self.start_recording_clock_at(now_ms());
    }

    fn start_recording_clock_at(&mut self, timestamp_ms: u128) {
        if self.recording_started_at_ms.is_none() {
            self.recording_started_at_ms = Some(timestamp_ms);
            self.recording_duration_ms = 0;
        }
    }

    pub(crate) fn stop_recording_clock_at(&mut self, timestamp_ms: u128) {
        let Some(started_at_ms) = self.recording_started_at_ms.take() else {
            return;
        };
        self.recording_duration_ms = timestamp_ms
            .saturating_sub(started_at_ms)
            .min(u64::MAX as u128) as u64;
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

        if let Some(custom_state_path) = self.inner.config.state_path()?
            && custom_state_path != runtime_path
        {
            if let Some(parent) = custom_state_path.parent() {
                fs::create_dir_all(parent)?;
            }
            self.write_snapshot(&custom_state_path, &snapshot)?;
        }

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
}

fn default_hud_enabled() -> bool {
    true
}

fn default_hud_margin_bottom() -> i32 {
    72
}

fn default_hud_height() -> i32 {
    56
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::Snapshot;
    use crate::config::Config;

    #[test]
    fn snapshot_carries_hud_configuration_in_extended_json() {
        let mut config = Config::default();
        config.hud.enabled = false;
        config.hud.margin_bottom = 101;
        config.hud.height = 64;
        let snapshot = Snapshot::idle(&config);
        let extended = snapshot.as_waybar_json(true);
        assert_eq!(extended["hud_enabled"], false);
        assert_eq!(extended["hud_margin_bottom"], 101);
        assert_eq!(extended["hud_height"], 64);
        assert_eq!(extended["recording_started_at_ms"], Value::Null);
        assert_eq!(extended["recording_duration_ms"], 0);
    }

    #[test]
    fn recording_clock_starts_once_and_freezes_at_stop() {
        let mut snapshot = Snapshot::idle(&Config::default());
        snapshot.start_recording_clock_at(1_000);
        snapshot.start_recording_clock_at(1_500);
        snapshot.stop_recording_clock_at(3_750);
        assert_eq!(snapshot.recording_started_at_ms, None);
        assert_eq!(snapshot.recording_duration_ms, 2_750);
    }

    #[test]
    fn older_snapshot_uses_safe_hud_defaults() {
        let config = Config::default();
        let mut value = serde_json::to_value(Snapshot::idle(&config)).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("hud_enabled");
        object.remove("hud_margin_bottom");
        object.remove("hud_height");
        object.remove("recording_started_at_ms");
        object.remove("recording_duration_ms");
        let snapshot: Snapshot = serde_json::from_value(Value::Object(object.clone())).unwrap();
        assert!(snapshot.hud_enabled);
        assert_eq!(snapshot.hud_margin_bottom, 72);
        assert_eq!(snapshot.hud_height, 56);
        assert_eq!(snapshot.recording_started_at_ms, None);
        assert_eq!(snapshot.recording_duration_ms, 0);
    }
}
