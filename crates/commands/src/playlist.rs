// SPDX-License-Identifier: GPL-3.0-or-later
//! Pattern and playlist edits (spec "Playlist").
//!
//! Structural edits (adding/removing patterns or lanes, Make unique) are
//! validated against the whole arrangement and undone with a snapshot of
//! the patterns and the playlist, like mixer structure edits.

use crate::{Edit, EditError, History};
use jdance_domain::{
    AudioAsset, AudioClip, AutomationClip, ChannelId, ClipId, LANE_COLORS, Lane, LaneId,
    MAX_PATTERN_STEPS, Pattern, PatternClip, PatternId, Playlist, Project, Step, TICKS_PER_STEP,
};
use std::collections::BTreeSet;

fn arrangement_err(m: String) -> EditError {
    EditError::Arrangement(m)
}

fn validated(p: &mut Project, backup: (Vec<Pattern>, Playlist)) -> Result<Edit, EditError> {
    match p.validate_arrangement() {
        Ok(()) => Ok(Edit::RestoreArrangement {
            patterns: backup.0,
            playlist: Box::new(backup.1),
        }),
        Err(e) => {
            p.patterns = backup.0;
            p.playlist = backup.1;
            Err(arrangement_err(e))
        }
    }
}

/// Runs a structural arrangement change with validation and a snapshot
/// inverse.
fn structural(
    p: &mut Project,
    f: impl FnOnce(&mut Project) -> Result<(), EditError>,
) -> Result<Edit, EditError> {
    let backup = (p.patterns.clone(), p.playlist.clone());
    if let Err(e) = f(p) {
        p.patterns = backup.0;
        p.playlist = backup.1;
        return Err(e);
    }
    validated(p, backup)
}

fn clip_mut(p: &mut Project, id: ClipId) -> Result<&mut PatternClip, EditError> {
    p.playlist
        .clip_mut(id)
        .ok_or_else(|| arrangement_err(format!("clip {} does not exist", id.0)))
}

fn lane_mut(p: &mut Project, id: LaneId) -> Result<&mut Lane, EditError> {
    p.playlist
        .lane_mut(id)
        .ok_or_else(|| arrangement_err(format!("lane {} does not exist", id.0)))
}

/// Number of active steps at or after `len` in `pattern`.
pub fn steps_beyond(pattern: &Pattern, len: u32) -> usize {
    let end = len * TICKS_PER_STEP;
    pattern
        .rows
        .iter()
        .map(|r| {
            r.steps.iter().skip(len as usize).filter(|s| s.on).count()
                + r.notes.iter().filter(|n| n.start >= end).count()
        })
        .sum()
}

pub(crate) fn apply(edit: &Edit, p: &mut Project) -> Result<Edit, EditError> {
    match edit {
        Edit::RenamePattern(id, name) => {
            let pat = p.pattern_mut(*id).ok_or(EditError::NoSuchPattern(*id))?;
            Ok(Edit::RenamePattern(
                *id,
                std::mem::replace(&mut pat.name, name.clone()),
            ))
        }
        Edit::SetPatternColor(id, color) => {
            let pat = p.pattern_mut(*id).ok_or(EditError::NoSuchPattern(*id))?;
            Ok(Edit::SetPatternColor(
                *id,
                std::mem::replace(&mut pat.color, *color),
            ))
        }
        Edit::ReplacePattern(new) => {
            if new.length_steps == 0 || new.length_steps > MAX_PATTERN_STEPS {
                return Err(EditError::OutOfRange(f64::from(new.length_steps)));
            }
            let pat = p
                .pattern_mut(new.id)
                .ok_or(EditError::NoSuchPattern(new.id))?;
            Ok(Edit::ReplacePattern(std::mem::replace(pat, new.clone())))
        }
        Edit::SetPatternLength {
            pattern,
            steps,
            truncate,
        } => {
            if *steps == 0 || *steps > MAX_PATTERN_STEPS {
                return Err(EditError::OutOfRange(f64::from(*steps)));
            }
            let pat = p
                .pattern(*pattern)
                .ok_or(EditError::NoSuchPattern(*pattern))?;
            let dropped = steps_beyond(pat, *steps);
            if dropped > 0 && !truncate {
                return Err(EditError::WouldDropSteps(dropped));
            }
            let mut new = pat.clone();
            new.length_steps = *steps;
            let end = *steps * TICKS_PER_STEP;
            for row in &mut new.rows {
                row.steps.resize(*steps as usize, Step::default());
                // Notes reaching past the end stay; they are cut at each
                // repetition's end during playback.
                row.notes.retain(|n| n.start < end);
            }
            Edit::ReplacePattern(new).apply(p)
        }
        Edit::AddPattern { pattern, index } => structural(p, |p| {
            if p.pattern(pattern.id).is_some() {
                return Err(arrangement_err(format!("pattern {} exists", pattern.id.0)));
            }
            let at = (*index).min(p.patterns.len());
            p.patterns.insert(at, pattern.clone());
            Ok(())
        }),
        Edit::RemovePattern(id) => structural(p, |p| {
            let i = p
                .patterns
                .iter()
                .position(|x| x.id == *id)
                .ok_or(EditError::NoSuchPattern(*id))?;
            p.patterns.remove(i);
            p.playlist.clips.retain(|c| c.pattern != *id);
            Ok(())
        }),
        Edit::AddLane { lane, index } => structural(p, |p| {
            let at = (*index).min(p.playlist.lanes.len());
            p.playlist.lanes.insert(at, lane.clone());
            Ok(())
        }),
        Edit::RemoveLane(id) => structural(p, |p| {
            let i = p
                .playlist
                .lanes
                .iter()
                .position(|l| l.id == *id)
                .ok_or_else(|| arrangement_err(format!("lane {} does not exist", id.0)))?;
            let pl = &mut p.playlist;
            pl.lanes.remove(i);
            pl.clips.retain(|c| c.lane != *id);
            pl.audio_clips.retain(|c| c.lane != *id);
            pl.automation_clips.retain(|c| c.lane != *id);
            Ok(())
        }),
        Edit::RenameLane(id, name) => {
            let l = lane_mut(p, *id)?;
            Ok(Edit::RenameLane(
                *id,
                std::mem::replace(&mut l.name, name.clone()),
            ))
        }
        Edit::SetLaneColor(id, c) => {
            let l = lane_mut(p, *id)?;
            Ok(Edit::SetLaneColor(*id, std::mem::replace(&mut l.color, *c)))
        }
        Edit::SetLaneMuted(id, m) => {
            let l = lane_mut(p, *id)?;
            Ok(Edit::SetLaneMuted(*id, std::mem::replace(&mut l.muted, *m)))
        }
        Edit::SetLaneSolo(id, s) => {
            let l = lane_mut(p, *id)?;
            Ok(Edit::SetLaneSolo(*id, std::mem::replace(&mut l.solo, *s)))
        }
        Edit::AddClip(clip) => {
            if p.playlist.clip(clip.id).is_some() {
                return Err(arrangement_err(format!("clip {} exists", clip.id.0)));
            }
            // One clip checked, not the whole arrangement: a batch of
            // many clips stays linear (spec 0.2.5, Ctrl+D on 1000 clips).
            p.playlist
                .validate_new_clip(clip, &p.patterns)
                .map_err(arrangement_err)?;
            p.playlist.clips.push(*clip);
            Ok(Edit::RemoveClip(clip.id))
        }
        Edit::RemoveClip(id) => {
            let i = p
                .playlist
                .clips
                .iter()
                .position(|c| c.id == *id)
                .ok_or_else(|| arrangement_err(format!("clip {} does not exist", id.0)))?;
            Ok(Edit::AddClip(p.playlist.clips.remove(i)))
        }
        Edit::MoveClip { id, start, lane } => {
            if p.playlist.lane(*lane).is_none() {
                return Err(arrangement_err(format!("lane {} does not exist", lane.0)));
            }
            if *start > jdance_domain::MAX_START_TICKS {
                return Err(EditError::OutOfRange(*start as f64));
            }
            let c = clip_mut(p, *id)?;
            let old = (c.start, c.lane);
            c.start = *start;
            c.lane = *lane;
            Ok(Edit::MoveClip {
                id: *id,
                start: old.0,
                lane: old.1,
            })
        }
        Edit::SetClipShape {
            id,
            start,
            offset,
            duration,
            looped,
        } => {
            if *duration == 0 || *duration > jdance_domain::MAX_CLIP_TICKS {
                return Err(EditError::OutOfRange(*duration as f64));
            }
            if *offset > jdance_domain::MAX_CLIP_TICKS {
                return Err(EditError::OutOfRange(*offset as f64));
            }
            if *start > jdance_domain::MAX_START_TICKS {
                return Err(EditError::OutOfRange(*start as f64));
            }
            let c = clip_mut(p, *id)?;
            let old = Edit::SetClipShape {
                id: *id,
                start: c.start,
                offset: c.offset,
                duration: c.duration,
                looped: c.looped,
            };
            (c.start, c.offset, c.duration, c.looped) = (*start, *offset, *duration, *looped);
            Ok(old)
        }
        Edit::AddAsset(asset) => {
            if p.asset(asset.id).is_some() {
                return Err(arrangement_err(format!("asset {} exists", asset.id.0)));
            }
            asset.validate().map_err(arrangement_err)?;
            p.assets.push(asset.clone());
            Ok(Edit::RemoveAsset(asset.id))
        }
        Edit::RemoveAsset(id) => {
            let used = p.asset_usage(*id);
            if used > 0 {
                return Err(arrangement_err(format!(
                    "the audio file is used by {used} clip(s)"
                )));
            }
            let channels = p.audio_channels_of(*id).count();
            if channels > 0 {
                return Err(arrangement_err(format!(
                    "the audio file is used by {channels} audio channel(s)"
                )));
            }
            let i = p
                .assets
                .iter()
                .position(|a| a.id == *id)
                .ok_or_else(|| arrangement_err(format!("asset {} does not exist", id.0)))?;
            Ok(Edit::AddAsset(p.assets.remove(i)))
        }
        Edit::RenameAsset(id, name) => {
            let a = p
                .assets
                .iter_mut()
                .find(|a| a.id == *id)
                .ok_or_else(|| arrangement_err(format!("asset {} does not exist", id.0)))?;
            Ok(Edit::RenameAsset(
                *id,
                std::mem::replace(&mut a.name, name.clone()),
            ))
        }
        Edit::AddAutomationClip(clip) => {
            if p.playlist.has_clip(clip.id) {
                return Err(arrangement_err(format!("clip {} exists", clip.id.0)));
            }
            p.playlist.automation_clips.push(*clip);
            if let Err(e) = p.validate_arrangement() {
                p.playlist.automation_clips.pop();
                return Err(arrangement_err(e));
            }
            Ok(Edit::RemoveAutomationClip(clip.id))
        }
        Edit::RemoveAutomationClip(id) => {
            let i = p
                .playlist
                .automation_clips
                .iter()
                .position(|c| c.id == *id)
                .ok_or_else(|| arrangement_err(format!("clip {} does not exist", id.0)))?;
            Ok(Edit::AddAutomationClip(
                p.playlist.automation_clips.remove(i),
            ))
        }
        Edit::SetAutomationClip(clip) => {
            let i = p
                .playlist
                .automation_clips
                .iter()
                .position(|c| c.id == clip.id)
                .ok_or_else(|| arrangement_err(format!("clip {} does not exist", clip.id.0)))?;
            let old = std::mem::replace(&mut p.playlist.automation_clips[i], *clip);
            if let Err(e) = p.validate_arrangement() {
                p.playlist.automation_clips[i] = old;
                return Err(arrangement_err(e));
            }
            Ok(Edit::SetAutomationClip(old))
        }
        Edit::AddAudioClip(clip) => {
            if p.playlist.has_clip(clip.id) {
                return Err(arrangement_err(format!("clip {} exists", clip.id.0)));
            }
            p.playlist.audio_clips.push(*clip);
            if let Err(e) = p.validate_arrangement() {
                p.playlist.audio_clips.pop();
                return Err(arrangement_err(e));
            }
            Ok(Edit::RemoveAudioClip(clip.id))
        }
        Edit::RemoveAudioClip(id) => {
            let i = p
                .playlist
                .audio_clips
                .iter()
                .position(|c| c.id == *id)
                .ok_or_else(|| arrangement_err(format!("clip {} does not exist", id.0)))?;
            Ok(Edit::AddAudioClip(p.playlist.audio_clips.remove(i)))
        }
        Edit::SetAudioClip(clip) => {
            let c = p
                .playlist
                .audio_clip_mut(clip.id)
                .ok_or_else(|| arrangement_err(format!("clip {} does not exist", clip.id.0)))?;
            let old = std::mem::replace(c, *clip);
            if let Err(e) = p.validate_arrangement() {
                *p.playlist.audio_clip_mut(clip.id).expect("just replaced") = old;
                return Err(arrangement_err(e));
            }
            Ok(Edit::SetAudioClip(old))
        }
        Edit::SetClipMuted(id, m) => {
            let c = clip_mut(p, *id)?;
            Ok(Edit::SetClipMuted(*id, std::mem::replace(&mut c.muted, *m)))
        }
        Edit::MakeUnique { clip, pattern } => structural(p, |p| {
            let source = p
                .playlist
                .clip(*clip)
                .ok_or_else(|| arrangement_err(format!("clip {} does not exist", clip.0)))?
                .pattern;
            let at = p
                .patterns
                .iter()
                .position(|x| x.id == source)
                .ok_or(EditError::NoSuchPattern(source))?;
            p.patterns.insert(at + 1, pattern.clone());
            clip_mut(p, *clip)?.pattern = pattern.id;
            Ok(())
        }),
        Edit::SetLoop { range, enabled } => {
            if let Some((a, b)) = range
                && a >= b
            {
                return Err(arrangement_err("empty loop range".into()));
            }
            let old = Edit::SetLoop {
                range: p.playlist.loop_range,
                enabled: p.playlist.loop_enabled,
            };
            p.playlist.loop_range = *range;
            p.playlist.loop_enabled = *enabled;
            Ok(old)
        }
        Edit::RestoreArrangement { patterns, playlist } => {
            let backup = (p.patterns.clone(), p.playlist.clone());
            p.patterns = patterns.clone();
            p.playlist = (**playlist).clone();
            validated(p, backup)
        }
        _ => unreachable!("non-playlist edit {edit:?} routed to playlist::apply"),
    }
}

/// Adds an empty pattern after the last one.
pub fn add_pattern(
    history: &mut History,
    p: &mut Project,
    name: impl Into<String>,
) -> Result<PatternId, EditError> {
    let id = PatternId(p.ids.next());
    let index = p.patterns.len();
    // One bar of the project signature (SIG-005).
    let mut pattern = Pattern::new(id, name);
    pattern.length_steps = p.time_signature.steps_per_bar();
    history.execute(p, Edit::AddPattern { pattern, index })?;
    Ok(id)
}

/// A new pattern named `name` holding `notes` (from tick 0) for
/// `channel`, as long as whole bars of the project signature need (spec
/// 0.3.0 §2.4); notes past the longest pattern are left out. Not added.
pub fn pattern_with_notes(
    p: &mut Project,
    name: impl Into<String>,
    channel: ChannelId,
    notes: &[jdance_domain::Note],
) -> (Pattern, usize) {
    let mut pattern = Pattern::new(PatternId(p.ids.next()), name);
    let bar = p.time_signature.steps_per_bar().max(1);
    let limit = MAX_PATTERN_STEPS * TICKS_PER_STEP;
    let kept: Vec<_> = notes.iter().filter(|n| n.start < limit).copied().collect();
    let end = kept.iter().map(|n| n.end().min(limit)).max().unwrap_or(0);
    let steps = end.div_ceil(TICKS_PER_STEP).div_ceil(bar).max(1) * bar;
    pattern.length_steps = steps.min(MAX_PATTERN_STEPS / bar * bar).max(bar);
    let row = pattern.row_mut(channel);
    row.notes = kept.clone();
    jdance_domain::assign_note_ids(&mut row.notes);
    jdance_domain::sort_notes(&mut row.notes);
    (pattern, notes.len() - kept.len())
}

/// Adds a lane at the end with the next default color.
pub fn add_lane(history: &mut History, p: &mut Project) -> Result<LaneId, EditError> {
    let name = format!("Lane {}", p.playlist.lanes.len() + 1);
    let (id, edit) = new_lane_edit(p, name);
    history.execute(p, edit)?;
    Ok(id)
}

/// The edit adding a lane named `name` at the bottom (not executed).
pub fn new_lane_edit(p: &mut Project, name: String) -> (LaneId, Edit) {
    let id = LaneId(p.ids.next());
    let n = p.playlist.lanes.len();
    let edit = Edit::AddLane {
        lane: Lane {
            id,
            name,
            color: LANE_COLORS[n % LANE_COLORS.len()],
            muted: false,
            solo: false,
        },
        index: n,
    };
    (id, edit)
}

/// Creates a looped clip, one repetition long, of `pattern` on `lane` at `start` ticks.
pub fn add_clip(
    history: &mut History,
    p: &mut Project,
    lane: LaneId,
    pattern: PatternId,
    start: u64,
) -> Result<ClipId, EditError> {
    let len = p
        .pattern(pattern)
        .ok_or(EditError::NoSuchPattern(pattern))?
        .length_ticks();
    let id = ClipId(p.ids.next());
    history.execute(
        p,
        Edit::AddClip(PatternClip {
            id,
            lane,
            pattern,
            start,
            offset: 0,
            duration: len,
            looped: true,
            muted: false,
        }),
    )?;
    Ok(id)
}

/// Duplicates clips (pattern, audio, automation) as one block placed
/// right at the end of the lanes they are on (spec 0.2.5 §1): the block
/// moves by the latest clip end on those lanes, any clip counted, minus
/// its own start; lanes and the relative layout are kept, nothing is
/// snapped. New clip IDs, same patterns and files. One undo step.
pub fn duplicate_clips(
    history: &mut History,
    p: &mut Project,
    ids: &[ClipId],
) -> Result<Vec<ClipId>, EditError> {
    let clips: Vec<PatternClip> = ids
        .iter()
        .filter_map(|id| p.playlist.clip(*id).copied())
        .collect();
    let audio: Vec<AudioClip> = ids
        .iter()
        .filter_map(|id| p.playlist.audio_clip(*id).copied())
        .collect();
    let automation: Vec<AutomationClip> = ids
        .iter()
        .filter_map(|id| p.playlist.automation_clip(*id).copied())
        .collect();
    let Some(first) = clips
        .iter()
        .map(|c| c.start as f64)
        .chain(
            audio
                .iter()
                .filter_map(|c| p.audio_clip_ticks(c).map(|s| s.0)),
        )
        .chain(automation.iter().map(|c| c.start as f64))
        .reduce(f64::min)
    else {
        return Ok(Vec::new());
    };
    let lanes: BTreeSet<LaneId> = clips
        .iter()
        .map(|c| c.lane)
        .chain(audio.iter().map(|c| c.lane))
        .chain(automation.iter().map(|c| c.lane))
        .collect();
    let end = lane_end(p, &lanes);
    // Pattern and automation clips start on whole ticks, audio clips on
    // whole frames: round up so a copy never overlaps the clip it follows.
    let shift = (end - first).max(0.0);
    let offset = (shift - 1e-9).ceil().max(0.0) as u64;
    let fpt = jdance_domain::frames_per_tick(p.tempo_bpm, p.timebase_rate);
    let frames = (shift * fpt - 1e-6).ceil().max(0.0) as u64;
    let mut edits = Vec::new();
    let mut new_ids = Vec::new();
    for c in clips {
        let id = ClipId(p.ids.next());
        new_ids.push(id);
        edits.push(Edit::AddClip(PatternClip {
            id,
            start: c.start + offset,
            ..c
        }));
    }
    for c in audio {
        let id = ClipId(p.ids.next());
        new_ids.push(id);
        edits.push(Edit::AddAudioClip(AudioClip {
            id,
            start: c.start + frames,
            ..c
        }));
    }
    for c in automation {
        let id = ClipId(p.ids.next());
        new_ids.push(id);
        edits.push(Edit::AddAutomationClip(AutomationClip {
            id,
            start: c.start + offset,
            ..c
        }));
    }
    history.execute(p, Edit::Batch(edits))?;
    Ok(new_ids)
}

/// The latest clip end on `lanes`, in ticks; any kind of clip counts.
fn lane_end(p: &Project, lanes: &BTreeSet<LaneId>) -> f64 {
    let pl = &p.playlist;
    pl.clips
        .iter()
        .filter(|c| lanes.contains(&c.lane))
        .map(|c| c.interval().1 as f64)
        .chain(
            pl.audio_clips
                .iter()
                .filter(|c| lanes.contains(&c.lane))
                .filter_map(|c| p.audio_clip_ticks(c).map(|s| s.1)),
        )
        .chain(
            pl.automation_clips
                .iter()
                .filter(|c| lanes.contains(&c.lane))
                .map(|c| c.interval().1 as f64),
        )
        .fold(0.0, f64::max)
}

/// Copies the clip's pattern (new pattern ID) and points only this clip
/// at the copy.
pub fn make_unique(
    history: &mut History,
    p: &mut Project,
    clip: ClipId,
) -> Result<PatternId, EditError> {
    let source = p
        .playlist
        .clip(clip)
        .and_then(|c| p.pattern(c.pattern))
        .cloned()
        .ok_or_else(|| arrangement_err(format!("clip {} does not exist", clip.0)))?;
    let id = PatternId(p.ids.next());
    let pattern = Pattern {
        id,
        name: format!("{} (copy)", source.name),
        ..source
    };
    history.execute(p, Edit::MakeUnique { clip, pattern })?;
    Ok(id)
}

/// Which clip edge a resize drags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
}

/// Pattern clip resized so `edge` lands on `tick` (spec 0.2.0 §4): the
/// right edge changes the duration; the left edge moves the start and the
/// offset together, keeping the end and what is already visible. Without
/// looping the clip stays inside the pattern. Returns `(start, offset,
/// duration)`.
pub fn pattern_clip_edge(
    clip: &PatternClip,
    pattern_len: u64,
    edge: Edge,
    tick: i64,
) -> (u64, u64, u64) {
    let len = pattern_len.max(1) as i64;
    let (start, offset, dur) = (clip.start as i64, clip.offset as i64, clip.duration as i64);
    let end = start + dur;
    match edge {
        Edge::Right => {
            let mut new_end = tick.max(start + 1);
            if !clip.looped {
                new_end = new_end.min(start + (len - offset).max(1));
            }
            (clip.start, clip.offset, (new_end - start) as u64)
        }
        Edge::Left => {
            // Not looped: cannot reveal before the pattern start.
            let earliest = if clip.looped { 0 } else { start - offset };
            let new_start = tick.clamp(earliest.max(0), end - 1);
            let mut new_offset = offset + (new_start - start);
            if clip.looped {
                new_offset = new_offset.rem_euclid(len);
            }
            (
                new_start as u64,
                new_offset as u64,
                (end - new_start) as u64,
            )
        }
    }
}

/// Audio clip resized so `edge` lands on project frame `at` (spec 0.2.0
/// §4): the right edge moves `source_out` (up to the file end), the left
/// edge moves `source_in` and the start together, keeping the end. No
/// stretching; at least one source frame stays. `stretch` is the length
/// factor of the clip's sampler ([`Project::channel_stretch`]).
pub fn audio_clip_edge(
    clip: &AudioClip,
    asset: &AudioAsset,
    timebase: u32,
    stretch: f64,
    edge: Edge,
    at: i64,
) -> AudioClip {
    let ratio = f64::from(asset.sample_rate.max(1))
        / f64::from(timebase.max(1))
        / clip.scale(stretch).max(1e-6);
    let start = clip.start as i64;
    match edge {
        Edge::Right => {
            let src = ((at - start) as f64 * ratio).round() as i64;
            let out =
                (clip.source_in as i64 + src).clamp(clip.source_in as i64 + 1, asset.frames as i64);
            AudioClip {
                source_out: out as u64,
                ..*clip
            }
        }
        Edge::Left => {
            let delta = ((at - start) as f64 * ratio).round() as i64;
            let new_in = (clip.source_in as i64 + delta).clamp(0, clip.source_out as i64 - 1);
            let moved = ((new_in - clip.source_in as i64) as f64 / ratio).round() as i64;
            AudioClip {
                source_in: new_in as u64,
                start: (start + moved).max(0) as u64,
                ..*clip
            }
        }
    }
}

/// Splits a pattern clip at `tick` (spec 0.2.0 §8): the left part keeps
/// the ID, the right part (`new_id`) continues the same pattern position.
/// `None` at or outside the edges.
pub fn slice_pattern_clip(
    clip: &PatternClip,
    pattern_len: u64,
    tick: u64,
    new_id: ClipId,
) -> Option<(PatternClip, PatternClip)> {
    let (a, b) = clip.interval();
    if tick <= a || tick >= b {
        return None;
    }
    let left = PatternClip {
        duration: tick - a,
        ..*clip
    };
    let mut offset = clip.offset + (tick - a);
    if clip.looped {
        offset %= pattern_len.max(1);
    }
    let right = PatternClip {
        id: new_id,
        start: tick,
        offset,
        duration: b - tick,
        ..*clip
    };
    Some((left, right))
}

/// Splits an audio clip at project frame `at`: both parts share one
/// source frame boundary (no frame lost or doubled) and the same asset.
/// `None` at or outside the edges.
pub fn slice_audio_clip(
    clip: &AudioClip,
    asset: &AudioAsset,
    timebase: u32,
    stretch: f64,
    at: u64,
    new_id: ClipId,
) -> Option<(AudioClip, AudioClip)> {
    let ratio = f64::from(asset.sample_rate.max(1))
        / f64::from(timebase.max(1))
        / clip.scale(stretch).max(1e-6);
    if at <= clip.start {
        return None;
    }
    let boundary = clip.source_in + ((at - clip.start) as f64 * ratio).round() as u64;
    if boundary <= clip.source_in || boundary >= clip.source_out {
        return None;
    }
    let left = AudioClip {
        source_out: boundary,
        ..*clip
    };
    let right = AudioClip {
        id: new_id,
        start: clip.start + left.length(asset, timebase, stretch),
        source_in: boundary,
        ..*clip
    };
    Some((left, right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_sampler_channel;

    fn setup() -> (Project, History, PatternId, LaneId) {
        let mut p = Project::new();
        let mut h = History::default();
        let pat = p.patterns[0].id;
        let lane = p.playlist.lanes[0].id;
        add_sampler_channel(&mut h, &mut p, "Kick").unwrap();
        h.mark_saved();
        (p, h, pat, lane)
    }

    #[test]
    fn clip_create_move_repeat_and_undo() {
        let (mut p, mut h, pat, lane) = setup();
        let lane2 = p.playlist.lanes[1].id;
        let c = add_clip(&mut h, &mut p, lane, pat, 0).unwrap();
        h.execute(
            &mut p,
            Edit::MoveClip {
                id: c,
                start: 7_680,
                lane: lane2,
            },
        )
        .unwrap();
        let shape = |duration| Edit::SetClipShape {
            id: c,
            start: 7_680,
            offset: 0,
            duration,
            looped: true,
        };
        h.execute(&mut p, shape(4 * 3840)).unwrap();
        let clip = *p.playlist.clip(c).unwrap();
        assert_eq!(
            (clip.start, clip.lane, clip.duration),
            (7_680, lane2, 4 * 3840)
        );
        assert!(h.execute(&mut p, shape(0)).is_err());
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert!(p.playlist.clips.is_empty());
        assert!(!h.is_dirty());
    }

    #[test]
    fn drag_of_several_clips_is_one_undo_step() {
        let (mut p, mut h, pat, lane) = setup();
        let a = add_clip(&mut h, &mut p, lane, pat, 0).unwrap();
        let b = add_clip(&mut h, &mut p, lane, pat, 3_840).unwrap();
        h.execute(
            &mut p,
            Edit::Batch(vec![
                Edit::MoveClip {
                    id: a,
                    start: 960,
                    lane,
                },
                Edit::MoveClip {
                    id: b,
                    start: 4_800,
                    lane,
                },
            ]),
        )
        .unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p.playlist.clip(a).unwrap().start, 0);
        assert_eq!(p.playlist.clip(b).unwrap().start, 3_840);
    }

    /// Spec 0.2.5 §1: copies go to the end of their lanes, as a block.
    #[test]
    fn duplicate_goes_to_the_end_of_the_lane() {
        let (mut p, mut h, pat, lane) = setup();
        let bar = 3_840;
        let a = add_clip(&mut h, &mut p, lane, pat, 0).unwrap();
        add_clip(&mut h, &mut p, lane, pat, bar).unwrap();
        add_clip(&mut h, &mut p, lane, pat, 2 * bar).unwrap();
        let d1 = duplicate_clips(&mut h, &mut p, &[a]).unwrap();
        assert_eq!(p.playlist.clip(d1[0]).unwrap().start, 3 * bar, "after C");
        let mut starts = vec![];
        let mut sel = d1;
        for _ in 0..4 {
            sel = duplicate_clips(&mut h, &mut p, &sel).unwrap();
            starts.push(p.playlist.clip(sel[0]).unwrap().start);
        }
        assert_eq!(starts, [4 * bar, 5 * bar, 6 * bar, 7 * bar], "a series");
        let n = p.playlist.clips.len();
        h.undo(&mut p).unwrap();
        assert_eq!(p.playlist.clips.len(), n - 1, "one undo, one copy");
        assert!(duplicate_clips(&mut h, &mut p, &[]).unwrap().is_empty());

        // Two lanes: one block, at the latest end of both lanes.
        let lane2 = add_lane(&mut h, &mut p).unwrap();
        let x = add_clip(&mut h, &mut p, lane2, pat, 20 * bar).unwrap();
        let y = add_clip(&mut h, &mut p, lane, pat, 21 * bar).unwrap();
        // Latest end on these lanes: the last copy on `lane` (ends at 7)
        // and y (ends at 22).
        let d = duplicate_clips(&mut h, &mut p, &[x, y]).unwrap();
        let cx = *p.playlist.clip(d[0]).unwrap();
        let cy = *p.playlist.clip(d[1]).unwrap();
        assert_eq!((cx.lane, cx.start), (lane2, 22 * bar));
        assert_eq!((cy.lane, cy.start), (lane, 23 * bar), "layout kept");
    }

    /// Spec 0.2.5 «Общие условия»: Ctrl+D on 1000 clips within 100 ms
    /// (checked in release builds; debug builds get ten times as long).
    #[test]
    fn duplicating_a_thousand_clips_is_fast() {
        let (mut p, mut h, pat, lane) = setup();
        let ids: Vec<ClipId> = (0..1000)
            .map(|i| add_clip(&mut h, &mut p, lane, pat, i * 3_840).unwrap())
            .collect();
        let t = std::time::Instant::now();
        let new = duplicate_clips(&mut h, &mut p, &ids).unwrap();
        let ms = t.elapsed().as_millis();
        assert_eq!(new.len(), 1000);
        eprintln!("duplicate 1000 clips: {ms} ms");
        let limit = if cfg!(debug_assertions) { 1000 } else { 100 };
        assert!(ms < limit, "{ms} ms");
    }

    #[test]
    fn duplicate_keeps_pattern_and_make_unique_copies_it() {
        let (mut p, mut h, pat, lane) = setup();
        let ch = p.channels[0].id;
        h.execute(
            &mut p,
            Edit::SetStep {
                pattern: pat,
                channel: ch,
                index: 2,
                step: Step {
                    on: true,
                    velocity: 1.0,
                },
            },
        )
        .unwrap();
        let a = add_clip(&mut h, &mut p, lane, pat, 0).unwrap();
        let dup = duplicate_clips(&mut h, &mut p, &[a]).unwrap();
        let d = *p.playlist.clip(dup[0]).unwrap();
        assert_ne!(d.id, a);
        assert_eq!(d.pattern, pat);
        assert_eq!(d.start, 3_840, "placed right after the original");

        let copy = make_unique(&mut h, &mut p, dup[0]).unwrap();
        assert_ne!(copy, pat);
        assert_eq!(p.playlist.clip(dup[0]).unwrap().pattern, copy);
        assert_eq!(
            p.playlist.clip(a).unwrap().pattern,
            pat,
            "other clips unchanged"
        );
        assert!(
            p.pattern(copy).unwrap().step(ch, 2).unwrap().on,
            "events copied"
        );
        // Editing the copy leaves the original alone.
        h.execute(
            &mut p,
            Edit::SetStep {
                pattern: copy,
                channel: ch,
                index: 2,
                step: Step::default(),
            },
        )
        .unwrap();
        assert!(p.pattern(pat).unwrap().step(ch, 2).unwrap().on);
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert!(p.pattern(copy).is_none());
        assert_eq!(p.playlist.clip(dup[0]).unwrap().pattern, pat);
    }

    #[test]
    fn shortening_a_pattern_needs_explicit_truncation() {
        let (mut p, mut h, pat, lane) = setup();
        let ch = p.channels[0].id;
        h.execute(
            &mut p,
            Edit::SetStep {
                pattern: pat,
                channel: ch,
                index: 12,
                step: Step {
                    on: true,
                    velocity: 1.0,
                },
            },
        )
        .unwrap();
        let c = add_clip(&mut h, &mut p, lane, pat, 0).unwrap();
        h.execute(
            &mut p,
            Edit::SetClipShape {
                id: c,
                start: 0,
                offset: 0,
                duration: 3 * 3840,
                looped: true,
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
        assert_eq!(
            p.pattern(pat).unwrap().length_steps,
            16,
            "cancel leaves it unchanged"
        );
        h.execute(&mut p, shorten(true)).unwrap();
        assert_eq!(p.pattern(pat).unwrap().length_steps, 8);
        assert_eq!(
            p.playlist.clip(c).unwrap().duration,
            3 * 3840,
            "the clip keeps its length; it now repeats the shorter pattern"
        );
        // Undo restores the truncated step in the same step.
        h.undo(&mut p).unwrap();
        assert!(p.pattern(pat).unwrap().step(ch, 12).unwrap().on);
        // Lengthening needs no confirmation.
        h.execute(
            &mut p,
            Edit::SetPatternLength {
                pattern: pat,
                steps: 32,
                truncate: false,
            },
        )
        .unwrap();
        assert_eq!(p.pattern(pat).unwrap().rows[0].steps.len(), 32);
    }

    #[test]
    fn removing_lane_or_pattern_removes_its_clips_and_undoes() {
        let (mut p, mut h, pat, lane) = setup();
        let c = add_clip(&mut h, &mut p, lane, pat, 0).unwrap();
        let before = p.clone();
        h.execute(&mut p, Edit::RemoveLane(lane)).unwrap();
        assert!(p.playlist.clip(c).is_none());
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
        let second = add_pattern(&mut h, &mut p, "B").unwrap();
        h.execute(&mut p, Edit::RemovePattern(pat)).unwrap();
        assert!(p.playlist.clips.is_empty());
        assert_eq!(p.patterns[0].id, second);
        assert!(
            h.execute(&mut p, Edit::RemovePattern(second)).is_err(),
            "last pattern stays"
        );
    }
    #[test]
    fn audio_assets_clips_and_channel_routing() {
        let (mut p, mut h, _, lane) = setup();
        let asset = jdance_domain::AudioAsset {
            id: jdance_domain::AssetId(p.ids.next()),
            name: "loop".into(),
            path: "assets/loop.wav".into(),
            hash: 1,
            size: 100,
            sample_rate: 48_000,
            channels: 2,
            frames: 48_000,
        };
        h.execute(&mut p, Edit::AddAsset(asset.clone())).unwrap();
        let (ch, add) =
            crate::audio_channels::new_audio_channel(&mut p, "loop".into(), asset.id, 0);
        h.execute(&mut p, add).unwrap();
        let clip = jdance_domain::AudioClip {
            id: ClipId(p.ids.next()),
            lane,
            channel: ch,
            start: 0,
            source_in: 0,
            source_out: 48_000,
            gain: 1.0,
            muted: false,
            fade_in: 0,
            fade_out: 0,
            speed: 1.0,
        };
        h.execute(&mut p, Edit::AddAudioClip(clip)).unwrap();
        // Trimming past the file end is rejected and changes nothing.
        let long = jdance_domain::AudioClip {
            source_out: 48_001,
            ..clip
        };
        assert!(h.execute(&mut p, Edit::SetAudioClip(long)).is_err());
        assert_eq!(p.playlist.audio_clip(clip.id), Some(&clip));
        assert!(
            h.execute(&mut p, Edit::RemoveAsset(asset.id)).is_err(),
            "a used file cannot be removed"
        );
        assert_eq!(
            p.arrangement_end_ticks(),
            2080,
            "1 s at 130 BPM = 2080 ticks"
        );
        // The channel routed to a track; removing the track falls back to
        // the master (ACH-051), undo restores the route.
        let t = crate::add_track(&mut h, &mut p, "Bus").unwrap();
        h.execute(&mut p, Edit::SetChannelTrack(ch, t)).unwrap();
        h.execute(&mut p, Edit::RemoveTrack(t)).unwrap();
        assert_eq!(p.channel(ch).unwrap().track, jdance_domain::TrackId::MASTER);
        h.undo(&mut p).unwrap();
        assert_eq!(p.channel(ch).unwrap().track, t);
        while h.undo(&mut p).unwrap() {}
        assert!(p.assets.is_empty() && p.playlist.audio_clips.is_empty());
    }

    #[test]
    fn pattern_clip_edges_and_slice() {
        let pat = Pattern::new(PatternId(1), "A");
        let len = pat.length_ticks(); // 3840
        let c = PatternClip::repeating(ClipId(1), LaneId(2), &pat, 3840, 2);
        // Right edge: any positive duration, partial last repetition.
        assert_eq!(
            pattern_clip_edge(&c, len, Edge::Right, 3840 + 5000),
            (3840, 0, 5000)
        );
        assert_eq!(
            pattern_clip_edge(&c, len, Edge::Right, 0),
            (3840, 0, 1),
            "at least 1 tick"
        );
        // Left edge right: start and offset move, end stays.
        assert_eq!(
            pattern_clip_edge(&c, len, Edge::Left, 3840 + 960),
            (4800, 960, 7680 - 960)
        );
        // Left edge left of the pattern start, looped: normalized phase.
        assert_eq!(
            pattern_clip_edge(&c, len, Edge::Left, 3840 - 960),
            (2880, 2880, 7680 + 960)
        );
        // Not looped: bounded by the pattern.
        let once = PatternClip {
            looped: false,
            duration: 3840,
            ..c
        };
        assert_eq!(
            pattern_clip_edge(&once, len, Edge::Left, 0),
            (3840, 0, 3840)
        );
        assert_eq!(
            pattern_clip_edge(&once, len, Edge::Right, 99_999),
            (3840, 0, 3840)
        );

        let (l, r) = slice_pattern_clip(&c, len, 3840 + 4800, ClipId(9)).unwrap();
        assert_eq!((l.id, l.start, l.duration), (c.id, 3840, 4800));
        assert_eq!(
            (r.id, r.start, r.offset, r.duration),
            (ClipId(9), 8640, 960, 2880)
        );
        assert_eq!(r.pattern, c.pattern, "same pattern, unchanged");
        assert!(
            slice_pattern_clip(&c, len, 3840, ClipId(9)).is_none(),
            "edge: no-op"
        );
    }

    #[test]
    fn audio_clip_edges_and_slice_share_one_boundary() {
        let asset = AudioAsset {
            id: jdance_domain::AssetId(1),
            name: "a".into(),
            path: "a.wav".into(),
            hash: 0,
            size: 0,
            sample_rate: 44_100,
            channels: 1,
            frames: 44_100,
        };
        let c = AudioClip {
            id: ClipId(1),
            lane: LaneId(2),
            channel: jdance_domain::ChannelId(3),
            start: 48_000,
            source_in: 11_025,
            source_out: 33_075,
            gain: 1.0,
            muted: false,
            fade_in: 0,
            fade_out: 0,
            speed: 1.0,
        };
        // Right edge reveals up to the file end, never past it.
        let r = audio_clip_edge(&c, &asset, 48_000, 1.0, Edge::Right, 10_000_000);
        assert_eq!(r.source_out, 44_100);
        assert_eq!(r.start, c.start);
        // Left edge left: reveals earlier content, keeps the end.
        let l = audio_clip_edge(&c, &asset, 48_000, 1.0, Edge::Left, 48_000 - 12_000);
        assert_eq!((l.source_in, l.start), (0, 36_000));
        assert_eq!(
            l.interval(&asset, 48_000, 1.0).1,
            c.interval(&asset, 48_000, 1.0).1
        );
        // Left edge right trims and moves the start.
        let t = audio_clip_edge(&c, &asset, 48_000, 1.0, Edge::Left, 48_000 + 4_800);
        assert_eq!((t.source_in, t.start), (11_025 + 4_410, 52_800));

        let at = 48_000 + 12_000;
        let (a, b) = slice_audio_clip(&c, &asset, 48_000, 1.0, at, ClipId(5)).unwrap();
        assert_eq!(a.source_out, b.source_in, "one boundary");
        assert_eq!((a.source_in, b.source_out), (c.source_in, c.source_out));
        assert_eq!(b.start, a.start + a.length(&asset, 48_000, 1.0));
        // A clip at double speed: a frame of the timeline is two of the file.
        let fast = AudioClip { speed: 2.0, ..c };
        let r = audio_clip_edge(&fast, &asset, 48_000, 1.0, Edge::Right, 48_000 + 4_800);
        assert_eq!(r.source_out, 11_025 + 8_820);
        let (a, b) =
            slice_audio_clip(&fast, &asset, 48_000, 1.0, 48_000 + 4_800, ClipId(6)).unwrap();
        assert_eq!(a.source_out, 11_025 + 8_820);
        assert_eq!(b.start, 48_000 + 4_800);
        assert_eq!((a.channel, b.channel), (c.channel, c.channel));
        assert!(slice_audio_clip(&c, &asset, 48_000, 1.0, c.start, ClipId(5)).is_none());
    }
}
