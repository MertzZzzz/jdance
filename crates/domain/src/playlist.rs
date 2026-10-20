// SPDX-License-Identifier: GPL-3.0-or-later
//! Playlist: lanes and pattern clips (spec "Playlist").
//!
//! - A lane is organizational only: no instrument, no routing. Its
//!   mute/solo gates which clips' events are admitted to playback.
//! - A clip references a pattern; its duration is always
//!   `pattern.length_ticks · repeat_count` (never stored separately).
//! - Clips may overlap freely; every admitted event plays.

use crate::{ClipId, LaneId, Pattern, PatternId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Upper bound for clip repetitions.
pub const MAX_REPEAT: u32 = 1024;
/// Upper bound for a clip start, ≈ 4 hours at 120 BPM.
pub const MAX_START_TICKS: u64 = 960 * 4 * 60 * 120;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    /// Display order only.
    pub lanes: Vec<Lane>,
    pub clips: Vec<PatternClip>,
    /// Global loop region in ticks `[start, end)` for song playback.
    pub loop_range: Option<(u64, u64)>,
    pub loop_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    pub id: LaneId,
    pub name: String,
    /// RGB.
    pub color: [u8; 3],
    pub muted: bool,
    pub solo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternClip {
    pub id: ClipId,
    pub lane: LaneId,
    pub pattern: PatternId,
    /// Start in ticks.
    pub start: u64,
    /// `>= 1`.
    pub repeat: u32,
    pub muted: bool,
}

impl PatternClip {
    /// `[start, start + duration)` in ticks for the given pattern.
    pub fn interval(&self, pattern: &Pattern) -> (u64, u64) {
        (
            self.start,
            self.start + pattern.length_ticks() * u64::from(self.repeat),
        )
    }
}

/// Default lane colors, cycled for new lanes.
pub const LANE_COLORS: [[u8; 3]; 8] = [
    [235, 120, 60],
    [90, 170, 235],
    [120, 200, 110],
    [210, 110, 200],
    [230, 200, 80],
    [100, 210, 200],
    [200, 90, 90],
    [160, 160, 230],
];

impl Playlist {
    /// An empty playlist with `lanes` lanes whose IDs come from `next_id`.
    pub fn with_lanes(lanes: usize, mut next_id: impl FnMut() -> u64) -> Self {
        Self {
            lanes: (0..lanes)
                .map(|i| Lane {
                    id: LaneId(next_id()),
                    name: format!("Lane {}", i + 1),
                    color: LANE_COLORS[i % LANE_COLORS.len()],
                    muted: false,
                    solo: false,
                })
                .collect(),
            clips: Vec::new(),
            loop_range: None,
            loop_enabled: false,
        }
    }

    pub fn lane(&self, id: LaneId) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.id == id)
    }

    pub fn lane_mut(&mut self, id: LaneId) -> Option<&mut Lane> {
        self.lanes.iter_mut().find(|l| l.id == id)
    }

    pub fn clip(&self, id: ClipId) -> Option<&PatternClip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut PatternClip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// Whether the clip's events are admitted to playback (spec
    /// "Mute/Solo"): clip mute first, then lane mute, then lane solo.
    pub fn admits(&self, clip: &PatternClip) -> bool {
        if clip.muted {
            return false;
        }
        let Some(lane) = self.lane(clip.lane) else {
            return false;
        };
        if lane.muted {
            return false;
        }
        let any_solo = self.lanes.iter().any(|l| l.solo);
        !any_solo || lane.solo
    }

    /// Right edge of all clips in ticks, muted clips included.
    pub fn arrangement_end(&self, patterns: &[Pattern]) -> u64 {
        self.clips
            .iter()
            .filter_map(|c| {
                let p = patterns.iter().find(|p| p.id == c.pattern)?;
                Some(c.interval(p).1)
            })
            .max()
            .unwrap_or(0)
    }

    /// Structural checks against the project's patterns.
    pub fn validate(&self, patterns: &[Pattern]) -> Result<(), String> {
        let mut ids = BTreeSet::new();
        for l in &self.lanes {
            if !ids.insert(l.id.0) {
                return Err(format!("duplicate lane ID {}", l.id.0));
            }
        }
        for c in &self.clips {
            if !ids.insert(c.id.0) {
                return Err(format!("duplicate clip ID {}", c.id.0));
            }
            if self.lane(c.lane).is_none() {
                return Err(format!("clip {} is on a missing lane", c.id.0));
            }
            if !patterns.iter().any(|p| p.id == c.pattern) {
                return Err(format!("clip {} references a missing pattern", c.id.0));
            }
            if c.repeat == 0 || c.repeat > MAX_REPEAT {
                return Err(format!("clip {} repeat {} out of range", c.id.0, c.repeat));
            }
            if c.start > MAX_START_TICKS {
                return Err(format!("clip {} starts too late", c.id.0));
            }
        }
        if let Some((a, b)) = self.loop_range
            && a >= b
        {
            return Err("empty loop range".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Playlist, Vec<Pattern>) {
        let mut n = 100;
        let pl = Playlist::with_lanes(3, || {
            n += 1;
            n
        });
        let pats = vec![Pattern::new(PatternId(1), "A")];
        (pl, pats)
    }

    fn clip(id: u64, lane: LaneId, start: u64, repeat: u32) -> PatternClip {
        PatternClip {
            id: ClipId(id),
            lane,
            pattern: PatternId(1),
            start,
            repeat,
            muted: false,
        }
    }

    #[test]
    fn duration_and_end_follow_pattern_length() {
        let (mut pl, mut pats) = setup();
        let lane = pl.lanes[0].id;
        pl.clips.push(clip(1, lane, 3840, 3));
        assert_eq!(pl.clips[0].interval(&pats[0]), (3840, 3840 + 3 * 3840));
        pats[0].length_steps = 8;
        assert_eq!(pl.arrangement_end(&pats), 3840 + 3 * 1920);
        pl.clips[0].muted = true;
        assert_eq!(
            pl.arrangement_end(&pats),
            3840 + 3 * 1920,
            "muted clips count"
        );
    }

    #[test]
    fn admission_priorities() {
        let (mut pl, _) = setup();
        let (a, b) = (pl.lanes[0].id, pl.lanes[1].id);
        let ca = clip(1, a, 0, 1);
        let cb = clip(2, b, 0, 1);
        assert!(pl.admits(&ca) && pl.admits(&cb));
        pl.lanes[0].solo = true;
        assert!(pl.admits(&ca) && !pl.admits(&cb), "only solo lanes");
        pl.lanes[0].muted = true;
        assert!(!pl.admits(&ca), "mute beats solo");
        pl.lanes[0].muted = false;
        let muted_clip = PatternClip { muted: true, ..ca };
        assert!(!pl.admits(&muted_clip), "clip mute beats lane solo");
    }

    #[test]
    fn validation() {
        let (mut pl, pats) = setup();
        let lane = pl.lanes[0].id;
        pl.clips.push(clip(1, lane, 0, 1));
        assert!(pl.validate(&pats).is_ok());
        pl.clips.push(clip(2, LaneId(9), 0, 1));
        assert!(pl.validate(&pats).is_err());
        pl.clips.pop();
        pl.clips.push(clip(3, lane, 0, 0));
        assert!(pl.validate(&pats).is_err());
    }
}
