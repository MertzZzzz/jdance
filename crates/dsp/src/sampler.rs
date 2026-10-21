// SPDX-License-Identifier: GPL-3.0-or-later
//! One-shot sample playback.
//!
//! Samples are converted to stereo at the engine sample rate when loaded
//! (outside the audio thread), so a voice at the root key plays frames 1:1
//! with no interpolation. Other keys play at `2^(semitones/12)` speed with
//! linear interpolation.

/// Stereo, deinterleaved audio at a known sample rate.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleBuffer {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl SampleBuffer {
    /// Builds a stereo buffer from interleaved frames with `channels`
    /// channels. Mono is duplicated to both sides; for more than two
    /// channels only the first two are used.
    pub fn from_interleaved(sample_rate: u32, channels: usize, data: &[f32]) -> Self {
        assert!(channels > 0, "channel count must be positive");
        let frames = data.len() / channels;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for frame in data.chunks_exact(channels) {
            left.push(frame[0]);
            right.push(if channels > 1 { frame[1] } else { frame[0] });
        }
        Self {
            sample_rate,
            left,
            right,
        }
    }

    pub fn len(&self) -> usize {
        self.left.len()
    }

    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
    }

    /// Converts to `target_rate` (no-op if equal). Not realtime-safe.
    pub fn resampled(&self, target_rate: u32) -> Self {
        if target_rate == self.sample_rate {
            return self.clone();
        }
        Self {
            sample_rate: target_rate,
            left: super::resample_cubic(&self.left, self.sample_rate, target_rate),
            right: super::resample_cubic(&self.right, self.sample_rate, target_rate),
        }
    }
}

/// A voice playing one [`SampleBuffer`] from the start.
///
/// The voice does not own the buffer: it stores which sample slot it plays
/// and receives the buffer on every render call, so no reference counting
/// happens on the audio thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct SamplerVoice {
    /// Slot index chosen by the owner (e.g. channel index).
    pub slot: usize,
    /// Monotonic start order, used for voice stealing.
    pub started_at: u64,
    pos: usize,
    gain_l: f32,
    gain_r: f32,
    active: bool,
    /// Release envelope: current level and per-sample decrement; `step`
    /// is 0 while not releasing.
    env: f32,
    step: f32,
    /// Playback speed (1 = root key) and fractional read position.
    rate: f64,
    frac: f64,
}

impl SamplerVoice {
    pub fn start(&mut self, slot: usize, started_at: u64, gain_l: f32, gain_r: f32) {
        *self = Self {
            slot,
            started_at,
            pos: 0,
            gain_l,
            gain_r,
            active: true,
            env: 1.0,
            step: 0.0,
            rate: 1.0,
            frac: 0.0,
        };
    }

    /// Starts at `rate` times the original speed (pitch shift by
    /// resampling; `rate` > 0).
    pub fn start_pitched(
        &mut self,
        slot: usize,
        started_at: u64,
        gain_l: f32,
        gain_r: f32,
        rate: f64,
    ) {
        self.start(slot, started_at, gain_l, gain_r);
        self.rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
    }

    /// Fades the voice out linearly over `samples` and then stops it (a
    /// short release for Stop, seek, mute or loop wrap).
    pub fn release(&mut self, samples: u32) {
        if self.active && self.step == 0.0 {
            self.step = self.env / samples.max(1) as f32;
        }
    }

    pub fn is_releasing(&self) -> bool {
        self.step > 0.0
    }

    pub fn stop(&mut self) {
        self.active = false;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn set_gains(&mut self, gain_l: f32, gain_r: f32) {
        self.gain_l = gain_l;
        self.gain_r = gain_r;
    }

    /// Adds up to `out_l.len()` frames into the outputs. The voice stops at
    /// the end of the sample. Realtime-safe.
    #[inline]
    pub fn render_add(&mut self, sample: &SampleBuffer, out_l: &mut [f32], out_r: &mut [f32]) {
        if !self.active {
            return;
        }
        if self.rate != 1.0 {
            self.render_pitched(sample, out_l, out_r);
            return;
        }
        let n = out_l.len().min(out_r.len());
        let available = sample.len().saturating_sub(self.pos);
        let count = n.min(available);
        let src_l = &sample.left[self.pos..self.pos + count];
        let src_r = &sample.right[self.pos..self.pos + count];
        if self.step == 0.0 {
            for i in 0..count {
                out_l[i] += src_l[i] * self.gain_l;
                out_r[i] += src_r[i] * self.gain_r;
            }
        } else {
            for i in 0..count {
                self.env -= self.step;
                if self.env <= 0.0 {
                    self.active = false;
                    return;
                }
                out_l[i] += src_l[i] * self.gain_l * self.env;
                out_r[i] += src_r[i] * self.gain_r * self.env;
            }
        }
        self.pos += count;
        if self.pos >= sample.len() {
            self.active = false;
        }
    }

    fn render_pitched(&mut self, sample: &SampleBuffer, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(out_r.len());
        let len = sample.len();
        for i in 0..n {
            if self.pos + 1 >= len {
                self.active = false;
                return;
            }
            let t = self.frac as f32;
            let l = sample.left[self.pos] + (sample.left[self.pos + 1] - sample.left[self.pos]) * t;
            let r =
                sample.right[self.pos] + (sample.right[self.pos + 1] - sample.right[self.pos]) * t;
            let mut g = 1.0;
            if self.step > 0.0 {
                self.env -= self.step;
                if self.env <= 0.0 {
                    self.active = false;
                    return;
                }
                g = self.env;
            }
            out_l[i] += l * self.gain_l * g;
            out_r[i] += r * self.gain_r * g;
            self.frac += self.rate;
            let whole = self.frac.floor();
            self.pos += whole as usize;
            self.frac -= whole;
        }
    }
}

/// Playback rate for `key` relative to `root` (equal temperament).
pub fn key_rate(key: u8, root: u8) -> f64 {
    2f64.powf((f64::from(key) - f64::from(root)) / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_fades_and_stops() {
        let s = SampleBuffer::from_interleaved(48_000, 1, &[1.0; 100]);
        let mut v = SamplerVoice::default();
        v.start(0, 0, 1.0, 1.0);
        v.release(4);
        let (mut l, mut r) = ([0.0; 8], [0.0; 8]);
        v.render_add(&s, &mut l, &mut r);
        assert_eq!(l, [0.75, 0.5, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(!v.is_active());
    }

    #[test]
    fn octave_up_plays_twice_as_fast_and_root_is_exact() {
        let data: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let s = SampleBuffer::from_interleaved(48_000, 1, &data);
        let mut v = SamplerVoice::default();
        v.start_pitched(0, 0, 1.0, 1.0, key_rate(72, 60));
        let (mut l, mut r) = ([0.0; 4], [0.0; 4]);
        v.render_add(&s, &mut l, &mut r);
        assert_eq!(l, [0.0, 2.0, 4.0, 6.0]);
        // A fifth down interpolates between frames.
        let mut v = SamplerVoice::default();
        v.start_pitched(0, 0, 1.0, 1.0, 0.5);
        let (mut l, mut r) = ([0.0; 4], [0.0; 4]);
        v.render_add(&s, &mut l, &mut r);
        assert_eq!(l, [0.0, 0.5, 1.0, 1.5]);
        assert_eq!(key_rate(60, 60), 1.0);
        assert!((key_rate(67, 60) - 1.498_307).abs() < 1e-6);
    }

    #[test]
    fn mono_is_duplicated() {
        let s = SampleBuffer::from_interleaved(48_000, 1, &[0.1, 0.2]);
        assert_eq!(s.left, [0.1, 0.2]);
        assert_eq!(s.right, [0.1, 0.2]);
        let s = SampleBuffer::from_interleaved(48_000, 3, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(s.left, [1.0, 4.0]);
        assert_eq!(s.right, [2.0, 5.0]);
    }

    #[test]
    fn voice_plays_to_end_across_blocks() {
        let s = SampleBuffer::from_interleaved(48_000, 1, &[1.0, 2.0, 3.0, 4.0, 5.0]);
        let mut v = SamplerVoice::default();
        v.start(0, 0, 1.0, 0.5);
        let (mut l, mut r) = ([0.0; 3], [0.0; 3]);
        v.render_add(&s, &mut l, &mut r);
        assert_eq!(l, [1.0, 2.0, 3.0]);
        assert_eq!(r, [0.5, 1.0, 1.5]);
        assert!(v.is_active());
        let (mut l, mut r) = ([0.0; 3], [0.0; 3]);
        v.render_add(&s, &mut l, &mut r);
        assert_eq!(l, [4.0, 5.0, 0.0]);
        assert!(!v.is_active());
    }
}
