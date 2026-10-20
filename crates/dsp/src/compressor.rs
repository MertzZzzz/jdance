// SPDX-License-Identifier: GPL-3.0-or-later
//! Feed-forward peak compressor with external sidechain and lookahead
//! (MIX-040).
//!
//! The detector follows the peak of the sidechain (or of the input when no
//! sidechain is connected) with one-pole attack/release smoothing; gain
//! reduction is applied to the input delayed by the lookahead, which is the
//! effect's reported latency.

use crate::{DelayLine, db_to_gain};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompressorSettings {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
    pub lookahead_ms: f32,
}

#[derive(Debug, Clone)]
pub struct Compressor {
    threshold_db: f32,
    slope: f32,
    attack: f32,
    release: f32,
    makeup: f32,
    lookahead: DelayLine,
    env: f32,
}

/// One-pole coefficient for a time constant; `0` ms means instant.
fn coefficient(ms: f32, sample_rate: u32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * sample_rate as f32)).exp()
    }
}

impl Compressor {
    pub fn new(s: &CompressorSettings, sample_rate: u32) -> Self {
        let lookahead = (s.lookahead_ms * 0.001 * sample_rate as f32).round() as usize;
        Self {
            threshold_db: s.threshold_db,
            slope: 1.0 - 1.0 / s.ratio.max(1.0),
            attack: coefficient(s.attack_ms, sample_rate),
            release: coefficient(s.release_ms, sample_rate),
            makeup: db_to_gain(s.makeup_db),
            lookahead: DelayLine::new(lookahead),
            env: 0.0,
        }
    }

    /// Latency in samples, reported for delay compensation.
    pub fn latency(&self) -> usize {
        self.lookahead.len()
    }

    /// Continues from `old`'s envelope and buffered audio.
    pub fn take_state(&mut self, old: &mut Compressor) {
        self.env = old.env;
        self.lookahead.take_state(&mut old.lookahead);
    }

    /// Processes in place. `sidechain`, if given, must be at least as long
    /// as the input. With `bypass` only the lookahead delay is applied, so
    /// bypassing does not shift timing (MIX-033). Realtime-safe.
    pub fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        sidechain: Option<(&[f32], &[f32])>,
        bypass: bool,
    ) {
        let n = l.len().min(r.len());
        // Detector input must be the undelayed signal.
        for i in 0..n {
            let det = match sidechain {
                Some((sl, sr)) => sl[i].abs().max(sr[i].abs()),
                None => l[i].abs().max(r[i].abs()),
            };
            let coef = if det > self.env {
                self.attack
            } else {
                self.release
            };
            self.env = det + coef * (self.env - det);
            let gain = if bypass { 1.0 } else { self.gain_for(self.env) };
            // The gain computed from the undelayed detector is applied to
            // the signal leaving the lookahead buffer.
            let (mut a, mut b) = ([l[i]], [r[i]]);
            self.lookahead.process(&mut a, &mut b);
            l[i] = a[0] * gain;
            r[i] = b[0] * gain;
        }
    }

    #[inline]
    fn gain_for(&self, env: f32) -> f32 {
        if self.slope == 0.0 && self.makeup == 1.0 {
            return 1.0;
        }
        let level_db = if env > 0.0 {
            20.0 * env.log10()
        } else {
            f32::NEG_INFINITY
        };
        let over = level_db - self.threshold_db;
        let reduction_db = if over > 0.0 { -over * self.slope } else { 0.0 };
        db_to_gain(reduction_db) * self.makeup
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> CompressorSettings {
        CompressorSettings {
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 0.0,
            release_ms: 50.0,
            makeup_db: 0.0,
            lookahead_ms: 0.0,
        }
    }

    #[test]
    fn unity_ratio_is_a_pure_delay() {
        let mut c = Compressor::new(
            &CompressorSettings {
                ratio: 1.0,
                lookahead_ms: 1.0,
                ..settings()
            },
            48_000,
        );
        assert_eq!(c.latency(), 48);
        let input: Vec<f32> = (0..200)
            .map(|i| ((i * 7) % 13) as f32 / 13.0 - 0.5)
            .collect();
        let (mut l, mut r) = (input.clone(), input.clone());
        c.process(&mut l, &mut r, None, false);
        assert!(l[..48].iter().all(|s| *s == 0.0));
        assert_eq!(&l[48..], &input[..152]);
    }

    #[test]
    fn reduces_loud_signal_by_ratio() {
        let mut c = Compressor::new(&settings(), 48_000);
        // Constant 0 dBFS: 20 dB over threshold, 4:1 → −15 dB.
        let (mut l, mut r) = (vec![1.0; 64], vec![1.0; 64]);
        c.process(&mut l, &mut r, None, false);
        assert!((crate::gain_to_db(l[63]) + 15.0).abs() < 1e-3);
        // Quiet signal below threshold passes unchanged.
        let mut c = Compressor::new(&settings(), 48_000);
        let (mut l, mut r) = (vec![0.05; 8], vec![0.05; 8]);
        c.process(&mut l, &mut r, None, false);
        assert!(l.iter().all(|s| (*s - 0.05).abs() < 1e-7));
    }

    #[test]
    fn sidechain_drives_reduction_and_bypass_keeps_delay() {
        let mut c = Compressor::new(&settings(), 48_000);
        let (mut l, mut r) = (vec![0.1; 16], vec![0.1; 16]);
        let mut sc = vec![0.0; 16];
        sc[8] = 1.0;
        c.process(&mut l, &mut r, Some((&sc, &sc)), false);
        assert!(
            (l[7] - 0.1).abs() < 1e-7,
            "no reduction before the sidechain hit"
        );
        assert!(l[8] < 0.05, "instant attack at the sidechain hit");

        let mut c = Compressor::new(
            &CompressorSettings {
                lookahead_ms: 0.5,
                ..settings()
            },
            48_000,
        );
        let (mut l, mut r) = (vec![1.0; 30], vec![1.0; 30]);
        c.process(&mut l, &mut r, None, true);
        assert!(l[..24].iter().all(|s| *s == 0.0));
        assert!(l[24..].iter().all(|s| *s == 1.0));
    }
}
