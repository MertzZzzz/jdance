// SPDX-License-Identifier: GPL-3.0-or-later
//! Editable project document.
//!
//! This crate holds plain data only: no audio, no UI, no I/O. The engine
//! compiles a [`Project`] into its own realtime representation; the UI edits
//! it exclusively through `jdance-commands`.

mod ids;
mod mixer;

pub use ids::{ChannelId, IdAllocator, InsertId, PatternId, SendId, TrackId};
pub use mixer::{
    CompressorParams, Effect, Insert, MAX_INSERTS_PER_TRACK, MAX_SEND_LEVEL, MAX_SENDS_PER_TRACK,
    MAX_TRACKS, Mixer, MixerTrack, RoutingError, Send, Sidechain, Tap,
};

use serde::{Deserialize, Serialize};

/// Ticks per quarter note used for all musical time in the document.
pub const PPQ: u32 = 960;
/// Ticks in one sixteenth-note step.
pub const TICKS_PER_STEP: u32 = PPQ / 4;
/// Default and, for now, only supported pattern length in steps.
pub const DEFAULT_PATTERN_STEPS: u32 = 16;

pub const MIN_TEMPO_BPM: f64 = 20.0;
pub const MAX_TEMPO_BPM: f64 = 999.0;
/// Upper bound for channel/track volume (linear gain).
pub const MAX_VOLUME: f32 = 1.25;
/// Channel Rack limit (RT-011: the engine preallocates per-channel buffers).
pub const MAX_CHANNELS: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub tempo_bpm: f64,
    pub channels: Vec<Channel>,
    pub patterns: Vec<Pattern>,
    pub mixer: Mixer,
    pub ids: IdAllocator,
}

impl Project {
    /// An empty project with one pattern and the master track.
    pub fn new() -> Self {
        let mut ids = IdAllocator::default();
        let pattern = Pattern::new(PatternId(ids.next()), "Pattern 1");
        Self {
            tempo_bpm: 130.0,
            channels: Vec::new(),
            patterns: vec![pattern],
            mixer: Mixer::default(),
            ids,
        }
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn channel_mut(&mut self, id: ChannelId) -> Option<&mut Channel> {
        self.channels.iter_mut().find(|c| c.id == id)
    }

    pub fn channel_index(&self, id: ChannelId) -> Option<usize> {
        self.channels.iter().position(|c| c.id == id)
    }

    pub fn pattern(&self, id: PatternId) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.id == id)
    }

    pub fn pattern_mut(&mut self, id: PatternId) -> Option<&mut Pattern> {
        self.patterns.iter_mut().find(|p| p.id == id)
    }

    /// The pattern played by the step-sequencer loop.
    pub fn active_pattern(&self) -> Option<&Pattern> {
        self.patterns.first()
    }

    /// Checks references from channels into the mixer and the routing
    /// graph (MIX-002).
    pub fn validate_routing(&self) -> Result<(), RoutingError> {
        for c in &self.channels {
            if !self.mixer.has_track(c.track) {
                return Err(RoutingError::UnknownTrack(c.track));
            }
        }
        self.mixer.validate()
    }
}

impl Default for Project {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub source: ChannelSource,
    /// Linear gain, `0.0..=MAX_VOLUME`.
    pub volume: f32,
    /// `-1.0` (left) ..= `1.0` (right).
    pub pan: f32,
    pub muted: bool,
    /// Mixer track this channel plays into (MIX-001).
    pub track: TrackId,
}

impl Channel {
    pub fn new_sampler(id: ChannelId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            source: ChannelSource::Sampler { sample: None },
            volume: 0.8,
            pan: 0.0,
            muted: false,
            track: TrackId::MASTER,
        }
    }

    pub fn sample(&self) -> Option<&SampleRef> {
        match &self.source {
            ChannelSource::Sampler { sample } => sample.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelSource {
    Sampler { sample: Option<SampleRef> },
}

/// Reference to an audio file used by a sampler channel.
///
/// `path` is stored as written by the user/persistence layer; relative paths
/// are resolved against the project directory (PER-003).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SampleRef {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub id: PatternId,
    pub name: String,
    pub length_steps: u32,
    pub rows: Vec<StepRow>,
}

impl Pattern {
    pub fn new(id: PatternId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            length_steps: DEFAULT_PATTERN_STEPS,
            rows: Vec::new(),
        }
    }

    pub fn length_ticks(&self) -> u64 {
        u64::from(self.length_steps) * u64::from(TICKS_PER_STEP)
    }

    pub fn row(&self, channel: ChannelId) -> Option<&StepRow> {
        self.rows.iter().find(|r| r.channel == channel)
    }

    /// Returns the row for `channel`, creating an empty one if needed.
    pub fn row_mut(&mut self, channel: ChannelId) -> &mut StepRow {
        if let Some(i) = self.rows.iter().position(|r| r.channel == channel) {
            &mut self.rows[i]
        } else {
            self.rows.push(StepRow::new(channel, self.length_steps));
            self.rows.last_mut().expect("just pushed")
        }
    }

    pub fn step(&self, channel: ChannelId, index: u32) -> Option<Step> {
        self.row(channel)?.steps.get(index as usize).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRow {
    pub channel: ChannelId,
    pub steps: Vec<Step>,
}

impl StepRow {
    pub fn new(channel: ChannelId, length: u32) -> Self {
        Self {
            channel,
            steps: vec![Step::default(); length as usize],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub on: bool,
    /// `0.0..=1.0`.
    pub velocity: f32,
}

impl Default for Step {
    fn default() -> Self {
        Self {
            on: false,
            velocity: 0.8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_project_has_one_pattern_and_unique_ids() {
        let mut p = Project::new();
        assert_eq!(p.patterns.len(), 1);
        let a = ChannelId(p.ids.next());
        let b = ChannelId(p.ids.next());
        assert_ne!(a, b);
        assert_ne!(a.0, p.patterns[0].id.0);
    }

    #[test]
    fn row_mut_creates_row_with_pattern_length() {
        let mut p = Project::new();
        let ch = ChannelId(p.ids.next());
        let pat = &mut p.patterns[0];
        assert_eq!(pat.row_mut(ch).steps.len(), DEFAULT_PATTERN_STEPS as usize);
        pat.row_mut(ch).steps[3].on = true;
        assert_eq!(pat.rows.len(), 1);
        assert!(pat.step(ch, 3).unwrap().on);
    }

    #[test]
    fn project_serde_round_trip() {
        let mut p = Project::new();
        let id = ChannelId(p.ids.next());
        let mut ch = Channel::new_sampler(id, "Kick");
        ch.source = ChannelSource::Sampler {
            sample: Some(SampleRef {
                path: "samples/kick.wav".into(),
            }),
        };
        p.channels.push(ch);
        p.patterns[0].row_mut(id).steps[0].on = true;
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
