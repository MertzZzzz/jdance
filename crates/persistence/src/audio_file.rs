// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio import: WAV (BLD-040) and MP3 (spec 0.2.5 §5); WAV export.
//!
//! `hound` handles PCM 8/16/24/32-bit, IEEE float 32-bit and
//! `WAVE_FORMAT_EXTENSIBLE`. It does not read 64-bit float, so a small RIFF
//! reader below covers that one subtype. MP3 is decoded whole to `f32` by
//! `nanomp3` (MIT OR Apache-2.0, a port of minimp3), with the encoder and
//! decoder delays trimmed. [`read_audio`] tells the formats apart by
//! content, not by the file name.

use crate::{PersistError, io_err};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_CHANNELS: u16 = 32;
const MIN_RATE: u32 = 8_000;
const MAX_RATE: u32 = 384_000;

/// Interleaved `f32` audio as read from a file.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
    /// What the file was, for display (spec 0.2.5 §5).
    pub format: SourceFormat,
    /// Problems that did not stop the import (damaged MP3 frames, …).
    pub warnings: Vec<String>,
}

/// The encoding of an imported file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceFormat {
    /// WAV with this many bits per sample.
    Wav { bits: u16 },
    /// MP3: average bitrate and channel mode.
    Mp3 { kbps: u32, mode: Mp3Mode },
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp3Mode {
    Mono,
    Stereo,
    JointStereo,
    DualChannel,
}

impl std::fmt::Display for SourceFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wav { bits } => write!(f, "{bits}"),
            Self::Mp3 { kbps, mode } => {
                let mode = match mode {
                    Mp3Mode::Mono => "mono",
                    Mp3Mode::Stereo => "stereo",
                    Mp3Mode::JointStereo => "joint stereo",
                    Mp3Mode::DualChannel => "dual channel",
                };
                write!(f, "MP3 {kbps} kbit/s {mode}")
            }
            Self::Unknown => Ok(()),
        }
    }
}

/// Encoder (576) plus decoder (529) delay, trimmed from MP3 files without a
/// LAME/Lavc tag that records the real values.
const MP3_DEFAULT_DELAY: usize = 1105;

/// Reads WAV or MP3, whichever the content is.
pub fn read_audio(path: &Path) -> Result<DecodedAudio, PersistError> {
    let mut head = [0u8; 12];
    let n = {
        let mut f = std::fs::File::open(path).map_err(io_err(path))?;
        let mut n = 0;
        while n < head.len() {
            match f.read(&mut head[n..]).map_err(io_err(path))? {
                0 => break,
                k => n += k,
            }
        }
        n
    };
    if n >= 12 && (&head[0..4] == b"RIFF" || &head[0..4] == b"RIFX") && &head[8..12] == b"WAVE" {
        return read_wav(path);
    }
    let data = std::fs::read(path).map_err(io_err(path))?;
    if nanomp3::detect(&data) {
        return decode_mp3(&data);
    }
    Err(PersistError::UnsupportedAudio(
        "not an audio file (WAV or MP3)".into(),
    ))
}

/// Decodes a whole MP3 file (spec 0.2.5 §5): Layer III of MPEG-1/2/2.5,
/// any bitrate mode; tags skipped; delays trimmed by the LAME tag or by
/// the default delay; the length counted from the decoded audio.
pub fn decode_mp3(data: &[u8]) -> Result<DecodedAudio, PersistError> {
    let decoded = nanomp3::decode_all::<f32>(data);
    let channels = match decoded.channels {
        Some(nanomp3::Channels::Mono) => 1u16,
        Some(nanomp3::Channels::Stereo) => 2,
        None if decoded.samples.is_empty() => {
            return Err(PersistError::UnsupportedAudio("MP3 without audio".into()));
        }
        None => {
            return Err(PersistError::UnsupportedAudio(
                "MP3 switching between mono and stereo".into(),
            ));
        }
    };
    if decoded.layer != 3 {
        return Err(PersistError::UnsupportedAudio(format!(
            "MPEG layer {} (only Layer III)",
            decoded.layer
        )));
    }
    check_spec(channels, decoded.sample_rate)?;
    let body = nanomp3::strip_tags(data);
    let mut frames = nanomp3::frames(body);
    let first = frames.next();
    let tag = first.as_ref().and_then(|f| nanomp3::VbrTag::parse(f.data));
    let mode = first
        .as_ref()
        .map_or(Mp3Mode::Stereo, |f| match f.data.get(3).map(|b| b >> 6) {
            Some(3) => Mp3Mode::Mono,
            Some(2) => Mp3Mode::DualChannel,
            Some(1) => Mp3Mode::JointStereo,
            _ => Mp3Mode::Stereo,
        });
    let mut samples = decoded.samples;
    let mut warnings = Vec::new();
    match tag {
        Some(t) if t.encoder_delay_padding.is_some() => {}
        _ => {
            // No recorded delay: drop the usual encoder plus decoder delay.
            let cut = (MP3_DEFAULT_DELAY * usize::from(channels)).min(samples.len());
            samples.drain(..cut);
        }
    }
    // A tag that counts more frames than the file holds: cut short.
    let audio_frames = frames.count() as u64 + u64::from(tag.is_none() && first.is_some());
    if let Some(expected) = tag.and_then(|t| t.frames)
        && u64::from(expected) > audio_frames
    {
        warnings.push(format!(
            "the MP3 file ends early: {} of {expected} frames",
            audio_frames
        ));
    }
    if decoded.format_changed {
        warnings.push("the MP3 stream changes format; the rest was skipped".into());
    }
    Ok(DecodedAudio {
        sample_rate: decoded.sample_rate,
        channels,
        samples,
        format: SourceFormat::Mp3 {
            kbps: decoded.avg_bitrate_kbps,
            mode,
        },
        warnings,
    })
}

impl DecodedAudio {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
}

pub fn read_wav(path: &Path) -> Result<DecodedAudio, PersistError> {
    match hound::WavReader::open(path) {
        Ok(reader) if reader.spec().bits_per_sample == 64 => read_float64(path),
        Ok(reader) => read_hound(reader),
        Err(hound::Error::IoError(e)) => Err(io_err(path)(e)),
        // hound rejects 64-bit float as malformed; try our reader, and
        // report hound's message if that fails too.
        Err(e) => read_float64(path).map_err(|_| PersistError::UnsupportedAudio(e.to_string())),
    }
}

fn check_spec(channels: u16, rate: u32) -> Result<(), PersistError> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(PersistError::UnsupportedAudio(format!(
            "{channels} channels"
        )));
    }
    if !(MIN_RATE..=MAX_RATE).contains(&rate) {
        return Err(PersistError::UnsupportedAudio(format!(
            "sample rate {rate} Hz"
        )));
    }
    Ok(())
}

fn read_hound<R: Read>(reader: hound::WavReader<R>) -> Result<DecodedAudio, PersistError> {
    let spec = reader.spec();
    check_spec(spec.channels, spec.sample_rate)?;
    let audio = |e: hound::Error| PersistError::Audio(e.to_string());
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .into_samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(audio)?,
        (hound::SampleFormat::Int, bits @ 8..=32) => {
            let scale = 1.0 / (1u64 << (bits - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(audio)?
        }
        (fmt, bits) => {
            return Err(PersistError::UnsupportedAudio(format!(
                "{fmt:?} {bits}-bit"
            )));
        }
    };
    Ok(DecodedAudio {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        samples,
        format: SourceFormat::Wav {
            bits: spec.bits_per_sample,
        },
        warnings: Vec::new(),
    })
}

/// Reads IEEE float 64-bit WAV (plain or extensible).
fn read_float64(path: &Path) -> Result<DecodedAudio, PersistError> {
    let mut f = std::fs::File::open(path).map_err(io_err(path))?;
    let unsupported = |m: &str| PersistError::UnsupportedAudio(m.to_owned());
    let mut header = [0u8; 12];
    f.read_exact(&mut header).map_err(io_err(path))?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(unsupported("not a RIFF/WAVE file"));
    }
    let mut fmt: Option<(u16, u32)> = None;
    loop {
        let mut ch = [0u8; 8];
        if f.read_exact(&mut ch).is_err() {
            return Err(unsupported("no data chunk"));
        }
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]);
        match &ch[0..4] {
            b"fmt " => {
                if !(16..=1024).contains(&size) {
                    return Err(unsupported("bad fmt chunk"));
                }
                let mut b = vec![0u8; size as usize];
                f.read_exact(&mut b).map_err(io_err(path))?;
                let tag = u16::from_le_bytes([b[0], b[1]]);
                let channels = u16::from_le_bytes([b[2], b[3]]);
                let rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                let bits = u16::from_le_bytes([b[14], b[15]]);
                let float = tag == 3 || (tag == 0xFFFE && size >= 26 && b[24] == 3 && b[25] == 0);
                if !float || bits != 64 {
                    return Err(unsupported(&format!("format tag {tag:#x}, {bits}-bit")));
                }
                check_spec(channels, rate)?;
                fmt = Some((channels, rate));
                if size % 2 == 1 {
                    f.seek(SeekFrom::Current(1)).map_err(io_err(path))?;
                }
            }
            b"data" => {
                let (channels, sample_rate) = fmt.ok_or_else(|| unsupported("data before fmt"))?;
                let mut bytes = Vec::new();
                f.take(u64::from(size))
                    .read_to_end(&mut bytes)
                    .map_err(io_err(path))?;
                let frame = 8 * usize::from(channels);
                let usable = bytes.len() / frame * frame;
                let samples = bytes[..usable]
                    .chunks_exact(8)
                    .map(|c| f64::from_le_bytes(c.try_into().expect("8 bytes")) as f32)
                    .collect();
                return Ok(DecodedAudio {
                    sample_rate,
                    channels,
                    samples,
                    format: SourceFormat::Wav { bits: 64 },
                    warnings: Vec::new(),
                });
            }
            _ => {
                let skip = i64::from(size) + i64::from(size % 2);
                f.seek(SeekFrom::Current(skip)).map_err(io_err(path))?;
            }
        }
    }
}

/// WAV sample format for export.
pub use jdance_domain::ExportFormat as WavExportFormat;

/// Writes stereo audio to `path` (not atomically; exports are not
/// documents). Values are clipped to [-1, 1] for integer formats.
pub fn write_wav(
    path: &Path,
    sample_rate: u32,
    left: &[f32],
    right: &[f32],
    format: WavExportFormat,
) -> Result<(), PersistError> {
    let (bits, sample_format) = match format {
        WavExportFormat::Pcm16 => (16, hound::SampleFormat::Int),
        WavExportFormat::Pcm24 => (24, hound::SampleFormat::Int),
        WavExportFormat::Float32 => (32, hound::SampleFormat::Float),
    };
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: bits,
        sample_format,
    };
    let err = |e: hound::Error| PersistError::Audio(e.to_string());
    let mut w = hound::WavWriter::create(path, spec).map_err(err)?;
    for (l, r) in left.iter().zip(right) {
        for s in [*l, *r] {
            match format {
                WavExportFormat::Float32 => w.write_sample(s).map_err(err)?,
                WavExportFormat::Pcm16 => w
                    .write_sample((s.clamp(-1.0, 1.0) * 32_767.0).round() as i16)
                    .map_err(err)?,
                WavExportFormat::Pcm24 => w
                    .write_sample((s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32)
                    .map_err(err)?,
            }
        }
    }
    w.finalize().map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_with(spec: hound::WavSpec, path: &Path, samples: &[f32]) {
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for s in samples {
            match (spec.sample_format, spec.bits_per_sample) {
                (hound::SampleFormat::Float, _) => w.write_sample(*s).unwrap(),
                (_, 8) => w.write_sample((s * 127.0) as i8).unwrap(),
                (_, 16) => w.write_sample((s * 32_767.0) as i16).unwrap(),
                (_, bits) => w
                    .write_sample((s * ((1i64 << (bits - 1)) - 1) as f32) as i32)
                    .unwrap(),
            }
        }
        w.finalize().unwrap();
    }

    #[test]
    fn reads_required_subtypes() {
        let dir = tempfile::tempdir().unwrap();
        let src = [0.0f32, 0.5, -0.5, 0.25];
        let cases = [
            (hound::SampleFormat::Int, 8, 1e-2),
            (hound::SampleFormat::Int, 16, 1e-4),
            (hound::SampleFormat::Int, 24, 1e-6),
            (hound::SampleFormat::Int, 32, 1e-6),
            (hound::SampleFormat::Float, 32, 0.0),
        ];
        for (fmt, bits, tol) in cases {
            for channels in [1u16, 2] {
                let p = dir.path().join(format!("{fmt:?}{bits}_{channels}.wav"));
                let spec = hound::WavSpec {
                    channels,
                    sample_rate: 44_100,
                    bits_per_sample: bits,
                    sample_format: fmt,
                };
                write_with(spec, &p, &src);
                let d = read_wav(&p).unwrap();
                assert_eq!((d.channels, d.sample_rate), (channels, 44_100));
                for (a, b) in d.samples.iter().zip(src) {
                    assert!((a - b).abs() <= tol, "{fmt:?} {bits}: {a} vs {b}");
                }
            }
        }
    }

    fn float64_wav(extensible: bool, samples: &[f64]) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&(if extensible { 0xFFFEu16 } else { 3 }).to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&48_000u32.to_le_bytes());
        fmt.extend_from_slice(&(48_000u32 * 16).to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());
        fmt.extend_from_slice(&64u16.to_le_bytes());
        if extensible {
            fmt.extend_from_slice(&22u16.to_le_bytes());
            fmt.extend_from_slice(&64u16.to_le_bytes());
            fmt.extend_from_slice(&3u32.to_le_bytes());
            fmt.extend_from_slice(&[
                3, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71,
            ]);
        }
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&((4 + 8 + fmt.len() + 8 + 8 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        out.extend_from_slice(&fmt);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn reads_float64_plain_and_extensible() {
        let dir = tempfile::tempdir().unwrap();
        for ext in [false, true] {
            let p = dir.path().join(format!("f64_{ext}.wav"));
            std::fs::write(&p, float64_wav(ext, &[0.5, -0.5, 0.25, 1.0])).unwrap();
            let d = read_wav(&p).unwrap();
            assert_eq!((d.channels, d.sample_rate), (2, 48_000));
            assert_eq!(d.samples, [0.5, -0.5, 0.25, 1.0]);
        }
    }

    #[test]
    fn rejects_garbage_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.wav");
        let good = float64_wav(false, &[0.1; 8]);
        for n in [0, 4, 12, 20, 40, good.len() - 3] {
            std::fs::write(&p, &good[..n]).unwrap();
            let _ = read_wav(&p);
        }
        std::fs::write(&p, b"not audio at all").unwrap();
        assert!(read_wav(&p).is_err());
    }

    #[test]
    fn export_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("out.wav");
        let l = [0.0, 0.5, 2.0];
        let r = [0.0, -0.5, -2.0];
        write_wav(&p, 48_000, &l, &r, WavExportFormat::Pcm24).unwrap();
        let d = read_wav(&p).unwrap();
        assert_eq!(d.channels, 2);
        assert!((d.samples[2] - 0.5).abs() < 1e-6);
        assert!((d.samples[4] - 1.0).abs() < 1e-6);
        assert!((d.samples[5] + 1.0).abs() < 1e-6);
    }
}
