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
const INPUT_GAIN: f32 = 8.0;
const VISIBLE_FLOOR: f32 = 0.0;
const MAX_CLIENTS: usize = 8;

#[derive(Debug)]
pub struct WaveformAnalyzer {
    sample_rate: u32,
    window: VecDeque<i16>,
    samples_since_frame: usize,
    half_levels: [f32; WAVEFORM_BAR_COUNT / 2],
}

impl WaveformAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            window: VecDeque::with_capacity(ANALYSIS_WINDOW_SAMPLES),
            samples_since_frame: 0,
            half_levels: [VISIBLE_FLOOR; WAVEFORM_BAR_COUNT / 2],
        }
    }

    pub fn push(&mut self, samples: &[i16], voice_active: bool) -> Vec<[f32; WAVEFORM_BAR_COUNT]> {
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
                frames.push(mirrored_bars(&self.half_levels));
            }
        }
        frames
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
        bars: [f32; WAVEFORM_BAR_COUNT],
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
                        PublisherMessage::Frame { session_id, bars } => {
                            serde_json::to_vec(&WireMessage {
                                kind: "waveform",
                                session_id: *session_id,
                                sequence,
                                bars: Some(bars),
                            })
                        }
                        PublisherMessage::Reset { session_id } => {
                            serde_json::to_vec(&WireMessage {
                                kind: "reset",
                                session_id: *session_id,
                                sequence,
                                bars: None,
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

    pub fn try_publish(&self, session_id: u64, bars: [f32; WAVEFORM_BAR_COUNT]) {
        let _ = self.try_send(PublisherMessage::Frame { session_id, bars });
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
        VISIBLE_FLOOR, WAVEFORM_BAR_COUNT, WaveformAnalyzer, WaveformPublisher,
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
        assert!(actual.iter().all(|frame| frame.len() == WAVEFORM_BAR_COUNT));
    }

    #[test]
    fn analyzer_produces_center_mirrored_frames() {
        let samples = (0..ANALYSIS_WINDOW_SAMPLES)
            .map(|index| ((index % 97) as i16) * 240)
            .collect::<Vec<_>>();
        let mut analyzer = WaveformAnalyzer::new(16_000);
        let frame = analyzer.push(&samples, true).pop().unwrap();
        for index in 0..WAVEFORM_BAR_COUNT / 2 {
            assert_eq!(frame[index], frame[WAVEFORM_BAR_COUNT - 1 - index]);
        }
    }

    #[test]
    fn analyzer_keeps_silence_at_visible_floor() {
        let mut analyzer = WaveformAnalyzer::new(16_000);
        let frames = analyzer.push(&vec![0; ANALYSIS_WINDOW_SAMPLES], false);
        assert_eq!(frames.len(), 1);
        assert!(
            frames[0]
                .iter()
                .all(|bar| (*bar - VISIBLE_FLOOR).abs() < f32::EPSILON)
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
        publisher.try_publish(7, [0.42; WAVEFORM_BAR_COUNT]);

        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(payload["type"], "waveform");
        assert_eq!(payload["session_id"], 7);
        assert_eq!(
            payload["bars"].as_array().unwrap().len(),
            WAVEFORM_BAR_COUNT
        );
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
