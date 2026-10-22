// SPDX-License-Identifier: GPL-3.0-or-later
//! Editable project document.
//!
//! This crate holds plain data only: no audio, no UI, no I/O. The engine
//! compiles a [`Project`] into its own realtime representation; the UI edits
//! it exclusively through `jdance-commands`.

mod audio;
mod automation;
mod effects;
mod export;
mod ids;
mod mixer;
mod playlist;
mod plugin;

pub use audio::{
    AudioAsset, AudioClip, DEFAULT_TIMEBASE, EDGE_FADE_SECONDS, MAX_START_FRAMES, content_hash,
    frames_per_tick,
};
pub use automation::{
    AutoPoint, AutomationLane, AutomationTarget, Curve, MAX_AUTOMATION_POINTS,
    MAX_AUTOMATION_TICKS, curve_weight, sort_points,
};
pub use effects::{
    DELAY_DIVISIONS, EQ_MAX_FREQ, Effect, EffectKind, LimiterSettings, Mapping, ParamSpec,
    ParamUnit, Smoothing,
};
pub use export::{
    DEFAULT_EXPORT_TAIL_SECONDS, ExportFormat, ExportOptions, ExportRange, MAX_EXPORT_TAIL_SECONDS,
    Mp3Options,
};
pub use ids::{
    AssetId, AutomationId, ChannelId, ClipId, IdAllocator, InsertId, LaneId, PatternId, SendId,
    TrackId,
};
pub use mixer::{
    Insert, MAX_INSERTS_PER_TRACK, MAX_SEND_LEVEL, MAX_SENDS_PER_TRACK, MAX_TRACKS, Mixer,
    MixerTrack, RoutingError, Send, Sidechain, Tap,
};
pub use playlist::{
    AutomationClip, LANE_COLORS, Lane, MAX_CLIP_TICKS, MAX_REPEAT, MAX_START_TICKS, PatternClip,
    Playlist,
};
pub use plugin::{PluginSlot, base64};

use serde::{Deserialize, Serialize};

/// Ticks per quarter note used for all musical time in the document.
pub const PPQ: u32 = 960;
/// Ticks in one sixteenth-note step.
pub const TICKS_PER_STEP: u32 = PPQ / 4;
/// Default pattern length in steps (one 4/4 bar of sixteenths).
pub const DEFAULT_PATTERN_STEPS: u32 = 16;
/// Pattern length limit in steps (16 bars).
pub const MAX_PATTERN_STEPS: u32 = 256;
/// Upper bound for notes in one piano-roll row.
pub const MAX_NOTES_PER_ROW: usize = 10_000;
/// MIDI key played by steps and the sampler's root key (C5 = 60).
pub const ROOT_KEY: u8 = 60;
/// Lanes in a new project.
pub const DEFAULT_LANES: usize = 6;

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
    pub playlist: Playlist,
    /// Automation sources (spec 0.2.0 §11); clips in the Playlist place
    /// them on the timeline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<AutomationLane>,
    /// Frames per second of audio clip positions (spec 0.2.0 §2).
    #[serde(default = "default_timebase")]
    pub timebase_rate: u32,
    /// Imported audio files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<AudioAsset>,
    /// Mixer track for the audio channels of recorded takes (spec 0.2.2
    /// REC-003).
    #[serde(default, skip_serializing_if = "RecordTo::is_master")]
    pub record_to: RecordTo,
    pub ids: IdAllocator,
}

/// Where the channel of a recorded take plays (REC-003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordTo {
    #[default]
    Master,
    Track {
        track: TrackId,
    },
    /// A new track named after the input; the setting then points at it,
    /// so the next takes go there too.
    NewTrack,
}

impl RecordTo {
    pub fn is_master(&self) -> bool {
        *self == Self::Master
    }
}

fn default_timebase() -> u32 {
    DEFAULT_TIMEBASE
}

impl Project {
    /// An empty project with one pattern and the master track.
    pub fn new() -> Self {
        let mut ids = IdAllocator::default();
        let pattern = Pattern::new(PatternId(ids.next()), "Pattern 1");
        let playlist = Playlist::with_lanes(DEFAULT_LANES, || ids.next());
        Self {
            tempo_bpm: 130.0,
            channels: Vec::new(),
            patterns: vec![pattern],
            mixer: Mixer::default(),
            playlist,
            automation: Vec::new(),
            timebase_rate: DEFAULT_TIMEBASE,
            assets: Vec::new(),
            record_to: RecordTo::Master,
            ids,
        }
    }

    pub fn asset(&self, id: AssetId) -> Option<&AudioAsset> {
        self.assets.iter().find(|a| a.id == id)
    }

    /// The asset an audio clip plays: the one of its channel.
    pub fn clip_asset(&self, clip: &AudioClip) -> Option<&AudioAsset> {
        self.channel(clip.channel)?
            .audio_asset()
            .and_then(|a| self.asset(a))
    }

    /// Audio channels of `asset`, in Channel Rack order.
    pub fn audio_channels_of(&self, asset: AssetId) -> impl Iterator<Item = &Channel> {
        self.channels
            .iter()
            .filter(move |c| c.audio_asset() == Some(asset))
    }

    /// Playlist clips of channel `id` (its instances).
    pub fn channel_clips(&self, id: ChannelId) -> impl Iterator<Item = &AudioClip> {
        self.playlist
            .audio_clips
            .iter()
            .filter(move |c| c.channel == id)
    }

    /// `[start, end)` of an audio clip in (fractional) ticks at the
    /// current tempo.
    pub fn audio_clip_ticks(&self, clip: &AudioClip) -> Option<(f64, f64)> {
        let asset = self.clip_asset(clip)?;
        let fpt = frames_per_tick(self.tempo_bpm, self.timebase_rate);
        let (a, b) = clip.interval(asset, self.timebase_rate);
        Some((a as f64 / fpt, b as f64 / fpt))
    }

    /// Right edge of the arrangement in ticks: pattern and audio clips.
    pub fn arrangement_end_ticks(&self) -> u64 {
        let audio = self
            .playlist
            .audio_clips
            .iter()
            .filter_map(|c| self.audio_clip_ticks(c))
            .map(|(_, b)| b.ceil() as u64)
            .max()
            .unwrap_or(0);
        self.playlist.arrangement_end().max(audio)
    }

    /// Audio clips and samplers using `asset` (spec 0.2.0 §10).
    pub fn asset_usage(&self, asset: AssetId) -> usize {
        self.playlist
            .audio_clips
            .iter()
            .filter(|c| {
                self.channel(c.channel)
                    .is_some_and(|ch| ch.audio_asset() == Some(asset))
            })
            .count()
    }

    pub fn automation_lane(&self, id: AutomationId) -> Option<&AutomationLane> {
        self.automation.iter().find(|l| l.id == id)
    }

    /// What deleting channel `id` would take with it: patterns where it
    /// has active steps or notes, and automation lanes of its instrument.
    pub fn channel_usage(&self, id: ChannelId) -> (usize, usize) {
        let patterns = self
            .patterns
            .iter()
            .filter(|p| {
                p.row(id)
                    .is_some_and(|r| r.steps.iter().any(|s| s.on) || !r.notes.is_empty())
            })
            .count();
        let target = PluginTarget::Channel(id);
        let lanes = self
            .automation
            .iter()
            .filter(|l| matches!(&l.target, AutomationTarget::PluginParam { target: t, .. } if *t == target))
            .count();
        (patterns, lanes)
    }

    /// Automation clips using source `id`.
    pub fn automation_usage(&self, id: AutomationId) -> usize {
        self.playlist
            .automation_clips
            .iter()
            .filter(|c| c.source == id)
            .count()
    }

    /// First pair of overlapping automation clips with the same target,
    /// as a message (spec 0.2.0 §11: such placement is rejected).
    pub fn automation_overlap(&self) -> Option<String> {
        let mut spans: Vec<(&AutomationTarget, u64, u64, &str)> = self
            .playlist
            .automation_clips
            .iter()
            .filter_map(|c| {
                let s = self.automation_lane(c.source)?;
                let (a, b) = c.interval();
                Some((&s.target, a, b, s.name.as_str()))
            })
            .collect();
        spans.sort();
        spans.windows(2).find_map(|w| {
            (w[0].0 == w[1].0 && w[1].1 < w[0].2).then(|| {
                format!(
                    "automation clips of one parameter may not overlap ({} and {})",
                    w[0].3, w[1].3
                )
            })
        })
    }

    /// Format v12 migration (spec 0.2.0 §13, plan D1): sources of length
    /// 0 came from timeline lanes. Each becomes a source over the whole
    /// arrangement plus a bar, placed by one clip at the song start on a
    /// new lane, so playback stays the same within the arrangement.
    pub fn adopt_timeline_automation(&mut self) {
        let end = self.arrangement_end_ticks();
        for i in 0..self.automation.len() {
            if self.automation[i].length > 0 {
                continue;
            }
            let last = self.automation[i].points.last().map_or(0, |p| p.tick);
            let length = (last.max(end) + 4 * u64::from(PPQ)).min(MAX_AUTOMATION_TICKS);
            let n = i + 1;
            let source = &mut self.automation[i];
            source.length = length;
            if source.name.is_empty() {
                source.name = format!("Automation {n}");
            }
            let (id, name) = (source.id, source.name.clone());
            let lane = LaneId(self.ids.next());
            let colors = &LANE_COLORS;
            self.playlist.lanes.push(Lane {
                id: lane,
                name,
                color: colors[self.playlist.lanes.len() % colors.len()],
                muted: false,
                solo: false,
            });
            let clip = ClipId(self.ids.next());
            self.playlist.automation_clips.push(AutomationClip {
                id: clip,
                lane,
                source: id,
                start: 0,
                offset: 0,
                duration: length,
                muted: false,
            });
        }
    }

    /// The lane controlling `target`, if any (one lane per target).
    pub fn lane_for(&self, target: &AutomationTarget) -> Option<&AutomationLane> {
        self.automation.iter().find(|l| l.target == *target)
    }

    /// Checks automation sources. Sources whose target is gone are kept
    /// (AUT-050); they are reported by [`AutomationTarget::exists`].
    pub fn validate_automation(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeSet::new();
        for l in &self.automation {
            if !seen.insert(l.id) {
                return Err(format!("duplicate automation ID {}", l.id.0));
            }
            l.validate()?;
        }
        Ok(())
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

    /// Checks patterns and the playlist (references, ranges, IDs).
    pub fn validate_arrangement(&self) -> Result<(), String> {
        if self.patterns.is_empty() {
            return Err("a project needs at least one pattern".into());
        }
        for pat in &self.patterns {
            if pat.length_steps == 0 || pat.length_steps > MAX_PATTERN_STEPS {
                return Err(format!("pattern {:?} has invalid length", pat.id));
            }
            for row in &pat.rows {
                if row.steps.len() != pat.length_steps as usize {
                    return Err(format!("pattern {:?} row length mismatch", pat.id));
                }
                if row.notes.len() > MAX_NOTES_PER_ROW {
                    return Err(format!("pattern {:?} has too many notes", pat.id));
                }
                for n in &row.notes {
                    n.validate()
                        .map_err(|e| format!("pattern {:?}: {e}", pat.id))?;
                }
            }
        }
        self.playlist.validate(&self.patterns)?;
        if self.timebase_rate == 0 {
            return Err("project timebase is zero".into());
        }
        let mut assets = std::collections::BTreeSet::new();
        for a in &self.assets {
            if !assets.insert(a.id) {
                return Err(format!("duplicate asset ID {}", a.id.0));
            }
            a.validate()?;
        }
        let mut ids: std::collections::BTreeSet<u64> =
            self.playlist.clips.iter().map(|c| c.id.0).collect();
        for c in &self.playlist.audio_clips {
            if !ids.insert(c.id.0) {
                return Err(format!("duplicate clip ID {}", c.id.0));
            }
            if self.playlist.lane(c.lane).is_none() {
                return Err(format!("audio clip {} is on a missing lane", c.id.0));
            }
            // ACH-015: every audio clip is an instance of an audio channel.
            let channel = self
                .channel(c.channel)
                .filter(|ch| ch.is_audio())
                .ok_or_else(|| format!("audio clip {} has no audio channel", c.id.0))?;
            let asset = channel
                .audio_asset()
                .and_then(|a| self.asset(a))
                .ok_or_else(|| format!("audio clip {} references a missing file", c.id.0))?;
            c.validate(asset)?;
        }
        for c in &self.playlist.automation_clips {
            if !ids.insert(c.id.0) {
                return Err(format!("duplicate clip ID {}", c.id.0));
            }
            if self.playlist.lane(c.lane).is_none() {
                return Err(format!("automation clip {} is on a missing lane", c.id.0));
            }
            let source = self
                .automation_lane(c.source)
                .ok_or_else(|| format!("automation clip {} references a missing source", c.id.0))?;
            if c.duration == 0 || c.offset + c.duration > source.length.max(1) {
                return Err(format!(
                    "automation clip {} reaches past its source",
                    c.id.0
                ));
            }
            if c.start > MAX_START_TICKS {
                return Err(format!("automation clip {} starts too late", c.id.0));
            }
        }
        if let Some(e) = self.automation_overlap() {
            return Err(e);
        }
        // ACH-003: audio channels have a file and no steps or notes.
        for ch in &self.channels {
            let Some(asset) = ch.audio_asset() else {
                continue;
            };
            if self.asset(asset).is_none() {
                return Err(format!(
                    "audio channel {:?} references a missing file",
                    ch.name
                ));
            }
            let used = self.patterns.iter().any(|p| {
                p.row(ch.id)
                    .is_some_and(|r| r.steps.iter().any(|s| s.on) || !r.notes.is_empty())
            });
            if used {
                return Err(format!(
                    "audio channel {:?} cannot have steps or notes",
                    ch.name
                ));
            }
        }
        Ok(())
    }

    /// Checks references from channels into the mixer and the routing
    /// graph (MIX-002).
    pub fn validate_routing(&self) -> Result<(), RoutingError> {
        for c in &self.channels {
            if !self.mixer.has_track(c.track) {
                return Err(RoutingError::UnknownTrack(c.track));
            }
        }
        if let RecordTo::Track { track } = self.record_to
            && !self.mixer.has_track(track)
        {
            return Err(RoutingError::UnknownTrack(track));
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
    /// Display color; `None` = from [`PALETTE`] by position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    /// Transposition in semitones applied to every note.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub pitch: i32,
    /// Most notes sounding at once; 0 = no limit (up to the engine's).
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub polyphony: u16,
    /// A new note cuts the sounding one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mono: bool,
    /// Sample playback settings; ignored by instrument channels.
    #[serde(default, skip_serializing_if = "SamplerSettings::is_default")]
    pub sampler: SamplerSettings,
}

fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

/// Transposition range of a channel, semitones.
pub const MAX_CHANNEL_PITCH: i32 = 48;
/// Polyphony limit range (0 = no limit).
pub const MAX_POLYPHONY: u16 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    /// Plays to the sample end (or the end marker), ignoring note length
    /// unless the envelope is on.
    #[default]
    OneShot,
    /// Repeats the loop region while the note is held, then releases.
    Loop,
}

/// Sample playback of a sampler channel (spec "DAW UI clean room", §12).
/// Positions are fractions of the sample length, so they stay valid when
/// the sample is replaced or resampled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SamplerSettings {
    /// Key at which the sample plays at its original speed.
    pub root_key: u8,
    /// Extra gain in dB, `-24.0..=12.0`.
    pub gain_db: f32,
    /// Start and end markers, `0.0 <= start < end <= 1.0`.
    pub start: f32,
    pub end: f32,
    pub mode: PlaybackMode,
    /// Loop region inside `start..end`, used in [`PlaybackMode::Loop`].
    pub loop_start: f32,
    pub loop_end: f32,
    /// ADSR on; off plays the sample as is (one-shot ignores note length).
    pub envelope: bool,
    pub attack_ms: f32,
    pub decay_ms: f32,
    /// Sustain level `0.0..=1.0`.
    pub sustain: f32,
    pub release_ms: f32,
}

/// Longest envelope stage, ms.
pub const MAX_ENVELOPE_MS: f32 = 10_000.0;

impl Default for SamplerSettings {
    fn default() -> Self {
        Self {
            root_key: ROOT_KEY,
            gain_db: 0.0,
            start: 0.0,
            end: 1.0,
            mode: PlaybackMode::OneShot,
            loop_start: 0.0,
            loop_end: 1.0,
            envelope: false,
            attack_ms: 2.0,
            decay_ms: 120.0,
            sustain: 0.7,
            release_ms: 80.0,
        }
    }
}

impl SamplerSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Checks ranges and marker order.
    pub fn validate(&self) -> Result<(), String> {
        let unit = |v: f32| (0.0..=1.0).contains(&v);
        let ms = |v: f32| (0.0..=MAX_ENVELOPE_MS).contains(&v);
        if self.root_key > 127 {
            return Err("root key out of range".into());
        }
        if !(-24.0..=12.0).contains(&self.gain_db) {
            return Err("sampler gain out of range".into());
        }
        if !(unit(self.start) && unit(self.end) && self.start < self.end) {
            return Err("sample start must be before the end".into());
        }
        if !(unit(self.loop_start) && unit(self.loop_end) && self.loop_start < self.loop_end) {
            return Err("loop start must be before the loop end".into());
        }
        if !(ms(self.attack_ms) && ms(self.decay_ms) && ms(self.release_ms) && unit(self.sustain)) {
            return Err("envelope value out of range".into());
        }
        Ok(())
    }
}

/// Default colors for channels and mixer tracks, used in order.
pub const PALETTE: [[u8; 3]; 8] = [
    [0xC9, 0x77, 0x4F],
    [0x4F, 0x8F, 0xC9],
    [0x5F, 0xA8, 0x6B],
    [0xB5, 0x65, 0xA7],
    [0xC9, 0xA9, 0x4F],
    [0x4F, 0xB3, 0xAE],
    [0xC9, 0x6F, 0x6F],
    [0x8F, 0x8F, 0xD6],
];

/// The explicit color or the palette entry for position `index`.
pub fn color_or_default(color: Option<[u8; 3]>, index: usize) -> [u8; 3] {
    color.unwrap_or(PALETTE[index % PALETTE.len()])
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
            color: None,
            pitch: 0,
            polyphony: 0,
            mono: false,
            sampler: SamplerSettings::default(),
        }
    }

    /// An audio channel: the Playlist clips of `asset` play through it
    /// (spec 0.2.2 ACH-001). Unity volume and centered balance, so a clip
    /// sounds as it did before channels existed.
    pub fn new_audio(id: ChannelId, name: impl Into<String>, asset: AssetId) -> Self {
        Self {
            source: ChannelSource::Audio { asset },
            volume: 1.0,
            ..Self::new_sampler(id, name)
        }
    }

    /// A channel played by a plugin instrument (PLG-001).
    pub fn new_instrument(id: ChannelId, name: impl Into<String>, plugin: PluginSlot) -> Self {
        Self {
            source: ChannelSource::Plugin { plugin },
            ..Self::new_sampler(id, name)
        }
    }

    pub fn sample(&self) -> Option<&SampleRef> {
        match &self.source {
            ChannelSource::Sampler { sample } => sample.as_ref(),
            ChannelSource::Plugin { .. } | ChannelSource::Audio { .. } => None,
        }
    }

    pub fn instrument(&self) -> Option<&PluginSlot> {
        match &self.source {
            ChannelSource::Plugin { plugin } => Some(plugin),
            ChannelSource::Sampler { .. } | ChannelSource::Audio { .. } => None,
        }
    }

    /// The asset of an audio channel.
    pub fn audio_asset(&self) -> Option<AssetId> {
        match self.source {
            ChannelSource::Audio { asset } => Some(asset),
            ChannelSource::Sampler { .. } | ChannelSource::Plugin { .. } => None,
        }
    }

    pub fn is_audio(&self) -> bool {
        self.audio_asset().is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelSource {
    Sampler {
        sample: Option<SampleRef>,
    },
    /// A VST3 instrument; notes come from steps and the piano roll.
    Plugin {
        plugin: PluginSlot,
    },
    /// The Playlist clips of one audio asset (spec 0.2.2 ACH-001); no
    /// steps or notes.
    Audio {
        asset: AssetId,
    },
}

/// What a plugin instance belongs to: an insert slot or an instrument
/// channel. Keys the host's live instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginTarget {
    Insert(InsertId),
    Channel(ChannelId),
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
    /// Piano-roll notes of this channel, sorted by `(start, key)`. Steps
    /// and notes both play.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

impl StepRow {
    pub fn new(channel: ChannelId, length: u32) -> Self {
        Self {
            channel,
            steps: vec![Step::default(); length as usize],
            notes: Vec::new(),
        }
    }
}

/// A piano-roll note. Times are in ticks relative to the pattern start.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// Stable within its row (spec 0.2.0 §18.2: edits keep it, new notes
    /// get a new one); 0 = not assigned yet ([`assign_note_ids`]).
    #[serde(default)]
    pub id: u32,
    pub start: u32,
    /// `>= 1` tick.
    pub length: u32,
    /// MIDI key `0..=127`; 60 is C5.
    pub key: u8,
    /// `0.0..=1.0`.
    pub velocity: f32,
}

impl Note {
    pub fn end(&self) -> u32 {
        self.start.saturating_add(self.length)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.key > 127 {
            return Err(format!("note key {} out of range", self.key));
        }
        if self.length == 0 {
            return Err("note length must be at least one tick".into());
        }
        if !(0.0..=1.0).contains(&self.velocity) {
            return Err(format!("note velocity {} out of range", self.velocity));
        }
        if self.start > MAX_PATTERN_STEPS * TICKS_PER_STEP {
            return Err("note starts after the longest pattern".into());
        }
        Ok(())
    }
}

/// Sorts notes into canonical order (`start`, then `key`, then ID).
pub fn sort_notes(notes: &mut [Note]) {
    notes.sort_by_key(|n| (n.start, n.key, n.id));
}

/// Gives notes without an ID (0) or with a repeated one a new ID above
/// the row's largest; the first note with an ID keeps it.
pub fn assign_note_ids(notes: &mut [Note]) {
    let mut seen = std::collections::BTreeSet::new();
    let mut next = notes.iter().map(|n| n.id).max().unwrap_or(0);
    for n in notes.iter_mut() {
        if n.id == 0 || !seen.insert(n.id) {
            next += 1;
            n.id = next;
            seen.insert(next);
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
