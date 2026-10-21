// SPDX-License-Identifier: GPL-3.0-or-later
//! Stereo-linked 4-band parametric EQ: low shelf, two bells, high shelf.
//!
//! Filters are RBJ "Audio EQ Cookbook" biquads in transposed direct form
//! II with `f64` state. Shelves use a fixed slope `S = 1` (MVP). A band
//! that is switched off ramps its gain to 0 dB, where the biquad is the
//! identity, so toggling never clicks. Frequencies are clamped to
//! `20 Hz ..= min(20 kHz, 0.45 · sample_rate)`.
//!
//! While a parameter ramps, coefficients are recomputed every
//! [`UPDATE_INTERVAL`] samples, counted globally so the output does not
//! depend on block boundaries.

use crate::Smoothed;
use crate::processor::{ProcessContext, Processor, Tail, ramp_samples};

/// Parameter order shared with the domain schema.
pub const PARAM_IDS: &[&str] = &[
    "low_on",
    "low_freq",
    "low_gain",
    "mid1_on",
    "mid1_freq",
    "mid1_gain",
    "mid1_q",
    "mid2_on",
    "mid2_freq",
    "mid2_gain",
    "mid2_q",
    "high_on",
    "high_freq",
    "high_gain",
];

const UPDATE_INTERVAL: u32 = 16;
/// Shelf slope (RBJ `S`), fixed for the MVP.
pub const SHELF_SLOPE: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
    LowShelf,
    Bell,
    HighShelf,
}

/// Normalized biquad coefficients (`a0 = 1`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// RBJ cookbook design. Inputs are clamped to safe ranges, so the
    /// result is always finite and stable.
    pub fn design(kind: BandKind, freq: f64, gain_db: f64, q: f64, sample_rate: u32) -> Coeffs {
        let sr = f64::from(sample_rate);
        let f = freq.clamp(20.0, max_freq(sample_rate));
        let q = q.clamp(0.1, 18.0);
        let gain_db = gain_db.clamp(-24.0, 24.0);
        if gain_db == 0.0 {
            return Coeffs::IDENTITY;
        }
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * f / sr;
        let (sin, cos) = w0.sin_cos();
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BandKind::Bell => {
                let alpha = sin / (2.0 * q);
                (
                    1.0 + alpha * a,
                    -2.0 * cos,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cos,
                    1.0 - alpha / a,
                )
            }
            BandKind::LowShelf | BandKind::HighShelf => {
                let alpha = sin / 2.0 * ((a + 1.0 / a) * (1.0 / SHELF_SLOPE - 1.0) + 2.0).sqrt();
                let k = 2.0 * a.sqrt() * alpha;
                if kind == BandKind::LowShelf {
                    (
                        a * ((a + 1.0) - (a - 1.0) * cos + k),
                        2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                        a * ((a + 1.0) - (a - 1.0) * cos - k),
                        (a + 1.0) + (a - 1.0) * cos + k,
                        -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                        (a + 1.0) + (a - 1.0) * cos - k,
                    )
                } else {
                    (
                        a * ((a + 1.0) + (a - 1.0) * cos + k),
                        -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                        a * ((a + 1.0) + (a - 1.0) * cos - k),
                        (a + 1.0) - (a - 1.0) * cos + k,
                        2.0 * ((a - 1.0) - (a + 1.0) * cos),
                        (a + 1.0) - (a - 1.0) * cos - k,
                    )
                }
            }
        };
        Coeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Magnitude response in dB at `freq`.
    pub fn response_db(&self, freq: f64, sample_rate: u32) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / f64::from(sample_rate);
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        let mag2 = (nr * nr + ni * ni) / (dr * dr + di * di);
        10.0 * mag2.max(1e-30).log10()
    }
}

/// Upper frequency limit for `sample_rate`.
pub fn max_freq(sample_rate: u32) -> f64 {
    (0.45 * f64::from(sample_rate)).min(20_000.0)
}

#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f64,
    z2: f64,
}

impl BiquadState {
    #[inline]
    fn tick(&mut self, c: &Coeffs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }
}

#[derive(Debug, Clone)]
struct Band {
    kind: BandKind,
    on: bool,
    /// User gain; the effective target is 0 dB while the band is off.
    gain_db: f32,
    freq: Smoothed,
    gain: Smoothed,
    q: Smoothed,
    coeffs: Coeffs,
    l: BiquadState,
    r: BiquadState,
}

impl Band {
    fn new(kind: BandKind, freq: f32, ramp: u32) -> Self {
        Self {
            kind,
            on: true,
            gain_db: 0.0,
            freq: Smoothed::new(freq, ramp),
            gain: Smoothed::new(0.0, ramp),
            q: Smoothed::new(1.0, ramp),
            coeffs: Coeffs::IDENTITY,
            l: BiquadState::default(),
            r: BiquadState::default(),
        }
    }

    fn update_gain_target(&mut self) {
        self.gain
            .set_target(if self.on { self.gain_db } else { 0.0 });
    }

    fn ramping(&self) -> bool {
        self.freq.is_ramping() || self.gain.is_ramping() || self.q.is_ramping()
    }

    fn redesign(&mut self, sample_rate: u32) {
        self.coeffs = Coeffs::design(
            self.kind,
            f64::from(self.freq.current()),
            f64::from(self.gain.current()),
            f64::from(self.q.current()),
            sample_rate,
        );
    }

    /// Settings as the UI graph should show them (targets, not ramps).
    fn target_coeffs(&self, sample_rate: u32) -> Coeffs {
        Coeffs::design(
            self.kind,
            f64::from(self.freq.target()),
            f64::from(if self.on { self.gain_db } else { 0.0 }),
            f64::from(self.q.target()),
            sample_rate,
        )
    }
}

#[derive(Debug, Clone)]
pub struct ParametricEq {
    sample_rate: u32,
    bands: [Band; 4],
    counter: u32,
}

impl Default for ParametricEq {
    fn default() -> Self {
        Self::new()
    }
}

impl ParametricEq {
    pub fn new() -> Self {
        let ramp = ramp_samples(48_000);
        Self {
            sample_rate: 48_000,
            bands: [
                Band::new(BandKind::LowShelf, 100.0, ramp),
                Band::new(BandKind::Bell, 500.0, ramp),
                Band::new(BandKind::Bell, 2_500.0, ramp),
                Band::new(BandKind::HighShelf, 8_000.0, ramp),
            ],
            counter: 0,
        }
    }

    /// Combined target response in dB at `freq` (for the UI graph).
    pub fn response_db(&self, freq: f64) -> f64 {
        self.bands
            .iter()
            .map(|b| {
                b.target_coeffs(self.sample_rate)
                    .response_db(freq, self.sample_rate)
            })
            .sum()
    }

    /// Continues from `old`'s filter state and parameter positions.
    pub fn take_state(&mut self, old: &ParametricEq) {
        for (b, o) in self.bands.iter_mut().zip(&old.bands) {
            b.l = o.l;
            b.r = o.r;
            b.freq.continue_from(&o.freq);
            b.gain.continue_from(&o.gain);
            b.q.continue_from(&o.q);
            b.coeffs = o.coeffs;
        }
        self.counter = old.counter;
    }
}

impl Processor for ParametricEq {
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let ramp = ramp_samples(sample_rate);
        for b in &mut self.bands {
            b.freq = Smoothed::new(b.freq.target(), ramp);
            b.gain = Smoothed::new(b.gain.target(), ramp);
            b.q = Smoothed::new(b.q.target(), ramp);
            b.redesign(sample_rate);
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        // Layout: [on, freq, gain] for shelves, [on, freq, gain, q] for bells.
        let (band, field) = match index {
            0..=2 => (0, index),
            3..=6 => (1, index - 3),
            7..=10 => (2, index - 7),
            11..=13 => (3, index - 11),
            _ => return,
        };
        let b = &mut self.bands[band];
        match field {
            0 => {
                b.on = value >= 0.5;
                b.update_gain_target();
            }
            1 => b.freq.set_target(value),
            2 => {
                b.gain_db = value;
                b.update_gain_target();
            }
            3 => b.q.set_target(value),
            _ => {}
        }
    }

    fn settle(&mut self) {
        for b in &mut self.bands {
            b.freq.reset(b.freq.target());
            b.gain.reset(b.gain.target());
            b.q.reset(b.q.target());
            b.redesign(self.sample_rate);
        }
    }

    fn process_block(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &ProcessContext) {
        for (xl, xr) in l.iter_mut().zip(r.iter_mut()) {
            if self.counter == 0 {
                for b in &mut self.bands {
                    if b.ramping() {
                        // Advance the smoothers by the interval at once.
                        for _ in 0..UPDATE_INTERVAL {
                            b.freq.next_value();
                            b.gain.next_value();
                            b.q.next_value();
                        }
                        b.redesign(self.sample_rate);
                    }
                }
            }
            self.counter = (self.counter + 1) % UPDATE_INTERVAL;
            let (mut yl, mut yr) = (f64::from(*xl), f64::from(*xr));
            for b in &mut self.bands {
                yl = b.l.tick(&b.coeffs, yl);
                yr = b.r.tick(&b.coeffs, yr);
            }
            *xl = yl as f32;
            *xr = yr as f32;
        }
    }

    fn reset_step(&mut self, _max_samples: usize) -> bool {
        for b in &mut self.bands {
            b.l = BiquadState::default();
            b.r = BiquadState::default();
        }
        true
    }

    fn latency_frames(&self) -> u32 {
        0
    }

    fn tail(&self) -> Tail {
        Tail::Seconds(0.05)
    }
}
