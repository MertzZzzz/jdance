// SPDX-License-Identifier: GPL-3.0-or-later
//! Safety limiter for the master output: protection against sample peaks.
//!
//! Not a true-peak or mastering limiter: stereo-linked sample-peak
//! detection, instant gain reduction, smoothed recovery, no lookahead, no
//! oversampling, no makeup gain. Intersample peaks are not controlled and
//! strong peaks may distort.
//!
//! Guarantee: the applied gain never exceeds `ceiling / peak` for the
//! current sample, so every output sample satisfies
//! `|out| ≤ ceiling` up to `f32` rounding.

use crate::db_to_gain;
use crate::processor::one_pole;

#[derive(Debug, Clone)]
pub struct SafetyLimiter {
    sample_rate: u32,
    ceiling: f32,
    release_coef: f32,
    gain: f32,
    /// Lowest gain since the last [`SafetyLimiter::take_min_gain`].
    min_gain: f32,
}

impl SafetyLimiter {
    pub fn new(ceiling_db: f32, release_ms: f32, sample_rate: u32) -> Self {
        Self {
            sample_rate,
            ceiling: db_to_gain(ceiling_db.clamp(-12.0, 0.0)),
            release_coef: one_pole(release_ms.clamp(10.0, 500.0), sample_rate),
            gain: 1.0,
            min_gain: 1.0,
        }
    }

    pub fn set(&mut self, ceiling_db: f32, release_ms: f32) {
        self.ceiling = db_to_gain(ceiling_db.clamp(-12.0, 0.0));
        self.release_coef = one_pole(release_ms.clamp(10.0, 500.0), self.sample_rate);
    }

    pub fn take_state(&mut self, old: &SafetyLimiter) {
        self.gain = old.gain;
        self.min_gain = old.min_gain;
    }

    pub fn reset(&mut self) {
        self.gain = 1.0;
    }

    /// Lowest gain applied since the previous call (for the GR meter).
    pub fn take_min_gain(&mut self) -> f32 {
        std::mem::replace(&mut self.min_gain, 1.0)
    }

    /// Limits in place. Realtime-safe.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            // Non-finite input would poison the gain; treat it as silence.
            if !a.is_finite() {
                *a = 0.0;
            }
            if !b.is_finite() {
                *b = 0.0;
            }
            let peak = a.abs().max(b.abs());
            let required = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            let recovered = 1.0 - (1.0 - self.gain) * self.release_coef;
            self.gain = recovered.min(required);
            *a *= self.gain;
            *b *= self.gain;
            // Rounding can overshoot by one ulp; clamp to the ceiling.
            *a = a.clamp(-self.ceiling, self.ceiling);
            *b = b.clamp(-self.ceiling, self.ceiling);
            self.min_gain = self.min_gain.min(self.gain);
        }
    }
}
