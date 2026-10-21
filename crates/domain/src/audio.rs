// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio assets and audio clips (spec 0.2.0 §2).
//!
//! An asset is an imported file; a clip places a range of it on the
//! playlist. Several clips may use one asset with different ranges. Clip
//! positions are integer frames of the project timebase
//! ([`crate::Project::timebase_rate`]): they keep their absolute time when
//! the tempo changes, and nothing is time-stretched.

use crate::{AssetId, ClipId, LaneId, MAX_VOLUME};
use serde::{Deserialize, Serialize};

/// Default project timebase in frames per second (D2).
pub const DEFAULT_TIMEBASE: u32 = 48_000;
/// Upper bound for a clip start, ≈ 6 hours at 48 kHz.
pub const MAX_START_FRAMES: u64 = 48_000 * 60 * 60 * 6;
/// Anti-click fade applied at both clip edges, bounded by the clip length.
pub const EDGE_FADE_SECONDS: f64 = 0.003;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioAsset {
    pub id: AssetId,
    /// Display name; renaming never renames the file.
    pub name: String,
    /// Relative to the project folder (inside `assets/`), or absolute until
    /// the project is saved.
    pub path: String,
    /// FNV-1a 64 of the file content (identity, not security).
    pub hash: u64,
    /// File size in bytes.
    pub size: u64,
    pub sample_rate: u32,
    pub channels: u16,
    /// Frames of the source file at `sample_rate`.
    pub frames: u64,
}

impl AudioAsset {
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate.max(1))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.sample_rate == 0 || self.channels == 0 || self.frames == 0 {
            return Err(format!("asset {} has no audio", self.id.0));
        }
        if self.path.is_empty() {
            return Err(format!("asset {} has no path", self.id.0));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    pub id: ClipId,
    pub lane: LaneId,
    pub asset: AssetId,
    /// Timeline start in project frames.
    pub start: u64,
    /// Source range `[source_in, source_out)` in frames of the asset.
    pub source_in: u64,
    pub source_out: u64,
    /// Linear gain `0.0..=MAX_VOLUME`.
    pub gain: f32,
    pub muted: bool,
    /// Extra fades in project frames (0 = only the anti-click fade).
    #[serde(default)]
    pub fade_in: u64,
    #[serde(default)]
    pub fade_out: u64,
}

impl AudioClip {
    /// Length on the timeline in project frames for `asset`'s rate.
    pub fn length(&self, asset: &AudioAsset, timebase: u32) -> u64 {
        let src = self.source_out.saturating_sub(self.source_in);
        (src as f64 * f64::from(timebase) / f64::from(asset.sample_rate.max(1))).round() as u64
    }

    /// `[start, end)` in project frames.
    pub fn interval(&self, asset: &AudioAsset, timebase: u32) -> (u64, u64) {
        (self.start, self.start + self.length(asset, timebase))
    }

    pub fn validate(&self, asset: &AudioAsset) -> Result<(), String> {
        let id = self.id.0;
        if self.source_out <= self.source_in {
            return Err(format!("audio clip {id} has an empty source range"));
        }
        if self.source_out > asset.frames {
            return Err(format!("audio clip {id} reaches past the end of its file"));
        }
        if self.start > MAX_START_FRAMES {
            return Err(format!("audio clip {id} starts too late"));
        }
        if !(0.0..=MAX_VOLUME).contains(&self.gain) {
            return Err(format!("audio clip {id} gain {} out of range", self.gain));
        }
        Ok(())
    }
}

/// FNV-1a 64 over `bytes`.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Project frames ↔ ticks at `tempo_bpm` (960 PPQ).
pub fn frames_per_tick(tempo_bpm: f64, timebase: u32) -> f64 {
    f64::from(timebase) * 60.0 / (tempo_bpm * f64::from(crate::PPQ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset() -> AudioAsset {
        AudioAsset {
            id: AssetId(1),
            name: "a".into(),
            path: "assets/a.wav".into(),
            hash: 0,
            size: 0,
            sample_rate: 44_100,
            channels: 2,
            frames: 44_100,
        }
    }

    #[test]
    fn length_follows_the_source_range_and_rates() {
        let a = asset();
        let c = AudioClip {
            id: ClipId(2),
            lane: LaneId(3),
            asset: a.id,
            start: 1000,
            source_in: 0,
            source_out: 22_050,
            gain: 1.0,
            muted: false,
            fade_in: 0,
            fade_out: 0,
        };
        assert_eq!(c.length(&a, 48_000), 24_000, "half a second");
        assert_eq!(c.interval(&a, 48_000), (1000, 25_000));
        assert!(c.validate(&a).is_ok());
        assert!(
            AudioClip {
                source_out: 44_101,
                ..c
            }
            .validate(&a)
            .is_err()
        );
        assert!(
            AudioClip {
                source_in: 22_050,
                ..c
            }
            .validate(&a)
            .is_err()
        );
    }

    #[test]
    fn hash_is_fnv1a() {
        assert_eq!(content_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(content_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
