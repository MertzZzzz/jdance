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
    Compressor, DelayLine, ParamEvent, ParametricEq, ProcessContext, Processor, Reverb, Smoothed,
    Tail, TempoDelay, process_with_events,
};
use jdance_plugin_api::{PluginProcessor, Transport};

/// Bypass crossfade length, ≈ 10 ms.
fn xfade_len(sample_rate: u32) -> u32 {
    (sample_rate / 100).max(1)
}

/// Buffer samples cleared per block during a reset.
pub const RESET_BUDGET: usize = 16_384;

/// A hosted plugin in an insert slot.
///
/// The host creates the instance once and sends it in one snapshot; later
/// snapshots are compiled without it ([`PluginRt::carried`]) and take it
/// over on the swap, so an instance is never rebuilt by a recompile. A
/// slot without an instance passes audio through, delayed by the declared
/// latency so delay compensation still holds (missing plugin, PLG-072).
pub struct PluginRt {
    processor: Option<Box<dyn PluginProcessor>>,
    latency: u32,
    /// Takes over the instance of the same slot in the previous snapshot.
    carried: bool,
    fallback: DelayLine,
    transport: Transport,
}

impl std::fmt::Debug for PluginRt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRt")
            .field("instance", &self.processor.is_some())
            .field("latency", &self.latency)
            .field("carried", &self.carried)
            .finish_non_exhaustive()
    }
}

impl PluginRt {
    /// A new instance; its latency comes from the instance.
    pub fn fresh(processor: Box<dyn PluginProcessor>) -> Self {
        let latency = processor.latency_samples();
        Self {
            processor: Some(processor),
            latency,
            carried: false,
            fallback: DelayLine::new(latency as usize),
            transport: Transport::default(),
        }
    }

    /// The instance already sent in an earlier snapshot, with its current
    /// latency.
    pub fn carried(latency: u32) -> Self {
        Self {
            processor: None,
            latency,
            carried: true,
            fallback: DelayLine::new(latency as usize),
            transport: Transport::default(),
        }
    }

    /// No instance (plugin missing or failed to load): pass-through.
    pub fn missing() -> Self {
        Self {
            processor: None,
            latency: 0,
            carried: false,
            fallback: DelayLine::new(0),
            transport: Transport::default(),
        }
    }

    pub fn has_instance(&self) -> bool {
        self.processor.is_some()
    }

    pub fn latency(&self) -> u32 {
        self.latency
    }

    /// Queues an automation point for the instance's next block.
    pub fn queue_param(&mut self, id: u32, offset: u32, value: f64) {
        if let Some(p) = &mut self.processor {
            p.queue_param(id, offset, value);
        }
    }

    pub(crate) fn take_state(&mut self, old: &mut PluginRt) {
        if self.carried && self.processor.is_none() {
            self.processor = old.processor.take();
        }
        self.fallback.take_state(&mut old.fallback);
    }
}

impl Processor for PluginRt {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}

    /// Plugin parameters go through the plugin controller (PLG-042).
    fn set_param(&mut self, _index: usize, _value: f32) {}

    fn settle(&mut self) {}

    fn process_block(&mut self, l: &mut [f32], r: &mut [f32], ctx: &ProcessContext) {
        match &mut self.processor {
            Some(p) => p.process(l, r, ctx.sidechain, &self.transport),
            None => self.fallback.process(l, r),
        }
    }

    /// VST3 has no realtime reset; the plugin keeps its state. Reported
    /// clean so bypass and Stop never wait on it.
    fn reset_step(&mut self, _max_samples: usize) -> bool {
        true
    }

    fn latency_frames(&self) -> u32 {
        self.latency
    }

    fn tail(&self) -> Tail {
        Tail::None
    }
}

#[derive(Debug)]
pub enum EffectRt {
    Eq(ParametricEq),
    Delay(TempoDelay),
    Reverb(Reverb),
    Compressor(Compressor),
    Plugin(PluginRt),
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
            EffectKind::Plugin => EffectRt::Plugin(PluginRt::missing()),
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
            EffectRt::Plugin(p) => p,
        }
    }

    fn processor_ref(&self) -> &dyn Processor {
        match self {
            EffectRt::Eq(p) => p,
            EffectRt::Delay(p) => p,
            EffectRt::Reverb(p) => p,
            EffectRt::Compressor(p) => p,
            EffectRt::Plugin(p) => p,
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
            (EffectRt::Plugin(n), EffectRt::Plugin(o)) => n.take_state(o),
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct SidechainRt {
    /// Index of the source track (always earlier in processing order).
    pub source: usize,
    pub tap: Tap,
    pub delay: DelayLine,
}

#[derive(Debug)]
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
    /// Automated built-in parameters: (schema index, lane, project value).
    pub auto_params: Vec<(u16, usize, f32)>,
    /// Automated bypass: (lane, project bypass).
    pub auto_bypass: Option<(usize, bool)>,
    /// Automated plugin parameters: (ParamID, lane).
    pub auto_plugin: Vec<(u32, usize)>,
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
            auto_params: Vec::new(),
            auto_bypass: None,
            auto_plugin: Vec::new(),
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
        transport: &Transport,
        events: &[ParamEvent],
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
            // Parameter changes still land while dry.
            for e in events {
                self.effect
                    .processor()
                    .set_param(usize::from(e.index), e.value);
            }
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
        if let EffectRt::Plugin(p) = &mut self.effect {
            p.transport = *transport;
        }
        let ctx = ProcessContext {
            sidechain,
            tempo_bpm: transport.tempo_bpm,
        };
        if events.is_empty() {
            self.effect.processor().process_block(l, r, &ctx);
        } else {
            process_with_events(self.effect.processor(), l, r, events, &ctx);
        }
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

    const T: Transport = Transport {
        playing: true,
        tempo_bpm: 120.0,
        project_time_samples: 0,
        project_time_quarters: 0.0,
        cycle: None,
    };

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
            slot.process(&mut l, &mut r, (&mut dl, &mut dr), None, &T, &[]);
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
        slot.process(&mut l, &mut r, (&mut dl, &mut dr), None, &T, &[]);
        assert_eq!(l.iter().position(|v| *v != 0.0), Some(48));
    }
}

/// Notes per block and sounding notes per instrument channel.
pub const MAX_INSTRUMENT_EVENTS: usize = 512;
pub const MAX_SOUNDING_NOTES: usize = 256;

/// A plugin instrument playing a channel (PLG-033): note-ons at their
/// sample offsets, note-offs when the gate ends, and note-off for every
/// sounding note on stop, seek and loop wrap. Storage is pre-allocated.
#[derive(Debug)]
pub struct InstrumentRt {
    pub plugin: PluginRt,
    /// Automated plugin parameters: (ParamID, lane).
    pub auto_plugin: Vec<(u32, usize)>,
    events: Vec<jdance_plugin_api::NoteEvent>,
    /// (timeline end position, key, note id) of sounding notes.
    sounding: Vec<(u64, u8, i32)>,
    next_id: i32,
}

impl InstrumentRt {
    pub fn new(plugin: PluginRt) -> Self {
        Self {
            plugin,
            auto_plugin: Vec::new(),
            events: Vec::with_capacity(MAX_INSTRUMENT_EVENTS),
            sounding: Vec::with_capacity(MAX_SOUNDING_NOTES),
            next_id: 0,
        }
    }

    fn push(&mut self, e: jdance_plugin_api::NoteEvent) {
        // A full list drops the event (RT-011).
        if self.events.len() < self.events.capacity() {
            self.events.push(e);
        }
    }

    /// Note-offs for gates ending in `[from, to)` of the timeline, which
    /// starts at block offset `offset`.
    pub fn gates(&mut self, from: u64, to: u64, offset: usize) {
        let mut i = 0;
        while i < self.sounding.len() {
            let (end, key, note_id) = self.sounding[i];
            if end >= from && end < to {
                self.push(jdance_plugin_api::NoteEvent {
                    offset: (offset as u64 + end - from) as u32,
                    note_id,
                    key,
                    kind: jdance_plugin_api::NoteKind::Off,
                });
                self.sounding.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// A note-on at block `offset`, timeline `pos`, gate `length` samples.
    pub fn note_on(&mut self, offset: usize, pos: u64, key: u8, velocity: f32, length: u64) {
        if self.sounding.len() == self.sounding.capacity() {
            return;
        }
        self.next_id = self.next_id.wrapping_add(1);
        let note_id = self.next_id;
        self.push(jdance_plugin_api::NoteEvent {
            offset: offset as u32,
            note_id,
            key,
            kind: jdance_plugin_api::NoteKind::On { velocity },
        });
        self.sounding.push((pos + length.max(1), key, note_id));
    }

    /// Note-off for every sounding note at block `offset` (stop, seek,
    /// loop wrap, panic).
    pub fn all_off(&mut self, offset: usize) {
        while let Some((_, key, note_id)) = self.sounding.pop() {
            self.push(jdance_plugin_api::NoteEvent {
                offset: offset as u32,
                note_id,
                key,
                kind: jdance_plugin_api::NoteKind::Off,
            });
        }
    }

    /// Renders the block into `l`/`r` (overwritten). Realtime-safe on the
    /// host side.
    pub fn render(&mut self, l: &mut [f32], r: &mut [f32], transport: &Transport) {
        // Offs before ons at the same offset (retriggers).
        self.events.sort_unstable_by_key(|e| {
            (
                e.offset,
                matches!(e.kind, jdance_plugin_api::NoteKind::On { .. }),
                e.note_id,
            )
        });
        match &mut self.plugin.processor {
            Some(p) => p.process_instrument(l, r, &self.events, transport),
            None => {
                l.fill(0.0);
                r.fill(0.0);
            }
        }
        self.events.clear();
    }

    pub fn take_state(&mut self, old: &mut InstrumentRt) {
        self.plugin.take_state(&mut old.plugin);
        if self.plugin.processor.is_some() && old.plugin.processor.is_none() {
            // Same instance: keep its sounding notes.
            std::mem::swap(&mut self.sounding, &mut old.sounding);
            self.next_id = old.next_id;
        }
    }
}
