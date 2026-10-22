// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano roll note tools (spec "DAW UI clean room", §6): quantize,
//! humanize, transpose, scales, duplicate. Each returns the whole new note
//! list for one `Edit::SetNotes`; `selected` holds indices into `notes`
//! (empty = every note). These are our own simple algorithms.

use jdance_domain::{MAX_PATTERN_STEPS, Note, TICKS_PER_STEP};

const LAST_START: u32 = MAX_PATTERN_STEPS * TICKS_PER_STEP;

fn chosen(notes: &[Note], selected: &[usize]) -> Vec<usize> {
    if selected.is_empty() {
        (0..notes.len()).collect()
    } else {
        selected
            .iter()
            .copied()
            .filter(|&i| i < notes.len())
            .collect()
    }
}

fn round_to(t: u32, grid: u32) -> u32 {
    if grid == 0 {
        return t;
    }
    ((t + grid / 2) / grid) * grid
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantize {
    /// Grid in ticks.
    pub grid: u32,
    /// `0.0..=1.0`: how far notes move towards the grid.
    pub strength: f32,
    pub starts: bool,
    /// Quantize note ends (lengths) too.
    pub ends: bool,
}

impl Default for Quantize {
    fn default() -> Self {
        Self {
            grid: TICKS_PER_STEP,
            strength: 1.0,
            starts: true,
            ends: false,
        }
    }
}

fn toward(from: u32, to: u32, strength: f32) -> u32 {
    let d = f64::from(to) - f64::from(from);
    (f64::from(from) + d * f64::from(strength.clamp(0.0, 1.0))).round() as u32
}

pub fn quantize(notes: &[Note], selected: &[usize], q: Quantize) -> Vec<Note> {
    let mut out = notes.to_vec();
    for i in chosen(notes, selected) {
        let n = &mut out[i];
        let end = n.end();
        if q.starts {
            n.start = toward(n.start, round_to(n.start, q.grid), q.strength).min(LAST_START);
        }
        let new_end = if q.ends {
            let target = round_to(end, q.grid).max(n.start + q.grid.max(1));
            toward(end, target, q.strength)
        } else {
            // Starts move, lengths stay.
            n.start + (end - notes[i].start)
        };
        n.length = new_end.saturating_sub(n.start).max(1);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Humanize {
    /// Maximum shift of starts, ± ticks.
    pub time: u32,
    /// Maximum change of velocity, ± (0..1).
    pub velocity: f32,
    pub seed: u64,
}

impl Default for Humanize {
    fn default() -> Self {
        Self {
            time: 20,
            velocity: 0.1,
            seed: 1,
        }
    }
}

/// xorshift64*: small, deterministic for a given seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// Uniform in `-1.0..=1.0`.
    fn signed(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        v as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

pub fn humanize(notes: &[Note], selected: &[usize], h: Humanize) -> Vec<Note> {
    let mut rng = Rng::new(h.seed);
    let mut out = notes.to_vec();
    for i in chosen(notes, selected) {
        let n = &mut out[i];
        let dt = (rng.signed() * f64::from(h.time)).round() as i64;
        n.start = (i64::from(n.start) + dt).clamp(0, i64::from(LAST_START)) as u32;
        let dv = rng.signed() as f32 * h.velocity;
        n.velocity = ((n.velocity + dv).clamp(0.01, 1.0) * 100.0).round() / 100.0;
    }
    out
}

/// `None` when a note would leave the MIDI range.
pub fn transpose(notes: &[Note], selected: &[usize], semitones: i32) -> Option<Vec<Note>> {
    let mut out = notes.to_vec();
    for i in chosen(notes, selected) {
        let k = i32::from(out[i].key) + semitones;
        out[i].key = u8::try_from(k).ok().filter(|k| *k <= 127)?;
    }
    Some(out)
}

/// The selection repeated right after itself, shifted by its span rounded
/// up to whole beats of `beat` ticks. Returns the new list and the indices
/// of the copies.
pub fn duplicate(notes: &[Note], selected: &[usize], beat: u32) -> (Vec<Note>, Vec<usize>) {
    let idx = chosen(notes, selected);
    let (Some(first), Some(last)) = (
        idx.iter().map(|&i| notes[i].start).min(),
        idx.iter().map(|&i| notes[i].end()).max(),
    ) else {
        return (notes.to_vec(), Vec::new());
    };
    let span = (last - first).div_ceil(beat).max(1) * beat;
    paste(
        notes,
        &idx.iter().map(|&i| notes[i]).collect::<Vec<_>>(),
        first + span,
    )
}

/// `clip` placed so its first note starts at `at`; notes past the longest
/// pattern are dropped.
pub fn paste(notes: &[Note], clip: &[Note], at: u32) -> (Vec<Note>, Vec<usize>) {
    let mut out = notes.to_vec();
    let mut added = Vec::new();
    let Some(first) = clip.iter().map(|n| n.start).min() else {
        return (out, added);
    };
    for n in clip {
        let start = at + (n.start - first);
        if start <= LAST_START {
            added.push(out.len());
            out.push(Note { start, ..*n });
        }
    }
    (out, added)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleMode {
    Major,
    Minor,
    HarmonicMinor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    PentatonicMajor,
    PentatonicMinor,
    Blues,
}

impl ScaleMode {
    pub const ALL: [Self; 11] = [
        Self::Major,
        Self::Minor,
        Self::HarmonicMinor,
        Self::Dorian,
        Self::Phrygian,
        Self::Lydian,
        Self::Mixolydian,
        Self::Locrian,
        Self::PentatonicMajor,
        Self::PentatonicMinor,
        Self::Blues,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Major => "Major",
            Self::Minor => "Minor",
            Self::HarmonicMinor => "Harmonic minor",
            Self::Dorian => "Dorian",
            Self::Phrygian => "Phrygian",
            Self::Lydian => "Lydian",
            Self::Mixolydian => "Mixolydian",
            Self::Locrian => "Locrian",
            Self::PentatonicMajor => "Major pentatonic",
            Self::PentatonicMinor => "Minor pentatonic",
            Self::Blues => "Blues",
        }
    }

    /// Semitones above the tonic.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Self::Major => &[0, 2, 4, 5, 7, 9, 11],
            Self::Minor => &[0, 2, 3, 5, 7, 8, 10],
            Self::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            Self::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Self::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            Self::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            Self::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            Self::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            Self::PentatonicMajor => &[0, 2, 4, 7, 9],
            Self::PentatonicMinor => &[0, 3, 5, 7, 10],
            Self::Blues => &[0, 3, 5, 6, 7, 10],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale {
    /// Pitch class of the tonic, `0` = C.
    pub tonic: u8,
    pub mode: ScaleMode,
}

impl Scale {
    pub fn contains(self, key: u8) -> bool {
        let pc = (key + 12 - self.tonic % 12) % 12;
        self.mode.intervals().contains(&pc)
    }

    /// The nearest key in the scale (downwards on ties).
    pub fn nearest(self, key: u8) -> u8 {
        (0..12u8)
            .flat_map(|d| [key.checked_sub(d), key.checked_add(d).filter(|k| *k <= 127)])
            .flatten()
            .find(|k| self.contains(*k))
            .unwrap_or(key)
    }
}

/// Moves notes outside `scale` to the nearest key inside it.
pub fn fit_to_scale(notes: &[Note], selected: &[usize], scale: Scale) -> Vec<Note> {
    let mut out = notes.to_vec();
    for i in chosen(notes, selected) {
        out[i].key = scale.nearest(out[i].key);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(start: u32, length: u32, key: u8) -> Note {
        Note {
            id: 0,
            start,
            length,
            key,
            velocity: 0.8,
            muted: false,
        }
    }

    #[test]
    fn quantize_starts_lengths_and_strength() {
        let notes = [n(250, 100, 60), n(470, 200, 62)];
        let q = quantize(&notes, &[], Quantize::default());
        assert_eq!((q[0].start, q[0].length), (240, 100), "length kept");
        assert_eq!(q[1].start, 480);
        let half = quantize(
            &notes,
            &[1],
            Quantize {
                strength: 0.5,
                ..Quantize::default()
            },
        );
        assert_eq!(half[0], notes[0], "only the selection");
        assert_eq!(half[1].start, 475);
        let ends = quantize(
            &notes,
            &[],
            Quantize {
                ends: true,
                ..Quantize::default()
            },
        );
        assert_eq!(ends[0].end(), 480, "350 → 480 (at least one grid)");
        assert_eq!(ends[1].end(), 720, "670 → 720");
    }

    #[test]
    fn humanize_is_deterministic_and_bounded() {
        let notes: Vec<Note> = (0..50).map(|i| n(1000 + i * 240, 120, 60)).collect();
        let h = Humanize {
            time: 30,
            velocity: 0.2,
            seed: 7,
        };
        let a = humanize(&notes, &[], h);
        assert_eq!(a, humanize(&notes, &[], h), "same seed, same result");
        assert_ne!(a, humanize(&notes, &[], Humanize { seed: 8, ..h }));
        for (o, x) in notes.iter().zip(&a) {
            assert!(o.start.abs_diff(x.start) <= 30);
            assert!((o.velocity - x.velocity).abs() <= 0.2 + 1e-6);
            assert_eq!(o.length, x.length);
        }
        assert!(
            notes.iter().zip(&a).any(|(o, x)| o.start != x.start),
            "moves notes"
        );
    }

    #[test]
    fn transpose_keeps_the_midi_range() {
        let notes = [n(0, 10, 60), n(0, 10, 120)];
        let up = transpose(&notes, &[0], 12).unwrap();
        assert_eq!((up[0].key, up[1].key), (72, 120));
        assert!(transpose(&notes, &[], 8).is_none(), "128 is out of range");
        assert!(transpose(&notes, &[0], -61).is_none());
    }

    #[test]
    fn duplicate_and_paste_keep_intervals() {
        let notes = [n(0, 240, 60), n(480, 240, 64), n(5000, 10, 70)];
        let (d, added) = duplicate(&notes, &[0, 1], jdance_domain::PPQ);
        assert_eq!(added, [3, 4]);
        assert_eq!((d[3].start, d[4].start), (960, 1440), "span 720 → one beat");
        let (p, added) = paste(&notes, &[notes[1], notes[2]], 100);
        assert_eq!((p[added[0]].start, p[added[1]].start), (100, 4620));
    }

    #[test]
    fn scales() {
        let a_minor = Scale {
            tonic: 9,
            mode: ScaleMode::Minor,
        };
        assert!(a_minor.contains(69) && a_minor.contains(60) && !a_minor.contains(61));
        assert_eq!(a_minor.nearest(61), 60);
        assert_eq!(a_minor.nearest(66), 65, "F# → F (downwards on ties)");
        let fitted = fit_to_scale(&[n(0, 1, 61), n(0, 1, 62)], &[], a_minor);
        assert_eq!((fitted[0].key, fitted[1].key), (60, 62));
        for m in ScaleMode::ALL {
            assert_eq!(m.intervals()[0], 0, "{}", m.name());
        }
    }
}
