// SPDX-License-Identifier: GPL-3.0-or-later
//! All document changes go through [`Edit`]s executed by a [`History`].
//!
//! Every edit, when applied, returns its exact inverse; undo applies the
//! inverse and redo applies the inverse of the inverse. Dirty state is
//! tracked by revision IDs so that undoing back to the saved state clears it.

pub mod audio_channels;
mod mixer;
pub mod note_tools;
pub mod notes;
mod playlist;

pub use mixer::{add_insert, add_send, add_track};
pub use playlist::{
    Edge, add_clip, add_lane, add_pattern, audio_clip_edge, duplicate_clips, make_unique,
    new_lane_edit, pattern_clip_edge, slice_audio_clip, slice_pattern_clip, steps_beyond,
};

use jdance_domain::{
    AssetId, AudioAsset, AudioClip, AutoPoint, AutomationClip, AutomationId, AutomationLane,
    AutomationTarget, Channel, ChannelId, ClipId, Curve, Effect, InsertId, Lane, LaneId,
    LimiterSettings, MAX_CHANNEL_PITCH, MAX_CHANNELS, MAX_NOTES_PER_ROW, MAX_POLYPHONY,
    MAX_TEMPO_BPM, MAX_VOLUME, MIN_TEMPO_BPM, Mixer, MixerTrack, Note, Pattern, PatternClip,
    PatternId, Playlist, Project, RoutingError, SampleRef, SamplerSettings, SendId, Sidechain,
    Step, StepRow, TrackId, sort_notes, sort_points,
};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum EditError {
    #[error("channel {0:?} does not exist")]
    NoSuchChannel(ChannelId),
    #[error("pattern {0:?} does not exist")]
    NoSuchPattern(PatternId),
    #[error("step index {index} is outside pattern length {length}")]
    StepOutOfRange { index: u32, length: u32 },
    #[error("channel {0:?} already exists")]
    DuplicateChannel(ChannelId),
    #[error("value {0} is out of range")]
    OutOfRange(f64),
    #[error("too many channels (limit {MAX_CHANNELS})")]
    TooManyChannels,
    #[error("{0}")]
    Routing(RoutingError),
    #[error("mixer object {0} does not exist")]
    NoSuchMixerObject(u64),
    #[error("{0}")]
    Arrangement(String),
    /// Shortening a pattern would remove active steps; retry with
    /// `truncate: true` after the user confirms.
    #[error("shortening the pattern would remove {0} active step(s)")]
    WouldDropSteps(usize),
}

/// A reversible change to a [`Project`].
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    SetTempo(f64),
    /// Project time signature (spec 0.2.3 SIG-011).
    SetTimeSignature(jdance_domain::TimeSignature),
    /// Mixer track for the channels of recorded takes (REC-003).
    SetRecordTo(jdance_domain::RecordTo),
    SetStep {
        pattern: PatternId,
        channel: ChannelId,
        index: u32,
        step: Step,
    },
    /// Replaces the piano-roll notes of `channel` in `pattern` (sorted on
    /// apply). One drag is one gesture and one undo step.
    SetNotes {
        pattern: PatternId,
        channel: ChannelId,
        notes: Vec<Note>,
    },
    /// Inserts `channel` at `index` (clamped) and restores its step rows.
    AddChannel {
        channel: Channel,
        index: usize,
        rows: Vec<(PatternId, StepRow)>,
        /// Automation lanes of the channel's instrument (restored by undo).
        automation: Vec<AutomationLane>,
    },
    RemoveChannel(ChannelId),
    SetChannelVolume(ChannelId, f32),
    SetChannelPan(ChannelId, f32),
    SetChannelMuted(ChannelId, bool),
    RenameChannel(ChannelId, String),
    /// `None` = palette color by position.
    SetChannelColor(ChannelId, Option<[u8; 3]>),
    /// Moves a channel to `index` (clamped) in the rack.
    MoveChannel(ChannelId, usize),
    /// Transposition, polyphony limit and mono mode of a channel.
    SetChannelPlayback {
        channel: ChannelId,
        pitch: i32,
        polyphony: u16,
        mono: bool,
    },
    /// Sample playback settings (markers, loop, envelope, root key).
    SetSampler(ChannelId, SamplerSettings),
    SetChannelSample(ChannelId, Option<SampleRef>),
    SetMasterVolume(f32),
    SetChannelTrack(ChannelId, TrackId),

    // Mixer parameters (cheap inverses, mergeable).
    SetTrackVolume(TrackId, f32),
    SetTrackPan(TrackId, f32),
    SetTrackMuted(TrackId, bool),
    SetTrackSolo(TrackId, bool),
    RenameTrack(TrackId, String),
    SetTrackColor(TrackId, Option<[u8; 3]>),
    SetSendLevel(SendId, f32),
    SetInsertBypass(InsertId, bool),
    SetInsertEffect(InsertId, Effect),

    // Mixer structure: validated against the whole routing graph; the
    // inverse is a [`Edit::RestoreMixer`] snapshot.
    AddTrack {
        track: MixerTrack,
        index: usize,
    },
    RemoveTrack(TrackId),
    SetTrackMainOut(TrackId, TrackId),
    AddSend {
        track: TrackId,
        send: jdance_domain::Send,
    },
    RemoveSend(SendId),
    SetSendPreFader(SendId, bool),
    AddInsert {
        track: TrackId,
        insert: jdance_domain::Insert,
        index: usize,
    },
    RemoveInsert(InsertId),
    /// Moves an insert to a new position within its track.
    MoveInsert(InsertId, usize),
    SetInsertSidechain(InsertId, Option<Sidechain>),
    SetLimiter(LimiterSettings),
    /// Several edits applied as one undo step, in order; all or nothing.
    Batch(Vec<Edit>),

    // Patterns and playlist.
    RenamePattern(PatternId, String),
    /// Replaces a pattern (same ID) wholesale; inverse of length changes.
    ReplacePattern(Pattern),
    /// Keeps clips' repeat counts. Shortening with active steps beyond
    /// the new length fails unless `truncate` (spec: no silent deletion).
    SetPatternLength {
        pattern: PatternId,
        steps: u32,
        truncate: bool,
    },
    AddPattern {
        pattern: Pattern,
        index: usize,
    },
    /// Also removes the pattern's clips.
    RemovePattern(PatternId),
    AddLane {
        lane: Lane,
        index: usize,
    },
    /// Also removes the lane's clips.
    RemoveLane(LaneId),
    RenameLane(LaneId, String),
    SetLaneColor(LaneId, [u8; 3]),
    SetLaneMuted(LaneId, bool),
    SetLaneSolo(LaneId, bool),
    AddClip(PatternClip),
    RemoveClip(ClipId),
    MoveClip {
        id: ClipId,
        start: u64,
        lane: LaneId,
    },
    /// Start, visible pattern range and looping of a pattern clip
    /// (resize from either edge, loop on/off).
    SetClipShape {
        id: ClipId,
        start: u64,
        offset: u64,
        duration: u64,
        looped: bool,
    },
    /// Imports an audio file into the project.
    AddAsset(AudioAsset),
    /// Removes an unused asset (the file on disk stays).
    RemoveAsset(AssetId),
    RenameAsset(AssetId, String),
    AddAudioClip(AudioClip),
    RemoveAudioClip(ClipId),
    AddAutomationClip(AutomationClip),
    RemoveAutomationClip(ClipId),
    /// Replaces an automation clip (move, resize, mute).
    SetAutomationClip(AutomationClip),
    /// Replaces an audio clip by ID (move, trim, gain, mute, fades).
    SetAudioClip(AudioClip),
    SetClipMuted(ClipId, bool),
    /// Inserts `pattern` (a copy with a new ID) and points `clip` at it.
    MakeUnique {
        clip: ClipId,
        pattern: Pattern,
    },
    SetLoop {
        range: Option<(u64, u64)>,
        enabled: bool,
    },
    /// Restores patterns and playlist (inverse of structural edits).
    RestoreArrangement {
        patterns: Vec<Pattern>,
        playlist: Box<Playlist>,
    },
    /// Restores the mixer and the channel → track assignments.
    RestoreMixer {
        mixer: Box<Mixer>,
        channel_tracks: Vec<(ChannelId, TrackId)>,
        automation: Vec<AutomationLane>,
        record_to: jdance_domain::RecordTo,
    },
    /// Adds (`lane: Some`, new id), replaces or removes (`None`) an
    /// automation source. Points are sorted on apply; a source used by
    /// clips cannot be removed.
    SetAutomation {
        id: AutomationId,
        lane: Option<AutomationLane>,
    },
}

/// Edits with equal merge keys executed back-to-back inside one gesture
/// (e.g. a slider drag) collapse into a single undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeKey {
    Tempo,
    ChannelVolume(ChannelId),
    ChannelPan(ChannelId),
    MasterVolume,
    TrackVolume(TrackId),
    TrackPan(TrackId),
    SendLevel(SendId),
    InsertEffect(InsertId),
    Limiter,
    Notes(PatternId, ChannelId),
    ChannelPlayback(ChannelId),
    Sampler(ChannelId),
    Automation(AutomationId),
    /// An audio or automation clip.
    Clip(ClipId),
}

impl Edit {
    pub fn label(&self) -> &'static str {
        match self {
            Edit::SetTempo(_) => "Change tempo",
            Edit::SetTimeSignature(_) => "Change time signature",
            Edit::SetRecordTo(_) => "Change recording track",
            Edit::SetStep { .. } => "Edit step",
            Edit::SetNotes { .. } => "Edit notes",
            Edit::AddChannel { .. } => "Add channel",
            Edit::RemoveChannel(_) => "Remove channel",
            Edit::SetChannelVolume(..) => "Change channel volume",
            Edit::SetChannelPan(..) => "Change channel pan",
            Edit::SetChannelMuted(..) => "Mute channel",
            Edit::RenameChannel(..) => "Rename channel",
            Edit::SetChannelColor(..) => "Change channel color",
            Edit::MoveChannel(..) => "Move channel",
            Edit::SetChannelPlayback { .. } => "Change channel settings",
            Edit::SetSampler(..) => "Change sampler",
            Edit::SetChannelSample(..) => "Change sample",
            Edit::SetMasterVolume(_) => "Change master volume",
            Edit::SetChannelTrack(..) => "Route channel",
            Edit::SetTrackVolume(..) => "Change track volume",
            Edit::SetTrackPan(..) => "Change track pan",
            Edit::SetTrackMuted(..) => "Mute track",
            Edit::SetTrackSolo(..) => "Solo track",
            Edit::RenameTrack(..) => "Rename track",
            Edit::SetTrackColor(..) => "Change track color",
            Edit::SetSendLevel(..) => "Change send level",
            Edit::SetInsertBypass(..) => "Bypass effect",
            Edit::SetInsertEffect(..) => "Change effect",
            Edit::AddTrack { .. } => "Add mixer track",
            Edit::RemoveTrack(_) => "Remove mixer track",
            Edit::SetTrackMainOut(..) => "Route track",
            Edit::AddSend { .. } => "Add send",
            Edit::RemoveSend(_) => "Remove send",
            Edit::SetSendPreFader(..) => "Change send mode",
            Edit::AddInsert { .. } => "Add effect",
            Edit::RemoveInsert(_) => "Remove effect",
            Edit::MoveInsert(..) => "Reorder effects",
            Edit::SetLimiter(_) => "Change limiter",
            Edit::Batch(_) => "Edit",
            Edit::RenamePattern(..) => "Rename pattern",
            Edit::ReplacePattern(_) | Edit::SetPatternLength { .. } => "Change pattern length",
            Edit::AddPattern { .. } => "Add pattern",
            Edit::RemovePattern(_) => "Remove pattern",
            Edit::AddLane { .. } => "Add lane",
            Edit::RemoveLane(_) => "Remove lane",
            Edit::RenameLane(..) => "Rename lane",
            Edit::SetLaneColor(..) => "Change lane color",
            Edit::SetLaneMuted(..) => "Mute lane",
            Edit::SetLaneSolo(..) => "Solo lane",
            Edit::AddClip(_) => "Add clip",
            Edit::RemoveClip(_) => "Delete clip",
            Edit::MoveClip { .. } => "Move clip",
            Edit::SetClipShape { .. } => "Resize clip",
            Edit::AddAsset(_) => "Import audio",
            Edit::RemoveAsset(_) => "Remove audio file",
            Edit::RenameAsset(..) => "Rename audio file",
            Edit::AddAudioClip(_) => "Add audio clip",
            Edit::RemoveAudioClip(_) => "Delete audio clip",
            Edit::SetAudioClip(_) => "Edit audio clip",
            Edit::AddAutomationClip(_) => "Add automation clip",
            Edit::RemoveAutomationClip(_) => "Delete automation clip",
            Edit::SetAutomationClip(_) => "Edit automation clip",
            Edit::SetClipMuted(..) => "Mute clip",
            Edit::MakeUnique { .. } => "Make unique",
            Edit::SetLoop { .. } => "Change loop",
            Edit::RestoreArrangement { .. } => "Change arrangement",
            Edit::SetInsertSidechain(..) => "Change sidechain",
            Edit::RestoreMixer { .. } => "Change mixer",
            Edit::SetAutomation { lane: None, .. } => "Remove automation",
            Edit::SetAutomation { .. } => "Edit automation",
        }
    }

    pub fn merge_key(&self) -> Option<MergeKey> {
        match self {
            Edit::SetTempo(_) => Some(MergeKey::Tempo),
            Edit::SetAudioClip(c) => Some(MergeKey::Clip(c.id)),
            Edit::SetAutomationClip(c) => Some(MergeKey::Clip(c.id)),
            Edit::SetChannelVolume(id, _) => Some(MergeKey::ChannelVolume(*id)),
            Edit::SetChannelPan(id, _) => Some(MergeKey::ChannelPan(*id)),
            Edit::SetMasterVolume(_) => Some(MergeKey::MasterVolume),
            Edit::SetTrackVolume(id, _) => Some(MergeKey::TrackVolume(*id)),
            Edit::SetTrackPan(id, _) => Some(MergeKey::TrackPan(*id)),
            Edit::SetSendLevel(id, _) => Some(MergeKey::SendLevel(*id)),
            Edit::SetInsertEffect(id, _) => Some(MergeKey::InsertEffect(*id)),
            Edit::SetLimiter(_) => Some(MergeKey::Limiter),
            Edit::SetNotes {
                pattern, channel, ..
            } => Some(MergeKey::Notes(*pattern, *channel)),
            Edit::SetAutomation { id, lane: Some(_) } => Some(MergeKey::Automation(*id)),
            Edit::SetChannelPlayback { channel, .. } => Some(MergeKey::ChannelPlayback(*channel)),
            Edit::SetSampler(id, _) => Some(MergeKey::Sampler(*id)),
            _ => None,
        }
    }

    /// Applies the edit and returns its inverse. On error the project is
    /// left unchanged.
    pub fn apply(&self, p: &mut Project) -> Result<Edit, EditError> {
        match self {
            Edit::SetRecordTo(to) => {
                if let jdance_domain::RecordTo::Track { track } = to
                    && !p.mixer.has_track(*track)
                {
                    return Err(EditError::Routing(RoutingError::UnknownTrack(*track)));
                }
                Ok(Edit::SetRecordTo(std::mem::replace(&mut p.record_to, *to)))
            }
            Edit::SetTempo(bpm) => {
                if !(MIN_TEMPO_BPM..=MAX_TEMPO_BPM).contains(bpm) {
                    return Err(EditError::OutOfRange(*bpm));
                }
                let old = std::mem::replace(&mut p.tempo_bpm, *bpm);
                Ok(Edit::SetTempo(old))
            }
            Edit::SetTimeSignature(sig) => {
                sig.validate().map_err(EditError::Arrangement)?;
                let old = std::mem::replace(&mut p.time_signature, *sig);
                Ok(Edit::SetTimeSignature(old))
            }
            Edit::SetStep {
                pattern,
                channel,
                index,
                step,
            } => {
                if p.channel(*channel).is_none() {
                    return Err(EditError::NoSuchChannel(*channel));
                }
                let pat = p
                    .pattern_mut(*pattern)
                    .ok_or(EditError::NoSuchPattern(*pattern))?;
                if *index >= pat.length_steps {
                    return Err(EditError::StepOutOfRange {
                        index: *index,
                        length: pat.length_steps,
                    });
                }
                let slot = &mut pat.row_mut(*channel).steps[*index as usize];
                let old = std::mem::replace(slot, sanitize_step(*step));
                Ok(Edit::SetStep {
                    pattern: *pattern,
                    channel: *channel,
                    index: *index,
                    step: old,
                })
            }
            Edit::SetAutomation { id, lane } => {
                let at = p.automation.iter().position(|l| l.id == *id);
                let old = at.map(|i| p.automation[i].clone());
                match lane {
                    Some(new) => {
                        let mut new = new.clone();
                        new.id = *id;
                        sort_points(&mut new.points);
                        new.validate().map_err(EditError::Arrangement)?;
                        match at {
                            Some(i) => p.automation[i] = new,
                            None => p.automation.push(new),
                        }
                        // A shorter source or a new target must still fit
                        // its clips.
                        if let Err(e) = p.validate_arrangement() {
                            match (at, old.clone()) {
                                (Some(i), Some(o)) => p.automation[i] = o,
                                _ => {
                                    p.automation.pop();
                                }
                            }
                            return Err(EditError::Arrangement(e));
                        }
                    }
                    None => {
                        let used = p.automation_usage(*id);
                        if used > 0 {
                            return Err(EditError::Arrangement(format!(
                                "the automation is used by {used} clip(s)"
                            )));
                        }
                        if let Some(i) = at {
                            p.automation.remove(i);
                        }
                    }
                }
                Ok(Edit::SetAutomation { id: *id, lane: old })
            }
            Edit::SetNotes {
                pattern,
                channel,
                notes,
            } => {
                if p.channel(*channel).is_none() {
                    return Err(EditError::NoSuchChannel(*channel));
                }
                if notes.len() > MAX_NOTES_PER_ROW {
                    return Err(EditError::OutOfRange(notes.len() as f64));
                }
                for n in notes {
                    n.validate().map_err(EditError::Arrangement)?;
                }
                let pat = p
                    .pattern_mut(*pattern)
                    .ok_or(EditError::NoSuchPattern(*pattern))?;
                let mut sorted = notes.clone();
                jdance_domain::assign_note_ids(&mut sorted);
                sort_notes(&mut sorted);
                let old = std::mem::replace(&mut pat.row_mut(*channel).notes, sorted);
                Ok(Edit::SetNotes {
                    pattern: *pattern,
                    channel: *channel,
                    notes: old,
                })
            }
            Edit::AddChannel {
                channel,
                index,
                rows,
                automation,
            } => {
                if p.channel(channel.id).is_some() {
                    return Err(EditError::DuplicateChannel(channel.id));
                }
                if p.channels.len() >= MAX_CHANNELS {
                    return Err(EditError::TooManyChannels);
                }
                if !p.mixer.has_track(channel.track) {
                    return Err(EditError::Routing(RoutingError::UnknownTrack(
                        channel.track,
                    )));
                }
                if let Some(asset) = channel.audio_asset() {
                    if p.asset(asset).is_none() {
                        return Err(EditError::Arrangement(format!(
                            "audio channel {:?} references a missing file",
                            channel.name
                        )));
                    }
                    if !rows.is_empty() {
                        return Err(EditError::Arrangement(
                            "an audio channel has no steps or notes".into(),
                        ));
                    }
                }
                for (pid, _) in rows {
                    if p.pattern(*pid).is_none() {
                        return Err(EditError::NoSuchPattern(*pid));
                    }
                }
                let at = (*index).min(p.channels.len());
                p.channels.insert(at, channel.clone());
                for (pid, row) in rows {
                    let pat = p.pattern_mut(*pid).expect("checked above");
                    pat.rows.retain(|r| r.channel != channel.id);
                    pat.rows.push(row.clone());
                }
                p.automation.extend(automation.iter().cloned());
                Ok(Edit::RemoveChannel(channel.id))
            }
            Edit::RemoveChannel(id) => {
                let index = p.channel_index(*id).ok_or(EditError::NoSuchChannel(*id))?;
                // ACH-015: the clips go first (one batch), never orphaned.
                let clips = p.channel_clips(*id).count();
                if clips > 0 {
                    return Err(EditError::Arrangement(format!(
                        "the channel still has {clips} clip(s) in the Playlist"
                    )));
                }
                let channel = p.channels.remove(index);
                let mut rows = Vec::new();
                for pat in &mut p.patterns {
                    if let Some(i) = pat.rows.iter().position(|r| r.channel == *id) {
                        rows.push((pat.id, pat.rows.remove(i)));
                    }
                }
                // Automation of the channel's instrument stays, shown as
                // missing (AUT-050); undo reconnects it.
                Ok(Edit::AddChannel {
                    channel,
                    index,
                    rows,
                    automation: Vec::new(),
                })
            }
            Edit::SetChannelVolume(id, v) => {
                let v = check_range(*v, 0.0, MAX_VOLUME)?;
                let ch = p.channel_mut(*id).ok_or(EditError::NoSuchChannel(*id))?;
                Ok(Edit::SetChannelVolume(
                    *id,
                    std::mem::replace(&mut ch.volume, v),
                ))
            }
            Edit::SetChannelPan(id, v) => {
                let v = check_range(*v, -1.0, 1.0)?;
                let ch = p.channel_mut(*id).ok_or(EditError::NoSuchChannel(*id))?;
                Ok(Edit::SetChannelPan(*id, std::mem::replace(&mut ch.pan, v)))
            }
            Edit::SetChannelMuted(id, m) => {
                let ch = p.channel_mut(*id).ok_or(EditError::NoSuchChannel(*id))?;
                Ok(Edit::SetChannelMuted(
                    *id,
                    std::mem::replace(&mut ch.muted, *m),
                ))
            }
            Edit::RenameChannel(id, name) => {
                let ch = p.channel_mut(*id).ok_or(EditError::NoSuchChannel(*id))?;
                Ok(Edit::RenameChannel(
                    *id,
                    std::mem::replace(&mut ch.name, name.clone()),
                ))
            }
            Edit::SetChannelColor(id, color) => {
                let ch = p.channel_mut(*id).ok_or(EditError::NoSuchChannel(*id))?;
                Ok(Edit::SetChannelColor(
                    *id,
                    std::mem::replace(&mut ch.color, *color),
                ))
            }
            Edit::SetChannelPlayback {
                channel,
                pitch,
                polyphony,
                mono,
            } => {
                if pitch.abs() > MAX_CHANNEL_PITCH {
                    return Err(EditError::OutOfRange(f64::from(*pitch)));
                }
                if *polyphony > MAX_POLYPHONY {
                    return Err(EditError::OutOfRange(f64::from(*polyphony)));
                }
                let ch = p
                    .channel_mut(*channel)
                    .ok_or(EditError::NoSuchChannel(*channel))?;
                let old = Edit::SetChannelPlayback {
                    channel: *channel,
                    pitch: ch.pitch,
                    polyphony: ch.polyphony,
                    mono: ch.mono,
                };
                ch.pitch = *pitch;
                ch.polyphony = *polyphony;
                ch.mono = *mono;
                Ok(old)
            }
            Edit::SetSampler(id, settings) => {
                settings.validate().map_err(EditError::Arrangement)?;
                let ch = p.channel_mut(*id).ok_or(EditError::NoSuchChannel(*id))?;
                Ok(Edit::SetSampler(
                    *id,
                    std::mem::replace(&mut ch.sampler, *settings),
                ))
            }
            Edit::MoveChannel(id, index) => {
                let from = p.channel_index(*id).ok_or(EditError::NoSuchChannel(*id))?;
                let ch = p.channels.remove(from);
                let to = (*index).min(p.channels.len());
                p.channels.insert(to, ch);
                Ok(Edit::MoveChannel(*id, from))
            }
            Edit::SetChannelSample(id, sample) => {
                let ch = p.channel_mut(*id).ok_or(EditError::NoSuchChannel(*id))?;
                let jdance_domain::ChannelSource::Sampler { sample: slot } = &mut ch.source else {
                    return Err(EditError::Arrangement(
                        "only a sampler channel has a sample".into(),
                    ));
                };
                Ok(Edit::SetChannelSample(
                    *id,
                    std::mem::replace(slot, sample.clone()),
                ))
            }
            Edit::SetMasterVolume(v) => {
                let v = check_range(*v, 0.0, MAX_VOLUME)?;
                Ok(Edit::SetMasterVolume(std::mem::replace(
                    &mut p.mixer.master.volume,
                    v,
                )))
            }
            Edit::Batch(edits) => {
                let mut inverses = Vec::with_capacity(edits.len());
                for e in edits {
                    match e.apply(p) {
                        Ok(inv) => inverses.push(inv),
                        Err(err) => {
                            // Roll back what was applied so far.
                            for inv in inverses.iter().rev() {
                                inv.apply(p).expect("inverse of an applied edit applies");
                            }
                            return Err(err);
                        }
                    }
                }
                inverses.reverse();
                Ok(Edit::Batch(inverses))
            }
            Edit::RenamePattern(..)
            | Edit::ReplacePattern(_)
            | Edit::SetPatternLength { .. }
            | Edit::AddPattern { .. }
            | Edit::RemovePattern(_)
            | Edit::AddLane { .. }
            | Edit::RemoveLane(_)
            | Edit::RenameLane(..)
            | Edit::SetLaneColor(..)
            | Edit::SetLaneMuted(..)
            | Edit::SetLaneSolo(..)
            | Edit::AddClip(_)
            | Edit::RemoveClip(_)
            | Edit::MoveClip { .. }
            | Edit::SetClipShape { .. }
            | Edit::SetClipMuted(..)
            | Edit::AddAsset(_)
            | Edit::RemoveAsset(_)
            | Edit::RenameAsset(..)
            | Edit::AddAudioClip(_)
            | Edit::RemoveAudioClip(_)
            | Edit::SetAudioClip(_)
            | Edit::AddAutomationClip(_)
            | Edit::RemoveAutomationClip(_)
            | Edit::SetAutomationClip(_)
            | Edit::MakeUnique { .. }
            | Edit::SetLoop { .. }
            | Edit::RestoreArrangement { .. } => playlist::apply(self, p),
            other => mixer::apply(other, p),
        }
    }
}

pub(crate) fn check_range(v: f32, min: f32, max: f32) -> Result<f32, EditError> {
    if v.is_finite() && (min..=max).contains(&v) {
        Ok(v)
    } else {
        Err(EditError::OutOfRange(f64::from(v)))
    }
}

fn sanitize_step(mut s: Step) -> Step {
    s.velocity = if s.velocity.is_finite() {
        s.velocity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    s
}

#[derive(Debug)]
struct Entry {
    inverse: Edit,
    label: &'static str,
    merge: Option<MergeKey>,
    rev_before: u64,
    rev_after: u64,
}

/// Undo/redo stacks plus dirty tracking.
#[derive(Debug)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    current_rev: u64,
    saved_rev: u64,
    next_rev: u64,
    gesture_open: bool,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(1000)
    }
}

impl History {
    /// `limit` is the maximum number of undo steps kept.
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            current_rev: 0,
            saved_rev: 0,
            next_rev: 1,
            gesture_open: false,
            limit: limit.max(1),
        }
    }

    /// Applies `edit` as a new undo step.
    pub fn execute(&mut self, p: &mut Project, edit: Edit) -> Result<(), EditError> {
        self.gesture_open = false;
        self.push(p, edit, false)
    }

    /// Applies `edit`, merging it into the previous step if both carry the
    /// same [`MergeKey`] and no [`History::end_gesture`] happened in between.
    pub fn execute_merging(&mut self, p: &mut Project, edit: Edit) -> Result<(), EditError> {
        let merge = self.gesture_open
            && edit.merge_key().is_some()
            && self.undo.last().and_then(|e| e.merge) == edit.merge_key();
        self.push(p, edit, merge)?;
        self.gesture_open = true;
        Ok(())
    }

    /// Closes the current merge gesture (e.g. on slider release).
    /// Labels of the undoable edits, most recent first.
    pub fn undo_labels(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.undo.iter().rev().map(|e| e.label)
    }

    pub fn end_gesture(&mut self) {
        self.gesture_open = false;
    }

    fn push(&mut self, p: &mut Project, edit: Edit, merge: bool) -> Result<(), EditError> {
        let inverse = edit.apply(p)?;
        let rev = self.alloc_rev();
        self.redo.clear();
        if merge {
            let last = self.undo.last_mut().expect("merge requires an entry");
            // Keep the oldest inverse so undo restores the pre-gesture value.
            last.rev_after = rev;
        } else {
            self.undo.push(Entry {
                inverse,
                label: edit.label(),
                merge: edit.merge_key(),
                rev_before: self.current_rev,
                rev_after: rev,
            });
            if self.undo.len() > self.limit {
                self.undo.remove(0);
            }
        }
        self.current_rev = rev;
        Ok(())
    }

    fn alloc_rev(&mut self) -> u64 {
        let r = self.next_rev;
        self.next_rev += 1;
        r
    }

    pub fn undo(&mut self, p: &mut Project) -> Result<bool, EditError> {
        self.gesture_open = false;
        let Some(entry) = self.undo.pop() else {
            return Ok(false);
        };
        match entry.inverse.apply(p) {
            Ok(redo) => {
                self.current_rev = entry.rev_before;
                self.redo.push(Entry {
                    inverse: redo,
                    ..entry
                });
                Ok(true)
            }
            Err(e) => {
                self.undo.push(entry);
                Err(e)
            }
        }
    }

    pub fn redo(&mut self, p: &mut Project) -> Result<bool, EditError> {
        self.gesture_open = false;
        let Some(entry) = self.redo.pop() else {
            return Ok(false);
        };
        match entry.inverse.apply(p) {
            Ok(undo) => {
                self.current_rev = entry.rev_after;
                self.undo.push(Entry {
                    inverse: undo,
                    ..entry
                });
                Ok(true)
            }
            Err(e) => {
                self.redo.push(entry);
                Err(e)
            }
        }
    }

    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.last().map(|e| e.label)
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|e| e.label)
    }

    pub fn is_dirty(&self) -> bool {
        self.current_rev != self.saved_rev
    }

    /// Call after a successful save.
    pub fn mark_saved(&mut self) {
        self.saved_rev = self.current_rev;
        self.gesture_open = false;
    }

    /// Call after loading or creating a project.
    pub fn reset(&mut self) {
        *self = Self::new(self.limit);
    }
}

/// Convenience: allocates an ID and adds a sampler channel at the end.
pub fn add_sampler_channel(
    history: &mut History,
    p: &mut Project,
    name: impl Into<String>,
) -> Result<ChannelId, EditError> {
    add_sample_channel(history, p, name, None)
}

/// Appends a sampler channel already holding `sample`: one undo step.
pub fn add_sample_channel(
    history: &mut History,
    p: &mut Project,
    name: impl Into<String>,
    sample: Option<SampleRef>,
) -> Result<ChannelId, EditError> {
    let id = ChannelId(p.ids.next());
    let index = p.channels.len();
    let mut channel = Channel::new_sampler(id, name);
    channel.source = jdance_domain::ChannelSource::Sampler { sample };
    history.execute(
        p,
        Edit::AddChannel {
            channel,
            index,
            rows: Vec::new(),
            automation: Vec::new(),
        },
    )?;
    Ok(id)
}

/// Copies channel `id` with its steps and notes in every pattern, right
/// after the original. Automation stays with the original.
pub fn clone_channel(
    history: &mut History,
    p: &mut Project,
    id: ChannelId,
) -> Result<ChannelId, EditError> {
    let index = p.channel_index(id).ok_or(EditError::NoSuchChannel(id))?;
    let new_id = ChannelId(p.ids.next());
    let mut channel = p.channels[index].clone();
    channel.id = new_id;
    channel.name = format!("{} copy", channel.name);
    let rows = p
        .patterns
        .iter()
        .filter_map(|pat| {
            pat.row(id).map(|r| {
                let mut r = r.clone();
                r.channel = new_id;
                (pat.id, r)
            })
        })
        .collect();
    history.execute(
        p,
        Edit::AddChannel {
            channel,
            index: index + 1,
            rows,
            automation: Vec::new(),
        },
    )?;
    Ok(new_id)
}

/// Allocates an ID and appends a channel played by a plugin instrument.
pub fn add_instrument_channel(
    history: &mut History,
    p: &mut Project,
    name: impl Into<String>,
    plugin: jdance_domain::PluginSlot,
) -> Result<ChannelId, EditError> {
    let id = ChannelId(p.ids.next());
    let index = p.channels.len();
    history.execute(
        p,
        Edit::AddChannel {
            channel: Channel::new_instrument(id, name, plugin),
            index,
            rows: Vec::new(),
            automation: Vec::new(),
        },
    )?;
    Ok(id)
}

/// Creates an automation source for `target` named `name`, with one
/// point holding `initial` (normalized), and places it with a clip from
/// the song start over the arrangement (at least 4 bars) on a new lane;
/// one undo step. A target that already has a source returns that one.
pub fn add_automation(
    history: &mut History,
    p: &mut Project,
    target: AutomationTarget,
    initial: f32,
    name: String,
) -> Result<AutomationId, EditError> {
    if let Some(l) = p.lane_for(&target) {
        return Ok(l.id);
    }
    if !target.exists(p) {
        return Err(EditError::Arrangement("automation target not found".into()));
    }
    let id = AutomationId(p.ids.next());
    let bar = u64::from(p.time_signature.bar_ticks());
    let length = p.arrangement_end_ticks().div_ceil(bar).max(4) * bar;
    let (lane, add_lane) = new_lane_edit(p, name.clone());
    let clip = ClipId(p.ids.next());
    history.execute(
        p,
        Edit::Batch(vec![
            Edit::SetAutomation {
                id,
                lane: Some(AutomationLane {
                    id,
                    name,
                    target,
                    length,
                    points: vec![AutoPoint {
                        tick: 0,
                        value: initial.clamp(0.0, 1.0),
                        curve: Curve::Linear,
                        tension: 0.0,
                    }],
                }),
            },
            add_lane,
            Edit::AddAutomationClip(AutomationClip {
                id: clip,
                lane,
                source: id,
                start: 0,
                offset: 0,
                duration: length,
                muted: false,
            }),
        ]),
    )?;
    Ok(id)
}

/// Removes an automation source with all its clips; one undo step.
pub fn remove_automation(
    history: &mut History,
    p: &mut Project,
    id: AutomationId,
) -> Result<(), EditError> {
    let mut edits: Vec<Edit> = p
        .playlist
        .automation_clips
        .iter()
        .filter(|c| c.source == id)
        .map(|c| Edit::RemoveAutomationClip(c.id))
        .collect();
    edits.push(Edit::SetAutomation { id, lane: None });
    history.execute(p, Edit::Batch(edits))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Project, History, ChannelId, PatternId) {
        let mut p = Project::new();
        let mut h = History::default();
        let ch = add_sampler_channel(&mut h, &mut p, "Kick").unwrap();
        let pat = p.patterns[0].id;
        h.mark_saved();
        (p, h, ch, pat)
    }

    fn on(velocity: f32) -> Step {
        Step { on: true, velocity }
    }

    fn note(start: u32, key: u8) -> Note {
        Note {
            id: 0,
            start,
            length: 240,
            key,
            velocity: 0.8,
        }
    }

    #[test]
    fn set_notes_sorts_validates_and_drags_undo_as_one_step() {
        let (mut p, mut h, ch, pat) = setup();
        let set = |notes| Edit::SetNotes {
            pattern: pat,
            channel: ch,
            notes,
        };
        h.execute(&mut p, set(vec![note(480, 64), note(0, 60)]))
            .unwrap();
        let row = p.patterns[0].row(ch).unwrap();
        let keys: Vec<_> = row.notes.iter().map(|n| (n.start, n.key)).collect();
        assert_eq!(keys, [(0, 60), (480, 64)], "sorted");
        assert!(row.notes.iter().all(|n| n.id > 0) && row.notes[0].id != row.notes[1].id);
        let kept = row.notes.clone();
        h.end_gesture();
        // A drag: many merged edits, one undo step.
        for x in 1..10 {
            h.execute_merging(&mut p, set(vec![note(x * 24, 60), note(480, 64)]))
                .unwrap();
        }
        h.end_gesture();
        h.undo(&mut p).unwrap();
        assert_eq!(p.patterns[0].row(ch).unwrap().notes[0].start, 0);
        h.undo(&mut p).unwrap();
        assert!(p.patterns[0].row(ch).unwrap().notes.is_empty());
        let bad = Note {
            key: 128,
            ..note(0, 60)
        };
        assert!(h.execute(&mut p, set(vec![bad])).is_err());
        let zero = Note {
            length: 0,
            ..note(0, 60)
        };
        assert!(h.execute(&mut p, set(vec![zero])).is_err());
        // Edits keep IDs.
        h.execute(&mut p, set(kept.clone())).unwrap();
        assert_eq!(p.patterns[0].row(ch).unwrap().notes, kept);
    }

    #[test]
    fn shortening_counts_and_drops_notes_beyond_the_end() {
        let (mut p, mut h, ch, pat) = setup();
        h.execute(
            &mut p,
            Edit::SetNotes {
                pattern: pat,
                channel: ch,
                notes: vec![note(0, 60), note(8 * 240, 62), note(7 * 240, 61)],
            },
        )
        .unwrap();
        let shorten = |truncate| Edit::SetPatternLength {
            pattern: pat,
            steps: 8,
            truncate,
        };
        assert_eq!(
            h.execute(&mut p, shorten(false)),
            Err(EditError::WouldDropSteps(1))
        );
        h.execute(&mut p, shorten(true)).unwrap();
        let keys: Vec<u8> = p.patterns[0]
            .row(ch)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.key)
            .collect();
        assert_eq!(keys, [60, 61], "the note reaching past the end stays");
        h.undo(&mut p).unwrap();
        assert_eq!(p.patterns[0].row(ch).unwrap().notes.len(), 3);
    }

    #[test]
    fn undo_redo_step() {
        let (mut p, mut h, ch, pat) = setup();
        let before = p.clone();
        h.execute(
            &mut p,
            Edit::SetStep {
                pattern: pat,
                channel: ch,
                index: 4,
                step: on(1.0),
            },
        )
        .unwrap();
        assert!(p.patterns[0].step(ch, 4).unwrap().on);
        assert!(h.is_dirty());
        h.undo(&mut p).unwrap();
        assert!(!p.patterns[0].step(ch, 4).unwrap().on);
        assert!(!h.is_dirty());
        h.redo(&mut p).unwrap();
        assert!(p.patterns[0].step(ch, 4).unwrap().on);
        assert!(h.is_dirty());
        h.undo(&mut p).unwrap();
        // Undo leaves an (empty) row behind for the channel; step data equal.
        assert_eq!(p.channels, before.channels);
    }

    #[test]
    fn remove_channel_restores_rows_and_position() {
        let (mut p, mut h, kick, pat) = setup();
        let snare = add_sampler_channel(&mut h, &mut p, "Snare").unwrap();
        h.execute(
            &mut p,
            Edit::SetStep {
                pattern: pat,
                channel: kick,
                index: 0,
                step: on(0.5),
            },
        )
        .unwrap();
        let before = p.clone();
        h.execute(&mut p, Edit::RemoveChannel(kick)).unwrap();
        assert_eq!(p.channels.len(), 1);
        assert!(p.patterns[0].row(kick).is_none());
        h.undo(&mut p).unwrap();
        assert_eq!(p.channels[0].id, kick);
        assert_eq!(p.channels[1].id, snare);
        assert_eq!(p.patterns[0].step(kick, 0), Some(on(0.5)));
        assert_eq!(p.channels, before.channels);
    }

    #[test]
    fn gesture_merges_into_one_undo_step() {
        let (mut p, mut h, ch, _) = setup();
        let orig = p.channel(ch).unwrap().volume;
        for v in [0.1, 0.2, 0.3] {
            h.execute_merging(&mut p, Edit::SetChannelVolume(ch, v))
                .unwrap();
        }
        h.end_gesture();
        h.execute_merging(&mut p, Edit::SetChannelVolume(ch, 0.9))
            .unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p.channel(ch).unwrap().volume, 0.3);
        h.undo(&mut p).unwrap();
        assert_eq!(p.channel(ch).unwrap().volume, orig);
        assert!(!h.is_dirty());
    }

    #[test]
    fn time_signature_is_one_undo_step_and_validated() {
        let (mut p, mut h, ..) = setup();
        let three = jdance_domain::TimeSignature::new(3, 4);
        h.execute(&mut p, Edit::SetTimeSignature(three)).unwrap();
        assert_eq!(p.time_signature, three);
        // New patterns are one bar of the new signature (SIG-005).
        let id = crate::playlist::add_pattern(&mut h, &mut p, "B").unwrap();
        assert_eq!(p.pattern(id).unwrap().length_steps, 12);
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert!(p.time_signature.is_common());
        let before = p.clone();
        let bad = jdance_domain::TimeSignature::new(4, 5);
        assert!(h.execute(&mut p, Edit::SetTimeSignature(bad)).is_err());
        assert_eq!(p, before);
    }

    #[test]
    fn invalid_edit_changes_nothing() {
        let (mut p, mut h, ch, pat) = setup();
        let before = p.clone();
        assert!(h.execute(&mut p, Edit::SetTempo(5000.0)).is_err());
        assert!(
            h.execute(&mut p, Edit::SetChannelVolume(ch, f32::NAN))
                .is_err()
        );
        assert!(
            h.execute(
                &mut p,
                Edit::SetStep {
                    pattern: pat,
                    channel: ch,
                    index: 99,
                    step: on(1.0)
                }
            )
            .is_err()
        );
        assert_eq!(p, before);
        assert!(!h.is_dirty());
    }

    #[test]
    fn batch_is_one_step_and_atomic() {
        let (mut p, mut h, ch, pat) = setup();
        let before = p.clone();
        let step = |i| Edit::SetStep {
            pattern: pat,
            channel: ch,
            index: i,
            step: on(1.0),
        };
        h.execute(
            &mut p,
            Edit::Batch(vec![step(1), step(2), Edit::SetTempo(100.0)]),
        )
        .unwrap();
        assert!(p.patterns[0].step(ch, 2).unwrap().on);
        h.undo(&mut p).unwrap();
        assert_eq!(p.tempo_bpm, before.tempo_bpm);
        assert!(!p.patterns[0].step(ch, 1).unwrap().on);
        // A failing member rolls everything back.
        let snapshot = p.clone();
        assert!(
            h.execute(&mut p, Edit::Batch(vec![step(3), Edit::SetTempo(1e9)]))
                .is_err()
        );
        assert_eq!(p, snapshot);
    }

    #[test]
    fn new_edit_after_undo_clears_redo() {
        let (mut p, mut h, _, _) = setup();
        h.execute(&mut p, Edit::SetTempo(100.0)).unwrap();
        h.undo(&mut p).unwrap();
        h.execute(&mut p, Edit::SetTempo(90.0)).unwrap();
        assert!(!h.redo(&mut p).unwrap());
        assert_eq!(p.tempo_bpm, 90.0);
    }

    #[test]
    fn add_automation_starts_from_the_current_value_once() {
        let mut p = Project::new();
        let mut h = History::default();
        let t = add_track(&mut h, &mut p, "Bus").unwrap();
        h.execute(&mut p, Edit::SetTrackPan(t, 0.5)).unwrap();
        let target = AutomationTarget::TrackPan(t);
        let n = target.normalized(&p).unwrap();
        assert_eq!(n, 0.75);
        let lanes = p.playlist.lanes.len();
        let id = add_automation(&mut h, &mut p, target.clone(), n, "Bus pan".into()).unwrap();
        let source = p.automation_lane(id).unwrap();
        assert_eq!(source.points[0].value, 0.75);
        assert_eq!(source.length, 4 * 3840, "at least 4 bars");
        let clip = p.playlist.automation_clips[0];
        assert_eq!((clip.source, clip.start, clip.duration), (id, 0, 4 * 3840));
        assert_eq!(p.playlist.lane(clip.lane).unwrap().name, "Bus pan");
        assert_eq!(p.playlist.lanes.len(), lanes + 1);
        assert_eq!(
            add_automation(&mut h, &mut p, target, 0.1, "x".into()).unwrap(),
            id
        );
        assert_eq!(p.automation.len(), 1);
        assert!(
            add_automation(
                &mut h,
                &mut p,
                AutomationTarget::TrackPan(TrackId(999)),
                0.0,
                "x".into()
            )
            .is_err()
        );
        remove_automation(&mut h, &mut p, id).unwrap();
        assert!(p.automation.is_empty() && p.playlist.automation_clips.is_empty());
        h.undo(&mut p).unwrap();
        assert_eq!(p.automation_usage(id), 1);
        h.undo(&mut p).unwrap();
        assert!(p.automation.is_empty());
        assert_eq!(p.playlist.lanes.len(), lanes, "one undo step");
    }

    #[test]
    fn channels_move_clone_and_recolor() {
        let (mut p, mut h, kick, pat) = setup();
        let snare = add_sampler_channel(&mut h, &mut p, "Snare").unwrap();
        h.execute(
            &mut p,
            Edit::SetStep {
                pattern: pat,
                channel: kick,
                index: 3,
                step: Step {
                    on: true,
                    velocity: 0.5,
                },
            },
        )
        .unwrap();
        let copy = clone_channel(&mut h, &mut p, kick).unwrap();
        let order: Vec<_> = p.channels.iter().map(|c| c.id).collect();
        assert_eq!(order, [kick, copy, snare]);
        assert_eq!(p.channel(copy).unwrap().name, "Kick copy");
        assert!(p.pattern(pat).unwrap().step(copy, 3).unwrap().on);
        h.execute(&mut p, Edit::MoveChannel(snare, 0)).unwrap();
        assert_eq!(p.channels[0].id, snare);
        h.execute(&mut p, Edit::SetChannelColor(kick, Some([1, 2, 3])))
            .unwrap();
        assert_eq!(p.channel(kick).unwrap().color, Some([1, 2, 3]));
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        let order: Vec<_> = p.channels.iter().map(|c| c.id).collect();
        assert_eq!(order, [kick, snare]);
        assert_eq!(p.channel(kick).unwrap().color, None);
    }
}
