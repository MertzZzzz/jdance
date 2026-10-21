// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano Roll Tools (spec 0.2.0 §18): pure note processing, independent
//! of the UI and audio. A tool reads an immutable snapshot of one
//! channel's notes in the current pattern and returns the whole new note
//! list plus what to select; the editor previews it and applies it as one
//! undo step (`Edit::SetNotes`).
//!
//! Contract (§18.2):
//! - scope: the selected notes, else all notes of the row; generators use
//!   the time range, else the whole pattern; empty input is a no-op;
//! - integer ticks (PPQ 960), length ≥ 1, key 0..=127, MIDI velocity
//!   1..=127 (stored as `velocity / 127`);
//! - changed notes keep their ID, new notes get new IDs (a split keeps the
//!   ID on its first part);
//! - notes outside the scope are untouched;
//! - a note whose start leaves the pattern is not dropped silently: it is
//!   counted in [`Outcome::dropped`] and the editor asks before applying;
//!   lengths are cut at the pattern end;
//! - random tools take a `u64` seed and use [`Rng`] (SplitMix64, version
//!   [`RNG_VERSION`]) over input in a stable order (start, key, ID);
//! - at most [`MAX_RESULT_NOTES`] notes per result, else nothing changes.

mod generate;
mod lfo;
mod transform;

pub use generate::*;
pub use lfo::*;
pub use transform::*;

use jdance_domain::{Note, PPQ, assign_note_ids, sort_notes};

/// Largest note list one tool may produce.
pub const MAX_RESULT_NOTES: usize = 100_000;
/// PRNG algorithm version of [`Rng`]; bump when its output changes.
pub const RNG_VERSION: u32 = 1;
/// Settings schema version of the tools in this module.
pub const SETTINGS_VERSION: u32 = 1;
pub const TICKS_PER_BEAT: u32 = PPQ;
pub const TICKS_PER_BAR: u32 = 4 * PPQ;

/// `[start, end)` in pattern ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

impl Range {
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub fn contains(&self, t: u32) -> bool {
        (self.start..self.end).contains(&t)
    }
}

/// What a tool works on.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    /// Every note of the current channel in the current pattern.
    pub notes: &'a [Note],
    /// Indices into `notes`; empty = every note.
    pub selected: &'a [usize],
    /// Pattern length in ticks.
    pub pattern_len: u32,
    /// Grid step in ticks for quick tools (the last explicit snap, default
    /// a 1/16 step).
    pub grid: u32,
    /// Selected time range for generators; `None` = the whole pattern.
    pub range: Option<Range>,
}

impl Input<'_> {
    /// Scope indices in stable order: start, key, ID.
    pub fn scope(&self) -> Vec<usize> {
        let mut v: Vec<usize> = if self.selected.is_empty() {
            (0..self.notes.len()).collect()
        } else {
            self.selected
                .iter()
                .copied()
                .filter(|&i| i < self.notes.len())
                .collect()
        };
        v.sort_by_key(|&i| {
            let n = &self.notes[i];
            (n.start, n.key, n.id, i)
        });
        v.dedup();
        v
    }

    /// The generator range: the selection's time range, else the pattern.
    pub fn gen_range(&self) -> Range {
        let r = self.range.unwrap_or(Range {
            start: 0,
            end: self.pattern_len,
        });
        Range {
            start: r.start.min(self.pattern_len),
            end: r
                .end
                .min(self.pattern_len)
                .max(r.start.min(self.pattern_len)),
        }
    }

    pub fn grid(&self) -> u32 {
        if self.grid == 0 { PPQ / 4 } else { self.grid }
    }
}

/// A note as computed by a tool, before the boundary checks: signed
/// positions, MIDI velocity. `id` 0 = new note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cand {
    pub id: u32,
    pub start: i64,
    pub length: i64,
    pub key: i32,
    /// MIDI velocity; clamped to 1..=127.
    pub velocity: i32,
}

impl Cand {
    pub fn of(n: &Note) -> Self {
        Self {
            id: n.id,
            start: i64::from(n.start),
            length: i64::from(n.length),
            key: i32::from(n.key),
            velocity: i32::from(midi_velocity(n.velocity)),
        }
    }

    /// The same note as a new one (new ID).
    pub fn fresh(mut self) -> Self {
        self.id = 0;
        self
    }

    pub fn end(&self) -> i64 {
        self.start + self.length
    }
}

/// MIDI velocity 1..=127 of a stored velocity.
pub fn midi_velocity(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 127.0).round().clamp(1.0, 127.0) as u8
}

/// Stored velocity of a MIDI velocity.
pub fn stored_velocity(v: i32) -> f32 {
    v.clamp(1, 127) as f32 / 127.0
}

/// A tool's result: the whole new row and what to select.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    /// Sorted, IDs assigned.
    pub notes: Vec<Note>,
    /// Indices into `notes` of the changed and created notes.
    pub select: Vec<usize>,
    /// Notes left out because they would start outside the pattern; the
    /// editor asks before applying.
    pub dropped: usize,
    /// Notes whose key was clamped to 0..=127.
    pub clamped: usize,
    /// Lines shown in the preview (what changes, mismatches, …).
    pub info: Vec<String>,
}

/// Selection-only tools return the indices to select.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SelectionOutcome {
    pub select: Vec<usize>,
    pub info: Vec<String>,
}

/// The row with the notes at `removed` replaced by `new`, after the
/// boundary checks of §18.2.
pub fn replace(input: &Input, removed: &[usize], new: Vec<Cand>) -> Result<Outcome, String> {
    let mut gone = vec![false; input.notes.len()];
    for &i in removed {
        if let Some(g) = gone.get_mut(i) {
            *g = true;
        }
    }
    let kept: Vec<Note> = input
        .notes
        .iter()
        .zip(&gone)
        .filter(|(_, g)| !**g)
        .map(|(n, _)| *n)
        .collect();
    if kept.len() + new.len() > MAX_RESULT_NOTES {
        return Err(format!(
            "the result would have {} notes; at most {MAX_RESULT_NOTES} are allowed",
            kept.len() + new.len()
        ));
    }
    let end = i64::from(input.pattern_len.max(1));
    let (mut dropped, mut clamped) = (0, 0);
    let mut made = Vec::with_capacity(new.len());
    for c in new {
        if c.start < 0 || c.start >= end || c.length < 1 {
            dropped += 1;
            continue;
        }
        if !(0..=127).contains(&c.key) {
            clamped += 1;
        }
        let length = c.length.min(end - c.start).max(1);
        made.push(Note {
            id: c.id,
            start: c.start as u32,
            length: length as u32,
            key: c.key.clamp(0, 127) as u8,
            velocity: stored_velocity(c.velocity),
        });
    }
    // A kept ID may not appear twice (a split keeps it on its first part).
    let mut all: Vec<(Note, bool)> = kept.into_iter().map(|n| (n, false)).collect();
    all.extend(made.into_iter().map(|n| (n, true)));
    let mut notes: Vec<Note> = all.iter().map(|(n, _)| *n).collect();
    assign_note_ids(&mut notes);
    let mut tagged: Vec<(Note, bool)> =
        notes.into_iter().zip(all.iter().map(|(_, m)| *m)).collect();
    tagged.sort_by_key(|(n, _)| (n.start, n.key, n.id));
    let select = tagged
        .iter()
        .enumerate()
        .filter(|(_, (_, m))| *m)
        .map(|(i, _)| i)
        .collect();
    let mut notes: Vec<Note> = tagged.into_iter().map(|(n, _)| n).collect();
    sort_notes(&mut notes);
    let mut info = Vec::new();
    if dropped > 0 {
        info.push(format!(
            "{dropped} note(s) would start outside the pattern and are left out"
        ));
    }
    if clamped > 0 {
        info.push(format!("{clamped} note(s) clamped to the key range"));
    }
    Ok(Outcome {
        notes,
        select,
        dropped,
        clamped,
        info,
    })
}

/// Onset groups of `scope` (indices in stable order): notes with the
/// same start.
pub fn onset_groups(input: &Input, scope: &[usize]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in scope {
        let s = input.notes[i].start;
        match groups.last_mut() {
            Some(g) if input.notes[g[0]].start == s => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    groups
}

/// Nearest grid point of `t` (ties toward the later one).
pub fn snap(t: i64, grid: u32) -> i64 {
    let g = i64::from(grid.max(1));
    let r = t.rem_euclid(g);
    let down = t - r;
    if 2 * r >= g { down + g } else { down }
}

/// SplitMix64 (version [`RNG_VERSION`]): small, fast and pinned, so the
/// same seed gives the same notes on every platform and release.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in `lo..=hi` (`lo` when `hi < lo`).
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        let span = (hi - lo) as u64 + 1;
        lo + (self.next_u64() % span) as i64
    }
}

/// Major or natural minor (spec §18.3/§18.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Major,
    Minor,
}

impl Mode {
    pub fn intervals(self) -> [u8; 7] {
        match self {
            Self::Major => [0, 2, 4, 5, 7, 9, 11],
            Self::Minor => [0, 2, 3, 5, 7, 8, 10],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Major => "Major",
            Self::Minor => "Natural minor",
        }
    }
}

/// Key of scale degree `degree` (0-based, may exceed 6 or be negative)
/// above `root`.
pub fn degree_key(root: i32, mode: Mode, degree: i32) -> i32 {
    let iv = mode.intervals();
    let oct = degree.div_euclid(7);
    root + 12 * oct + i32::from(iv[degree.rem_euclid(7) as usize])
}

/// Whether `key` is in the scale of `root` (pitch class) and `mode`.
pub fn in_scale(key: i32, root: i32, mode: Mode) -> bool {
    let pc = (key - root).rem_euclid(12) as u8;
    mode.intervals().contains(&pc)
}

/// How new notes join the row (spec §18.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Insertion {
    /// Remove the row's notes that start in the range, then add.
    #[default]
    ReplaceRange,
    /// Keep every note.
    AppendLayer,
}

/// Indices of the row's notes starting in `r` (removed by Replace range).
pub fn starting_in(input: &Input, r: Range) -> Vec<usize> {
    (0..input.notes.len())
        .filter(|&i| r.contains(input.notes[i].start))
        .collect()
}

/// Every tool with its settings (§18.3–§18.21). Script tools are run by
/// the script host and return the same [`Outcome`].
#[derive(Debug, Clone, PartialEq)]
pub enum Tool {
    QuickLegato,
    QuickQuantize,
    QuickQuantizeStarts,
    QuickChop,
    Glue(Glue),
    Flip(Flip),
    Articulate(Articulate),
    Quantize(Quantize),
    Chop(Chop),
    Strum(Strum),
    Flam(Flam),
    Limit(Limit),
    ScaleLevels(ScaleLevels),
    Arpeggiate(Arpeggiate),
    ChordProgression(ChordProgression),
    Randomize(Randomize),
    ClawMachine(ClawMachine),
    RiffMachine(RiffMachine),
}

impl Tool {
    pub fn name(&self) -> &'static str {
        match self {
            Self::QuickLegato => "Quick legato",
            Self::QuickQuantize => "Quick quantize",
            Self::QuickQuantizeStarts => "Quick quantize start times",
            Self::QuickChop => "Quick chop",
            Self::Glue(_) => "Glue",
            Self::Flip(_) => "Flip",
            Self::Articulate(_) => "Articulate",
            Self::Quantize(_) => "Quantize",
            Self::Chop(_) => "Chop",
            Self::Strum(_) => "Strum",
            Self::Flam(_) => "Flam",
            Self::Limit(_) => "Limit",
            Self::ScaleLevels(_) => "Scale levels",
            Self::Arpeggiate(_) => "Arpeggiate",
            Self::ChordProgression(_) => "Generate chord progression",
            Self::Randomize(_) => "Randomize",
            Self::ClawMachine(_) => "Claw machine",
            Self::RiffMachine(_) => "Riff machine",
        }
    }

    /// Generators make notes from nothing; the others transform the scope
    /// (empty scope = no-op).
    pub fn is_generator(&self) -> bool {
        matches!(
            self,
            Self::ChordProgression(_)
                | Self::RiffMachine(_)
                | Self::Randomize(Randomize {
                    mode: RandomMode::Generate,
                    ..
                })
        )
    }
}

/// Runs `tool` on `input`. `Ok(None)` = nothing to do (empty scope).
pub fn run(tool: &Tool, input: &Input) -> Result<Option<Outcome>, String> {
    if !tool.is_generator() && input.scope().is_empty() {
        return Ok(None);
    }
    let out = match tool {
        Tool::QuickLegato => quick_legato(input),
        Tool::QuickQuantize => quick_quantize(input, true),
        Tool::QuickQuantizeStarts => quick_quantize(input, false),
        Tool::QuickChop => quick_chop(input),
        Tool::Glue(s) => glue(input, s),
        Tool::Flip(s) => flip(input, s),
        Tool::Articulate(s) => articulate(input, s),
        Tool::Quantize(s) => quantize(input, s),
        Tool::Chop(s) => chop(input, s),
        Tool::Strum(s) => strum(input, s),
        Tool::Flam(s) => flam(input, s),
        Tool::Limit(s) => limit(input, s),
        Tool::ScaleLevels(s) => scale_levels(input, s),
        Tool::Arpeggiate(s) => arpeggiate(input, s),
        Tool::ChordProgression(s) => chord_progression(input, s),
        Tool::Randomize(s) => randomize(input, s),
        Tool::ClawMachine(s) => claw_machine(input, s),
        Tool::RiffMachine(s) => riff_machine(input, s),
    }?;
    Ok(Some(out))
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    pub fn n(id: u32, start: u32, length: u32, key: u8, vel: u8) -> Note {
        Note {
            id,
            start,
            length,
            key,
            velocity: stored_velocity(i32::from(vel)),
        }
    }

    pub fn input<'a>(notes: &'a [Note], selected: &'a [usize]) -> Input<'a> {
        Input {
            notes,
            selected,
            pattern_len: TICKS_PER_BAR,
            grid: PPQ / 4,
            range: None,
        }
    }

    /// (start, length, key) of every note, for compact assertions.
    pub fn shape(o: &Outcome) -> Vec<(u32, u32, u8)> {
        o.notes.iter().map(|n| (n.start, n.length, n.key)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;

    #[test]
    fn replace_keeps_ids_assigns_new_and_checks_bounds() {
        let notes = [n(1, 0, 240, 60, 100), n(2, 480, 240, 62, 100)];
        let inp = input(&notes, &[]);
        let new = vec![
            Cand::of(&notes[0]),
            Cand {
                start: 3000,
                length: 5000,
                ..Cand::of(&notes[1]).fresh()
            },
            Cand {
                start: -10,
                ..Cand::of(&notes[1]).fresh()
            },
            Cand {
                key: 200,
                ..Cand::of(&notes[1])
            },
        ];
        let o = replace(&inp, &[0, 1], new).unwrap();
        assert_eq!(o.dropped, 1, "start before the pattern");
        assert_eq!(o.clamped, 1);
        assert_eq!(shape(&o), [(0, 240, 60), (480, 240, 127), (3000, 840, 62)]);
        let ids: Vec<u32> = o.notes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [1, 2, 3], "kept IDs, a new one for the new note");
        assert_eq!(o.select.len(), 3);
    }

    #[test]
    fn rng_is_pinned() {
        let mut r = Rng::new(42);
        // Golden values of SplitMix64 (RNG_VERSION 1).
        assert_eq!(r.next_u64(), 13_679_457_532_755_275_413);
        assert_eq!(r.next_u64(), 2_949_826_092_126_892_291);
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert!((0..100).all(|_| a.range(-3, 3) == b.range(-3, 3)));
    }

    #[test]
    fn snap_ties_go_later_and_scales() {
        assert_eq!(snap(120, 240), 240);
        assert_eq!(snap(119, 240), 0);
        assert_eq!(snap(-130, 240), -240);
        assert_eq!(degree_key(60, Mode::Major, 7), 72);
        assert_eq!(degree_key(57, Mode::Minor, 2), 60);
        assert_eq!(degree_key(60, Mode::Major, -1), 59);
        assert!(in_scale(61, 1, Mode::Major) && !in_scale(61, 0, Mode::Major));
        assert_eq!(midi_velocity(stored_velocity(100)), 100);
        assert_eq!(midi_velocity(0.0), 1);
    }
}
