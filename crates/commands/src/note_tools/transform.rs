// SPDX-License-Identifier: GPL-3.0-or-later
//! Transforming tools (spec 0.2.0 §18.5–§18.12, §18.14–§18.18, §18.20):
//! they change the notes in scope and leave the rest alone.

use super::{Input, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Glue {
    /// Notes of one key this close (ticks) or closer join; 0 = touching
    /// or overlapping only.
    pub gap: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flip {
    pub horizontal: bool,
    pub vertical: bool,
    /// `None` = the centre of the scope's lowest and highest key, rounded
    /// down.
    pub pivot: Option<u8>,
}

impl Default for Flip {
    fn default() -> Self {
        Self {
            horizontal: true,
            vertical: false,
            pivot: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArticulateMode {
    #[default]
    Gate,
    Legato,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Articulate {
    pub mode: ArticulateMode,
    /// 1..=200 % of the original length (Gate).
    pub gate_percent: u32,
    /// Ticks taken off the end.
    pub gap: u32,
    /// 0..=100 % random length variation.
    pub variation_percent: u32,
    pub seed: u64,
    /// Ends stop at the next onset group.
    pub prevent_overlap: bool,
}

impl Default for Articulate {
    fn default() -> Self {
        Self {
            mode: ArticulateMode::Gate,
            gate_percent: 100,
            gap: 0,
            variation_percent: 0,
            seed: 1,
            prevent_overlap: false,
        }
    }
}

/// A custom groove: grid points within one cycle (own JSON schema, see
/// [`Groove::from_json`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Groove {
    /// Cycle length in ticks.
    pub length: u32,
    /// Sorted positions within the cycle, `< length`.
    pub points: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quantize {
    /// Grid step in ticks (triplets: e.g. 160).
    pub grid: u32,
    /// 0..=100 %.
    pub strength: u32,
    pub starts: bool,
    pub ends: bool,
    /// 0..=75 % of a grid step; odd grid points move later.
    pub swing: u32,
    /// Only notes this close (ticks) to a grid point move; 0 = any.
    pub window: u32,
    /// With starts only: keep lengths.
    pub preserve_length: bool,
    /// Used instead of grid and swing when set.
    pub groove: Option<Groove>,
}

impl Default for Quantize {
    fn default() -> Self {
        Self {
            grid: 240,
            strength: 100,
            starts: true,
            ends: false,
            swing: 0,
            window: 0,
            preserve_length: true,
            groove: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChopMode {
    /// Segments of this many ticks.
    Step(u32),
    /// This many equal parts.
    Parts(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chop {
    pub mode: ChopMode,
    /// 1..=100 % of each segment sounds.
    pub gate_percent: u32,
    /// Velocity factor of every second segment (1 = unchanged).
    pub alternate_velocity: f32,
    /// Velocities fall instead of rising along the segments (with the
    /// alternating factor applied from the end).
    pub reverse_velocity: bool,
}

impl Default for Chop {
    fn default() -> Self {
        Self {
            mode: ChopMode::Step(240),
            gate_percent: 100,
            alternate_velocity: 1.0,
            reverse_velocity: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strum {
    /// High to low instead of low to high.
    pub downward: bool,
    /// Delay per note in ticks.
    pub delay: u32,
    /// Notes starting within this many ticks of a group's first start
    /// belong to it.
    pub tolerance: u32,
    /// MIDI velocity added per note along the strum (may be negative).
    pub velocity_ramp: i32,
    /// Keep the original ends (lengths shrink) instead of the lengths.
    pub preserve_ends: bool,
}

impl Default for Strum {
    fn default() -> Self {
        Self {
            downward: false,
            delay: 30,
            tolerance: 10,
            velocity_ramp: 0,
            preserve_ends: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flam {
    /// Grace note start before the main note, ticks.
    pub offset: u32,
    pub grace_length: u32,
    /// Grace velocity as a factor of the main note's.
    pub grace_velocity: f32,
    /// A grace that would start before 0 moves the pair later instead of
    /// being skipped.
    pub shift_pair: bool,
}

impl Default for Flam {
    fn default() -> Self {
        Self {
            offset: 60,
            grace_length: 40,
            grace_velocity: 0.6,
            shift_pair: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub min: u8,
    pub max: u8,
    pub transpose: i32,
    /// Octave fold instead of clamping.
    pub fold: bool,
    /// Snap to this scale (root pitch class, mode) before limiting.
    pub scale: Option<(u8, super::Mode)>,
}

impl Default for Limit {
    fn default() -> Self {
        Self {
            min: 36,
            max: 84,
            transpose: 0,
            fold: true,
            scale: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleLevels {
    /// MIDI velocity 1..=127.
    pub center: u8,
    pub multiplier: f32,
    /// Normalized offset added after scaling.
    pub offset: f32,
    pub invert: bool,
    /// Power curve with `exponent` (> 0); off = linear.
    pub power: bool,
    pub exponent: f32,
}

impl Default for ScaleLevels {
    fn default() -> Self {
        Self {
            center: 64,
            multiplier: 1.0,
            offset: 0.0,
            invert: false,
            power: false,
            exponent: 1.0,
        }
    }
}

fn todo(name: &str) -> Result<Outcome, String> {
    Err(format!("{name} is not available yet"))
}

pub fn quick_legato(_input: &Input) -> Result<Outcome, String> {
    todo("Quick legato")
}

pub fn quick_quantize(_input: &Input, _ends: bool) -> Result<Outcome, String> {
    todo("Quick quantize")
}

pub fn quick_chop(_input: &Input) -> Result<Outcome, String> {
    todo("Quick chop")
}

pub fn glue(_input: &Input, _s: &Glue) -> Result<Outcome, String> {
    todo("Glue")
}

pub fn flip(_input: &Input, _s: &Flip) -> Result<Outcome, String> {
    todo("Flip")
}

pub fn articulate(_input: &Input, _s: &Articulate) -> Result<Outcome, String> {
    todo("Articulate")
}

pub fn quantize(_input: &Input, _s: &Quantize) -> Result<Outcome, String> {
    todo("Quantize")
}

pub fn chop(_input: &Input, _s: &Chop) -> Result<Outcome, String> {
    todo("Chop")
}

pub fn strum(_input: &Input, _s: &Strum) -> Result<Outcome, String> {
    todo("Strum")
}

pub fn flam(_input: &Input, _s: &Flam) -> Result<Outcome, String> {
    todo("Flam")
}

pub fn limit(_input: &Input, _s: &Limit) -> Result<Outcome, String> {
    todo("Limit")
}

pub fn scale_levels(_input: &Input, _s: &ScaleLevels) -> Result<Outcome, String> {
    todo("Scale levels")
}
