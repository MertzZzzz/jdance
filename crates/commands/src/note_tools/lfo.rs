// SPDX-License-Identifier: GPL-3.0-or-later
//! LFO (spec 0.2.0 §18.21): curve points for an automation source, not
//! an oscillator and not note velocities.

use super::Range;
use jdance_domain::{AutoPoint, Curve};

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

/// Phase in cycles after `x` ticks: the integral of `1 / period` with the
/// period going linearly from `p0` to `p1` over `len` ticks (never
/// `x / period(x)`).
fn phase_after(p0: f64, p1: f64, len: f64, x: f64) -> f64 {
    let k = (p1 - p0) / len;
    if k.abs() < 1e-12 {
        x / p0
    } else {
        (k * x / p0).ln_1p() / k
    }
}

/// Inverse of [`phase_after`]: ticks until the phase has grown by `d`.
fn ticks_for(p0: f64, p1: f64, len: f64, d: f64) -> f64 {
    let k = (p1 - p0) / len;
    if k.abs() < 1e-12 {
        d * p0
    } else {
        p0 * (k * d).exp_m1() / k
    }
}

/// `-1..=1` of `shape` at `phase` cycles; all start at 0 (square at +1)
/// and rise first.
fn wave(shape: LfoShape, phase: f64) -> f64 {
    let f = phase.rem_euclid(1.0);
    match shape {
        LfoShape::Sine => (std::f64::consts::TAU * f).sin(),
        LfoShape::Triangle => 1.0 - 4.0 * ((f + 0.25).rem_euclid(1.0) - 0.5).abs(),
        LfoShape::Square => {
            if f < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

/// The LFO's points over `range` (source ticks). Centre, amplitude and
/// period go linearly from their start to their end values; the phase
/// integrates the frequency. Sine and triangle: linear points every
/// `resolution` ticks plus one at the range end. Square: a hold point at
/// the start, at every half-cycle change and at the range end. Values
/// are clamped to 0..=1.
pub fn lfo_points(s: &Lfo, range: Range) -> Result<Vec<AutoPoint>, String> {
    if range.is_empty() {
        return Err("the LFO range is empty".into());
    }
    if s.period.0 == 0 || s.period.1 == 0 {
        return Err("the LFO period must be at least 1 tick".into());
    }
    let len_ticks = range.len();
    let len = f64::from(len_ticks);
    let (p0, p1) = (f64::from(s.period.0), f64::from(s.period.1));
    let phase0 = f64::from(s.phase);
    let lerp = |(a, b): (f32, f32), x: f64| f64::from(a) + (f64::from(b) - f64::from(a)) * x / len;
    let value =
        |x: f64, w: f64| (lerp(s.center, x) + lerp(s.amplitude, x) * w).clamp(0.0, 1.0) as f32;
    let point = |x: u32, value: f32, curve: Curve| AutoPoint {
        tick: u64::from(range.start) + u64::from(x),
        value,
        curve,
        tension: 0.0,
    };
    let too_many = |count: u64| {
        format!("the LFO would write {count} points; at most {MAX_LFO_POINTS} are allowed")
    };
    match s.shape {
        LfoShape::Sine | LfoShape::Triangle => {
            let res = s.resolution.max(1);
            let count = u64::from(len_ticks.div_ceil(res)) + 1;
            if count > MAX_LFO_POINTS as u64 {
                return Err(too_many(count));
            }
            let mut out = Vec::with_capacity(count as usize);
            let mut x = 0;
            loop {
                let xf = f64::from(x);
                let w = wave(s.shape, phase0 + phase_after(p0, p1, len, xf));
                out.push(point(x, value(xf, w), Curve::Linear));
                if x == len_ticks {
                    break;
                }
                x = x.saturating_add(res).min(len_ticks);
            }
            Ok(out)
        }
        LfoShape::Square => {
            // Half-cycle changes strictly inside the range: n / 2 cycles.
            let phase_end = phase0 + phase_after(p0, p1, len, len);
            let first = (2.0 * phase0).floor() as i64 + 1;
            let last = (2.0 * phase_end).ceil() as i64 - 1;
            let changes = (last - first + 1).max(0) as u64;
            let count = changes + 2;
            if count > MAX_LFO_POINTS as u64 {
                return Err(too_many(count));
            }
            let mut out = Vec::with_capacity(count as usize);
            out.push(point(0, value(0.0, wave(s.shape, phase0)), Curve::Hold));
            for n in first..=last {
                let x = ticks_for(p0, p1, len, n as f64 / 2.0 - phase0);
                let tick = x.round().clamp(0.0, len) as u32;
                if tick >= len_ticks {
                    break;
                }
                let w = if n.rem_euclid(2) == 0 { 1.0 } else { -1.0 };
                let p = point(tick, value(f64::from(tick), w), Curve::Hold);
                // Changes closer than a tick: the later value wins.
                match out.last_mut() {
                    Some(prev) if prev.tick >= p.tick => prev.value = p.value,
                    _ => out.push(p),
                }
            }
            let w = wave(s.shape, phase_end);
            out.push(point(len_ticks, value(len, w), Curve::Hold));
            Ok(out)
        }
    }
}

/// `existing` with the points inside `range` (and any at the tick of a
/// new point, e.g. the range end) replaced by `new`; sorted by tick.
pub fn splice_points(existing: &[AutoPoint], range: Range, new: &[AutoPoint]) -> Vec<AutoPoint> {
    let inside = |t: u64| u64::from(range.start) <= t && t < u64::from(range.end);
    let mut out: Vec<AutoPoint> = existing
        .iter()
        .filter(|p| !inside(p.tick) && !new.iter().any(|q| q.tick == p.tick))
        .copied()
        .collect();
    out.extend_from_slice(new);
    out.sort_by_key(|p| p.tick);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(start: u32, end: u32) -> Range {
        Range { start, end }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn at(points: &[AutoPoint], tick: u64) -> f32 {
        points.iter().find(|p| p.tick == tick).unwrap().value
    }

    #[test]
    fn sine_and_triangle_shapes() {
        let p = lfo_points(&Lfo::default(), r(100, 1060)).unwrap();
        assert_eq!(p.len(), 17);
        assert_eq!((p[0].tick, p[16].tick), (100, 1060));
        assert!(p.iter().all(|p| p.curve == Curve::Linear));
        for (t, v) in [(100, 0.5), (340, 1.0), (580, 0.5), (820, 0.0), (1060, 0.5)] {
            assert!(near(at(&p, t), v), "sine at {t}");
        }
        let tri = Lfo {
            shape: LfoShape::Triangle,
            ..Lfo::default()
        };
        let p = lfo_points(&tri, r(0, 1000)).unwrap();
        assert_eq!(p.last().unwrap().tick, 1000, "the range end is included");
        assert_eq!(p.len(), 18);
        for (t, v) in [(0, 0.5), (120, 0.75), (240, 1.0), (360, 0.75), (720, 0.0)] {
            assert!(near(at(&p, t), v), "triangle at {t}");
        }
    }

    #[test]
    fn square_is_step_segments() {
        let sq = Lfo {
            shape: LfoShape::Square,
            ..Lfo::default()
        };
        let p = lfo_points(&sq, r(0, 1920)).unwrap();
        let got: Vec<(u64, f32)> = p.iter().map(|p| (p.tick, p.value)).collect();
        assert_eq!(
            got,
            [(0, 1.0), (480, 0.0), (960, 1.0), (1440, 0.0), (1920, 1.0)]
        );
        assert!(p.iter().all(|p| p.curve == Curve::Hold));
        // Phase shifts the changes.
        let p = lfo_points(&Lfo { phase: 0.25, ..sq }, r(0, 960)).unwrap();
        let ticks: Vec<u64> = p.iter().map(|p| p.tick).collect();
        assert_eq!(ticks, [0, 240, 720, 960]);
    }

    #[test]
    fn variable_period_integrates_frequency() {
        // Period 480 → 1440 over 1920 ticks: k = 0.5, phase(x) =
        // ln(1 + x / 960) / 0.5, so 2.197 cycles (not 1920 / 1440).
        let s = Lfo {
            shape: LfoShape::Square,
            period: (480, 1440),
            ..Lfo::default()
        };
        let p = lfo_points(&s, r(0, 1920)).unwrap();
        let ticks: Vec<u64> = p.iter().map(|p| p.tick).collect();
        // Changes at 0.5, 1, 1.5, 2 cycles: 960 (e^(k n/2) - 1).
        let want: Vec<u64> = (1..=4)
            .map(|n| (960.0 * (0.25 * f64::from(n)).exp_m1()).round() as u64)
            .collect();
        assert_eq!(&ticks[1..5], want);
        assert_eq!((ticks.len(), ticks[1], ticks[5]), (6, 273, 1920));
        // Sine: the end phase is the integrated one.
        let sine = Lfo {
            shape: LfoShape::Sine,
            resolution: 1920,
            ..s
        };
        let p = lfo_points(&sine, r(0, 1920)).unwrap();
        let phase = 3f64.ln() / 0.5;
        let want = 0.5 + 0.5 * (std::f64::consts::TAU * phase).sin();
        assert!(near(p[1].value, want as f32));
    }

    #[test]
    fn centre_and_amplitude_ramp_and_clamp() {
        let s = Lfo {
            center: (0.0, 1.0),
            amplitude: (0.0, 0.0),
            resolution: 480,
            ..Lfo::default()
        };
        let p = lfo_points(&s, r(0, 960)).unwrap();
        let v: Vec<f32> = p.iter().map(|p| p.value).collect();
        assert_eq!(v, [0.0, 0.5, 1.0]);
        let loud = Lfo {
            amplitude: (2.0, 2.0),
            ..Lfo::default()
        };
        let p = lfo_points(&loud, r(0, 960)).unwrap();
        assert!(p.iter().all(|p| (0.0..=1.0).contains(&p.value)));
    }

    #[test]
    fn rejects_bad_input_and_too_many_points() {
        assert!(lfo_points(&Lfo::default(), r(10, 10)).is_err());
        let zero = Lfo {
            period: (960, 0),
            ..Lfo::default()
        };
        assert!(lfo_points(&zero, r(0, 960)).is_err());
        let fine = Lfo {
            resolution: 1,
            ..Lfo::default()
        };
        let e = lfo_points(&fine, r(0, 10_000)).unwrap_err();
        assert!(e.contains("10001"), "{e}");
        assert_eq!(lfo_points(&fine, r(0, 4095)).unwrap().len(), 4096);
        // Resolution 0 acts as 1.
        let p = lfo_points(
            &Lfo {
                resolution: 0,
                ..fine
            },
            r(0, 3),
        )
        .unwrap();
        assert_eq!(p.len(), 4);
        let fast = Lfo {
            shape: LfoShape::Square,
            period: (1, 1),
            ..Lfo::default()
        };
        assert!(lfo_points(&fast, r(0, 10_000)).is_err());
    }

    #[test]
    fn splice_replaces_inside_and_at_new_ticks() {
        let pt = |tick, value| AutoPoint {
            tick,
            value,
            curve: Curve::Linear,
            tension: 0.0,
        };
        let existing = [
            pt(0, 0.1),
            pt(100, 0.2),
            pt(500, 0.3),
            pt(960, 0.4),
            pt(2000, 0.5),
        ];
        let new = [pt(100, 0.9), pt(960, 0.8)];
        let out = splice_points(&existing, r(100, 960), &new);
        let got: Vec<(u64, f32)> = out.iter().map(|p| (p.tick, p.value)).collect();
        assert_eq!(got, [(0, 0.1), (100, 0.9), (960, 0.8), (2000, 0.5)]);
    }
}
