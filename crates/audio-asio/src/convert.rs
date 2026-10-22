// SPDX-License-Identifier: GPL-3.0-or-later
//! Driver sample formats ↔ `f32` (spec 0.2.2 ASIO-033, Amendment 02
//! ASIO-005). Realtime-safe: no allocation, fixed work per frame.

use crate::abi::sample_type as st;
use jdance_audio_api::SampleFormat;

/// A channel format the adapter converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Integer of `bytes` bytes; full scale `2^(bits-1)`; big endian when
    /// `msb`.
    Int {
        bytes: u8,
        bits: u8,
        msb: bool,
    },
    F32 {
        msb: bool,
    },
    F64 {
        msb: bool,
    },
}

impl Format {
    /// `None` for formats the adapter does not support (DSD and unknown):
    /// opening fails with the name (ASIO-033).
    pub fn of(sample_type: i32) -> Option<Self> {
        let int = |bytes, bits, msb| Some(Self::Int { bytes, bits, msb });
        match sample_type {
            st::INT16_LSB => int(2, 16, false),
            st::INT16_MSB => int(2, 16, true),
            st::INT24_LSB => int(3, 24, false),
            st::INT24_MSB => int(3, 24, true),
            st::INT32_LSB => int(4, 32, false),
            st::INT32_MSB => int(4, 32, true),
            st::INT32_LSB16 => int(4, 16, false),
            st::INT32_LSB18 => int(4, 18, false),
            st::INT32_LSB20 => int(4, 20, false),
            st::INT32_LSB24 => int(4, 24, false),
            st::INT32_MSB16 => int(4, 16, true),
            st::INT32_MSB18 => int(4, 18, true),
            st::INT32_MSB20 => int(4, 20, true),
            st::INT32_MSB24 => int(4, 24, true),
            st::FLOAT32_LSB => Some(Self::F32 { msb: false }),
            st::FLOAT32_MSB => Some(Self::F32 { msb: true }),
            st::FLOAT64_LSB => Some(Self::F64 { msb: false }),
            st::FLOAT64_MSB => Some(Self::F64 { msb: true }),
            _ => None,
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            Self::Int { bytes, .. } => usize::from(bytes),
            Self::F32 { .. } => 4,
            Self::F64 { .. } => 8,
        }
    }

    /// The nearest `audio-api` format, for display.
    pub fn api(self) -> SampleFormat {
        match self {
            Self::Int { bytes: 2, .. } => SampleFormat::I16,
            Self::Int { bytes: 3, .. } => SampleFormat::I24,
            Self::Int { .. } => SampleFormat::I32,
            Self::F32 { .. } => SampleFormat::F32,
            Self::F64 { .. } => SampleFormat::F64,
        }
    }

    /// Reads `dst.len()` frames from `src` (at least that many frames of
    /// this format).
    pub fn read(self, src: &[u8], dst: &mut [f32]) {
        let n = self.bytes();
        for (d, s) in dst.iter_mut().zip(src.chunks_exact(n)) {
            *d = match self {
                Self::Int { bits, msb, .. } => {
                    let v = int_from(s, msb);
                    (v as f64 / f64::from(1u32 << (bits - 1))) as f32
                }
                Self::F32 { msb } => {
                    let b = [s[0], s[1], s[2], s[3]];
                    if msb {
                        f32::from_be_bytes(b)
                    } else {
                        f32::from_le_bytes(b)
                    }
                }
                Self::F64 { msb } => {
                    let b = [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]];
                    (if msb {
                        f64::from_be_bytes(b)
                    } else {
                        f64::from_le_bytes(b)
                    }) as f32
                }
            };
        }
    }

    /// Writes `src` into `dst` (room for `src.len()` frames), clipping
    /// integers to full scale.
    pub fn write(self, src: &[f32], dst: &mut [u8]) {
        let n = self.bytes();
        for (s, d) in src.iter().zip(dst.chunks_exact_mut(n)) {
            match self {
                Self::Int { bits, msb, .. } => {
                    let full = f64::from(1u32 << (bits - 1));
                    let v = (f64::from(s.clamp(-1.0, 1.0)) * full)
                        .round()
                        .clamp(-full, full - 1.0) as i64;
                    int_to(v, d, msb);
                }
                Self::F32 { msb } => d.copy_from_slice(&if msb {
                    s.to_be_bytes()
                } else {
                    s.to_le_bytes()
                }),
                Self::F64 { msb } => d.copy_from_slice(&if msb {
                    f64::from(*s).to_be_bytes()
                } else {
                    f64::from(*s).to_le_bytes()
                }),
            }
        }
    }
}

// Containers wider than the data (32-bit with 16…24-bit alignment) hold
// the value right-aligned and sign-extended, so they read and write like a
// plain 32-bit integer at the smaller full scale.

/// Sign-extended integer from 2, 3 or 4 bytes.
fn int_from(s: &[u8], msb: bool) -> i64 {
    let mut v: i64 = 0;
    if msb {
        for b in s {
            v = (v << 8) | i64::from(*b);
        }
    } else {
        for b in s.iter().rev() {
            v = (v << 8) | i64::from(*b);
        }
    }
    let width = 8 * s.len() as u32;
    let shift = 64 - width;
    (v << shift) >> shift
}

fn int_to(v: i64, d: &mut [u8], msb: bool) {
    let n = d.len();
    for (k, byte) in d.iter_mut().enumerate() {
        let i = if msb { n - 1 - k } else { k };
        *byte = (v >> (8 * i)) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(sample_type: i32, values: &[f32], tolerance: f32) {
        let f = Format::of(sample_type).unwrap();
        let mut bytes = vec![0u8; values.len() * f.bytes()];
        f.write(values, &mut bytes);
        let mut back = vec![0.0f32; values.len()];
        f.read(&bytes, &mut back);
        for (a, b) in values.iter().zip(&back) {
            assert!((a - b).abs() <= tolerance, "{sample_type}: {a} → {b}");
        }
    }

    #[test]
    fn every_supported_format_round_trips() {
        let v = [0.0, 0.5, -0.5, 0.999, -1.0, 0.123_456];
        for (t, tol) in [
            (st::INT16_LSB, 1.0 / 32_768.0),
            (st::INT16_MSB, 1.0 / 32_768.0),
            (st::INT24_LSB, 1.0 / 8_388_608.0),
            (st::INT24_MSB, 1.0 / 8_388_608.0),
            (st::INT32_LSB, 1e-7),
            (st::INT32_MSB, 1e-7),
            (st::INT32_LSB16, 1.0 / 32_768.0),
            (st::INT32_LSB18, 1.0 / 131_072.0),
            (st::INT32_LSB20, 1.0 / 524_288.0),
            (st::INT32_LSB24, 1.0 / 8_388_608.0),
            (st::INT32_MSB24, 1.0 / 8_388_608.0),
            (st::FLOAT32_LSB, 0.0),
            (st::FLOAT32_MSB, 0.0),
            (st::FLOAT64_LSB, 0.0),
            (st::FLOAT64_MSB, 0.0),
        ] {
            round_trip(t, &v, tol);
        }
    }

    #[test]
    fn integer_edges_and_byte_order() {
        let f = Format::of(st::INT16_LSB).unwrap();
        let mut b = [0u8; 6];
        f.write(&[1.0, -1.0, 2.0], &mut b);
        // +1.0 clips to 32767, -1.0 is -32768, out of range clips.
        assert_eq!(b, [0xFF, 0x7F, 0x00, 0x80, 0xFF, 0x7F]);
        let f = Format::of(st::INT24_MSB).unwrap();
        let mut b = [0u8; 3];
        f.write(&[-1.0], &mut b);
        assert_eq!(b, [0x80, 0x00, 0x00]);
        let mut out = [0.0f32];
        f.read(&[0xFF, 0xFF, 0xFF], &mut out);
        assert_eq!(out[0], -1.0 / 8_388_608.0, "sign extension");
        // 24 bits in a 32-bit container: right-aligned, sign-extended.
        let f = Format::of(st::INT32_LSB24).unwrap();
        let mut b = [0u8; 4];
        f.write(&[-0.5], &mut b);
        assert_eq!(b, [0x00, 0x00, 0xC0, 0xFF]);
    }

    #[test]
    fn unsupported_formats_are_refused() {
        assert!(Format::of(32).is_none(), "DSD");
        assert!(Format::of(99).is_none());
    }
}
