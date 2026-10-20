// SPDX-License-Identifier: GPL-3.0-or-later
//! Feed-forward peak compressor with external sidechain and lookahead
//! (MIX-040).
//!
//! The detector follows the peak of the sidechain (or of the input when no
//! sidechain is connected) with one-pole attack/release smoothing; gain
//! reduction is applied to the input delayed by the lookahead, which is the
//! effect's reported latency. Lookahead is structural: it is read in
//! `prepare` (changing it recompiles the mixer with new compensation).

use crate::processor::{ProcessContext, Processor, Tail, one_pole, ramp_samples};
use crate::{DelayLine, Smoothed, db_to_gain};

pub const PARAM_IDS: &[&str] = &[
    "threshold",
    "ratio",
    "attack",
    "release",
    "makeup",
    "lookahead",
];

#[derive(Debug, Clone)]
pub struct Compressor {
    sample_rate: u32,
    threshold_db: Smoothed,
    ratio: Smoothed,
    makeup_db: Smoothed,
    attack_ms: f32,
    release_ms: f32,
    attack: f32,
    release: f32,
    lookahead_ms: f32,
    lookahead: DelayLine,
    env: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor {
    pub fn new() -> Self {
        let ramp = ramp_samples(48_000);
        Self {
            sample_rate: 48_000,
            threshold_db: Smoothed::new(-18.0, ramp),
            ratio: Smoothed::new(4.0, ramp),
            makeup_db: Smoothed::new(0.0, ramp),
            attack_ms: 5.0,
            release_ms: 120.0,
            attack: 0.0,
            release: 0.0,
            lookahead_ms: 0.0,
            lookahead: DelayLine::new(0),
            env: 0.0,
        }
    }

    /// Continues from `old`'s envelope, buffered audio and parameters.
    pub fn take_state(&mut self, old: &mut Compressor) {
        self.env = old.env;
        self.lookahead.take_state(&mut old.lookahead);
        self.threshold_db.continue_from(&old.threshold_db);
        self.ratio.continue_from(&old.ratio);
        self.makeup_db.continue_from(&old.makeup_db);
    }

    #[inline]
    fn gain_for(env: f32, threshold_db: f32, ratio: f32, makeup_db: f32) -> f32 {
        let slope = 1.0 - 1.0 / ratio.max(1.0);
        let reduction_db = if slope == 0.0 || env <= 0.0 {
            0.0
        } else {
            let over = 20.0 * env.log10() - threshold_db;
            if over > 0.0 { -over * slope } else { 0.0 }
        };
        db_to_gain(reduction_db + makeup_db)
    }
}

impl Processor for Compressor {
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let ramp = ramp_samples(sample_rate);
        self.threshold_db = Smoothed::new(self.threshold_db.target(), ramp);
        self.ratio = Smoothed::new(self.ratio.target(), ramp);
        self.makeup_db = Smoothed::new(self.makeup_db.target(), ramp);
        self.attack = one_pole(self.attack_ms, sample_rate);
        self.release = one_pole(self.release_ms, sample_rate);
        let la = (self.lookahead_ms * 0.001 * sample_rate as f32).round() as usize;
        self.lookahead = DelayLine::new(la);
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.threshold_db.set_target(value),
            1 => self.ratio.set_target(value.max(1.0)),
            2 => {
                self.attack_ms = value;
                self.attack = one_pole(value, self.sample_rate);
            }
            3 => {
                self.release_ms = value;
                self.release = one_pole(value, self.sample_rate);
            }
            4 => self.makeup_db.set_target(value),
            // Structural: takes effect in the next `prepare`.
            5 => self.lookahead_ms = value,
            _ => {}
        }
    }

    fn settle(&mut self) {
        self.threshold_db.reset(self.threshold_db.target());
        self.ratio.reset(self.ratio.target());
        self.makeup_db.reset(self.makeup_db.target());
    }

    fn process_block(&mut self, l: &mut [f32], r: &mut [f32], ctx: &ProcessContext) {
        let n = l.len().min(r.len());
        for i in 0..n {
            // The detector sees the undelayed signal.
            let det = match ctx.sidechain {
                Some((sl, sr)) => sl[i].abs().max(sr[i].abs()),
                None => l[i].abs().max(r[i].abs()),
            };
            let coef = if det > self.env {
                self.attack
            } else {
                self.release
            };
            self.env = det + coef * (self.env - det);
            let gain = Self::gain_for(
                self.env,
                self.threshold_db.next_value(),
                self.ratio.next_value(),
                self.makeup_db.next_value(),
            );
            let (mut a, mut b) = ([l[i]], [r[i]]);
            self.lookahead.process(&mut a, &mut b);
            l[i] = a[0] * gain;
            r[i] = b[0] * gain;
        }
    }

    fn reset_step(&mut self, _max_samples: usize) -> bool {
        // Lookahead is at most 20 ms: cleared at once (bounded).
        self.lookahead.clear();
        self.env = 0.0;
        true
    }

    fn latency_frames(&self) -> u32 {
        self.lookahead.len() as u32
    }

    fn tail(&self) -> Tail {
        Tail::Seconds(self.release_ms / 1000.0 * 5.0 + self.lookahead_ms / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(params: &[(usize, f32)], sr: u32) -> Compressor {
        let mut c = Compressor::new();
        c.set_param(0, -20.0);
        c.set_param(1, 4.0);
        c.set_param(2, 0.0);
        c.set_param(3, 50.0);
        for (i, v) in params {
            c.set_param(*i, *v);
        }
        c.prepare(sr, 1024);
        c.settle();
        c
    }

    const CTX: ProcessContext<'static> = ProcessContext {
        sidechain: None,
        tempo_bpm: 120.0,
    };

    #[test]
    fn unity_ratio_is_a_pure_delay() {
        let mut c = comp(&[(1, 1.0), (5, 1.0)], 48_000);
        assert_eq!(c.latency_frames(), 48);
        let input: Vec<f32> = (0..200)
            .map(|i| ((i * 7) % 13) as f32 / 13.0 - 0.5)
            .collect();
        let (mut l, mut r) = (input.clone(), input.clone());
        c.process_block(&mut l, &mut r, &CTX);
        assert!(l[..48].iter().all(|s| *s == 0.0));
        assert_eq!(&l[48..], &input[..152]);
    }

    #[test]
    fn reduces_loud_signal_by_ratio() {
        let mut c = comp(&[], 48_000);
        // Constant 0 dBFS: 20 dB over threshold, 4:1 → −15 dB.
        let (mut l, mut r) = (vec![1.0; 64], vec![1.0; 64]);
        c.process_block(&mut l, &mut r, &CTX);
        assert!((crate::gain_to_db(l[63]) + 15.0).abs() < 1e-3);
        let mut c = comp(&[], 48_000);
        let (mut l, mut r) = (vec![0.05; 8], vec![0.05; 8]);
        c.process_block(&mut l, &mut r, &CTX);
        assert!(l.iter().all(|s| (*s - 0.05).abs() < 1e-7));
    }

    #[test]
    fn sidechain_drives_reduction() {
        let mut c = comp(&[], 48_000);
        let (mut l, mut r) = (vec![0.1; 16], vec![0.1; 16]);
        let mut sc = vec![0.0; 16];
        sc[8] = 1.0;
        let ctx = ProcessContext {
            sidechain: Some((&sc, &sc)),
            tempo_bpm: 120.0,
        };
        c.process_block(&mut l, &mut r, &ctx);
        assert!((l[7] - 0.1).abs() < 1e-7, "no reduction before the hit");
        assert!(l[8] < 0.05, "instant attack at the sidechain hit");
    }
}
