// SPDX-License-Identifier: GPL-3.0-or-later
//! Class IDs as portable hex strings.
//!
//! A VST3 `TUID` is 16 bytes whose order depends on the platform: on
//! Windows it has the COM `GUID` layout. Projects store the canonical form
//! (the four 32-bit words of `FUID::toString`), so a project made on one OS
//! finds the same plugin on another (PLG-070).

use vst3::Steinberg::TUID;

fn words(t: &TUID) -> [u32; 4] {
    let b: [u8; 16] = t.map(|x| x as u8);
    let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    if cfg!(windows) {
        let a = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let b1 = u32::from(u16::from_le_bytes([b[4], b[5]])) << 16
            | u32::from(u16::from_le_bytes([b[6], b[7]]));
        [a, b1, be(8), be(12)]
    } else {
        [be(0), be(4), be(8), be(12)]
    }
}

/// Canonical 32-character upper-case hex form.
pub fn to_hex(t: &TUID) -> String {
    words(t).iter().map(|w| format!("{w:08X}")).collect()
}

/// Parses [`to_hex`] output.
pub fn from_hex(s: &str) -> Option<TUID> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let mut w = [0u32; 4];
    for (i, w) in w.iter_mut().enumerate() {
        *w = u32::from_str_radix(&s[i * 8..i * 8 + 8], 16).ok()?;
    }
    Some(vst3::uid(w[0], w[1], w[2], w[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_matches_uid_words() {
        let t = vst3::uid(0x4A44_5446, 0x5845_0000, 1, 0xDEAD_BEEF);
        let hex = to_hex(&t);
        assert_eq!(hex, "4A4454465845000000000001DEADBEEF");
        assert_eq!(from_hex(&hex), Some(t));
        assert_eq!(from_hex("xyz"), None);
    }
}
