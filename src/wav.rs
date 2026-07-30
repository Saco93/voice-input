use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

use anyhow::{Context, Result, anyhow};

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
}
