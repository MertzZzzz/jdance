// SPDX-License-Identifier: GPL-3.0-or-later
//! Playlist: lanes and pattern clips (spec "Playlist").
//!
//! - A lane holds pattern and audio clips. Its mute/solo gates which
//!   clips play. Its `audio_track` routes only its audio clips; pattern
//!   clips play through their channels' own routing (spec 0.2.0 §2).
//! - A pattern clip shows `duration` ticks of its pattern starting at
//!   `offset`; looped clips repeat the pattern, so the number of
//!   repetitions is derived, never stored (spec 0.2.0 §4).
//! - Clips may overlap freely; every admitted event plays.

use crate::{AudioClip, AutomationId, ClipId, LaneId, Pattern, PatternId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Upper bound for repetitions of a looped clip (bounds `duration`).
pub const MAX_REPEAT: u32 = 1024;
/// Upper bound for a pattern clip's duration and offset in ticks.
pub const MAX_CLIP_TICKS: u64 = MAX_REPEAT as u64 * crate::MAX_PATTERN_STEPS as u64 * 240;
/// Upper bound for a clip start, ≈ 4 hours at 120 BPM.
pub const MAX_START_TICKS: u64 = 960 * 4 * 60 * 120;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    /// Display order only.
    pub lanes: Vec<Lane>,
    pub clips: Vec<PatternClip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_clips: Vec<AudioClip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation_clips: Vec<AutomationClip>,
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
    /// Mixer track for this lane's audio clips (default master).
    #[serde(default = "master")]
    pub audio_track: TrackId,
}

fn master() -> TrackId {
    TrackId::MASTER
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternClip {
    pub id: ClipId,
    pub lane: LaneId,
    pub pattern: PatternId,
    /// Start in ticks.
    pub start: u64,
    /// Pattern position shown at `start`, in ticks; looped clips use it
    /// modulo the pattern length.
    pub offset: u64,
    /// Length on the timeline in ticks, `>= 1`.
    pub duration: u64,
    /// Repeats the pattern for as long as `duration`; otherwise the clip
    /// shows at most the pattern's own range.
    pub looped: bool,
    pub muted: bool,
}

impl PatternClip {
    /// A looped clip of `repeat` whole repetitions of `pattern`.
    pub fn repeating(id: ClipId, lane: LaneId, pattern: &Pattern, start: u64, repeat: u32) -> Self {
        Self {
            id,
            lane,
            pattern: pattern.id,
            start,
            offset: 0,
            duration: pattern.length_ticks() * u64::from(repeat.max(1)),
            looped: true,
            muted: false,
        }
    }

    /// `[start, start + duration)` in ticks.
    pub fn interval(&self) -> (u64, u64) {
        (self.start, self.start + self.duration)
    }

    /// Pattern repetitions the clip touches (whole or partial).
    pub fn repetitions(&self, pattern: &Pattern) -> u64 {
        let len = pattern.length_ticks().max(1);
        if !self.looped {
            return 1;
        }
        ((self.offset % len) + self.duration).div_ceil(len)
    }
}

/// A placed range of an automation source (spec 0.2.0 §11). It does not
/// loop: `offset + duration` stays within the source length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationClip {
    pub id: ClipId,
    pub lane: LaneId,
    pub source: AutomationId,
    /// Start in ticks.
    pub start: u64,
    /// Source position shown at `start`, in ticks.
    #[serde(default)]
    pub offset: u64,
    /// Length on the timeline in ticks, `>= 1`.
    pub duration: u64,
    #[serde(default)]
    pub muted: bool,
}

impl AutomationClip {
    /// `[start, start + duration)` in ticks.
    pub fn interval(&self) -> (u64, u64) {
        (self.start, self.start + self.duration)
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
                    audio_track: TrackId::MASTER,
                })
                .collect(),
            clips: Vec::new(),
            audio_clips: Vec::new(),
            automation_clips: Vec::new(),
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

    pub fn audio_clip(&self, id: ClipId) -> Option<&AudioClip> {
        self.audio_clips.iter().find(|c| c.id == id)
    }

    pub fn audio_clip_mut(&mut self, id: ClipId) -> Option<&mut AudioClip> {
        self.audio_clips.iter_mut().find(|c| c.id == id)
    }

    pub fn automation_clip(&self, id: ClipId) -> Option<&AutomationClip> {
        self.automation_clips.iter().find(|c| c.id == id)
    }

    /// Whether any clip kind has `id`.
    pub fn has_clip(&self, id: ClipId) -> bool {
        self.clip(id).is_some()
            || self.audio_clip(id).is_some()
            || self.automation_clip(id).is_some()
    }

    /// Whether an automation clip is active (same rules as pattern clips).
    pub fn admits_automation(&self, clip: &AutomationClip) -> bool {
        !clip.muted && self.lane_plays(clip.lane)
    }

    /// Whether the clip's events are admitted to playback (spec
    /// "Mute/Solo"): clip mute first, then lane mute, then lane solo.
    pub fn admits(&self, clip: &PatternClip) -> bool {
        !clip.muted && self.lane_plays(clip.lane)
    }

    /// Whether an audio clip plays (same rules as pattern clips).
    pub fn admits_audio(&self, clip: &AudioClip) -> bool {
        !clip.muted && self.lane_plays(clip.lane)
    }

    fn lane_plays(&self, lane: LaneId) -> bool {
        let Some(lane) = self.lane(lane) else {
            return false;
        };
        if lane.muted {
            return false;
        }
        let any_solo = self.lanes.iter().any(|l| l.solo);
        !any_solo || lane.solo
    }

    /// Right edge of all pattern clips in ticks, muted clips included.
    /// Audio clips: [`crate::Project::arrangement_end_ticks`]. Automation
    /// clips make no sound and do not extend the song.
    pub fn arrangement_end(&self) -> u64 {
        self.clips.iter().map(|c| c.interval().1).max().unwrap_or(0)
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
            if c.duration == 0 || c.duration > MAX_CLIP_TICKS || c.offset > MAX_CLIP_TICKS {
                return Err(format!("clip {} duration or offset out of range", c.id.0));
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
        let pat = Pattern::new(PatternId(1), "A");
        PatternClip::repeating(ClipId(id), lane, &pat, start, repeat)
    }

    #[test]
    fn duration_is_stored_and_repetitions_are_derived() {
        let (mut pl, mut pats) = setup();
        let lane = pl.lanes[0].id;
        pl.clips.push(clip(1, lane, 3840, 3));
        assert_eq!(pl.clips[0].interval(), (3840, 3840 + 3 * 3840));
        pats[0].length_steps = 8;
        assert_eq!(
            pl.arrangement_end(),
            3840 + 3 * 3840,
            "the clip keeps its length when the pattern shrinks"
        );
        assert_eq!(pl.clips[0].repetitions(&pats[0]), 6);
        pl.clips[0].offset = 960;
        pl.clips[0].duration = 1920;
        assert_eq!(pl.clips[0].repetitions(&pats[0]), 2, "960..2880 spans two");
        pl.clips[0].looped = false;
        assert_eq!(pl.clips[0].repetitions(&pats[0]), 1);
        pl.clips[0].muted = true;
        assert_eq!(pl.arrangement_end(), 3840 + 1920, "muted clips count");
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
        let mut empty = clip(3, lane, 0, 1);
        empty.duration = 0;
        pl.clips.push(empty);
        assert!(pl.validate(&pats).is_err());
    }
}
