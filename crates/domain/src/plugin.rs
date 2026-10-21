// SPDX-License-Identifier: GPL-3.0-or-later
//! A plugin used as an insert effect, as stored in the project (PLG-070).
//!
//! Lookup is by `class_id`, never by path. The state blobs are opaque and
//! written back unchanged when the plugin is missing (PLG-072).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSlot {
    /// `"vst3"`.
    pub format: String,
    /// Canonical class ID (VST3: 32 hex characters).
    pub class_id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub category: String,
    #[serde(with = "base64")]
    pub component_state: Vec<u8>,
    #[serde(with = "base64")]
    pub controller_state: Vec<u8>,
}

/// Standard base64 (RFC 4648, with padding) for state blobs in JSON.
pub mod base64 {
    use super::*;

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(text: &str) -> Result<Vec<u8>, String> {
        let bytes = text.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return Err("base64 length is not a multiple of 4".into());
        }
        let value = |c: u8| -> Result<u32, String> {
            match c {
                b'A'..=b'Z' => Ok(u32::from(c - b'A')),
                b'a'..=b'z' => Ok(u32::from(c - b'a') + 26),
                b'0'..=b'9' => Ok(u32::from(c - b'0') + 52),
                b'+' => Ok(62),
                b'/' => Ok(63),
                _ => Err(format!("invalid base64 character {:?}", c as char)),
            }
        };
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for (i, q) in bytes.chunks(4).enumerate() {
            let last = i == bytes.len() / 4 - 1;
            let pad = q.iter().rev().take_while(|c| **c == b'=').count();
            if pad > 2 || (pad > 0 && !last) {
                return Err("misplaced base64 padding".into());
            }
            let mut n = 0;
            for &c in &q[..4 - pad] {
                n = n << 6 | value(c)?;
            }
            n <<= 6 * pad as u32;
            let b = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
            out.extend_from_slice(&b[..3 - pad]);
        }
        Ok(out)
    }

    pub fn serialize<S: Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        decode(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::base64::{decode, encode};

    #[test]
    fn base64_round_trip_and_vectors() {
        for (raw, enc) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(raw), enc);
            assert_eq!(decode(enc).unwrap(), raw);
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&all)).unwrap(), all);
        assert!(decode("Zg=").is_err());
        assert!(decode("Z===").is_err());
        assert!(decode("Zg==Zg==").is_err());
        assert!(decode("Z!==").is_err());
    }
}
