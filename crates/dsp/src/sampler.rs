// SPDX-License-Identifier: GPL-3.0-or-later
//! One-shot sample playback.
//!
//! Samples are converted to stereo at the engine sample rate when loaded
//! (outside the audio thread), so a voice plays frames 1:1 with no
//! interpolation in the callback.

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
        };
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
        let n = out_l.len().min(out_r.len());
        let available = sample.len().saturating_sub(self.pos);
        let count = n.min(available);
        let src_l = &sample.left[self.pos..self.pos + count];
        let src_r = &sample.right[self.pos..self.pos + count];
        for i in 0..count {
            out_l[i] += src_l[i] * self.gain_l;
            out_r[i] += src_r[i] * self.gain_r;
        }
        self.pos += count;
        if self.pos >= sample.len() {
            self.active = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
