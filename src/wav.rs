use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};

const MAX_WAV_METADATA_BYTES: usize = 1024 * 1024;

fn wav_lengths(sample_count: usize) -> Result<(u32, u32)> {
    let data_bytes = sample_count
        .checked_mul(std::mem::size_of::<i16>())
        .ok_or_else(|| anyhow!("PCM sample length overflow"))?;
    let data_len = u32::try_from(data_bytes).context("PCM data exceeds the WAV size limit")?;
    let chunk_size = 36_u32
        .checked_add(data_len)
        .ok_or_else(|| anyhow!("PCM data exceeds the RIFF size limit"))?;
    Ok((data_len, chunk_size))
}

pub fn read_pcm16_wav(
    path: &Path,
    expected_sample_rate: u32,
    max_duration_secs: u64,
) -> Result<Vec<i16>> {
    let max_samples = u64::from(expected_sample_rate)
        .checked_mul(max_duration_secs)
        .ok_or_else(|| anyhow!("WAV duration limit overflow"))?;
    let max_pcm_bytes = max_samples
        .checked_mul(2)
        .ok_or_else(|| anyhow!("WAV PCM size limit overflow"))?;
    let max_file_bytes = max_pcm_bytes
        .checked_add(MAX_WAV_METADATA_BYTES as u64)
        .ok_or_else(|| anyhow!("WAV file size limit overflow"))?;

    let file = File::open(path)
        .with_context(|| format!("failed to open streaming ASR WAV `{}`", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect streaming ASR WAV `{}`", path.display()))?;
    if !metadata.is_file() {
        bail!("streaming ASR WAV path is not a file: {}", path.display());
    }
    if metadata.len() > max_file_bytes {
        bail!("streaming ASR WAV exceeds the configured maximum capture duration");
    }

    let mut bytes = Vec::new();
    file.take(max_file_bytes + 1)
        .read_to_end(&mut bytes)
        .context("failed to read streaming ASR WAV")?;
    if bytes.len() as u64 > max_file_bytes {
        bail!("streaming ASR WAV exceeds the configured maximum capture duration");
    }
    let max_pcm_bytes = usize::try_from(max_pcm_bytes)
        .context("configured WAV duration is too large for this platform")?;
    parse_pcm16_wav(&bytes, expected_sample_rate, max_pcm_bytes)
}

fn parse_pcm16_wav(
    bytes: &[u8],
    expected_sample_rate: u32,
    max_pcm_bytes: usize,
) -> Result<Vec<i16>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("streaming ASR input must be a RIFF/WAVE file");
    }
    let riff_size = read_u32(bytes, 4)? as usize;
    let riff_end = riff_size
        .checked_add(8)
        .ok_or_else(|| anyhow!("WAV RIFF size overflow"))?;
    if riff_size < 4 || riff_end != bytes.len() {
        bail!("WAV RIFF size does not match the file length");
    }

    let mut offset = 12;
    let mut format_seen = false;
    let mut data = None;
    while offset < riff_end {
        if riff_end - offset < 8 {
            bail!("WAV contains a malformed trailing chunk header");
        }
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_len = read_u32(bytes, offset + 4)? as usize;
        let payload_start = offset + 8;
        let payload_end = payload_start
            .checked_add(chunk_len)
            .ok_or_else(|| anyhow!("WAV chunk size overflow"))?;
        let padded_end = payload_end
            .checked_add(chunk_len % 2)
            .ok_or_else(|| anyhow!("WAV chunk padding overflow"))?;
        if padded_end > riff_end {
            bail!("WAV chunk extends past the RIFF boundary");
        }

        match chunk_id {
            b"fmt " => {
                if format_seen {
                    bail!("WAV contains multiple format chunks");
                }
                if chunk_len < 16 {
                    bail!("WAV format chunk is too short");
                }
                let format = read_u16(bytes, payload_start)?;
                let channels = read_u16(bytes, payload_start + 2)?;
                let sample_rate = read_u32(bytes, payload_start + 4)?;
                let byte_rate = read_u32(bytes, payload_start + 8)?;
                let block_align = read_u16(bytes, payload_start + 12)?;
                let bits_per_sample = read_u16(bytes, payload_start + 14)?;
                if format != 1 {
                    bail!("streaming ASR WAV must use uncompressed PCM");
                }
                if channels != 1 {
                    bail!("streaming ASR WAV must be mono");
                }
                if bits_per_sample != 16 {
                    bail!("streaming ASR WAV must use signed 16-bit samples");
                }
                if sample_rate != expected_sample_rate {
                    bail!("streaming ASR WAV sample rate must be {expected_sample_rate} Hz");
                }
                if block_align != 2 || byte_rate != expected_sample_rate.saturating_mul(2) {
                    bail!("streaming ASR WAV has inconsistent PCM format fields");
                }
                format_seen = true;
            }
            b"data" => {
                if !format_seen {
                    bail!("WAV data chunk appears before the format chunk");
                }
                if data.is_some() {
                    bail!("WAV contains multiple data chunks");
                }
                if !chunk_len.is_multiple_of(2) {
                    bail!("streaming ASR WAV data length is not sample-aligned");
                }
                if chunk_len > max_pcm_bytes {
                    bail!("streaming ASR WAV exceeds the configured maximum capture duration");
                }
                data = Some(&bytes[payload_start..payload_end]);
            }
            _ => {}
        }
        offset = padded_end;
    }

    if !format_seen {
        bail!("WAV is missing the format chunk");
    }
    let data = data.ok_or_else(|| anyhow!("WAV is missing the data chunk"))?;
    Ok(data
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| anyhow!("WAV field extends past the RIFF boundary"))?;
    Ok(u16::from_le_bytes(value.try_into().unwrap()))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| anyhow!("WAV field extends past the RIFF boundary"))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

pub fn write_pcm16_wav(path: &Path, sample_rate: u32, samples: &[i16]) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let (data_len, chunk_size) = wav_lengths(samples.len())?;

    writer.write_all(b"RIFF")?;
    writer.write_all(&chunk_size.to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    writer.write_all(b"fmt ")?;
    writer.write_all(&16u32.to_le_bytes())?;
    writer.write_all(&1u16.to_le_bytes())?;
    writer.write_all(&1u16.to_le_bytes())?;
    writer.write_all(&sample_rate.to_le_bytes())?;
    let byte_rate = sample_rate
        .checked_mul(2)
        .ok_or_else(|| anyhow!("sample rate exceeds the WAV byte-rate limit"))?;
    writer.write_all(&byte_rate.to_le_bytes())?;
    writer.write_all(&2u16.to_le_bytes())?;
    writer.write_all(&16u16.to_le_bytes())?;
    writer.write_all(b"data")?;
    writer.write_all(&data_len.to_le_bytes())?;

    for sample in samples {
        writer.write_all(&sample.to_le_bytes())?;
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_valid_mono_pcm16_header() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.wav");
        write_pcm16_wav(&path, 16_000, &[1, -1]).unwrap();

        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 40);
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 4);
        assert_eq!(bytes.len(), 48);
    }

    #[test]
    fn rejects_lengths_that_riff_cannot_represent() {
        let overflowing_samples = (u32::MAX as usize / 2) + 1;
        assert!(wav_lengths(overflowing_samples).is_err());
    }

    #[test]
    fn parses_pcm16_with_metadata_and_odd_chunk_padding() {
        let mut wav = wav_with_chunks(&[
            (*b"JUNK", vec![1, 2, 3]),
            (*b"fmt ", pcm_format(1, 16_000, 16)),
            (*b"LIST", vec![4, 5, 6, 7]),
            (*b"data", vec![1, 0, 255, 255]),
        ]);
        assert_eq!(parse_pcm16_wav(&wav, 16_000, 4).unwrap(), vec![1, -1]);

        wav.pop();
        assert!(parse_pcm16_wav(&wav, 16_000, 4).is_err());
    }

    #[test]
    fn rejects_unsupported_pcm_shapes() {
        let cases = [
            pcm_format(3, 16_000, 16),
            pcm_format_with_channels(2),
            pcm_format(1, 16_000, 8),
            pcm_format(1, 48_000, 16),
        ];
        for format in cases {
            let wav = wav_with_chunks(&[(*b"fmt ", format), (*b"data", vec![0, 0])]);
            assert!(parse_pcm16_wav(&wav, 16_000, 2).is_err());
        }
    }

    #[test]
    fn rejects_malformed_or_oversized_wav_chunks() {
        let valid = wav_with_chunks(&[
            (*b"fmt ", pcm_format(1, 16_000, 16)),
            (*b"data", vec![0, 0, 1, 0]),
        ]);
        assert!(parse_pcm16_wav(&valid, 16_000, 2).is_err());

        let mut truncated = valid.clone();
        truncated.pop();
        assert!(parse_pcm16_wav(&truncated, 16_000, 4).is_err());

        let mut trailing = valid;
        trailing.extend_from_slice(b"bad");
        let riff_size = (trailing.len() - 8) as u32;
        trailing[4..8].copy_from_slice(&riff_size.to_le_bytes());
        assert!(parse_pcm16_wav(&trailing, 16_000, 4).is_err());
    }

    fn pcm_format(format: u16, sample_rate: u32, bits_per_sample: u16) -> Vec<u8> {
        let channels = 1_u16;
        let block_align = channels * (bits_per_sample / 8);
        let byte_rate = sample_rate * u32::from(block_align);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&format.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes
    }

    fn pcm_format_with_channels(channels: u16) -> Vec<u8> {
        let mut bytes = pcm_format(1, 16_000, 16);
        bytes[2..4].copy_from_slice(&channels.to_le_bytes());
        let block_align = channels * 2;
        bytes[8..12].copy_from_slice(&(16_000 * u32::from(block_align)).to_le_bytes());
        bytes[12..14].copy_from_slice(&block_align.to_le_bytes());
        bytes
    }

    fn wav_with_chunks(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut bytes = Vec::from(&b"RIFF\0\0\0\0WAVE"[..]);
        for (id, payload) in chunks {
            bytes.extend_from_slice(id);
            bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            bytes.extend_from_slice(payload);
            if !payload.len().is_multiple_of(2) {
                bytes.push(0);
            }
        }
        let riff_size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
        bytes
    }
}
