// SPDX-License-Identifier: GPL-3.0-or-later
//! Tools that make new notes (spec 0.2.0 §18.3, §18.4, §18.13, §18.16,
//! §18.19).

use super::{Input, Insertion, Mode, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArpDirection {
    #[default]
    Up,
    Down,
    UpDown,
    DownUp,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arpeggiate {
    pub direction: ArpDirection,
    /// Ticks per arpeggio note.
    pub step: u32,
    /// 1..=100 % of a step sounds.
    pub gate_percent: u32,
    /// 1..=4 octaves of the chord's keys.
    pub octaves: u8,
    pub seed: u64,
}

impl Default for Arpeggiate {
    fn default() -> Self {
        Self {
            direction: ArpDirection::Up,
            step: 240,
            gate_percent: 90,
            octaves: 1,
            seed: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordProgression {
    /// Pitch class of the tonic, 0 = C.
    pub tonic: u8,
    pub mode: Mode,
    /// Scale degrees 1..=7 in order.
    pub degrees: Vec<u8>,
    pub seventh: bool,
    /// Ticks per chord.
    pub chord_length: u32,
    /// Octave of the first chord's root (MIDI octave, C4 = 60 → 4).
    pub octave: i8,
    /// Inversion of the first chord: 0 = root position.
    pub inversion: u8,
    /// Voice leading: later chords take the inversion closest to the one
    /// before; off = every chord in `inversion`.
    pub close_voicing: bool,
    pub velocity: u8,
    pub insertion: Insertion,
}

impl Default for ChordProgression {
    fn default() -> Self {
        Self {
            tonic: 0,
            mode: Mode::Major,
            degrees: vec![1, 5, 6, 4],
            seventh: false,
            chord_length: 3840,
            octave: 4,
            inversion: 0,
            close_voicing: true,
            velocity: 100,
            insertion: Insertion::ReplaceRange,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RandomMode {
    #[default]
    Transform,
    Generate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GenScale {
    #[default]
    Major,
    Minor,
    Chromatic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Randomize {
    pub mode: RandomMode,
    pub seed: u64,
    // Transform: maximum deviations (0 = unchanged).
    pub key_range: u8,
    pub start_range: u32,
    pub length_range: u32,
    pub velocity_range: u8,
    /// Keep transformed keys in this scale (root pitch class, mode).
    pub scale: Option<(u8, Mode)>,
    // Generate.
    pub grid: u32,
    /// Probability of a note in each grid cell, 0..=1.
    pub density: f32,
    pub key_min: u8,
    pub key_max: u8,
    pub gen_scale: GenScale,
    pub root: u8,
    /// Most notes sounding at once.
    pub polyphony: u8,
    pub length_min: u32,
    pub length_max: u32,
    pub velocity_min: u8,
    pub velocity_max: u8,
    pub insertion: Insertion,
}

impl Default for Randomize {
    fn default() -> Self {
        Self {
            mode: RandomMode::Transform,
            seed: 1,
            key_range: 0,
            start_range: 0,
            length_range: 0,
            velocity_range: 0,
            scale: None,
            grid: 240,
            density: 0.5,
            key_min: 60,
            key_max: 72,
            gen_scale: GenScale::Major,
            root: 0,
            polyphony: 1,
            length_min: 240,
            length_max: 240,
            velocity_min: 80,
            velocity_max: 110,
            insertion: Insertion::ReplaceRange,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClawMode {
    #[default]
    Remove,
    Gate,
    Shift,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClawMachine {
    /// Ticks per period.
    pub period: u32,
    pub slices: u32,
    /// Every Nth slice (counted from 1 after the phase) is marked.
    pub every: u32,
    pub phase: u32,
    pub mode: ClawMode,
    /// Shift of marked fragments, ticks.
    pub shift: i32,
}

impl Default for ClawMachine {
    fn default() -> Self {
        Self {
            period: 960,
            slices: 4,
            every: 2,
            phase: 0,
            mode: ClawMode::Remove,
            shift: 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiffMachine {
    pub root: u8,
    pub mode: Mode,
    pub key_min: u8,
    pub key_max: u8,
    /// Rhythm grid in ticks.
    pub division: u32,
    /// 0..=100 % of grid cells get an onset.
    pub density: u32,
    /// Chord degrees 1..=7 (one entry = a fixed chord).
    pub progression: Vec<u8>,
    /// Ticks per chord of the progression.
    pub chord_length: u32,
    pub arp: ArpDirection,
    pub octaves: u8,
    /// 1..=100 % of a cell sounds.
    pub gate_percent: u32,
    pub velocity: u8,
    pub velocity_variation: u8,
    /// Random timing offset, ±ticks.
    pub timing: u32,
    pub seed: u64,
    pub insertion: Insertion,
}

impl Default for RiffMachine {
    fn default() -> Self {
        Self {
            root: 0,
            mode: Mode::Minor,
            key_min: 48,
            key_max: 72,
            division: 240,
            density: 70,
            progression: vec![1, 6, 4, 5],
            chord_length: 3840,
            arp: ArpDirection::UpDown,
            octaves: 2,
            gate_percent: 80,
            velocity: 100,
            velocity_variation: 10,
            timing: 0,
            seed: 1,
            insertion: Insertion::ReplaceRange,
        }
    }
}

fn todo(name: &str) -> Result<Outcome, String> {
    Err(format!("{name} is not available yet"))
}

pub fn arpeggiate(_input: &Input, _s: &Arpeggiate) -> Result<Outcome, String> {
    todo("Arpeggiate")
}

pub fn chord_progression(_input: &Input, _s: &ChordProgression) -> Result<Outcome, String> {
    todo("Chord progression")
}

pub fn randomize(_input: &Input, _s: &Randomize) -> Result<Outcome, String> {
    todo("Randomize")
}

pub fn claw_machine(_input: &Input, _s: &ClawMachine) -> Result<Outcome, String> {
    todo("Claw machine")
}

pub fn riff_machine(_input: &Input, _s: &RiffMachine) -> Result<Outcome, String> {
    todo("Riff machine")
}
