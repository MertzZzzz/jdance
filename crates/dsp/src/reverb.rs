// SPDX-License-Identifier: GPL-3.0-or-later
//! Stereo algorithmic reverb: an 8-line feedback delay network.
//!
//! Topology (fixed for the MVP): a mono pre-delay feeds eight delay lines
//! with alternating input signs; line outputs pass a one-pole damping
//! low-pass and a per-line gain, then a Householder matrix
//! (`I − 2/N · 11ᵀ`, orthogonal) mixes them back into the lines. Left and
//! right outputs are sign-weighted sums of the line outputs, then a
//! mid/side width control is applied.
//!
//! Stability: the matrix is orthogonal, every per-line gain is < 1 and the
//! damping filter has gain ≤ 1, so the loop gain is < 1 and the network
//! cannot self-oscillate.
//!
//! Decay: per-line gain `g = 10^(−3 · len / (sr · RT60))`, a target RT60
//! estimate, not an exact guarantee. Coefficients depend on the sample
//! rate; line lengths are fixed in milliseconds. Pre-delay delays the wet
//! path only and is not reported as latency.

use crate::Smoothed;
use crate::processor::{ProcessContext, Processor, Tail, clear_chunk, ramp_samples};

pub const PARAM_IDS: &[&str] = &["decay", "damping", "predelay", "width", "wet"];

const N: usize = 8;
const LINE_MS: [f64; N] = [29.7, 37.1, 41.1, 43.7, 53.0, 59.9, 67.3, 73.1];
const IN_SIGN: [f32; N] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
const OUT_L: [f32; N] = [1.0, 1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0];
const OUT_R: [f32; N] = [1.0, -1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 1.0];
const OUT_GAIN: f32 = 0.3;
const MAX_PREDELAY_MS: f64 = 100.0;
const UPDATE_INTERVAL: u32 = 32;
/// Damping parameter 0..1 maps to filter coefficient 0..MAX_DAMP.
const MAX_DAMP: f32 = 0.85;

#[derive(Debug, Clone)]
pub struct Reverb {
    sample_rate: u32,
    decay: Smoothed,
    damping: Smoothed,
    width: Smoothed,
    wet: Smoothed,
    predelay_ms: f32,
    lines: [Vec<f32>; N],
    pos: [usize; N],
    gains: [f32; N],
    lp: [f32; N],
    pre: Vec<f32>,
    pre_pos: usize,
    pre_len: usize,
    counter: u32,
    clear_line: usize,
    clear_cursor: usize,
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new()
    }
}

impl Reverb {
    pub fn new() -> Self {
        let ramp = ramp_samples(48_000);
        Self {
            sample_rate: 48_000,
            decay: Smoothed::new(1.8, ramp),
            damping: Smoothed::new(0.4, ramp),
            width: Smoothed::new(1.0, ramp),
            wet: Smoothed::new(0.25, ramp),
            predelay_ms: 10.0,
            lines: Default::default(),
            pos: [0; N],
            gains: [0.0; N],
            lp: [0.0; N],
            pre: Vec::new(),
            pre_pos: 0,
            pre_len: 0,
            counter: 0,
            clear_line: N,
            clear_cursor: 0,
        }
    }

    fn update_gains(&mut self) {
        let rt60 = self.decay.current().max(0.05);
        for (g, line) in self.gains.iter_mut().zip(&self.lines) {
            let len = line.len() as f32;
            *g = 10f32.powf(-3.0 * len / (self.sample_rate as f32 * rt60));
        }
    }

    fn update_predelay(&mut self) {
        let len =
            (f64::from(self.predelay_ms) / 1000.0 * f64::from(self.sample_rate)).round() as usize;
        self.pre_len = len.min(self.pre.len().saturating_sub(1));
    }

    /// Continues from `old`'s network state and parameter positions.
    pub fn take_state(&mut self, old: &mut Reverb) {
        if self
            .lines
            .iter()
            .zip(&old.lines)
            .all(|(a, b)| a.len() == b.len())
            && self.pre.len() == old.pre.len()
        {
            std::mem::swap(&mut self.lines, &mut old.lines);
            std::mem::swap(&mut self.pre, &mut old.pre);
            self.pos = old.pos;
            self.lp = old.lp;
            self.pre_pos = old.pre_pos;
            self.clear_line = old.clear_line;
            self.clear_cursor = old.clear_cursor;
        }
        self.decay.continue_from(&old.decay);
        self.damping.continue_from(&old.damping);
        self.width.continue_from(&old.width);
        self.wet.continue_from(&old.wet);
        self.update_gains();
    }
}

impl Processor for Reverb {
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let mut prev = 0;
        for (i, line) in self.lines.iter_mut().enumerate() {
            let mut len = (LINE_MS[i] / 1000.0 * f64::from(sample_rate))
                .round()
                .max(1.0) as usize;
            if len <= prev {
                len = prev + 1; // keep lengths distinct and increasing
            }
            prev = len;
            *line = vec![0.0; len];
        }
        self.pre =
            vec![0.0; (MAX_PREDELAY_MS / 1000.0 * f64::from(sample_rate)).ceil() as usize + 1];
        self.pos = [0; N];
        let ramp = ramp_samples(sample_rate);
        self.decay = Smoothed::new(self.decay.target(), ramp);
        self.damping = Smoothed::new(self.damping.target(), ramp);
        self.width = Smoothed::new(self.width.target(), ramp);
        self.wet = Smoothed::new(self.wet.target(), ramp);
        self.update_gains();
        self.update_predelay();
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.decay.set_target(value.clamp(0.2, 10.0)),
            1 => self.damping.set_target(value.clamp(0.0, 1.0)),
            2 => {
                self.predelay_ms = value.clamp(0.0, MAX_PREDELAY_MS as f32);
                self.update_predelay();
            }
            3 => self.width.set_target(value.clamp(0.0, 1.0)),
            4 => self.wet.set_target(value.clamp(0.0, 1.0)),
            _ => {}
        }
    }

    fn settle(&mut self) {
        self.decay.reset(self.decay.target());
        self.damping.reset(self.damping.target());
        self.width.reset(self.width.target());
        self.wet.reset(self.wet.target());
        self.update_gains();
    }

    fn process_block(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &ProcessContext) {
        if self.pre.is_empty() {
            return;
        }
        for (xl, xr) in l.iter_mut().zip(r.iter_mut()) {
            if self.counter == 0 && self.decay.is_ramping() {
                for _ in 0..UPDATE_INTERVAL {
                    self.decay.next_value();
                }
                self.update_gains();
            }
            self.counter = (self.counter + 1) % UPDATE_INTERVAL;
            let damp = self.damping.next_value() * MAX_DAMP;
            let width = self.width.next_value();
            let wet = self.wet.next_value();

            // Pre-delay (wet path only).
            let mono = 0.5 * (*xl + *xr);
            let plen = self.pre.len();
            self.pre[self.pre_pos] = mono;
            let input = self.pre[(self.pre_pos + plen - self.pre_len) % plen];
            self.pre_pos = (self.pre_pos + 1) % plen;

            // Read, damp and attenuate each line.
            let mut v = [0.0f32; N];
            for (i, vi) in v.iter_mut().enumerate() {
                let out = self.lines[i][self.pos[i]];
                self.lp[i] = (1.0 - damp) * out + damp * self.lp[i];
                *vi = self.lp[i] * self.gains[i];
            }
            // Householder mixing and write-back.
            let s = v.iter().sum::<f32>() * (2.0 / N as f32);
            let (mut ol, mut or) = (0.0, 0.0);
            for i in 0..N {
                let line = &mut self.lines[i];
                line[self.pos[i]] = v[i] - s + input * IN_SIGN[i];
                self.pos[i] = (self.pos[i] + 1) % line.len();
                ol += v[i] * OUT_L[i];
                or += v[i] * OUT_R[i];
            }
            let mid = 0.5 * (ol + or) * OUT_GAIN;
            let side = 0.5 * (ol - or) * OUT_GAIN * width;
            *xl = (1.0 - wet) * *xl + wet * (mid + side);
            *xr = (1.0 - wet) * *xr + wet * (mid - side);
        }
    }

    fn reset_step(&mut self, max_samples: usize) -> bool {
        if self.clear_line >= N {
            self.clear_line = 0;
            self.clear_cursor = 0;
        }
        let mut budget = max_samples;
        while self.clear_line < N && budget > 0 {
            let line = &mut self.lines[self.clear_line];
            budget -= clear_chunk(line, &mut self.clear_cursor, budget);
            if self.clear_cursor >= line.len() {
                self.clear_line += 1;
                self.clear_cursor = 0;
            }
        }
        if self.clear_line < N {
            return false;
        }
        self.pre.fill(0.0); // ≤ 100 ms, bounded
        self.lp = [0.0; N];
        true
    }

    fn latency_frames(&self) -> u32 {
        0
    }

    fn tail(&self) -> Tail {
        Tail::Seconds(self.decay.target() + self.predelay_ms / 1000.0)
    }
}
