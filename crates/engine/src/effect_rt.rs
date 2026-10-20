// SPDX-License-Identifier: GPL-3.0-or-later
//! Realtime insert slots: built-in processors plus bypass handling.
//!
//! Bypass is a hard bypass with a short crossfade (spec "Bypass and
//! tails"): the wet/dry mix ramps to dry; once fully dry the effect is
//! reset in bounded steps (large buffers are cleared over several blocks)
//! and the tail is dropped. Re-enabling waits until the state is clean and
//! then ramps back in. The dry path is delayed by the effect's latency so
//! bypassing never shifts timing (MIX-033).

use crate::MAX_BLOCK;
use jdance_domain::{Effect, EffectKind, InsertId, Tap};
use jdance_dsp::{
    Compressor, DelayLine, ParametricEq, ProcessContext, Processor, Reverb, Smoothed, Tail,
    TempoDelay,
};

/// Bypass crossfade length, ≈ 10 ms.
fn xfade_len(sample_rate: u32) -> u32 {
    (sample_rate / 100).max(1)
}

/// Buffer samples cleared per block during a reset.
pub const RESET_BUDGET: usize = 16_384;

#[derive(Debug, Clone)]
pub enum EffectRt {
    Eq(ParametricEq),
    Delay(TempoDelay),
    Reverb(Reverb),
    Compressor(Compressor),
}

impl EffectRt {
    /// Builds and prepares a processor (allocates; not realtime-safe).
    pub fn new(effect: &Effect, sample_rate: u32, tempo_bpm: f64) -> Self {
        let mut rt = match effect.kind {
            EffectKind::ParametricEq => EffectRt::Eq(ParametricEq::new()),
            EffectKind::TempoDelay => {
                let mut d = TempoDelay::new();
                d.set_tempo_hint(tempo_bpm);
                EffectRt::Delay(d)
            }
            EffectKind::Reverb => EffectRt::Reverb(Reverb::new()),
            EffectKind::Compressor => EffectRt::Compressor(Compressor::new()),
        };
        let p = rt.processor();
        for (i, spec) in effect.kind.params().iter().enumerate() {
            p.set_param(i, effect.get(spec.id));
        }
        p.prepare(sample_rate, MAX_BLOCK);
        p.settle();
        rt
    }

    pub fn processor(&mut self) -> &mut dyn Processor {
        match self {
            EffectRt::Eq(p) => p,
            EffectRt::Delay(p) => p,
            EffectRt::Reverb(p) => p,
            EffectRt::Compressor(p) => p,
        }
    }

    fn processor_ref(&self) -> &dyn Processor {
        match self {
            EffectRt::Eq(p) => p,
            EffectRt::Delay(p) => p,
            EffectRt::Reverb(p) => p,
            EffectRt::Compressor(p) => p,
        }
    }

    pub fn latency(&self) -> u32 {
        self.processor_ref().latency_frames()
    }

    pub fn tail(&self) -> Tail {
        self.processor_ref().tail()
    }

    /// Continues from `old`'s state when both are the same effect type.
    pub fn take_state(&mut self, old: &mut EffectRt) {
        match (self, old) {
            (EffectRt::Eq(n), EffectRt::Eq(o)) => n.take_state(o),
            (EffectRt::Delay(n), EffectRt::Delay(o)) => n.take_state(o),
            (EffectRt::Reverb(n), EffectRt::Reverb(o)) => n.take_state(o),
            (EffectRt::Compressor(n), EffectRt::Compressor(o)) => n.take_state(o),
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct SidechainRt {
    /// Index of the source track (always earlier in processing order).
    pub source: usize,
    pub tap: Tap,
    pub delay: DelayLine,
}

#[derive(Debug, Clone)]
pub struct InsertRt {
    pub id: InsertId,
    /// Delays the main signal before the effect when its sidechain
    /// arrives later than the main signal (MIX-030).
    pub pre_delay: DelayLine,
    pub effect: EffectRt,
    pub sidechain: Option<SidechainRt>,
    /// Wet/dry mix: 1 = active, 0 = bypassed.
    pub mix: Smoothed,
    /// Dry path delayed by the effect latency.
    pub dry_delay: DelayLine,
    /// The effect's state is clean (fresh or fully reset).
    pub clean: bool,
    /// Reset requested (e.g. after Stop) regardless of bypass.
    pub force_reset: bool,
}

impl InsertRt {
    pub fn new(id: InsertId, effect: EffectRt, bypass: bool, sample_rate: u32) -> Self {
        let latency = effect.latency() as usize;
        Self {
            id,
            pre_delay: DelayLine::new(0),
            effect,
            sidechain: None,
            mix: Smoothed::new(if bypass { 0.0 } else { 1.0 }, xfade_len(sample_rate)),
            dry_delay: DelayLine::new(latency),
            clean: true,
            force_reset: false,
        }
    }

    /// Continues from the same slot in the previous snapshot.
    pub fn take_state(&mut self, old: &mut InsertRt) {
        self.pre_delay.take_state(&mut old.pre_delay);
        self.effect.take_state(&mut old.effect);
        self.dry_delay.take_state(&mut old.dry_delay);
        self.mix.continue_from(&old.mix);
        self.clean = old.clean;
        self.force_reset = old.force_reset;
        if let (Some(sc), Some(osc)) = (&mut self.sidechain, &mut old.sidechain) {
            sc.delay.take_state(&mut osc.delay);
        }
    }

    /// Processes the slot in place. `dry` is scratch of at least `n`
    /// frames. Realtime-safe.
    pub fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        dry: (&mut [f32], &mut [f32]),
        sidechain: Option<(&[f32], &[f32])>,
        tempo_bpm: f64,
    ) {
        let n = l.len();
        self.pre_delay.process(l, r);
        let (dl, dr) = (&mut dry.0[..n], &mut dry.1[..n]);
        dl.copy_from_slice(l);
        dr.copy_from_slice(r);
        self.dry_delay.process(dl, dr);

        // Dry output while bypassed, or while the state is being cleared
        // (forced reset, or re-enabling before a reset finished). The mix
        // does not advance here, so re-enabling ramps in from clean state.
        let at_dry = self.mix.current() == 0.0;
        if self.force_reset || (at_dry && (!self.clean || self.mix.target() == 0.0)) {
            if !self.clean || self.force_reset {
                self.clean = self.effect.processor().reset_step(RESET_BUDGET);
                if self.clean {
                    self.force_reset = false;
                }
            }
            l.copy_from_slice(dl);
            r.copy_from_slice(dr);
            return;
        }
        let ctx = ProcessContext {
            sidechain,
            tempo_bpm,
        };
        self.effect.processor().process_block(l, r, &ctx);
        self.clean = false;
        if !self.mix.is_ramping() && self.mix.current() == 1.0 {
            return;
        }
        for i in 0..n {
            let m = self.mix.next_value();
            l[i] = dl[i] + (l[i] - dl[i]) * m;
            r[i] = dr[i] + (r[i] - dr[i]) * m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reverb_slot(bypass: bool) -> InsertRt {
        let effect = Effect::new(EffectKind::Reverb)
            .with("wet", 1.0)
            .with("predelay", 0.0);
        InsertRt::new(
            InsertId(1),
            EffectRt::new(&effect, 48_000, 120.0),
            bypass,
            48_000,
        )
    }

    fn run(slot: &mut InsertRt, input: f32, n: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut dl = vec![0.0; MAX_BLOCK];
        let mut dr = vec![0.0; MAX_BLOCK];
        let mut left = n;
        while left > 0 {
            let b = left.min(256);
            let mut l = vec![input; b];
            let mut r = vec![input; b];
            slot.process(&mut l, &mut r, (&mut dl, &mut dr), None, 120.0);
            out.extend(l);
            left -= b;
        }
        out
    }

    #[test]
    fn bypass_crossfades_then_resets_and_reenables_clean() {
        let mut slot = reverb_slot(false);
        let wet = run(&mut slot, 0.5, 20_000);
        assert!(
            wet.iter().any(|v| (v - 0.5).abs() > 0.01),
            "reverb is audible"
        );
        // Bypass: ramps to dry without a jump.
        slot.mix.set_target(0.0);
        let out = run(&mut slot, 0.5, 2_000);
        let max_step = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.05, "step {max_step}");
        assert!(
            out[1_000..].iter().all(|v| *v == 0.5),
            "fully dry after the fade"
        );
        // Reset happens in bounded steps while bypassed.
        run(&mut slot, 0.0, 48_000);
        assert!(slot.clean);
        // Re-enable on silence: the clean reverb outputs exact silence.
        slot.mix.set_target(1.0);
        let out = run(&mut slot, 0.0, 4_000);
        assert!(out.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn bypassed_compressor_keeps_latency() {
        let effect = Effect::new(EffectKind::Compressor).with("lookahead", 1.0);
        let mut slot = InsertRt::new(
            InsertId(1),
            EffectRt::new(&effect, 48_000, 120.0),
            true,
            48_000,
        );
        let mut l = vec![0.0; 100];
        l[0] = 1.0;
        let mut r = l.clone();
        let mut dl = vec![0.0; MAX_BLOCK];
        let mut dr = vec![0.0; MAX_BLOCK];
        slot.process(&mut l, &mut r, (&mut dl, &mut dr), None, 120.0);
        assert_eq!(l.iter().position(|v| *v != 0.0), Some(48));
    }
}
