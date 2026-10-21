// SPDX-License-Identifier: GPL-3.0-or-later
//! MP3 encoding (spec 0.2.0 §12) over LAME 3.100, built from source and
//! linked statically by `mp3lame-sys` (LAME is LGPL-2.0-or-later; see
//! docs/licenses.md). Constant bitrate, stereo, optional title/artist.
//!
//! The encoder writes into any `Write + Seek`: the LAME info frame (with
//! encoder delay and padding, which decoders use to trim the output) is
//! patched in at the start when encoding ends.

use mp3lame_encoder::{Bitrate, Builder, DualPcm, Encoder, FlushGap, Id3Tag, Mode, Quality};
use std::io::{Seek, SeekFrom, Write};

/// Offered constant bitrates in kbit/s; 320 is the default.
pub const BITRATES: [u16; 4] = [128, 192, 256, 320];
/// Offered sample rates.
pub const SAMPLE_RATES: [u32; 2] = [44_100, 48_000];
/// LAME's encoder delay in samples (decoders trim it via the info frame).
pub const ENCODER_DELAY: usize = 576 + 529;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp3Settings {
    pub kbps: u16,
    pub sample_rate: u32,
    pub title: String,
    pub artist: String,
}

pub fn version() -> String {
    mp3lame_encoder::mp3lame_version().into_owned()
}

#[allow(missing_debug_implementations)]
pub struct Mp3Writer {
    encoder: Encoder,
    buf: Vec<u8>,
    /// Where the info frame goes (after an ID3v2 tag).
    info_at: u64,
}

fn bitrate(kbps: u16) -> Option<Bitrate> {
    Some(match kbps {
        128 => Bitrate::Kbps128,
        192 => Bitrate::Kbps192,
        256 => Bitrate::Kbps256,
        320 => Bitrate::Kbps320,
        _ => return None,
    })
}

fn err(e: impl std::fmt::Debug) -> String {
    format!("MP3 encoder: {e:?}")
}

impl Mp3Writer {
    /// Encoder for stereo float input at `settings.sample_rate`.
    pub fn new(settings: &Mp3Settings) -> Result<Self, String> {
        if !SAMPLE_RATES.contains(&settings.sample_rate) {
            return Err(format!("unsupported MP3 rate {}", settings.sample_rate));
        }
        let brate = bitrate(settings.kbps)
            .ok_or_else(|| format!("unsupported MP3 bitrate {} kbit/s", settings.kbps))?;
        let mut b = Builder::new().ok_or("MP3 encoder: out of memory")?;
        b.set_num_channels(2).map_err(err)?;
        b.set_sample_rate(settings.sample_rate).map_err(err)?;
        b.set_brate(brate).map_err(err)?;
        b.set_mode(Mode::JointStereo).map_err(err)?;
        b.set_quality(Quality::Best).map_err(err)?;
        // The info frame carries delay and padding (gapless trimming).
        b.set_to_write_vbr_tag(true).map_err(err)?;
        if !settings.title.is_empty() || !settings.artist.is_empty() {
            b.set_id3_tag(Id3Tag {
                title: settings.title.as_bytes(),
                artist: settings.artist.as_bytes(),
                album: b"",
                album_art: b"",
                year: b"",
                comment: b"",
            })
            .map_err(err)?;
        }
        let encoder = b.build().map_err(err)?;
        let info_at = encoder.id3v2_tag_size() as u64;
        Ok(Self {
            encoder,
            buf: Vec::new(),
            info_at,
        })
    }

    /// Encodes one block (equal lengths) and writes what is ready.
    pub fn write(
        &mut self,
        left: &[f32],
        right: &[f32],
        out: &mut impl Write,
    ) -> Result<(), String> {
        self.buf.clear();
        self.buf
            .reserve(mp3lame_encoder::max_required_buffer_size(left.len()));
        self.encoder
            .encode_to_vec(DualPcm { left, right }, &mut self.buf)
            .map_err(err)?;
        out.write_all(&self.buf).map_err(|e| e.to_string())
    }

    /// Flushes the encoder and patches the info frame at the start.
    pub fn finish<W: Write + Seek>(mut self, out: &mut W) -> Result<(), String> {
        self.buf.clear();
        self.buf.reserve(7200);
        self.encoder
            .flush_to_vec::<FlushGap>(&mut self.buf)
            .map_err(err)?;
        out.write_all(&self.buf).map_err(|e| e.to_string())?;
        let mut tag = Vec::with_capacity(self.encoder.lame_tag_size());
        if self.encoder.lame_tag_encode_to_vec(&mut tag).is_some() {
            let end = out.stream_position().map_err(|e| e.to_string())?;
            out.seek(SeekFrom::Start(self.info_at))
                .and_then(|_| out.write_all(&tag))
                .and_then(|()| out.seek(SeekFrom::Start(end)).map(|_| ()))
                .map_err(|e| e.to_string())?;
        }
        out.flush().map_err(|e| e.to_string())
    }
}

/// MPEG-1 Layer III frames found in `data`: (count, sample rate).
/// For checks: each frame holds 1152 samples.
pub fn count_frames(data: &[u8]) -> (usize, u32) {
    const RATES: [u32; 3] = [44_100, 48_000, 32_000];
    const KBPS: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    let mut i = 0;
    // Skip an ID3v2 tag.
    if data.len() > 10 && &data[..3] == b"ID3" {
        let size = data[6..10]
            .iter()
            .fold(0usize, |a, b| (a << 7) | usize::from(b & 0x7f));
        i = 10 + size;
    }
    let (mut frames, mut rate) = (0, 0);
    while i + 4 <= data.len() {
        let h = &data[i..i + 4];
        let sync = h[0] == 0xFF && h[1] & 0xFE == 0xFA; // MPEG-1, Layer III
        let b = usize::from(h[2] >> 4);
        let r = usize::from((h[2] >> 2) & 3);
        if !sync || b == 0 || b >= KBPS.len() || r >= RATES.len() {
            break;
        }
        let pad = usize::from((h[2] >> 1) & 1);
        rate = RATES[r];
        let len = (144 * KBPS[b] * 1000 / rate) as usize + pad;
        frames += 1;
        i += len;
    }
    (frames, rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_cbr_stereo_with_tags_and_info_frame() {
        let rate = 48_000;
        let n = rate as usize; // 1 s
        let left: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect();
        let right = left.clone();
        let settings = Mp3Settings {
            kbps: 320,
            sample_rate: rate,
            title: "Song".into(),
            artist: "Me".into(),
        };
        let mut w = Mp3Writer::new(&settings).unwrap();
        let mut out = std::io::Cursor::new(Vec::new());
        for (l, r) in left.chunks(4096).zip(right.chunks(4096)) {
            w.write(l, r, &mut out).unwrap();
        }
        w.finish(&mut out).unwrap();
        let data = out.into_inner();
        assert_eq!(&data[..3], b"ID3", "title/artist tag");
        let (frames, r) = count_frames(&data);
        assert_eq!(r, rate);
        // Info frame + audio frames covering the input plus delay/padding.
        let samples = (frames - 1) * 1152;
        assert!(samples >= n + ENCODER_DELAY, "{samples}");
        assert!(samples < n + ENCODER_DELAY + 2 * 1152, "{samples}");
        // The info frame was patched in ("Info" for CBR).
        let at = data.windows(4).position(|w| w == b"Info");
        assert!(at.is_some(), "LAME info frame");
        // 320 kbit/s: about 40 kB per second.
        assert!((38_000..44_000).contains(&data.len()), "{}", data.len());
        assert!(version().starts_with('3'));
    }

    #[test]
    fn rejects_unsupported_settings() {
        let s = Mp3Settings {
            kbps: 100,
            sample_rate: 48_000,
            title: String::new(),
            artist: String::new(),
        };
        assert!(Mp3Writer::new(&s).is_err());
        assert!(
            Mp3Writer::new(&Mp3Settings {
                kbps: 128,
                sample_rate: 96_000,
                ..s
            })
            .is_err()
        );
    }
}
