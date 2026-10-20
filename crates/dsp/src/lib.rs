// SPDX-License-Identifier: GPL-3.0-or-later
//! DSP building blocks.
//!
//! Functions marked "realtime-safe" do not allocate, lock or panic for valid
//! inputs and run in time proportional to their buffer length.

mod resample;
mod sampler;

pub use resample::resample_cubic;
pub use sampler::{SampleBuffer, SamplerVoice};

/// Constant-power pan law, −3 dB at center (MIX-013 candidate).
///
/// `pan` is clamped to `-1.0..=1.0`. Returns `(left, right)` gains.
/// Realtime-safe.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let p = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let angle = (p + 1.0) * std::f32::consts::FRAC_PI_4;
    (angle.cos(), angle.sin())
}

/// Decibels to linear gain; `-inf` maps to `0.0`. Realtime-safe.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    if db == f32::NEG_INFINITY {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Linear gain to decibels; `0.0` maps to `-inf`. Realtime-safe.
#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * gain.log10()
    }
}

/// Linear ramp towards a target over a fixed number of samples, used to
/// avoid zipper noise on gain changes. Realtime-safe.
#[derive(Debug, Clone, Copy)]
pub struct Smoothed {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
    ramp_len: u32,
}

impl Smoothed {
    pub fn new(value: f32, ramp_len: u32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
            ramp_len: ramp_len.max(1),
        }
    }

    pub fn set_target(&mut self, target: f32) {
        if target == self.target {
            return;
        }
        self.target = target;
        self.remaining = self.ramp_len;
        self.step = (target - self.current) / self.ramp_len as f32;
    }

    /// Jumps to `value` without a ramp.
    pub fn reset(&mut self, value: f32) {
        self.current = value;
        self.target = value;
        self.remaining = 0;
    }

    #[inline]
    pub fn next_value(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            self.current = if self.remaining == 0 {
                self.target
            } else {
                self.current + self.step
            };
        }
        self.current
    }

    pub fn is_ramping(&self) -> bool {
        self.remaining > 0
    }

    pub fn target(&self) -> f32 {
        self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_law_is_constant_power() {
        for i in -10..=10 {
            let (l, r) = pan_gains(i as f32 / 10.0);
            assert!((l * l + r * r - 1.0).abs() < 1e-6);
        }
        let (l, r) = pan_gains(0.0);
        assert!((gain_to_db(l) + 3.0103).abs() < 1e-3);
        assert!((l - r).abs() < 1e-7);
        let (l, r) = pan_gains(-1.0);
        assert!((l - 1.0).abs() < 1e-7 && r.abs() < 1e-7);
        assert_eq!(pan_gains(f32::NAN), pan_gains(0.0));
    }

    #[test]
    fn db_round_trip() {
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert!((gain_to_db(db_to_gain(-12.0)) + 12.0).abs() < 1e-4);
    }

    #[test]
    fn smoothed_reaches_target_exactly() {
        let mut s = Smoothed::new(0.0, 4);
        s.set_target(1.0);
        let v: Vec<f32> = (0..6).map(|_| s.next_value()).collect();
        assert_eq!(v, [0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
        assert!(!s.is_ramping());
    }
}
