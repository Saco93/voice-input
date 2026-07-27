use std::{
    collections::VecDeque,
    fs,
    io::{ErrorKind, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc::{self, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::Serialize;

pub const WAVEFORM_BAR_COUNT: usize = 30;
pub const ANALYSIS_WINDOW_SAMPLES: usize = 512;
pub const ANALYSIS_HOP_SAMPLES: usize = 256;
pub const ASR_PACKET_SAMPLES: usize = 2_048;

const ATTACK_SECONDS: f32 = 0.040;
const RELEASE_SECONDS: f32 = 0.190;
const PITCH_ATTACK_SECONDS: f32 = 0.120;
const PITCH_RELEASE_SECONDS: f32 = 0.250;
const TIMBRE_ATTACK_SECONDS: f32 = 0.100;
const TIMBRE_RELEASE_SECONDS: f32 = 0.300;
const INPUT_GAIN: f32 = 8.0;
const VISIBLE_FLOOR: f32 = 0.0;
const MAX_CLIENTS: usize = 8;
// Autocorrelation-based pitch estimation over the voiced speech range.
const PITCH_MIN_FREQUENCY: f32 = 75.0;
const PITCH_MAX_FREQUENCY: f32 = 450.0;
const PITCH_CORRELATION_THRESHOLD: f32 = 0.35;
const PITCH_NEUTRAL: f32 = 0.35;
// One-pole low-pass split for the brightness (high-band energy ratio) timbre
// estimate, plus the ratio range mapped onto the normalized 0..1 scale.
const TIMBRE_CUTOFF_HZ: f32 = 1_400.0;
const TIMBRE_MIN_RATIO: f32 = 0.08;
const TIMBRE_MAX_RATIO: f32 = 0.65;
// Below this RMS the window is treated as silence and pitch/timbre estimates
// are discarded, because measurements of background noise are meaningless.
const ANALYSIS_MIN_RMS: f32 = 0.005;

/// One analysis frame: the legacy mirrored bar envelope plus aggregate voice
/// metrics used by glow-style HUD visualizations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveformFrame {
    pub bars: [f32; WAVEFORM_BAR_COUNT],
    /// Overall smoothed loudness of the window, 0..1.
    pub level: f32,
    /// Normalized fundamental-frequency estimate, 0..1. Low values correspond
    /// to a deep voice, high values to a high-pitched voice. The estimate is
    /// held while speech is unvoiced or silent so visuals stay stable.
    pub pitch: f32,
    /// Normalized brightness (high-band energy ratio), 0..1. Low values are
    /// muffled/voiced content, high values are bright or fricative content.
    pub timbre: f32,
}

#[derive(Debug)]
pub struct WaveformAnalyzer {
    sample_rate: u32,
    window: VecDeque<i16>,
    samples_since_frame: usize,
    half_levels: [f32; WAVEFORM_BAR_COUNT / 2],
    overall_level: f32,
    pitch_level: f32,
    timbre_level: f32,
}

impl WaveformAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            window: VecDeque::with_capacity(ANALYSIS_WINDOW_SAMPLES),
            samples_since_frame: 0,
            half_levels: [VISIBLE_FLOOR; WAVEFORM_BAR_COUNT / 2],
            overall_level: VISIBLE_FLOOR,
            pitch_level: PITCH_NEUTRAL,
            timbre_level: 0.0,
        }
    }

    pub fn push(&mut self, samples: &[i16], voice_active: bool) -> Vec<WaveformFrame> {
        let mut frames = Vec::new();
        for sample in samples {
            if self.window.len() == ANALYSIS_WINDOW_SAMPLES {
                self.window.pop_front();
            }
            self.window.push_back(*sample);
            self.samples_since_frame += 1;

            if self.window.len() == ANALYSIS_WINDOW_SAMPLES
                && self.samples_since_frame >= ANALYSIS_HOP_SAMPLES
            {
                self.samples_since_frame = 0;
                let window = self.window.iter().copied().collect::<Vec<_>>();
                let frame_seconds = ANALYSIS_HOP_SAMPLES as f32 / self.sample_rate as f32;
                let half_count = self.half_levels.len();
                for (index, level) in self.half_levels.iter_mut().enumerate() {
                    let start = index * window.len() / half_count;
                    let end = (index + 1) * window.len() / half_count;
                    let rms = rms_level(window[start..end].iter().copied());
                    let target = if voice_active {
                        (rms * INPUT_GAIN).clamp(0.0, 1.0).powf(0.65)
                    } else {
                        0.0
                    };
                    let time_constant = if target > *level {
                        ATTACK_SECONDS
                    } else {
                        RELEASE_SECONDS
                    };
                    let alpha = 1.0 - (-frame_seconds / time_constant).exp();
                    *level += (target - *level) * alpha;
                }

                let window_rms = rms_level(window.iter().copied());
                let level_target = if voice_active {
                    (window_rms * INPUT_GAIN).clamp(0.0, 1.0).powf(0.65)
                } else {
                    0.0
                };
                smooth(
                    &mut self.overall_level,
                    level_target,
                    frame_seconds,
                    ATTACK_SECONDS,
                    RELEASE_SECONDS,
                );

                let analyzed = voice_active && window_rms > ANALYSIS_MIN_RMS;
                if analyzed && let Some(frequency) = estimate_pitch(&window, self.sample_rate) {
                    let pitch_target = (frequency / PITCH_MIN_FREQUENCY).ln()
                        / (PITCH_MAX_FREQUENCY / PITCH_MIN_FREQUENCY).ln();
                    smooth(
                        &mut self.pitch_level,
                        pitch_target.clamp(0.0, 1.0),
                        frame_seconds,
                        PITCH_ATTACK_SECONDS,
                        PITCH_RELEASE_SECONDS,
                    );
                }
                // Unvoiced or noisy windows keep the previous pitch so the
                // breathing rate does not jump on fricatives and pauses.

                let timbre_target = if analyzed {
                    let ratio = high_band_ratio(&window, self.sample_rate);
                    ((ratio - TIMBRE_MIN_RATIO) / (TIMBRE_MAX_RATIO - TIMBRE_MIN_RATIO))
                        .clamp(0.0, 1.0)
                } else {
                    0.0
                };
                smooth(
                    &mut self.timbre_level,
                    timbre_target,
                    frame_seconds,
                    TIMBRE_ATTACK_SECONDS,
                    TIMBRE_RELEASE_SECONDS,
                );

                frames.push(WaveformFrame {
                    bars: mirrored_bars(&self.half_levels),
                    level: self.overall_level,
                    pitch: self.pitch_level,
                    timbre: self.timbre_level,
                });
            }
        }
        frames
    }
}

fn smooth(value: &mut f32, target: f32, frame_seconds: f32, attack: f32, release: f32) {
    let time_constant = if target > *value { attack } else { release };
    let alpha = 1.0 - (-frame_seconds / time_constant).exp();
    *value += (target - *value) * alpha;
}

/// Fundamental-frequency estimate from the autocorrelation peak inside the
/// voiced-speech lag range. Returns `None` when the window has no clear
/// periodicity (silence, fricatives, noise).
fn estimate_pitch(window: &[i16], sample_rate: u32) -> Option<f32> {
    let min_lag = (sample_rate as f32 / PITCH_MAX_FREQUENCY) as usize;
    let max_lag = ((sample_rate as f32 / PITCH_MIN_FREQUENCY) as usize).min(window.len() - 1);
    if min_lag >= max_lag {
        return None;
    }

    let energy: f32 = window.iter().map(|sample| (*sample as f32).powi(2)).sum();
    if energy <= f32::EPSILON {
        return None;
    }

    let mut best_lag = 0_usize;
    let mut best_correlation = 0.0_f32;
    for lag in min_lag..=max_lag {
        let correlation: f32 = window[..window.len() - lag]
            .iter()
            .zip(&window[lag..])
            .map(|(a, b)| *a as f32 * *b as f32)
            .sum();
        let normalized = correlation / energy;
        if normalized > best_correlation {
            best_correlation = normalized;
            best_lag = lag;
        }
    }

    (best_correlation >= PITCH_CORRELATION_THRESHOLD)
        .then_some(sample_rate as f32 / best_lag as f32)
}

/// Brightness estimate: energy ratio of a one-pole high band (signal minus
/// its low-passed copy) to the total signal energy.
fn high_band_ratio(window: &[i16], sample_rate: u32) -> f32 {
    let alpha = 1.0 - (-2.0 * std::f32::consts::PI * TIMBRE_CUTOFF_HZ / sample_rate as f32).exp();
    let mut low_passed = 0.0_f32;
    let mut total_power = 0.0_f32;
    let mut high_power = 0.0_f32;
    for sample in window {
        let value = *sample as f32 / i16::MAX as f32;
        low_passed += alpha * (value - low_passed);
        let high = value - low_passed;
        total_power += value * value;
        high_power += high * high;
    }
    if total_power <= f32::EPSILON {
        0.0
    } else {
        (high_power / total_power).sqrt()
    }
}

#[derive(Debug, Default)]
pub struct AsrPacketizer {
    pending: Vec<i16>,
}

impl AsrPacketizer {
    pub fn push(&mut self, samples: &[i16]) -> Vec<Vec<i16>> {
        self.pending.extend_from_slice(samples);
        let mut packets = Vec::new();
        while self.pending.len() >= ASR_PACKET_SAMPLES {
            packets.push(self.pending.drain(..ASR_PACKET_SAMPLES).collect());
        }
        packets
    }

    pub fn flush(&mut self) -> Option<Vec<i16>> {
        (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending))
    }
}

#[derive(Clone)]
pub struct WaveformPublisher {
    sender: SyncSender<PublisherMessage>,
}

#[derive(Debug)]
enum PublisherMessage {
    Frame {
        session_id: u64,
        frame: WaveformFrame,
    },
    Reset {
        session_id: u64,
    },
}

#[derive(Serialize)]
struct WireMessage<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    session_id: u64,
    sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    bars: Option<&'a [f32; WAVEFORM_BAR_COUNT]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pitch: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timbre: Option<f32>,
}

impl WaveformPublisher {
    pub fn start(path: PathBuf, sample_rate: u32) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create waveform socket directory {}",
                    parent.display()
                )
            })?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        if path.exists() {
            fs::remove_file(&path).with_context(|| {
                format!("failed to remove stale waveform socket {}", path.display())
            })?;
        }
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("failed to bind waveform socket {}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;

        let (sender, receiver) = mpsc::sync_channel(64);
        thread::Builder::new()
            .name("voice-input-waveform".into())
            .spawn(move || {
                let mut clients = Vec::new();
                let mut queue = VecDeque::with_capacity(64);
                let mut sequence = 0_u64;
                let frame_interval = Duration::from_secs_f64(
                    ANALYSIS_HOP_SAMPLES as f64 / sample_rate.max(1) as f64,
                );
                let mut next_frame_at = Instant::now();
                loop {
                    accept_clients(&listener, &mut clients);
                    while let Ok(message) = receiver.try_recv() {
                        if matches!(message, PublisherMessage::Reset { .. }) {
                            queue.clear();
                        }
                        queue.push_back(message);
                    }

                    let now = Instant::now();
                    if queue.is_empty() || now < next_frame_at {
                        let wait = if queue.is_empty() {
                            Duration::from_millis(8)
                        } else {
                            next_frame_at
                                .saturating_duration_since(now)
                                .min(Duration::from_millis(8))
                        };
                        match receiver.recv_timeout(wait) {
                            Ok(message) => {
                                if matches!(message, PublisherMessage::Reset { .. }) {
                                    queue.clear();
                                }
                                queue.push_back(message);
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                        continue;
                    }

                    let Some(next) = queue.pop_front() else {
                        continue;
                    };
                    sequence = sequence.wrapping_add(1);
                    let payload = match &next {
                        PublisherMessage::Frame { session_id, frame } => {
                            serde_json::to_vec(&WireMessage {
                                kind: "waveform",
                                session_id: *session_id,
                                sequence,
                                bars: Some(&frame.bars),
                                level: Some(frame.level),
                                pitch: Some(frame.pitch),
                                timbre: Some(frame.timbre),
                            })
                        }
                        PublisherMessage::Reset { session_id } => {
                            serde_json::to_vec(&WireMessage {
                                kind: "reset",
                                session_id: *session_id,
                                sequence,
                                bars: None,
                                level: None,
                                pitch: None,
                                timbre: None,
                            })
                        }
                    };
                    if let Ok(mut payload) = payload {
                        payload.push(b'\n');
                        broadcast(&mut clients, &payload);
                    }
                    next_frame_at = (next_frame_at + frame_interval).max(Instant::now());
                }
                let _ = fs::remove_file(path);
            })
            .context("failed to start waveform publisher")?;

        Ok(Self { sender })
    }

    pub fn try_publish(&self, session_id: u64, frame: WaveformFrame) {
        let _ = self.try_send(PublisherMessage::Frame { session_id, frame });
    }

    pub fn try_reset(&self, session_id: u64) {
        let _ = self.try_send(PublisherMessage::Reset { session_id });
    }

    fn try_send(&self, message: PublisherMessage) -> std::result::Result<(), ()> {
        match self.sender.try_send(message) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => Err(()),
        }
    }
}

fn accept_clients(listener: &UnixListener, clients: &mut Vec<UnixStream>) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if clients.len() >= MAX_CLIENTS {
                    continue;
                }
                if stream.set_nonblocking(true).is_ok() {
                    clients.push(stream);
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
}

fn broadcast(clients: &mut Vec<UnixStream>, payload: &[u8]) {
    clients.retain_mut(|client| match client.write(payload) {
        Ok(written) => written == payload.len(),
        Err(error) => error.kind() != ErrorKind::WouldBlock,
    });
}

fn mirrored_bars(half_levels: &[f32; WAVEFORM_BAR_COUNT / 2]) -> [f32; WAVEFORM_BAR_COUNT] {
    let mut bars = [VISIBLE_FLOOR; WAVEFORM_BAR_COUNT];
    for (index, level) in half_levels.iter().enumerate() {
        bars[index] = *level;
        bars[WAVEFORM_BAR_COUNT - 1 - index] = *level;
    }
    bars
}

fn rms_level(samples: impl Iterator<Item = i16>) -> f32 {
    let mut count = 0_usize;
    let mut power = 0.0_f32;
    for sample in samples {
        let normalized = sample as f32 / i16::MAX as f32;
        power += normalized * normalized;
        count += 1;
    }
    if count == 0 {
        0.0
    } else {
        (power / count as f32).sqrt()
    }
}

pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("waveform.sock")
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
        thread,
        time::Duration,
    };

    use super::{
        ANALYSIS_HOP_SAMPLES, ANALYSIS_WINDOW_SAMPLES, ASR_PACKET_SAMPLES, AsrPacketizer,
        VISIBLE_FLOOR, WAVEFORM_BAR_COUNT, WaveformAnalyzer, WaveformFrame, WaveformPublisher,
    };

    #[test]
    fn analyzer_is_independent_of_capture_chunking() {
        let samples = (0..4_096)
            .map(|index| if index % 256 < 128 { 8_000 } else { 0 })
            .collect::<Vec<_>>();
        let mut whole = WaveformAnalyzer::new(16_000);
        let expected = whole.push(&samples, true);

        let mut chunked = WaveformAnalyzer::new(16_000);
        let actual = samples
            .chunks(73)
            .flat_map(|chunk| chunked.push(chunk, true))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert_eq!(
            actual.len(),
            1 + (samples.len() - ANALYSIS_WINDOW_SAMPLES) / ANALYSIS_HOP_SAMPLES
        );
        assert!(
            actual
                .iter()
                .all(|frame| frame.bars.len() == WAVEFORM_BAR_COUNT)
        );
    }

    #[test]
    fn analyzer_produces_center_mirrored_frames() {
        let samples = (0..ANALYSIS_WINDOW_SAMPLES)
            .map(|index| ((index % 97) as i16) * 240)
            .collect::<Vec<_>>();
        let mut analyzer = WaveformAnalyzer::new(16_000);
        let frame = analyzer.push(&samples, true).pop().unwrap();
        for index in 0..WAVEFORM_BAR_COUNT / 2 {
            assert_eq!(
                frame.bars[index],
                frame.bars[WAVEFORM_BAR_COUNT - 1 - index]
            );
        }
    }

    #[test]
    fn analyzer_keeps_silence_at_visible_floor() {
        let mut analyzer = WaveformAnalyzer::new(16_000);
        let frames = analyzer.push(&vec![0; ANALYSIS_WINDOW_SAMPLES], false);
        assert_eq!(frames.len(), 1);
        assert!(
            frames[0]
                .bars
                .iter()
                .all(|bar| (*bar - VISIBLE_FLOOR).abs() < f32::EPSILON)
        );
        assert!(frames[0].level.abs() < f32::EPSILON);
        // Pitch holds its neutral value during silence so the breathing rate
        // does not jump when speech starts.
        assert!((frames[0].pitch - 0.35).abs() < f32::EPSILON);
        assert!(frames[0].timbre.abs() < f32::EPSILON);
    }

    fn sine_wave(frequency_hz: f32, amplitude: i16, sample_rate: u32, count: usize) -> Vec<i16> {
        (0..count)
            .map(|index| {
                let phase =
                    2.0 * std::f32::consts::PI * frequency_hz * index as f32 / sample_rate as f32;
                (phase.sin() * amplitude as f32) as i16
            })
            .collect()
    }

    #[test]
    fn analyzer_tracks_level_pitch_and_timbre() {
        let sample_rate = 16_000;
        let deep = sine_wave(150.0, 12_000, sample_rate, 8_192);
        let high_pitch = sine_wave(350.0, 12_000, sample_rate, 8_192);
        let bright = sine_wave(2_800.0, 12_000, sample_rate, 8_192);

        let mut deep_analyzer = WaveformAnalyzer::new(sample_rate);
        let deep_frame = *deep_analyzer.push(&deep, true).last().unwrap();
        let mut high_analyzer = WaveformAnalyzer::new(sample_rate);
        let high_frame = *high_analyzer.push(&high_pitch, true).last().unwrap();
        let mut bright_analyzer = WaveformAnalyzer::new(sample_rate);
        let bright_frame = *bright_analyzer.push(&bright, true).last().unwrap();

        assert!(deep_frame.level > 0.3, "voiced audio must raise the level");
        assert!(
            high_frame.pitch > deep_frame.pitch + 0.3,
            "a higher fundamental must raise the pitch estimate: deep={} high={}",
            deep_frame.pitch,
            high_frame.pitch
        );
        assert!(deep_frame.pitch < 0.5);
        assert!(
            bright_frame.timbre > deep_frame.timbre + 0.5,
            "bright content must raise the timbre estimate: deep={} bright={}",
            deep_frame.timbre,
            bright_frame.timbre
        );
    }

    #[test]
    fn publisher_streams_complete_ndjson_frames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("waveform.sock");
        let publisher = WaveformPublisher::start(path.clone(), 16_000).unwrap();
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        thread::sleep(Duration::from_millis(20));
        publisher.try_publish(
            7,
            WaveformFrame {
                bars: [0.42; WAVEFORM_BAR_COUNT],
                level: 0.5,
                pitch: 0.4,
                timbre: 0.25,
            },
        );

        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(payload["type"], "waveform");
        assert_eq!(payload["session_id"], 7);
        assert_eq!(
            payload["bars"].as_array().unwrap().len(),
            WAVEFORM_BAR_COUNT
        );
        assert_eq!(payload["level"].as_f64().unwrap(), 0.5);
        assert_eq!(payload["pitch"].as_f64().unwrap(), 0.4);
        assert_eq!(payload["timbre"].as_f64().unwrap(), 0.25);
    }

    #[test]
    fn packetizer_preserves_all_samples_and_flushes_tail() {
        let input = (0..5_000).map(|value| value as i16).collect::<Vec<_>>();
        let mut packetizer = AsrPacketizer::default();
        let mut output = packetizer.push(&input[..1_337]);
        output.extend(packetizer.push(&input[1_337..]));
        assert!(
            output
                .iter()
                .all(|packet| packet.len() == ASR_PACKET_SAMPLES)
        );
        if let Some(tail) = packetizer.flush() {
            output.push(tail);
        }
        assert_eq!(output.concat(), input);
    }
}
