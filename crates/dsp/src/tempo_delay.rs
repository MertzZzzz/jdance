// SPDX-License-Identifier: GPL-3.0-or-later
//! Stereo tempo delay.
//!
//! - Free mode: 1..2000 ms. Synced mode: a note division of the current
//!   tempo; the buffer holds [`MAX_DELAY_SECONDS`] (a half note at the
//!   minimum supported 20 BPM), so synced times are not capped at 2 s.
//! - A delay-time change crossfades between the old and the new read
//!   position over [`XFADE_MS`]; the read pointer never jumps.
//! - Feedback passes a one-pole low-pass.
//! - Ping-pong scheme: the input is summed to mono and enters the LEFT
//!   line only; the left line's output feeds the RIGHT line and the right
//!   line's output feeds the left. An impulse therefore produces echoes
//!   L at `d`, R at `2d`, L at `3d`, … with gain `feedback^(k-1)` for the
//!   k-th echo.
//! - Mix: `out = (1 − wet) · dry + wet · echo`.

use crate::Smoothed;
use crate::processor::{ProcessContext, Processor, Tail, clear_chunk, ramp_samples};

pub const PARAM_IDS: &[&str] = &[
    "sync",
    "time_ms",
    "division",
    "feedback",
    "wet",
    "lowpass",
    "ping_pong",
];

/// Half note at 20 BPM.
pub const MAX_DELAY_SECONDS: f64 = 6.0;
pub const XFADE_MS: f64 = 50.0;
/// Division lengths in quarter notes, in schema order.
pub const DIVISION_BEATS: [f64; 6] = [0.25, 0.5, 1.0, 2.0, 0.75, 1.0 / 3.0];

#[derive(Debug, Clone)]
pub struct TempoDelay {
    sample_rate: u32,
    sync: bool,
    time_ms: f32,
    division: usize,
    ping_pong: bool,
    feedback: Smoothed,
    wet: Smoothed,
    lowpass_hz: Smoothed,
    lp_coef: f32,
    lp_l: f32,
    lp_r: f32,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write: usize,
    /// Current delay in samples (≥ 1).
    delay: usize,
    /// Crossfade target and progress.
    xfade_to: Option<usize>,
    xfade_pos: usize,
    xfade_len: usize,
    clear_cursor: usize,
    tempo_hint: f64,
}

impl Default for TempoDelay {
    fn default() -> Self {
        Self::new()
    }
}

impl TempoDelay {
    pub fn new() -> Self {
        let ramp = ramp_samples(48_000);
        Self {
            sample_rate: 48_000,
            sync: true,
            time_ms: 375.0,
            division: 2,
            ping_pong: false,
            feedback: Smoothed::new(0.4, ramp),
            wet: Smoothed::new(0.3, ramp),
            lowpass_hz: Smoothed::new(8_000.0, ramp),
            lp_coef: 0.0,
            lp_l: 0.0,
            lp_r: 0.0,
            buf_l: Vec::new(),
            buf_r: Vec::new(),
            write: 0,
            delay: 1,
            xfade_to: None,
            xfade_pos: 0,
            xfade_len: 1,
            clear_cursor: 0,
            tempo_hint: 120.0,
        }
    }

    /// Tempo used for the initial delay time and the tail estimate until
    /// the first block brings the real tempo.
    pub fn set_tempo_hint(&mut self, bpm: f64) {
        self.tempo_hint = bpm;
    }

    /// Delay in seconds for the current settings at `tempo_bpm`.
    pub fn delay_seconds(&self, tempo_bpm: f64) -> f64 {
        if self.sync {
            DIVISION_BEATS[self.division.min(DIVISION_BEATS.len() - 1)] * 60.0 / tempo_bpm.max(1.0)
        } else {
            f64::from(self.time_ms) / 1000.0
        }
    }

    fn target_delay(&self, tempo_bpm: f64) -> usize {
        let samples =
            (self.delay_seconds(tempo_bpm) * f64::from(self.sample_rate)).round() as usize;
        samples.clamp(1, self.buf_l.len().saturating_sub(1).max(1))
    }

    fn update_lowpass(&mut self) {
        let f = self
            .lowpass_hz
            .current()
            .min(0.45 * self.sample_rate as f32);
        self.lp_coef = (-2.0 * std::f32::consts::PI * f / self.sample_rate as f32).exp();
    }

    #[inline]
    fn tap(&self, buf: &[f32], d: usize) -> f32 {
        let len = buf.len();
        buf[(self.write + len - d) % len]
    }

    /// Continues from `old`'s buffers and settings positions.
    pub fn take_state(&mut self, old: &mut TempoDelay) {
        if self.buf_l.len() == old.buf_l.len() {
            std::mem::swap(&mut self.buf_l, &mut old.buf_l);
            std::mem::swap(&mut self.buf_r, &mut old.buf_r);
            self.write = old.write;
            self.delay = old.delay;
            self.xfade_to = old.xfade_to;
            self.xfade_pos = old.xfade_pos;
            self.lp_l = old.lp_l;
            self.lp_r = old.lp_r;
            self.clear_cursor = old.clear_cursor;
        }
        self.feedback.continue_from(&old.feedback);
        self.wet.continue_from(&old.wet);
        self.lowpass_hz.continue_from(&old.lowpass_hz);
        self.update_lowpass();
    }
}

impl Processor for TempoDelay {
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let len = (MAX_DELAY_SECONDS * f64::from(sample_rate)).ceil() as usize + 4;
        self.buf_l = vec![0.0; len];
        self.buf_r = vec![0.0; len];
        self.write = 0;
        self.xfade_len = ((XFADE_MS / 1000.0) * f64::from(sample_rate))
            .round()
            .max(1.0) as usize;
        let ramp = ramp_samples(sample_rate);
        self.feedback = Smoothed::new(self.feedback.target(), ramp);
        self.wet = Smoothed::new(self.wet.target(), ramp);
        self.lowpass_hz = Smoothed::new(self.lowpass_hz.target(), ramp);
        self.update_lowpass();
        self.clear_cursor = len;
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.sync = value >= 0.5,
            1 => self.time_ms = value,
            2 => self.division = value.max(0.0) as usize,
            3 => self.feedback.set_target(value.clamp(0.0, 0.95)),
            4 => self.wet.set_target(value.clamp(0.0, 1.0)),
            5 => self.lowpass_hz.set_target(value),
            6 => self.ping_pong = value >= 0.5,
            _ => {}
        }
    }

    fn settle(&mut self) {
        self.feedback.reset(self.feedback.target());
        self.wet.reset(self.wet.target());
        self.lowpass_hz.reset(self.lowpass_hz.target());
        self.update_lowpass();
        self.delay = self.target_delay(self.tempo_hint);
        self.xfade_to = None;
    }

    fn process_block(&mut self, l: &mut [f32], r: &mut [f32], ctx: &ProcessContext) {
        if self.buf_l.is_empty() {
            return;
        }
        self.tempo_hint = ctx.tempo_bpm;
        let target = self.target_delay(ctx.tempo_bpm);
        if self.xfade_to.is_none() && target != self.delay {
            self.xfade_to = Some(target);
            self.xfade_pos = 0;
        }
        let len = self.buf_l.len();
        for (xl, xr) in l.iter_mut().zip(r.iter_mut()) {
            let fb = self.feedback.next_value();
            let wet = self.wet.next_value();
            if self.lowpass_hz.is_ramping() {
                self.lowpass_hz.next_value();
                self.update_lowpass();
            }
            let (mut yl, mut yr) = (
                self.tap(&self.buf_l, self.delay),
                self.tap(&self.buf_r, self.delay),
            );
            if let Some(to) = self.xfade_to {
                let t = self.xfade_pos as f32 / self.xfade_len as f32;
                yl = yl * (1.0 - t) + self.tap(&self.buf_l, to) * t;
                yr = yr * (1.0 - t) + self.tap(&self.buf_r, to) * t;
                self.xfade_pos += 1;
                if self.xfade_pos >= self.xfade_len {
                    self.delay = to;
                    self.xfade_to = None;
                }
            }
            self.lp_l = (1.0 - self.lp_coef) * yl + self.lp_coef * self.lp_l;
            self.lp_r = (1.0 - self.lp_coef) * yr + self.lp_coef * self.lp_r;
            let (in_l, in_r) = if self.ping_pong {
                (0.5 * (*xl + *xr) + fb * self.lp_r, fb * self.lp_l)
            } else {
                (*xl + fb * self.lp_l, *xr + fb * self.lp_r)
            };
            self.buf_l[self.write] = in_l;
            self.buf_r[self.write] = in_r;
            self.write = (self.write + 1) % len;
            *xl = (1.0 - wet) * *xl + wet * yl;
            *xr = (1.0 - wet) * *xr + wet * yr;
        }
    }

    fn reset_step(&mut self, max_samples: usize) -> bool {
        if self.clear_cursor >= self.buf_l.len() {
            self.clear_cursor = 0;
        }
        let mut cl = self.clear_cursor;
        clear_chunk(&mut self.buf_l, &mut cl, max_samples / 2);
        clear_chunk(&mut self.buf_r, &mut self.clear_cursor, max_samples / 2);
        self.lp_l = 0.0;
        self.lp_r = 0.0;
        let done = self.clear_cursor >= self.buf_l.len();
        if done {
            // Mark clean: the next reset starts over.
            self.clear_cursor = self.buf_l.len();
        }
        done
    }

    fn latency_frames(&self) -> u32 {
        0
    }

    fn tail(&self) -> Tail {
        let d = self.delay_seconds(self.tempo_hint) as f32;
        let fb = self.feedback.target();
        if fb <= 0.0 {
            Tail::Seconds(d)
        } else {
            // Echoes decay by `fb` per repeat: −60 dB after ln(1e-3)/ln(fb).
            Tail::Seconds(d * (1.0 + (1e-3f32).ln() / fb.ln()))
        }
    }
}
