// SPDX-License-Identifier: GPL-3.0-or-later
//! All document changes go through [`Edit`]s executed by a [`History`].
//!
//! Every edit, when applied, returns its exact inverse; undo applies the
//! inverse and redo applies the inverse of the inverse. Dirty state is
//! tracked by revision IDs so that undoing back to the saved state clears it.

mod mixer;
mod playlist;

pub use mixer::{add_insert, add_send, add_track};
pub use playlist::{add_clip, add_lane, add_pattern, duplicate_clips, make_unique, steps_beyond};

use jdance_domain::{
    AutomationId, AutomationLane, AutomationTarget, Channel, ChannelId, ClipId, Effect, InsertId,
    Lane, LaneId, LimiterSettings, MAX_CHANNELS, MAX_NOTES_PER_ROW, MAX_TEMPO_BPM, MAX_VOLUME,
    MIN_TEMPO_BPM, Mixer, MixerTrack, Note, Pattern, PatternClip, PatternId, Playlist, Project,
    RoutingError, SampleRef, SendId, Sidechain, Step, StepRow, TrackId, sort_notes, sort_points,
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
    SetChannelSample(ChannelId, Option<SampleRef>),
    SetMasterVolume(f32),
    SetChannelTrack(ChannelId, TrackId),

    // Mixer parameters (cheap inverses, mergeable).
    SetTrackVolume(TrackId, f32),
    SetTrackPan(TrackId, f32),
    SetTrackMuted(TrackId, bool),
    SetTrackSolo(TrackId, bool),
    RenameTrack(TrackId, String),
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
    SetClipRepeat(ClipId, u32),
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
    },
    /// Adds (`lane: Some`, new id), replaces or removes (`None`) an
    /// automation lane. Points are sorted on apply.
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
    Automation(AutomationId),
}

impl Edit {
    pub fn label(&self) -> &'static str {
        match self {
            Edit::SetTempo(_) => "Change tempo",
            Edit::SetStep { .. } => "Edit step",
            Edit::SetNotes { .. } => "Edit notes",
            Edit::AddChannel { .. } => "Add channel",
            Edit::RemoveChannel(_) => "Remove channel",
            Edit::SetChannelVolume(..) => "Change channel volume",
            Edit::SetChannelPan(..) => "Change channel pan",
            Edit::SetChannelMuted(..) => "Mute channel",
            Edit::RenameChannel(..) => "Rename channel",
            Edit::SetChannelSample(..) => "Change sample",
            Edit::SetMasterVolume(_) => "Change master volume",
            Edit::SetChannelTrack(..) => "Route channel",
            Edit::SetTrackVolume(..) => "Change track volume",
            Edit::SetTrackPan(..) => "Change track pan",
            Edit::SetTrackMuted(..) => "Mute track",
            Edit::SetTrackSolo(..) => "Solo track",
            Edit::RenameTrack(..) => "Rename track",
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
            Edit::SetClipRepeat(..) => "Resize clip",
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
            _ => None,
        }
    }

    /// Applies the edit and returns its inverse. On error the project is
    /// left unchanged.
    pub fn apply(&self, p: &mut Project) -> Result<Edit, EditError> {
        match self {
            Edit::SetTempo(bpm) => {
                if !(MIN_TEMPO_BPM..=MAX_TEMPO_BPM).contains(bpm) {
                    return Err(EditError::OutOfRange(*bpm));
                }
                let old = std::mem::replace(&mut p.tempo_bpm, *bpm);
                Ok(Edit::SetTempo(old))
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
                        if p.automation
                            .iter()
                            .any(|l| l.id != *id && l.target == new.target)
                        {
                            return Err(EditError::Arrangement(
                                "the target already has an automation lane".into(),
                            ));
                        }
                        match at {
                            Some(i) => p.automation[i] = new,
                            None => p.automation.push(new),
                        }
                    }
                    None => {
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
                let channel = p.channels.remove(index);
                let mut rows = Vec::new();
                for pat in &mut p.patterns {
                    if let Some(i) = pat.rows.iter().position(|r| r.channel == *id) {
                        rows.push((pat.id, pat.rows.remove(i)));
                    }
                }
                // The channel's automation goes with it (AUT-050).
                let target = jdance_domain::PluginTarget::Channel(*id);
                let (automation, keep): (Vec<_>, Vec<_>) =
                    std::mem::take(&mut p.automation).into_iter().partition(|l| {
                        matches!(&l.target, AutomationTarget::PluginParam { target: t, .. } if *t == target)
                    });
                p.automation = keep;
                Ok(Edit::AddChannel {
                    channel,
                    index,
                    rows,
                    automation,
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
            Edit::SetChannelSample(id, sample) => {
                let ch = p.channel_mut(*id).ok_or(EditError::NoSuchChannel(*id))?;
                let jdance_domain::ChannelSource::Sampler { sample: slot } = &mut ch.source else {
                    return Err(EditError::Arrangement(
                        "an instrument channel has no sample".into(),
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
            | Edit::SetClipRepeat(..)
            | Edit::SetClipMuted(..)
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
    let id = ChannelId(p.ids.next());
    let index = p.channels.len();
    history.execute(
        p,
        Edit::AddChannel {
            channel: Channel::new_sampler(id, name),
            index,
            rows: Vec::new(),
            automation: Vec::new(),
        },
    )?;
    Ok(id)
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
        assert_eq!(row.notes, [note(0, 60), note(480, 64)], "sorted");
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
}
