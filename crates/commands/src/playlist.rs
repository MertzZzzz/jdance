// SPDX-License-Identifier: GPL-3.0-or-later
//! Pattern and playlist edits (spec "Playlist").
//!
//! Structural edits (adding/removing patterns or lanes, Make unique) are
//! validated against the whole arrangement and undone with a snapshot of
//! the patterns and the playlist, like mixer structure edits.

use crate::{Edit, EditError, History};
use jdance_domain::{
    ClipId, LANE_COLORS, Lane, LaneId, MAX_PATTERN_STEPS, Pattern, PatternClip, PatternId,
    Playlist, Project, Step, TICKS_PER_STEP,
};

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
            p.playlist.lanes.remove(i);
            p.playlist.clips.retain(|c| c.lane != *id);
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
            p.playlist.clips.push(*clip);
            if let Err(e) = p.validate_arrangement() {
                p.playlist.clips.pop();
                return Err(arrangement_err(e));
            }
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
        Edit::SetClipRepeat(id, n) => {
            if *n == 0 || *n > jdance_domain::MAX_REPEAT {
                return Err(EditError::OutOfRange(f64::from(*n)));
            }
            let c = clip_mut(p, *id)?;
            Ok(Edit::SetClipRepeat(
                *id,
                std::mem::replace(&mut c.repeat, *n),
            ))
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
    history.execute(
        p,
        Edit::AddPattern {
            pattern: Pattern::new(id, name),
            index,
        },
    )?;
    Ok(id)
}

/// Adds a lane at the end with the next default color.
pub fn add_lane(history: &mut History, p: &mut Project) -> Result<LaneId, EditError> {
    let id = LaneId(p.ids.next());
    let n = p.playlist.lanes.len();
    history.execute(
        p,
        Edit::AddLane {
            lane: Lane {
                id,
                name: format!("Lane {}", n + 1),
                color: LANE_COLORS[n % LANE_COLORS.len()],
                muted: false,
                solo: false,
            },
            index: n,
        },
    )?;
    Ok(id)
}

/// Creates a one-repeat clip of `pattern` on `lane` at `start` ticks.
pub fn add_clip(
    history: &mut History,
    p: &mut Project,
    lane: LaneId,
    pattern: PatternId,
    start: u64,
) -> Result<ClipId, EditError> {
    let id = ClipId(p.ids.next());
    history.execute(
        p,
        Edit::AddClip(PatternClip {
            id,
            lane,
            pattern,
            start,
            repeat: 1,
            muted: false,
        }),
    )?;
    Ok(id)
}

/// Duplicates clips right after the end of the selection, preserving
/// their relative layout; new clip IDs, same patterns. One undo step.
pub fn duplicate_clips(
    history: &mut History,
    p: &mut Project,
    ids: &[ClipId],
) -> Result<Vec<ClipId>, EditError> {
    let clips: Vec<PatternClip> = ids
        .iter()
        .filter_map(|id| p.playlist.clip(*id).copied())
        .collect();
    if clips.is_empty() {
        return Ok(Vec::new());
    }
    let first = clips.iter().map(|c| c.start).min().unwrap_or(0);
    let end = clips
        .iter()
        .filter_map(|c| p.pattern(c.pattern).map(|pat| c.interval(pat).1))
        .max()
        .unwrap_or(first);
    let offset = end - first;
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
    history.execute(p, Edit::Batch(edits))?;
    Ok(new_ids)
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
        h.execute(&mut p, Edit::SetClipRepeat(c, 4)).unwrap();
        let clip = *p.playlist.clip(c).unwrap();
        assert_eq!((clip.start, clip.lane, clip.repeat), (7_680, lane2, 4));
        assert!(h.execute(&mut p, Edit::SetClipRepeat(c, 0)).is_err());
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
        h.execute(&mut p, Edit::SetClipRepeat(c, 3)).unwrap();
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
        assert_eq!(p.playlist.clip(c).unwrap().repeat, 3, "repeat count kept");
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
}
