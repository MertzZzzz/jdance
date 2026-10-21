// SPDX-License-Identifier: GPL-3.0-or-later
//! LFO (spec 0.2.0 §18.21): curve points for an automation source, not
//! an oscillator and not note velocities.

use super::Range;
use jdance_domain::AutoPoint;

/// Most points one LFO may write.
pub const MAX_LFO_POINTS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LfoShape {
    #[default]
    Sine,
    Triangle,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lfo {
    pub shape: LfoShape,
    /// Start phase, 0..1 of a cycle.
    pub phase: f32,
    /// Normalized centre at the range start and end.
    pub center: (f32, f32),
    /// Normalized amplitude at the range start and end.
    pub amplitude: (f32, f32),
    /// Period in ticks at the range start and end.
    pub period: (u32, u32),
    /// Ticks between points of sine and triangle.
    pub resolution: u32,
}

impl Default for Lfo {
    fn default() -> Self {
        Self {
            shape: LfoShape::Sine,
            phase: 0.0,
            center: (0.5, 0.5),
            amplitude: (0.5, 0.5),
            period: (960, 960),
            resolution: 60,
        }
    }
}

/// The LFO's points over `range` (source ticks).
pub fn lfo_points(_s: &Lfo, _range: Range) -> Result<Vec<AutoPoint>, String> {
    Err("LFO is not available yet".into())
}

/// `existing` with the points inside `range` replaced by `new`.
pub fn splice_points(existing: &[AutoPoint], range: Range, new: &[AutoPoint]) -> Vec<AutoPoint> {
    let mut out: Vec<AutoPoint> = existing
        .iter()
        .filter(|p| !(range.start..range.end).contains(&(p.tick.min(u64::from(u32::MAX)) as u32)))
        .copied()
        .collect();
    out.extend_from_slice(new);
    out.sort_by_key(|p| p.tick);
    out
}
