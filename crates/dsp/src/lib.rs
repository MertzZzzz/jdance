// SPDX-License-Identifier: GPL-3.0-or-later
//! DSP building blocks.
//!
//! Functions marked "realtime-safe" do not allocate, lock or panic for valid
//! inputs and run in time proportional to their buffer length.

pub mod compressor;
mod delay;
pub mod eq;
pub mod limiter;
pub mod processor;
mod resample;
pub mod reverb;
mod sample_tools;
mod sampler;
pub mod tempo_delay;

pub use compressor::Compressor;
pub use delay::DelayLine;
pub use eq::ParametricEq;
pub use limiter::SafetyLimiter;
pub use processor::{ParamEvent, ProcessContext, Processor, Tail, process_with_events};
pub use resample::resample_cubic;
pub use reverb::Reverb;
pub use sample_tools::{Tools, apply_tools, change_speed, stretch_and_pitch, time_stretch};
pub use sampler::{Adsr, Playback, SampleBuffer, SamplerVoice, key_rate};
pub use tempo_delay::TempoDelay;

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

/// Stereo balance for buses (MIX-013): unity at center; turning towards
/// one side attenuates the other side linearly down to silence.
/// Realtime-safe.
#[inline]
pub fn balance_gains(pan: f32) -> (f32, f32) {
    let p = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    ((1.0 - p).min(1.0), (1.0 + p).min(1.0))
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

    /// Ramps from the current value to `target` over exactly `samples`
    /// (automation: one segment per 32 samples).
    pub fn set_target_over(&mut self, target: f32, samples: u32) {
        let samples = samples.max(1);
        self.target = target;
        self.remaining = samples;
        self.step = (target - self.current) / samples as f32;
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

    pub fn current(&self) -> f32 {
        self.current
    }

    /// Continues from `old`'s current value towards this smoother's target.
    pub fn continue_from(&mut self, old: &Smoothed) {
        let target = self.target;
        self.current = old.current;
        self.target = old.current;
        self.remaining = 0;
        self.set_target(target);
    }

    /// Multiplies `buf` by the smoothed value, sample by sample.
    /// Realtime-safe.
    #[inline]
    pub fn apply(&mut self, buf: &mut [f32]) {
        if !self.is_ramping() {
            let g = self.current;
            if g != 1.0 {
                for s in buf {
                    *s *= g;
                }
            }
            return;
        }
        for s in buf {
            *s *= self.next_value();
        }
    }

    /// Writes the next `out.len()` smoothed values. Realtime-safe.
    #[inline]
    pub fn fill(&mut self, out: &mut [f32]) {
        for o in out {
            *o = self.next_value();
        }
    }
}

/// Independent smoothed gains for left and right.
#[derive(Debug, Clone, Copy)]
pub struct StereoGain {
    pub l: Smoothed,
    pub r: Smoothed,
}

impl StereoGain {
    pub fn new(gains: (f32, f32), ramp_len: u32) -> Self {
        Self {
            l: Smoothed::new(gains.0, ramp_len),
            r: Smoothed::new(gains.1, ramp_len),
        }
    }

    pub fn continue_from(&mut self, old: &StereoGain) {
        self.l.continue_from(&old.l);
        self.r.continue_from(&old.r);
    }

    #[inline]
    pub fn apply(&mut self, l: &mut [f32], r: &mut [f32]) {
        self.l.apply(l);
        self.r.apply(r);
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
    fn balance_is_unity_at_center() {
        assert_eq!(balance_gains(0.0), (1.0, 1.0));
        assert_eq!(balance_gains(1.0), (0.0, 1.0));
        assert_eq!(balance_gains(-0.5), (1.0, 0.5));
    }

    #[test]
    fn db_round_trip() {
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert!((gain_to_db(db_to_gain(-12.0)) + 12.0).abs() < 1e-4);
    }

    #[test]
    fn continue_from_ramps_from_old_value() {
        let old = Smoothed::new(1.0, 4);
        let mut new = Smoothed::new(0.0, 4);
        new.continue_from(&old);
        let mut buf = [1.0; 5];
        new.apply(&mut buf);
        assert_eq!(buf, [0.75, 0.5, 0.25, 0.0, 0.0]);
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
