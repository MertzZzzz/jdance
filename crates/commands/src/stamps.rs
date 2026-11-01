// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano roll stamps (spec 0.3.5 §1–§5): one click places a chord, a
//! scale, a drum figure or a slide figure. The tables are written from
//! common music theory; the drum figures are jdance's own.

use crate::notes::Scale;
use jdance_domain::{Note, TICKS_PER_STEP};

/// A chord or scale: a name and semitones above the reference key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub name: &'static str,
    pub intervals: &'static [u8],
}

const fn shape(name: &'static str, intervals: &'static [u8]) -> Shape {
    Shape { name, intervals }
}

/// "Melodic — chords" (§2).
pub const CHORDS: &[Shape] = &[
    shape("Augmented", &[0, 4, 8]),
    shape("Diminished", &[0, 3, 6]),
    shape("Fifth", &[0, 7]),
    shape("Fifth 9th", &[0, 7, 14]),
    shape("Fifth Octave", &[0, 7, 12]),
    shape("Major", &[0, 4, 7]),
    shape("Major 7th", &[0, 4, 7, 11]),
    shape("Minor", &[0, 3, 7]),
    shape("Minor 7th", &[0, 3, 7, 10]),
    shape("Minor 9th", &[0, 3, 7, 10, 14]),
    shape("Octave", &[0, 12]),
    shape("Suspended 2", &[0, 2, 7]),
    shape("Suspended 4", &[0, 5, 7]),
];

/// The Advanced submenu (§2).
pub const ADVANCED_CHORDS: &[Shape] = &[
    shape("6th", &[0, 4, 7, 9]),
    shape("Minor 6th", &[0, 3, 7, 9]),
    shape("6/9", &[0, 4, 7, 9, 14]),
    shape("Dominant 7th", &[0, 4, 7, 10]),
    shape("7th sus4", &[0, 5, 7, 10]),
    shape("7th \u{266D}5", &[0, 4, 6, 10]),
    shape("7th \u{266F}5", &[0, 4, 8, 10]),
    shape("7th \u{266D}9", &[0, 4, 7, 10, 13]),
    shape("7th \u{266F}9", &[0, 4, 7, 10, 15]),
    shape("Half-diminished 7th", &[0, 3, 6, 10]),
    shape("Diminished 7th", &[0, 3, 6, 9]),
    shape("Minor-major 7th", &[0, 3, 7, 11]),
    shape("Add 9", &[0, 4, 7, 14]),
    shape("Minor add 9", &[0, 3, 7, 14]),
    shape("Dominant 9th", &[0, 4, 7, 10, 14]),
    shape("Major 9th", &[0, 4, 7, 11, 14]),
    shape("Dominant 11th", &[0, 4, 7, 10, 14, 17]),
    shape("Minor 11th", &[0, 3, 7, 10, 14, 17]),
    shape("Dominant 13th", &[0, 4, 7, 10, 14, 21]),
    shape("Major 13th", &[0, 4, 7, 11, 14, 21]),
];

/// "Melodic — scales" (§3): degrees within one octave.
pub const SCALES: &[Shape] = &[
    shape("Major (Ionian)", &[0, 2, 4, 5, 7, 9, 11]),
    shape("Major Bebop", &[0, 2, 4, 5, 7, 8, 9, 11]),
    shape("Major Hexatonic", &[0, 2, 4, 5, 7, 9]),
    shape("Major Pentatonic", &[0, 2, 4, 7, 9]),
    shape("Harmonic Major", &[0, 2, 4, 5, 7, 8, 11]),
    shape("Double Harmonic (Arabic)", &[0, 1, 4, 5, 7, 8, 11]),
    shape("Minor Natural (Aeolian)", &[0, 2, 3, 5, 7, 8, 10]),
    shape("Minor Harmonic", &[0, 2, 3, 5, 7, 8, 11]),
    shape("Minor Melodic", &[0, 2, 3, 5, 7, 9, 11]),
    shape("Minor Hungarian", &[0, 2, 3, 6, 7, 8, 11]),
    shape("Minor Neapolitan", &[0, 1, 3, 5, 7, 8, 11]),
    shape("Minor Pentatonic", &[0, 3, 5, 7, 10]),
    shape("Ukrainian Dorian", &[0, 2, 3, 6, 7, 9, 10]),
    shape("Dorian", &[0, 2, 3, 5, 7, 9, 10]),
    shape("Phrygian", &[0, 1, 3, 5, 7, 8, 10]),
    shape("Phrygian Dominant", &[0, 1, 4, 5, 7, 8, 10]),
    shape("Lydian", &[0, 2, 4, 6, 7, 9, 11]),
    shape("Mixolydian", &[0, 2, 4, 5, 7, 9, 10]),
    shape("Locrian", &[0, 1, 3, 5, 6, 8, 10]),
    shape("Super Locrian", &[0, 1, 3, 4, 6, 8, 10]),
    shape("Bebop Dominant", &[0, 2, 4, 5, 7, 9, 10, 11]),
    shape("Blues", &[0, 3, 5, 6, 7, 10]),
    shape("Diminished (whole-half)", &[0, 2, 3, 5, 6, 8, 9, 11]),
    shape("Whole Tone", &[0, 2, 4, 6, 8, 10]),
    shape("Neapolitan Major", &[0, 1, 3, 5, 7, 9, 11]),
    shape("Enigmatic", &[0, 1, 4, 6, 8, 10, 11]),
    shape("Persian", &[0, 1, 4, 5, 6, 8, 11]),
    shape("Prometheus", &[0, 2, 4, 6, 9, 10]),
    shape("Egyptian", &[0, 2, 5, 7, 10]),
    shape("Hirajoshi", &[0, 2, 3, 7, 8]),
    shape("Iwato", &[0, 1, 5, 6, 10]),
    shape("Japanese In-sen", &[0, 1, 5, 7, 10]),
];

/// "Fill each N steps" (§4).
pub const FILLS: [u32; 5] = [1, 2, 3, 4, 8];

/// Flam and roll speeds (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    Fast,
    Med,
    Slow,
}

impl Speed {
    pub const ALL: [Self; 3] = [Self::Fast, Self::Med, Self::Slow];

    pub fn name(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Med => "med",
            Self::Slow => "slow",
        }
    }

    /// The grace note before a flam.
    fn flam_ticks(self) -> u32 {
        match self {
            Self::Fast => TICKS_PER_STEP / 8,
            Self::Med => TICKS_PER_STEP / 4,
            Self::Slow => TICKS_PER_STEP / 2,
        }
    }

    /// The distance between roll repeats.
    fn roll_ticks(self) -> u32 {
        match self {
            Self::Fast => TICKS_PER_STEP / 4,
            Self::Med => TICKS_PER_STEP / 2,
            Self::Slow => TICKS_PER_STEP,
        }
    }
}

/// A stamp of the Piano Roll's Stamp menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamp {
    /// A triad of the roll's scale on the clicked degree.
    AutoChord,
    /// Index into [`CHORDS`].
    Chord(usize),
    /// Index into [`ADVANCED_CHORDS`].
    Advanced(usize),
    /// Index into [`SCALES`].
    Scale(usize),
    /// A note every N steps.
    Fill(u32),
    Flam(Speed),
    /// Repeats; `dynamic` rises in velocity.
    Roll {
        speed: Speed,
        dynamic: bool,
    },
    /// A note and a slide an octave up in its second half.
    SlideUp,
}

impl Stamp {
    pub fn name(self) -> String {
        match self {
            Self::AutoChord => "Automatic chord".to_owned(),
            Self::Chord(i) => CHORDS.get(i).map_or("?", |s| s.name).to_owned(),
            Self::Advanced(i) => ADVANCED_CHORDS.get(i).map_or("?", |s| s.name).to_owned(),
            Self::Scale(i) => SCALES.get(i).map_or("?", |s| s.name).to_owned(),
            Self::Fill(n) => format!("Fill each {n} steps"),
            Self::Flam(s) => format!("Flam {}", s.name()),
            Self::Roll { speed, dynamic } => format!(
                "Roll {} {}",
                speed.name(),
                if dynamic { "dynamic" } else { "relative" }
            ),
            Self::SlideUp => "Slide up".to_owned(),
        }
    }
}

/// Keys of the triad on `key` in `scale` (C major without one): the key
/// moved to the nearest degree, then every other degree above it.
pub fn auto_chord(key: u8, scale: Option<Scale>) -> Vec<u8> {
    let scale = scale.unwrap_or(Scale {
        tonic: 0,
        mode: crate::notes::ScaleMode::Major,
    });
    let root = scale.nearest(key);
    let mut keys = vec![root];
    let mut k = root;
    let mut degree = 0;
    while keys.len() < 3 && k < 127 {
        k += 1;
        if scale.contains(k) {
            degree += 1;
            if degree % 2 == 0 {
                keys.push(k);
            }
        }
    }
    keys
}

fn note(start: u32, length: u32, key: u8, velocity: f32) -> Note {
    Note {
        id: 0,
        start,
        length: length.max(1),
        key,
        velocity: velocity.clamp(0.0, 1.0),
        muted: false,
        slide: false,
    }
}

/// Note positions starting at `start` every `every` ticks while they start
/// inside `length`; each lasts `each` (cut at the figure's end).
fn repeats(start: u32, length: u32, every: u32, each: u32) -> Vec<(u32, u32)> {
    let every = every.max(1);
    let end = start.saturating_add(length.max(1));
    let mut out = Vec::new();
    let mut t = start;
    while t < end {
        out.push((t, each.min(end - t)));
        t = t.saturating_add(every);
    }
    out
}

/// The notes of `stamp` placed at `start` on `key` with the figure length
/// `length` (§1.4–1.6). Keys above 127 are dropped; slide notes only come
/// from [`Stamp::SlideUp`].
pub fn stamp(
    stamp: Stamp,
    start: u32,
    key: u8,
    length: u32,
    velocity: f32,
    scale: Option<Scale>,
) -> Vec<Note> {
    let length = length.max(1);
    let stack = |intervals: &[u8]| -> Vec<Note> {
        intervals
            .iter()
            .filter_map(|&i| key.checked_add(i).filter(|k| *k <= 127))
            .map(|k| note(start, length, k, velocity))
            .collect()
    };
    match stamp {
        Stamp::AutoChord => auto_chord(key, scale)
            .into_iter()
            .map(|k| note(start, length, k, velocity))
            .collect(),
        Stamp::Chord(i) => CHORDS.get(i).map_or_else(Vec::new, |s| stack(s.intervals)),
        Stamp::Advanced(i) => ADVANCED_CHORDS
            .get(i)
            .map_or_else(Vec::new, |s| stack(s.intervals)),
        Stamp::Scale(i) => SCALES.get(i).map_or_else(Vec::new, |s| stack(s.intervals)),
        Stamp::Fill(n) => repeats(start, length, n * TICKS_PER_STEP, TICKS_PER_STEP)
            .into_iter()
            .map(|(t, l)| note(t, l, key, velocity))
            .collect(),
        Stamp::Flam(speed) => {
            let d = speed.flam_ticks();
            let (grace, main) = match start.checked_sub(d) {
                Some(g) => (g, start),
                None => (start, start + d),
            };
            vec![
                note(grace, d, key, velocity * 0.6),
                note(main, length, key, velocity),
            ]
        }
        Stamp::Roll { speed, dynamic } => {
            let r = speed.roll_ticks();
            let hits = repeats(start, length, r, r);
            let last = hits.len().saturating_sub(1).max(1) as f32;
            hits.iter()
                .enumerate()
                .map(|(i, &(t, l))| {
                    let v = if dynamic {
                        velocity * (0.35 + 0.65 * i as f32 / last)
                    } else {
                        velocity
                    };
                    note(t, l, key, v)
                })
                .collect()
        }
        Stamp::SlideUp => {
            let half = length / 2;
            let mut out = vec![note(start, length, key, velocity)];
            if length - half > 0 {
                let mut s = note(
                    start + half,
                    length - half,
                    key.saturating_add(12).min(127),
                    velocity,
                );
                s.slide = true;
                out.push(s);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::ScaleMode;

    const S: u32 = TICKS_PER_STEP;

    fn keys(n: &[Note]) -> Vec<u8> {
        n.iter().map(|n| n.key).collect()
    }

    #[test]
    fn chords_stack_their_intervals_on_the_root() {
        let major = CHORDS.iter().position(|s| s.name == "Major").unwrap();
        let n = stamp(Stamp::Chord(major), 480, 60, S * 4, 0.8, None);
        assert_eq!(keys(&n), [60, 64, 67]);
        assert!(
            n.iter()
                .all(|n| n.start == 480 && n.length == S * 4 && !n.slide)
        );
        let m9 = CHORDS.iter().position(|s| s.name == "Minor 9th").unwrap();
        assert_eq!(
            keys(&stamp(Stamp::Chord(m9), 0, 57, S, 0.8, None)),
            [57, 60, 64, 67, 71]
        );
        let d13 = ADVANCED_CHORDS
            .iter()
            .position(|s| s.name == "Dominant 13th")
            .unwrap();
        assert_eq!(
            keys(&stamp(Stamp::Advanced(d13), 0, 48, S, 0.8, None)),
            [48, 52, 55, 58, 62, 69]
        );
    }

    #[test]
    fn every_shape_starts_on_its_root_and_rises() {
        for s in CHORDS.iter().chain(ADVANCED_CHORDS).chain(SCALES) {
            assert_eq!(s.intervals[0], 0, "{}", s.name);
            assert!(s.intervals.windows(2).all(|w| w[0] < w[1]), "{}", s.name);
        }
        for s in SCALES {
            assert!(*s.intervals.last().unwrap() < 12, "{}", s.name);
        }
    }

    #[test]
    fn keys_above_127_are_dropped() {
        let octave = CHORDS.iter().position(|s| s.name == "Octave").unwrap();
        assert_eq!(
            keys(&stamp(Stamp::Chord(octave), 0, 120, S, 0.8, None)),
            [120]
        );
    }

    #[test]
    fn scales_stack_one_octave() {
        let n = stamp(Stamp::Scale(0), 0, 60, S, 0.8, None);
        assert_eq!(keys(&n), [60, 62, 64, 65, 67, 69, 71]);
        let blues = SCALES.iter().position(|s| s.name == "Blues").unwrap();
        assert_eq!(
            keys(&stamp(Stamp::Scale(blues), 0, 57, S, 0.8, None)),
            [57, 60, 62, 63, 64, 67]
        );
    }

    #[test]
    fn automatic_chords_follow_the_scale() {
        // C major: D gives D F A; C# (not in it) goes down to C.
        assert_eq!(auto_chord(62, None), [62, 65, 69]);
        assert_eq!(auto_chord(61, None), [60, 64, 67]);
        let a_minor = Scale {
            tonic: 9,
            mode: ScaleMode::Minor,
        };
        assert_eq!(auto_chord(57, Some(a_minor)), [57, 60, 64]);
        assert_eq!(auto_chord(59, Some(a_minor)), [59, 62, 65], "B dim");
    }

    #[test]
    fn fills_put_a_step_note_every_n_steps() {
        let n = stamp(Stamp::Fill(2), S, 36, S * 8, 0.8, None);
        let starts: Vec<u32> = n.iter().map(|n| n.start).collect();
        assert_eq!(starts, [S, 3 * S, 5 * S, 7 * S]);
        assert!(n.iter().all(|n| n.length == S && n.key == 36));
        // At least one note, cut at the figure's end.
        let one = stamp(Stamp::Fill(8), 0, 36, S / 2, 0.8, None);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].length, S / 2);
    }

    #[test]
    fn flams_put_a_quieter_grace_note_before() {
        let n = stamp(Stamp::Flam(Speed::Med), S, 38, S, 0.8, None);
        assert_eq!((n[0].start, n[0].length), (S - S / 4, S / 4));
        assert!((n[0].velocity - 0.48).abs() < 1e-6);
        assert_eq!((n[1].start, n[1].length), (S, S));
        // No room before tick 0: the main note moves right.
        let n = stamp(Stamp::Flam(Speed::Slow), 0, 38, S, 0.8, None);
        assert_eq!((n[0].start, n[1].start), (0, S / 2));
    }

    #[test]
    fn rolls_repeat_with_or_without_a_crescendo() {
        let n = stamp(
            Stamp::Roll {
                speed: Speed::Fast,
                dynamic: true,
            },
            0,
            42,
            S * 2,
            1.0,
            None,
        );
        assert_eq!(n.len(), 8);
        assert!((n[0].velocity - 0.35).abs() < 1e-6);
        assert!((n[7].velocity - 1.0).abs() < 1e-6);
        assert!(n.windows(2).all(|w| w[1].velocity > w[0].velocity));
        let flat = stamp(
            Stamp::Roll {
                speed: Speed::Slow,
                dynamic: false,
            },
            0,
            42,
            S * 3,
            0.7,
            None,
        );
        assert_eq!(flat.len(), 3);
        assert!(flat.iter().all(|n| n.velocity == 0.7 && n.length == S));
    }

    #[test]
    fn slide_up_slides_an_octave_in_the_second_half() {
        let n = stamp(Stamp::SlideUp, 0, 48, S * 4, 0.8, None);
        assert_eq!(n.len(), 2);
        assert!(!n[0].slide && n[0].key == 48 && n[0].length == S * 4);
        assert!(n[1].slide && n[1].key == 60);
        assert_eq!((n[1].start, n[1].length), (S * 2, S * 2));
        assert_eq!(stamp(Stamp::SlideUp, 0, 120, S, 0.8, None)[1].key, 127);
    }

    #[test]
    fn names() {
        assert_eq!(Stamp::Fill(3).name(), "Fill each 3 steps");
        assert_eq!(
            Stamp::Roll {
                speed: Speed::Med,
                dynamic: false
            }
            .name(),
            "Roll med relative"
        );
        assert_eq!(Stamp::Chord(5).name(), "Major");
    }
}
