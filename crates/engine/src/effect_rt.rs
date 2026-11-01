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
pub(crate) fn xfade_len(sample_rate: u32) -> u32 {
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
    /// The user's wet/dry of the slot (0..1), on top of `mix`.
    pub wet: Smoothed,
    /// Plugin effects after Stop: their tail is not heard (VST3 has no
    /// realtime reset) until the transport plays or a signal arrives.
    pub hush: Smoothed,
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
            wet: Smoothed::new(1.0, xfade_len(sample_rate)),
            hush: Smoothed::new(1.0, HUSH_RAMP),
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
        self.wet.continue_from(&old.wet);
        self.hush = old.hush;
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
        if self.hush.target() == 0.0
            && (transport.playing || dl.iter().chain(dr.iter()).any(|v| v.abs() > SIGNAL))
        {
            self.hush.set_target(1.0);
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
        let full = |s: &Smoothed| !s.is_ramping() && s.current() == 1.0;
        if full(&self.mix) && full(&self.wet) && full(&self.hush) {
            return;
        }
        for i in 0..n {
            let m = self.mix.next_value() * self.wet.next_value();
            // The hush fades the effect's share only: no dry leaks in.
            let h = self.hush.next_value();
            l[i] = dl[i] * (1.0 - m) + l[i] * m * h;
            r[i] = dr[i] * (1.0 - m) + r[i] * m * h;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Transport = Transport {
        playing: true,
        tempo_bpm: 120.0,
        time_sig: (4, 4),
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
    fn wet_dry_mixes_the_effect_with_the_dry_signal() {
        // A full-wet reverb on a constant: its output differs from the
        // input, so the mix shows in the result.
        let mut full = reverb_slot(false);
        let wet = run(&mut full, 0.5, 20_000);
        let mut dry = reverb_slot(false);
        dry.wet = Smoothed::new(0.0, 64);
        assert!(
            run(&mut dry, 0.5, 20_000).iter().all(|v| *v == 0.5),
            "dry only"
        );
        let mut half = reverb_slot(false);
        half.wet = Smoothed::new(0.5, 64);
        let mixed = run(&mut half, 0.5, 20_000);
        for (i, (m, w)) in mixed.iter().zip(&wet).enumerate() {
            let want = 0.5 * 0.5 + 0.5 * w;
            assert!((m - want).abs() < 1e-5, "{i}: {m} vs {want}");
        }
        // A change ramps without a jump.
        half.wet.set_target(1.0);
        let out = run(&mut half, 0.5, 4_000);
        let max_step = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.05, "step {max_step}");
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

/// Above this a block holds a signal (about -100 dBFS).
const SIGNAL: f32 = 1e-5;

/// Ramp of the after-Stop silence of plugin instruments, in samples.
const HUSH_RAMP: u32 = 64;
/// Silence after which an instrument's tail counts as over, in samples.
const TAIL_OVER: u32 = 8_192;

/// Notes per block and sounding notes per instrument channel.
pub const MAX_INSTRUMENT_EVENTS: usize = 512;
pub const MAX_SOUNDING_NOTES: usize = 256;
/// Gate end marking live notes: never reached by the timeline.
const LIVE: u64 = u64::MAX - 1;
/// Pitch glides of plugin notes send a point this often, in samples
/// (spec 0.3.6 §3).
pub const GLIDE_STEP: u64 = 64;

/// A plugin note gliding (slide notes): its tuning goes from `from` to
/// `to` semitones over `[start, start + len)` of the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Glide {
    note_id: i32,
    key: u8,
    from: f32,
    to: f32,
    start: u64,
    len: u64,
}

impl Glide {
    fn at(&self, pos: u64) -> f32 {
        if pos >= self.start + self.len {
            self.to
        } else if pos <= self.start {
            self.from
        } else {
            let t = (pos - self.start) as f32 / self.len as f32;
            self.from + (self.to - self.from) * t
        }
    }
}

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
    /// After Stop the plugin's release tail is not heard (VST3 has no
    /// realtime reset); the next note, playback, or the end of the tail
    /// brings the output back.
    hush: Smoothed,
    /// Samples the plugin has been silent while hushed.
    quiet: u32,
    /// Running pitch glides (slide notes, spec 0.3.6).
    glides: Vec<Glide>,
    /// A tuning other than 0 was sent: the next note-on resets it (§5).
    tuned: bool,
    /// The channel's pitch bend range for plugins without per-note
    /// tuning, in semitones.
    pub bend_range: f32,
}

impl InstrumentRt {
    pub fn new(plugin: PluginRt) -> Self {
        Self {
            plugin,
            auto_plugin: Vec::new(),
            events: Vec::with_capacity(MAX_INSTRUMENT_EVENTS),
            sounding: Vec::with_capacity(MAX_SOUNDING_NOTES),
            next_id: 0,
            hush: Smoothed::new(1.0, HUSH_RAMP),
            quiet: 0,
            glides: Vec::with_capacity(MAX_SOUNDING_NOTES),
            tuned: false,
            bend_range: 2.0,
        }
    }

    fn tuning(&mut self, offset: usize, note_id: i32, key: u8, semitones: f32) {
        if semitones != 0.0 {
            self.tuned = true;
        }
        self.push(jdance_plugin_api::NoteEvent {
            offset: offset as u32,
            note_id,
            key,
            kind: jdance_plugin_api::NoteKind::Tuning {
                semitones,
                bend_range: self.bend_range,
            },
        });
    }

    /// A slide note at block `offset`, timeline `pos` (spec 0.3.6 §2):
    /// the sounding sequencer notes glide over `length` samples so that
    /// the highest reaches `key`, the others by the same interval.
    pub fn slide(&mut self, pos: u64, key: u8, length: u64) {
        let target_of =
            |g: &[Glide], id: i32| g.iter().find(|g| g.note_id == id).map_or(0.0, |g| g.to);
        let top = self
            .sounding
            .iter()
            .filter(|(end, _, _)| *end != LIVE + 1 && *end > pos)
            .map(|&(_, k, id)| f32::from(k) + target_of(&self.glides, id))
            .fold(f32::NEG_INFINITY, f32::max);
        if !top.is_finite() {
            return;
        }
        let interval = f32::from(key) - top;
        for i in 0..self.sounding.len() {
            let (end, k, id) = self.sounding[i];
            if end == LIVE + 1 || end <= pos {
                continue;
            }
            let (from, to) = match self.glides.iter().position(|g| g.note_id == id) {
                Some(j) => {
                    let g = self.glides.swap_remove(j);
                    (g.at(pos), g.to + interval)
                }
                None => (0.0, interval),
            };
            if self.glides.len() < self.glides.capacity() {
                self.glides.push(Glide {
                    note_id: id,
                    key: k,
                    from,
                    to,
                    start: pos,
                    len: length.max(1),
                });
            }
        }
    }

    /// Tuning points of the running glides in `[from, to)` of the
    /// timeline, which starts at block offset `offset`: every
    /// [`GLIDE_STEP`] samples and the exact target at each glide's end.
    /// A finished glide stays (sending nothing) to remember the note's
    /// tuning for the next slide.
    pub fn glides(&mut self, from: u64, to: u64, offset: usize) {
        let sounding = &self.sounding;
        self.glides
            .retain(|g| sounding.iter().any(|s| s.2 == g.note_id));
        for i in 0..self.glides.len() {
            let g = self.glides[i];
            let end = g.start + g.len;
            let mut t = g.start.max(from);
            let rem = (t - g.start) % GLIDE_STEP;
            if rem != 0 {
                t += GLIDE_STEP - rem;
            }
            while t < to && t < end {
                self.tuning(offset + (t - from) as usize, g.note_id, g.key, g.at(t));
                t += GLIDE_STEP;
            }
            if end >= from && end < to {
                self.tuning(offset + (end - from) as usize, g.note_id, g.key, g.to);
            }
        }
    }

    /// Silences the plugin's tail at once (Stop).
    pub fn hush_tail(&mut self) {
        self.hush.reset(0.0);
        self.quiet = 0;
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

    /// Applies the channel's voice limits before a new note at block
    /// `offset`: mono ends every sounding note, a polyphony limit ends the
    /// oldest ones.
    pub fn make_room(&mut self, offset: usize, mono: bool, polyphony: u16) {
        if mono {
            self.all_off(offset);
            return;
        }
        let limit = usize::from(polyphony);
        while limit > 0 && self.sounding.len() >= limit {
            let oldest = (0..self.sounding.len())
                .min_by_key(|&i| self.sounding[i].2.wrapping_sub(self.next_id))
                .expect("non-empty");
            let (_, key, note_id) = self.sounding.swap_remove(oldest);
            self.push(jdance_plugin_api::NoteEvent {
                offset: offset as u32,
                note_id,
                key,
                kind: jdance_plugin_api::NoteKind::Off,
            });
        }
    }

    /// A note-on at block `offset`, timeline `pos`, gate `length` samples.
    pub fn note_on(&mut self, offset: usize, pos: u64, key: u8, velocity: f32, length: u64) {
        if self.sounding.len() == self.sounding.capacity() {
            return;
        }
        self.next_id = self.next_id.wrapping_add(1);
        let note_id = self.next_id;
        self.hush.set_target(1.0);
        self.push(jdance_plugin_api::NoteEvent {
            offset: offset as u32,
            note_id,
            key,
            kind: jdance_plugin_api::NoteKind::On { velocity },
        });
        // After a slide the new note plays at its own pitch (§5).
        if self.tuned {
            self.tuning(offset, note_id, key, 0.0);
            self.tuned = false;
        }
        self.sounding.push((pos + length.max(1), key, note_id));
    }

    /// A held note (keyboard, MIDI input): no gate, ends at
    /// [`Self::live_off`] or with every other note.
    pub fn live_on(&mut self, key: u8, velocity: f32) {
        self.note_on(0, LIVE, key, velocity, 0);
    }

    pub fn live_off(&mut self, key: u8) {
        let mut i = 0;
        while i < self.sounding.len() {
            let (end, k, note_id) = self.sounding[i];
            if end == LIVE + 1 && k == key {
                self.push(jdance_plugin_api::NoteEvent {
                    offset: 0,
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
        // Offs before ons, ons before their tuning, at the same offset
        // (retriggers, spec 0.3.6 §4).
        self.events.sort_unstable_by_key(|e| {
            let rank = match e.kind {
                jdance_plugin_api::NoteKind::Off => 0,
                jdance_plugin_api::NoteKind::On { .. } => 1,
                jdance_plugin_api::NoteKind::Tuning { .. } => 2,
            };
            (e.offset, rank, e.note_id)
        });
        match &mut self.plugin.processor {
            Some(p) => p.process_instrument(l, r, &self.events, transport),
            None => {
                l.fill(0.0);
                r.fill(0.0);
            }
        }
        self.events.clear();
        if self.hush.target() == 0.0 {
            // Once the tail has died away, or playback starts, sound the
            // plugin makes on its own is heard again: a preset or beat
            // preview, its own sequencer (Addictive Drums).
            let loud = l.iter().chain(r.iter()).any(|v| v.abs() > SIGNAL);
            self.quiet = if loud {
                0
            } else {
                self.quiet.saturating_add(l.len() as u32)
            };
            if transport.playing || self.quiet >= TAIL_OVER {
                self.hush.set_target(1.0);
            }
        }
        if self.hush.is_ramping() || self.hush.current() != 1.0 {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let g = self.hush.next_value();
                *a *= g;
                *b *= g;
            }
        }
    }

    pub fn take_state(&mut self, old: &mut InstrumentRt) {
        self.plugin.take_state(&mut old.plugin);
        self.hush = old.hush;
        self.quiet = old.quiet;
        if self.plugin.processor.is_some() && old.plugin.processor.is_none() {
            // Same instance: keep its sounding notes.
            std::mem::swap(&mut self.sounding, &mut old.sounding);
            std::mem::swap(&mut self.glides, &mut old.glides);
            self.next_id = old.next_id;
            self.tuned = old.tuned;
        }
    }
}
