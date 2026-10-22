// SPDX-License-Identifier: GPL-3.0-or-later
//! Sample playback: region, loop, ADSR and pitch.
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

/// Envelope stage lengths in samples and the sustain level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adsr {
    pub attack: u32,
    pub decay: u32,
    pub sustain: f32,
    pub release: u32,
}

/// What a voice plays: region (frames of the sample), loop, envelope and
/// note length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub start: usize,
    /// Exclusive; clamped to the sample length.
    pub end: usize,
    /// Repeated region `start..end` while the note is held.
    pub loop_range: Option<(usize, usize)>,
    pub adsr: Option<Adsr>,
    /// Samples until note-off; `None` plays to the end.
    pub gate: Option<u64>,
    /// Fade length at note-off when there is no envelope.
    pub gate_release: u32,
    /// Declicking (spec 0.2.5 §4): S-curve fades at the region's start
    /// and end, in samples; with `bleed` the fade out runs past the end
    /// into the data after it, where there is some.
    pub fade_in: u32,
    pub fade_out: u32,
    pub bleed: bool,
}

impl Playback {
    /// The whole sample, one-shot, no envelope.
    pub const WHOLE: Self = Self {
        start: 0,
        end: usize::MAX,
        loop_range: None,
        adsr: None,
        gate: None,
        gate_release: 1,
        fade_in: 0,
        fade_out: 0,
        bleed: false,
    };

    fn is_plain(&self) -> bool {
        self.start == 0
            && self.loop_range.is_none()
            && self.adsr.is_none()
            && self.gate.is_none()
            && self.fade_in == 0
            && self.fade_out == 0
    }
}

/// S-curve gain for `done` of `len` samples, rising from 0 to 1.
#[inline]
fn s_curve(done: usize, len: u32) -> f32 {
    if len == 0 || done >= len as usize {
        return 1.0;
    }
    let t = done as f32 / len as f32;
    0.5 - 0.5 * (std::f32::consts::PI * t).cos()
}

impl Playback {
    /// Where one-shot playback ends: the region's end, or past it by the
    /// fade out when bleeding.
    fn play_end(&self, len: usize) -> usize {
        let end = self.end.min(len);
        if self.bleed {
            end.saturating_add(self.fade_out as usize).min(len)
        } else {
            end
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum Stage {
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// A voice playing one [`SampleBuffer`].
///
/// The voice does not own the buffer: it stores which sample slot it plays
/// and receives the buffer on every render call, so no reference counting
/// happens on the audio thread.
#[derive(Debug, Clone, Copy)]
pub struct SamplerVoice {
    /// Slot index chosen by the owner (e.g. channel index).
    pub slot: usize,
    /// Monotonic start order, used for voice stealing.
    pub started_at: u64,
    pos: usize,
    gain_l: f32,
    gain_r: f32,
    active: bool,
    /// Short release fade: current level and per-sample decrement; `step`
    /// is 0 while not releasing.
    env: f32,
    step: f32,
    /// Playback speed (1 = root key) and fractional read position.
    rate: f64,
    frac: f64,
    playback: Playback,
    /// ADSR stage, level and the release decrement.
    stage: Stage,
    level: f32,
    release_step: f32,
}

impl Default for SamplerVoice {
    fn default() -> Self {
        Self {
            slot: 0,
            started_at: 0,
            pos: 0,
            gain_l: 0.0,
            gain_r: 0.0,
            active: false,
            env: 1.0,
            step: 0.0,
            rate: 1.0,
            frac: 0.0,
            playback: Playback::WHOLE,
            stage: Stage::Off,
            level: 1.0,
            release_step: 0.0,
        }
    }
}

impl SamplerVoice {
    pub fn start(&mut self, slot: usize, started_at: u64, gain_l: f32, gain_r: f32) {
        self.start_with(slot, started_at, gain_l, gain_r, 1.0, Playback::WHOLE);
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
        self.start_with(slot, started_at, gain_l, gain_r, rate, Playback::WHOLE);
    }

    /// Starts with a region, loop, envelope and note length.
    pub fn start_with(
        &mut self,
        slot: usize,
        started_at: u64,
        gain_l: f32,
        gain_r: f32,
        rate: f64,
        playback: Playback,
    ) {
        let (stage, level) = match playback.adsr {
            Some(a) if a.attack > 0 => (Stage::Attack, 0.0),
            Some(_) => (Stage::Decay, 1.0),
            None => (Stage::Off, 1.0),
        };
        *self = Self {
            slot,
            started_at,
            pos: playback.start,
            gain_l,
            gain_r,
            active: true,
            env: 1.0,
            step: 0.0,
            rate: if rate.is_finite() && rate > 0.0 {
                rate
            } else {
                1.0
            },
            frac: 0.0,
            playback,
            stage,
            level,
            release_step: 0.0,
        };
    }

    /// Fades the voice out linearly over `samples` and then stops it (a
    /// short release for Stop, seek, mute, voice limits or loop wrap).
    pub fn release(&mut self, samples: u32) {
        if self.active && self.step == 0.0 {
            self.step = self.env / samples.max(1) as f32;
        }
    }

    pub fn is_releasing(&self) -> bool {
        self.step > 0.0 || self.stage == Stage::Release
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
    /// the end of its region or when its release ends. Realtime-safe.
    #[inline]
    pub fn render_add(&mut self, sample: &SampleBuffer, out_l: &mut [f32], out_r: &mut [f32]) {
        if !self.active {
            return;
        }
        if self.rate != 1.0 || !self.playback.is_plain() {
            self.render_general(sample, out_l, out_r);
            return;
        }
        let n = out_l.len().min(out_r.len());
        let end = self.playback.end.min(sample.len());
        let available = end.saturating_sub(self.pos);
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
        if self.pos >= end {
            self.active = false;
        }
    }

    /// Note-off: the envelope's release, or a short fade without one.
    pub fn note_off(&mut self) {
        match self.playback.adsr {
            Some(a) => {
                if self.stage != Stage::Release {
                    self.stage = Stage::Release;
                    self.release_step = self.level / a.release.max(1) as f32;
                }
            }
            None => self.release(self.playback.gate_release),
        }
    }

    /// Envelope level for the next sample; `None` when the release ended.
    #[inline]
    fn next_level(&mut self) -> Option<f32> {
        let Some(a) = self.playback.adsr else {
            return Some(1.0);
        };
        match self.stage {
            Stage::Attack => {
                self.level += 1.0 / a.attack.max(1) as f32;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                if a.decay == 0 || self.level <= a.sustain {
                    self.level = a.sustain;
                    self.stage = Stage::Sustain;
                } else {
                    self.level = (self.level - (1.0 - a.sustain) / a.decay as f32).max(a.sustain);
                }
            }
            Stage::Release => {
                self.level -= self.release_step;
                if self.level <= 0.0 {
                    return None;
                }
            }
            Stage::Sustain | Stage::Off => {}
        }
        Some(self.level)
    }

    fn render_general(&mut self, sample: &SampleBuffer, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(out_r.len());
        let len = sample.len();
        let looping = self
            .playback
            .loop_range
            .map(|(a, b)| (a.min(len), b.min(len)))
            .filter(|(a, b)| a < b);
        let end = match looping {
            Some((_, b)) => b,
            None => self.playback.play_end(len),
        };
        for i in 0..n {
            if self.pos >= end {
                match looping {
                    Some((a, b)) => self.pos = a + (self.pos - b) % (b - a),
                    None => {
                        self.active = false;
                        return;
                    }
                }
            }
            if let Some(g) = self.playback.gate {
                if g == 0 {
                    self.playback.gate = None;
                    self.note_off();
                } else {
                    self.playback.gate = Some(g - 1);
                }
            }
            let Some(level) = self.next_level() else {
                self.active = false;
                return;
            };
            let next = if self.pos + 1 < end {
                self.pos + 1
            } else {
                match looping {
                    Some((a, _)) => a,
                    None => self.pos,
                }
            };
            let t = self.frac as f32;
            let l = sample.left[self.pos] + (sample.left[next] - sample.left[self.pos]) * t;
            let r = sample.right[self.pos] + (sample.right[next] - sample.right[self.pos]) * t;
            let mut g = level;
            // Declicking: in from the start, out before the end.
            let pb = &self.playback;
            if pb.fade_in > 0 {
                g *= s_curve(self.pos.saturating_sub(pb.start), pb.fade_in);
            }
            if pb.fade_out > 0 && looping.is_none() {
                g *= s_curve(end.saturating_sub(self.pos + 1), pb.fade_out);
            }
            if self.step > 0.0 {
                self.env -= self.step;
                if self.env <= 0.0 {
                    self.active = false;
                    return;
                }
                g *= self.env;
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

    fn ramp(n: usize) -> SampleBuffer {
        let data: Vec<f32> = (0..n).map(|i| i as f32).collect();
        SampleBuffer::from_interleaved(48_000, 1, &data)
    }

    fn render(v: &mut SamplerVoice, s: &SampleBuffer, n: usize) -> Vec<f32> {
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        v.render_add(s, &mut l, &mut r);
        l
    }

    #[test]
    fn region_and_loop() {
        let s = ramp(10);
        let mut v = SamplerVoice::default();
        let region = Playback {
            start: 2,
            end: 5,
            ..Playback::WHOLE
        };
        v.start_with(0, 0, 1.0, 1.0, 1.0, region);
        assert_eq!(render(&mut v, &s, 5), [2.0, 3.0, 4.0, 0.0, 0.0]);
        assert!(!v.is_active());
        let looped = Playback {
            start: 1,
            loop_range: Some((3, 6)),
            gate: Some(9),
            gate_release: 1,
            ..Playback::WHOLE
        };
        v.start_with(0, 0, 1.0, 1.0, 1.0, looped);
        assert_eq!(
            render(&mut v, &s, 12),
            [1.0, 2.0, 3.0, 4.0, 5.0, 3.0, 4.0, 5.0, 3.0, 0.0, 0.0, 0.0],
            "loops until the gate, then fades in one sample"
        );
        assert!(!v.is_active());
    }

    #[test]
    fn adsr_shapes_and_releases_at_note_off() {
        let s = SampleBuffer::from_interleaved(48_000, 1, &[1.0; 100]);
        let mut v = SamplerVoice::default();
        let p = Playback {
            adsr: Some(Adsr {
                attack: 4,
                decay: 2,
                sustain: 0.5,
                release: 2,
            }),
            gate: Some(8),
            ..Playback::WHOLE
        };
        v.start_with(0, 0, 1.0, 1.0, 1.0, p);
        let out = render(&mut v, &s, 14);
        assert_eq!(
            out,
            [
                0.25, 0.5, 0.75, 1.0, 0.75, 0.5, 0.5, 0.5, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0
            ]
        );
        assert!(!v.is_active());
    }

    #[test]
    fn plain_playback_is_unchanged() {
        let s = ramp(6);
        let mut a = SamplerVoice::default();
        a.start(0, 0, 0.5, 0.5);
        let mut b = SamplerVoice::default();
        b.start_with(0, 0, 0.5, 0.5, 1.0, Playback::WHOLE);
        assert_eq!(render(&mut a, &s, 8), render(&mut b, &s, 8));
    }
}

#[cfg(test)]
mod declick_tests {
    use super::*;

    fn render(fade_in: u32, fade_out: u32, bleed: bool, end: usize) -> Vec<f32> {
        let s = SampleBuffer::from_interleaved(48_000, 1, &[1.0; 200]);
        let mut v = SamplerVoice::default();
        v.start_with(
            0,
            0,
            1.0,
            1.0,
            1.0,
            Playback {
                end,
                fade_in,
                fade_out,
                bleed,
                ..Playback::WHOLE
            },
        );
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        v.render_add(&s, &mut l, &mut r);
        l
    }

    #[test]
    fn fades_shape_the_start_and_the_end() {
        let plain = render(0, 0, false, 100);
        assert_eq!(plain[99], 1.0);
        assert_eq!(plain[100], 0.0);
        // Fade out inside the region: silent at the end, full before.
        let out = render(0, 20, false, 100);
        assert!(out[99] < 0.05 && out[70] == 1.0, "{} {}", out[99], out[70]);
        assert_eq!(out[100], 0.0);
        // Bleeding: plays 20 past the end, fading.
        let bleed = render(0, 20, true, 100);
        assert!(bleed[105] > 0.0 && bleed[105] < 1.0);
        assert_eq!(bleed[120], 0.0);
        // Fade in: rises from 0.
        let fin = render(10, 0, false, 100);
        assert!(fin[0] < 0.01 && fin[5] > 0.3 && fin[12] == 1.0);
    }
}
