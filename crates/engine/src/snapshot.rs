// SPDX-License-Identifier: GPL-3.0-or-later
//! Compilation of the document into a realtime snapshot.
//!
//! A snapshot holds both the structure (events, routing in processing
//! order, delay-compensation amounts) and the runtime state that must
//! survive recompilation: smoothed gains, delay-line contents and effect
//! state. [`PlaybackSnapshot::take_state_from`] moves that state from the
//! previous snapshot by object ID without allocating, so edits do not
//! cause clicks or reset delay lines.

use crate::automation::{AutoMap, AutomationRt};
use crate::effect_rt::{EffectRt, InsertRt, InstrumentRt, PluginRt, SidechainRt};
use jdance_domain::{
    ChannelId, InsertId, MAX_CHANNELS, MixerTrack, PluginSlot, PluginTarget, Project, ROOT_KEY,
    SampleRef, SendId, TICKS_PER_STEP, TrackId,
};
use jdance_dsp::{
    DelayLine, SafetyLimiter, SampleBuffer, Smoothed, StereoGain, balance_gains, pan_gains,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Largest delay compensation applied on one edge (MIX-031), ≈ 2.7 s at
/// 48 kHz. Larger requirements are clamped and reported.
pub const MAX_PDC_SAMPLES: u32 = 1 << 17;

/// Gain ramp length, ≈ 10 ms (MIX-011).
fn ramp_len(sample_rate: u32) -> u32 {
    (sample_rate / 100).max(1)
}

#[derive(Debug)]
pub struct PlaybackSnapshot {
    pub sample_rate: u32,
    /// Timeline length shown by the playhead: the pattern length in
    /// pattern mode, the arrangement end in song mode.
    pub loop_len: u64,
    /// Where event generation ends (no loop) — arrangement end.
    pub end: u64,
    /// Region `[start, end)` that playback wraps around, in samples.
    pub loop_range: Option<(u64, u64)>,
    /// Samples played after `end` before the transport stops.
    pub end_tail: u64,
    pub samples_per_tick: f64,
    /// Triggers sorted by absolute timeline position `pos`.
    pub events: Vec<StepEvent>,
    pub channels: Vec<ChannelRt>,
    /// Mixer tracks in processing order; the master is last.
    pub tracks: Vec<TrackRt>,
    /// Total output latency at the master, in samples (MIX-034).
    pub latency: u32,
    /// Some edge needed more than [`MAX_PDC_SAMPLES`] of compensation.
    pub pdc_clamped: bool,
    pub tempo_bpm: f64,
    pub time_signature: jdance_domain::TimeSignature,
    /// Master safety limiter stage; `None` when disabled.
    pub limiter: Option<SafetyLimiter>,
    /// Automation lanes (song mode only, ADR-014).
    pub automation: Vec<AutomationRt>,
    /// Compiled for song playback.
    pub song: bool,
    /// Audio clips (song mode), sorted by start.
    pub audio_clips: Vec<AudioClipRt>,
}

/// An audio clip at engine-rate positions (spec 0.2.0 §2). Its output is a
/// pure function of the timeline position, so seeking into a clip, loop
/// wraps and delay compensation need no state.
#[derive(Debug)]
pub struct AudioClipRt {
    pub id: jdance_domain::ClipId,
    /// The asset converted to the engine rate.
    pub buffer: Arc<SampleBuffer>,
    /// Index into [`PlaybackSnapshot::channels`]: the audio channel the
    /// clip plays through (spec 0.2.2 ACH-010).
    pub channel: usize,
    /// Timeline `[start, end)` in samples.
    pub start: u64,
    pub end: u64,
    /// Buffer frame played at `start`.
    pub source: u64,
    pub gain: f32,
    /// Fade lengths in samples (at least the anti-click fade).
    pub fade_in: u64,
    pub fade_out: u64,
    /// Index into [`PlaybackSnapshot::automation`] for the clip gain.
    pub auto_gain: Option<usize>,
}

impl AudioClipRt {
    /// Adds the clip's output for timeline samples `[from, from + n)`
    /// into `l` and `r` (`n` = their length). Realtime-safe. Automated
    /// gain is read at 32-sample steps of the (undelayed) timeline and
    /// ramped linearly between them, so it is block-size independent.
    pub fn render_add(&self, from: u64, l: &mut [f32], r: &mut [f32], automation: &[AutomationRt]) {
        let n = l.len() as u64;
        let (start, end) = (self.start, self.end);
        let (a, b) = (from.max(start), (from + n).min(end));
        if a >= b {
            return;
        }
        let buf = &*self.buffer;
        for t in a..b {
            let k = t - start;
            let Some(i) = usize::try_from(self.source + k)
                .ok()
                .filter(|i| *i < buf.len())
            else {
                break;
            };
            let fin = if self.fade_in > 0 {
                ((k + 1) as f32 / self.fade_in as f32).min(1.0)
            } else {
                1.0
            };
            let fout = if self.fade_out > 0 {
                ((end - t) as f32 / self.fade_out as f32).min(1.0)
            } else {
                1.0
            };
            let g = self.gain_at(t, automation) * fin * fout;
            let o = (t - from) as usize;
            l[o] += buf.left[i] * g;
            r[o] += buf.right[i] * g;
        }
    }
}

impl AudioClipRt {
    fn gain_at(&self, pos: u64, automation: &[AutomationRt]) -> f32 {
        let Some(a) = self.auto_gain.and_then(|i| automation.get(i)) else {
            return self.gain;
        };
        let step = crate::automation::AUTOMATION_STEP as u64;
        let at = |p: u64| a.plain_at(p).unwrap_or(self.gain);
        let k = pos % step;
        if k == 0 {
            return at(pos);
        }
        let (g0, g1) = (at(pos - k), at(pos - k + step));
        g0 + (g1 - g0) * k as f32 / step as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepEvent {
    /// Absolute timeline position in samples.
    pub pos: u64,
    /// Index into [`PlaybackSnapshot::channels`].
    pub channel: usize,
    pub velocity: f32,
    /// MIDI key; steps play [`ROOT_KEY`].
    pub key: u8,
    /// Gate length in samples (one step for steps), cut at the end of the
    /// pattern repetition. One-shot samplers ignore it.
    pub length: u64,
}

#[derive(Debug)]
pub struct ChannelRt {
    pub id: ChannelId,
    /// `None` when the channel has no sample or it failed to load.
    pub sample: Option<Arc<SampleBuffer>>,
    /// Index into [`PlaybackSnapshot::tracks`].
    pub track: usize,
    /// Volume × pan × mute.
    pub gain: StereoGain,
    /// Delay compensation on the channel → track edge.
    pub delay: DelayLine,
    pub muted: bool,
    /// Plugin instrument instead of the sampler.
    pub instrument: Option<InstrumentRt>,
    /// Transposition in semitones.
    pub pitch: i32,
    /// 0 = no limit.
    pub polyphony: u16,
    pub mono: bool,
    pub sampler: SamplerRt,
    /// Cut group: notes of this channel cut voices of channels whose
    /// `cut_by` is this number (spec 0.2.5 §4); 0 = none.
    pub cut: u8,
    pub cut_by: u8,
}

/// Sampler settings in frames of the loaded sample and samples of the
/// engine rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplerRt {
    pub playback: jdance_dsp::Playback,
    /// The voice follows note length (envelope or loop mode).
    pub gated: bool,
    /// Linear sampler gain.
    pub gain: f32,
    pub root_key: u8,
    /// Fine tuning as a rate factor.
    pub fine: f64,
    /// The channel's pitch acts on notes; note keys change the pitch.
    pub main_pitch: bool,
    pub add_to_key: bool,
}

impl SamplerRt {
    pub fn new(s: &jdance_domain::SamplerSettings, frames: usize, sample_rate: u32) -> Self {
        let at = |f: f32| ((f64::from(f) * frames as f64).round() as usize).min(frames);
        let ms = |v: f32| (f64::from(v) * f64::from(sample_rate) / 1000.0).round() as u32;
        let looping = s.mode == jdance_domain::PlaybackMode::Loop;
        let (start, end) = (
            at(s.start),
            at(s.end).max(at(s.start) + 1).min(frames.max(1)),
        );
        let whole = start == 0 && end >= frames;
        let (fade_in, fade_out, bleed) = s.declick.fades();
        let playback = jdance_dsp::Playback {
            start,
            end: if whole { usize::MAX } else { end },
            loop_range: looping.then(|| (at(s.loop_start), at(s.loop_end))),
            adsr: s.envelope.then(|| jdance_dsp::Adsr {
                attack: ms(s.attack_ms),
                decay: ms(s.decay_ms),
                sustain: s.sustain,
                release: ms(s.release_ms),
            }),
            gate: None,
            gate_release: (sample_rate / 200).max(1),
            fade_in: ms(fade_in),
            fade_out: ms(fade_out),
            bleed,
        };
        Self {
            playback,
            gated: s.envelope || looping,
            gain: 10f32.powf(s.gain_db / 20.0),
            root_key: s.root_key,
            fine: 2f64.powf(f64::from(s.fine_tune) / 1200.0),
            main_pitch: s.main_pitch,
            add_to_key: s.add_to_key,
        }
    }
}

#[derive(Debug)]
pub struct TrackRt {
    pub id: TrackId,
    pub inserts: Vec<InsertRt>,
    /// Volume × pan.
    pub fader: StereoGain,
    /// Mute applies to the main out and post-fader sends (MIX-012).
    pub mute: Smoothed,
    pub sends: Vec<SendRt>,
    /// Destination index and compensation delay; `None` for the master.
    pub main_out: Option<(usize, DelayLine)>,
    /// Latency of the signal leaving this track, relative to the timeline.
    pub latency: u32,
    /// Fade applied after the inserts around structural edits (insert
    /// added, removed or reordered), so effect changes do not click.
    pub struct_fade: Smoothed,
    /// Automation lanes of volume and pan, and the project values.
    pub auto_volume: Option<usize>,
    pub auto_pan: Option<usize>,
    pub base_volume: f32,
    pub base_pan: f32,
    /// Live hardware input summed in before the inserts: first device
    /// channel and stereo; only while monitored (spec 0.2.3 INP-004).
    pub live_input: Option<(usize, bool)>,
}

#[derive(Debug, Clone)]
pub struct SendRt {
    pub id: SendId,
    pub dest: usize,
    pub pre_fader: bool,
    pub gain: Smoothed,
    pub delay: DelayLine,
    /// Automation lane of the level, and the project level.
    pub auto_level: Option<usize>,
    pub base_level: f32,
}

impl PlaybackSnapshot {
    /// A snapshot that plays nothing and outputs silence.
    pub fn empty(sample_rate: u32) -> Self {
        compile(&Project::new(), sample_rate, |_| None)
    }

    /// Whether `self` and `new` differ in insert structure (an insert
    /// added, removed or moved) on some track. Realtime-safe.
    pub fn insert_structure_differs(&self, new: &PlaybackSnapshot) -> bool {
        self.tracks.iter().any(|t| {
            let Some(n) = new.tracks.iter().find(|n| n.id == t.id) else {
                return false;
            };
            t.inserts.len() != n.inserts.len()
                || t.inserts.iter().zip(&n.inserts).any(|(a, b)| a.id != b.id)
        })
    }

    /// Starts the structural fade-out on tracks whose inserts differ in
    /// `new`, and marks the same tracks in `new` to fade in after the
    /// swap. Returns whether any track fades. Realtime-safe.
    pub fn begin_structural_fade(&mut self, new: &mut PlaybackSnapshot) -> bool {
        let mut any = false;
        for t in &mut self.tracks {
            let Some(n) = new.tracks.iter_mut().find(|n| n.id == t.id) else {
                continue;
            };
            let differs = t.inserts.len() != n.inserts.len()
                || t.inserts.iter().zip(&n.inserts).any(|(a, b)| a.id != b.id);
            if differs {
                t.struct_fade.set_target(0.0);
                n.struct_fade.reset(0.0);
                n.struct_fade.set_target(1.0);
                any = true;
            }
        }
        any
    }

    /// Whether every structural fade-out has reached silence.
    pub fn structural_fade_done(&self) -> bool {
        self.tracks
            .iter()
            .all(|t| t.struct_fade.target() != 0.0 || !t.struct_fade.is_ramping())
    }

    /// Estimated effect tail at the master in seconds: the longest chain
    /// of insert tails along any track plus the master chain. An estimate
    /// for export warnings, not a guarantee.
    pub fn tail_seconds(&self) -> f32 {
        let chain = |t: &TrackRt| {
            t.inserts
                .iter()
                .map(|i| i.effect.tail().seconds())
                .sum::<f32>()
        };
        let master = self.tracks.last().map_or(0.0, chain);
        let longest = self
            .tracks
            .iter()
            .take(self.tracks.len().saturating_sub(1))
            .map(chain)
            .fold(0.0, f32::max);
        longest + master
    }

    /// Latency of each track's output, by ID (for the UI, MIX-034).
    pub fn track_latencies(&self) -> Vec<(TrackId, u32)> {
        self.tracks.iter().map(|t| (t.id, t.latency)).collect()
    }

    /// Moves runtime state (gain positions, delay contents, effect state)
    /// from `old` into `self` for objects present in both. No allocation:
    /// buffers are swapped, so `old` ends up holding `self`'s fresh ones.
    pub fn take_state_from(&mut self, old: &mut PlaybackSnapshot) {
        if let (Some(l), Some(ol)) = (&mut self.limiter, &old.limiter) {
            l.take_state(ol);
        }
        for ch in &mut self.channels {
            if let Some(o) = old.channels.iter_mut().find(|o| o.id == ch.id) {
                ch.gain.continue_from(&o.gain);
                ch.delay.take_state(&mut o.delay);
                if let (Some(n), Some(oi)) = (&mut ch.instrument, &mut o.instrument) {
                    n.take_state(oi);
                }
            }
        }
        for t in &mut self.tracks {
            let Some(o) = old.tracks.iter_mut().find(|o| o.id == t.id) else {
                continue;
            };
            t.fader.continue_from(&o.fader);
            t.mute.continue_from(&o.mute);
            t.struct_fade.continue_from(&o.struct_fade);
            if let (Some((_, d)), Some((_, od))) = (&mut t.main_out, &mut o.main_out) {
                d.take_state(od);
            }
            for s in &mut t.sends {
                if let Some(os) = o.sends.iter_mut().find(|os| os.id == s.id) {
                    s.gain.continue_from(&os.gain);
                    s.delay.take_state(&mut os.delay);
                }
            }
            for i in &mut t.inserts {
                if let Some(oi) = o.inserts.iter_mut().find(|oi| oi.id == i.id) {
                    i.take_state(oi);
                }
            }
        }
    }
}

/// Compiles the project's automation per target: its sources, active
/// inside the windows of their (playing) clips (spec 0.2.0 §11), and
/// attaches it to the target. Sources whose target is gone are skipped
/// (AUT-050).
fn bind_automation(
    project: &Project,
    snap: &mut PlaybackSnapshot,
    to_sample: &impl Fn(u64) -> u64,
) {
    use jdance_domain::AutomationTarget as T;
    let find_insert = |snap: &PlaybackSnapshot, id: InsertId| {
        snap.tracks
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.inserts.iter().position(|i| i.id == id).map(|ii| (ti, ii)))
    };
    let pl = &project.playlist;
    let mut targets: Vec<(&jdance_domain::AutomationTarget, AutomationRt)> = Vec::new();
    for lane in &project.automation {
        let clips = pl
            .automation_clips
            .iter()
            .filter(|c| c.source == lane.id && pl.admits_automation(c));
        let k = match targets.iter().position(|(t, _)| **t == lane.target) {
            Some(k) => k,
            None => {
                targets.push((&lane.target, AutomationRt::new(AutoMap::Raw)));
                targets.len() - 1
            }
        };
        let rt = &mut targets[k].1;
        let source = rt.add_source(lane, to_sample);
        for c in clips {
            let (a, b) = c.interval();
            rt.add_window(source, to_sample(a), to_sample(b), to_sample(c.offset));
        }
    }
    for (target, mut rt) in targets {
        rt.finish();
        if rt.is_empty() {
            continue;
        }
        let index = snap.automation.len();
        let map = match target {
            T::TrackVolume(id) => {
                let Some(t) = snap.tracks.iter_mut().find(|t| t.id == *id) else {
                    continue;
                };
                t.auto_volume = Some(index);
                AutoMap::Volume
            }
            T::TrackPan(id) => {
                let Some(t) = snap.tracks.iter_mut().find(|t| t.id == *id) else {
                    continue;
                };
                t.auto_pan = Some(index);
                AutoMap::Pan
            }
            T::SendLevel(id) => {
                let Some(send) = snap
                    .tracks
                    .iter_mut()
                    .flat_map(|t| t.sends.iter_mut())
                    .find(|s| s.id == *id)
                else {
                    continue;
                };
                send.auto_level = Some(index);
                AutoMap::SendLevel
            }
            T::EffectParam { insert, param } => {
                let Some((ti, ii)) = find_insert(snap, *insert) else {
                    continue;
                };
                let Some(effect) = project
                    .mixer
                    .all_tracks()
                    .flat_map(|t| &t.inserts)
                    .find(|i| i.id == *insert)
                    .map(|i| &i.effect)
                else {
                    continue;
                };
                let Some(at) = effect.kind.params().iter().position(|p| p.id == param) else {
                    continue;
                };
                let spec = &effect.kind.params()[at];
                let base = effect.get(spec.id);
                snap.tracks[ti].inserts[ii]
                    .auto_params
                    .push((at as u16, index, base));
                AutoMap::Param(spec)
            }
            T::InsertBypass(insert) => {
                let Some((ti, ii)) = find_insert(snap, *insert) else {
                    continue;
                };
                let ins = &mut snap.tracks[ti].inserts[ii];
                ins.auto_bypass = Some((index, ins.mix.target() == 0.0));
                AutoMap::Bypass
            }
            T::PluginParam {
                target: PluginTarget::Insert(insert),
                param,
            } => {
                let Some((ti, ii)) = find_insert(snap, *insert) else {
                    continue;
                };
                snap.tracks[ti].inserts[ii]
                    .auto_plugin
                    .push((*param, index));
                AutoMap::Raw
            }
            T::PluginParam {
                target: PluginTarget::Channel(ch),
                param,
            } => {
                let Some(inst) = snap
                    .channels
                    .iter_mut()
                    .find(|c| c.id == *ch)
                    .and_then(|c| c.instrument.as_mut())
                else {
                    continue;
                };
                inst.auto_plugin.push((*param, index));
                AutoMap::Raw
            }
            T::AudioClipGain(id) => {
                let Some(c) = snap.audio_clips.iter_mut().find(|c| c.id == *id) else {
                    continue;
                };
                c.auto_gain = Some(index);
                AutoMap::Volume
            }
        };
        rt.map = map;
        snap.automation.push(rt);
    }
}

/// Sample position of a tick at a constant tempo. Uses absolute positions
/// so results never depend on block boundaries (RT-022).
pub fn tick_to_sample(tick: u64, tempo_bpm: f64, sample_rate: u32) -> u64 {
    (tick as f64 * samples_per_tick(tempo_bpm, sample_rate)).round() as u64
}

/// Tracks are stereo buses: balance, not constant-power pan (MIX-013).
fn fader_gains(t: &MixerTrack) -> (f32, f32) {
    let (l, r) = balance_gains(t.pan);
    (l * t.volume, r * t.volume)
}

struct Pdc {
    clamped: bool,
}

impl Pdc {
    fn line(&mut self, samples: u32) -> DelayLine {
        if samples > MAX_PDC_SAMPLES {
            self.clamped = true;
        }
        DelayLine::new(samples.min(MAX_PDC_SAMPLES) as usize)
    }
}

/// Compiles `project` for `sample_rate`. `samples` resolves sample
/// references to buffers already converted to `sample_rate`.
///
/// The project's routing must be valid (`Project::validate_routing`);
/// commands and persistence guarantee that. If it is not, the snapshot
/// falls back to routing every channel straight to the master.
pub fn compile(
    project: &Project,
    sample_rate: u32,
    samples: impl Fn(&SampleRef) -> Option<Arc<SampleBuffer>>,
) -> PlaybackSnapshot {
    let first = project
        .patterns
        .first()
        .map_or(jdance_domain::PatternId(0), |p| p.id);
    compile_with(project, sample_rate, PlayMode::Pattern(first), samples)
}

/// What the transport plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayMode {
    /// Loops one pattern.
    Pattern(jdance_domain::PatternId),
    /// Plays the playlist arrangement.
    Song,
}

/// Compiles `project` for `sample_rate` and `mode`. Plugin inserts
/// compile as pass-through placeholders; see [`compile_full`].
pub fn compile_with(
    project: &Project,
    sample_rate: u32,
    mode: PlayMode,
    samples: impl Fn(&SampleRef) -> Option<Arc<SampleBuffer>>,
) -> PlaybackSnapshot {
    compile_full(project, sample_rate, mode, samples, &mut |_, _| {
        PluginRt::missing()
    })
}

/// Like [`compile_with`]; `plugins` supplies each plugin insert: a fresh
/// instance, the instance carried over from the previous snapshot, or a
/// placeholder.
pub fn compile_full(
    project: &Project,
    sample_rate: u32,
    mode: PlayMode,
    samples: impl Fn(&SampleRef) -> Option<Arc<SampleBuffer>>,
    plugins: &mut dyn FnMut(PluginTarget, &PluginSlot) -> PluginRt,
) -> PlaybackSnapshot {
    let ramp = ramp_len(sample_rate);
    let mixer = &project.mixer;
    let order = match project
        .validate_routing()
        .and_then(|()| mixer.processing_order())
    {
        Ok(order) => order,
        Err(_) => vec![TrackId::MASTER],
    };
    let index: BTreeMap<TrackId, usize> = order.iter().enumerate().map(|(i, t)| (*t, i)).collect();
    let track_of = |id: TrackId| index.get(&id).copied().unwrap_or(order.len() - 1);

    // Incoming latency per track: max over its sources (MIX-030). Channels
    // contribute 0; tracks contribute their output latency.
    let silenced = mixer.silenced_by_solo();
    let mut in_lat = vec![0u32; order.len()];
    let mut out_lat = vec![0u32; order.len()];
    let mut pdc = Pdc { clamped: false };
    // Instruments first: their latency delays their channel (MIX-030).
    let mut instruments: Vec<Option<InstrumentRt>> = project
        .channels
        .iter()
        .take(MAX_CHANNELS)
        .map(|c| {
            c.instrument()
                .map(|slot| InstrumentRt::new(plugins(PluginTarget::Channel(c.id), slot)))
        })
        .collect();
    for (c, inst) in project.channels.iter().zip(&instruments) {
        if let Some(i) = inst {
            let t = track_of(c.track);
            in_lat[t] = in_lat[t].max(i.plugin.latency());
        }
    }
    let mut tracks: Vec<TrackRt> = Vec::with_capacity(order.len());

    for (ti, id) in order.iter().enumerate() {
        let t = mixer.track(*id).expect("ordered track exists");
        let mut acc = in_lat[ti];
        let mut inserts = Vec::with_capacity(t.inserts.len());
        for ins in &t.inserts {
            let effect = match &ins.effect.plugin {
                Some(slot) => EffectRt::Plugin(plugins(PluginTarget::Insert(ins.id), slot)),
                None => EffectRt::new(&ins.effect, sample_rate, project.tempo_bpm),
            };
            let sidechain_src = ins
                .sidechain
                .filter(|sc| index.contains_key(&sc.source))
                .map(|sc| (track_of(sc.source), sc.tap));
            let mut pre = 0;
            let sidechain = sidechain_src.map(|(src, tap)| {
                let sc_lat = out_lat[src];
                if sc_lat > acc {
                    pre = sc_lat - acc;
                }
                (src, tap, sc_lat)
            });
            acc += pre;
            let sidechain = sidechain.map(|(source, tap, sc_lat)| SidechainRt {
                source,
                tap,
                delay: pdc.line(acc - sc_lat),
            });
            acc += effect.latency();
            let mut slot = InsertRt::new(ins.id, effect, ins.bypass, sample_rate);
            slot.pre_delay = pdc.line(pre);
            slot.sidechain = sidechain;
            inserts.push(slot);
        }
        out_lat[ti] = acc;
        // Propagate to destinations, which come later in `order`.
        if !t.is_master() {
            let d = track_of(t.main_out);
            in_lat[d] = in_lat[d].max(acc);
        }
        for s in &t.sends {
            let d = track_of(s.dest);
            in_lat[d] = in_lat[d].max(acc);
        }
        tracks.push(TrackRt {
            id: t.id,
            inserts,
            fader: StereoGain::new(fader_gains(t), ramp),
            mute: Smoothed::new(
                if t.muted || silenced.contains(&t.id) {
                    0.0
                } else {
                    1.0
                },
                ramp,
            ),
            sends: Vec::new(),
            main_out: None,
            latency: acc,
            struct_fade: Smoothed::new(1.0, (sample_rate / 200).max(1)),
            auto_volume: None,
            auto_pan: None,
            base_volume: t.volume,
            base_pan: t.pan,
            live_input: t
                .input
                .filter(|_| t.monitor && !t.is_master())
                .map(|i| match i {
                    jdance_domain::TrackInput::Mono(c) => (usize::from(c), false),
                    jdance_domain::TrackInput::Stereo(c) => (usize::from(c), true),
                }),
        });
    }

    // Second pass: edges into destinations, now that every in_lat is final.
    for (ti, id) in order.iter().enumerate() {
        let t = mixer.track(*id).expect("ordered track exists");
        let sends = t
            .sends
            .iter()
            .map(|s| {
                let d = track_of(s.dest);
                SendRt {
                    id: s.id,
                    dest: d,
                    pre_fader: s.pre_fader,
                    gain: Smoothed::new(s.level, ramp),
                    delay: pdc.line(in_lat[d] - out_lat[ti]),
                    auto_level: None,
                    base_level: s.level,
                }
            })
            .collect();
        tracks[ti].sends = sends;
        if !t.is_master() {
            let d = track_of(t.main_out);
            tracks[ti].main_out = Some((d, pdc.line(in_lat[d] - out_lat[ti])));
        }
    }

    let channels: Vec<ChannelRt> = project
        .channels
        .iter()
        .take(MAX_CHANNELS)
        .enumerate()
        .map(|(i, c)| {
            let track = track_of(c.track);
            // Audio channels carry stereo material: balance, unity at
            // the center (ACH-011).
            // Level offsets of the sampler's Instrument tab (§4).
            let lv = c.sampler.levels;
            let pan = (c.pan + lv.pan).clamp(-1.0, 1.0);
            let (l, r) = if c.is_audio() {
                balance_gains(c.pan)
            } else {
                pan_gains(pan)
            };
            let g = if c.muted {
                0.0
            } else {
                c.volume * (1.0 + lv.vol).max(0.0)
            };
            let instrument = instruments[i].take();
            let own = instrument.as_ref().map_or(0, |x| x.plugin.latency());
            // The file, or its processed data (spec 0.2.5 §4).
            let sample = c
                .playback_sample(project.tempo_bpm)
                .and_then(|s| samples(&s))
                .filter(|s| s.sample_rate == sample_rate);
            let frames = sample.as_ref().map_or(0, |s| s.len());
            ChannelRt {
                id: c.id,
                sample,
                track,
                gain: StereoGain::new((l * g, r * g), ramp),
                delay: pdc.line(in_lat[track] - own),
                muted: c.muted,
                instrument,
                pitch: c.pitch,
                polyphony: c.polyphony,
                mono: c.mono,
                sampler: SamplerRt::new(&c.sampler, frames, sample_rate),
                cut: c.sampler.cut,
                cut_by: c.sampler.cut_by,
            }
        })
        .collect();

    let latency = out_lat.last().copied().unwrap_or(0);
    let mut snapshot = PlaybackSnapshot {
        sample_rate,
        loop_len: 0,
        end: 0,
        loop_range: None,
        end_tail: 0,
        samples_per_tick: 1.0,
        events: Vec::new(),
        channels,
        tracks,
        latency,
        pdc_clamped: pdc.clamped,
        tempo_bpm: project.tempo_bpm,
        time_signature: project.time_signature,
        limiter: mixer.limiter.enabled.then(|| {
            SafetyLimiter::new(
                mixer.limiter.ceiling_db,
                mixer.limiter.release_ms,
                sample_rate,
            )
        }),
        automation: Vec::new(),
        song: matches!(mode, PlayMode::Song),
        audio_clips: Vec::new(),
    };

    let spt = samples_per_tick(project.tempo_bpm, sample_rate);
    snapshot.samples_per_tick = spt;
    let to_sample = |tick: u64| (tick as f64 * spt).round() as u64;
    match mode {
        PlayMode::Pattern(id) => {
            let Some(pattern) = project.pattern(id).or_else(|| project.patterns.first()) else {
                return snapshot;
            };
            let len = to_sample(pattern.length_ticks());
            snapshot.loop_len = len;
            snapshot.end = len;
            snapshot.loop_range = Some((0, len));
            push_pattern_events(
                project,
                pattern,
                0,
                (0, pattern.length_ticks()),
                &to_sample,
                &mut snapshot.events,
            );
        }
        PlayMode::Song => {
            let pl = &project.playlist;
            for clip in pl.clips.iter().filter(|c| pl.admits(c)) {
                let Some(pattern) = project.pattern(clip.pattern) else {
                    continue;
                };
                // Repetitions in local pattern time, cut to the clip.
                let len = pattern.length_ticks().max(1);
                let (lo, hi) = clip.interval();
                let offset = if clip.looped {
                    clip.offset % len
                } else {
                    clip.offset
                };
                let mut base = lo as i64 - offset as i64;
                while base < hi as i64 {
                    push_pattern_events(
                        project,
                        pattern,
                        base,
                        (lo, hi),
                        &to_sample,
                        &mut snapshot.events,
                    );
                    if !clip.looped {
                        break;
                    }
                    base += len as i64;
                }
            }
            let audio_end =
                compile_audio_clips(project, sample_rate, &samples, &mut snapshot.audio_clips);
            let end = to_sample(pl.arrangement_end()).max(audio_end);
            snapshot.end = end;
            snapshot.loop_len = end;
            snapshot.loop_range = pl
                .loop_range
                .filter(|_| pl.loop_enabled)
                .map(|(a, b)| (to_sample(a), to_sample(b)))
                .filter(|(a, b)| a < b);
            // After the end: let effects ring for their estimated tail
            // (bounded), then stop the transport.
            let tail = snapshot.tail_seconds().clamp(0.0, MAX_END_TAIL_SECONDS);
            snapshot.end_tail = snapshot.latency as u64 + (tail * sample_rate as f32) as u64;
        }
    }
    if snapshot.song {
        bind_automation(project, &mut snapshot, &to_sample);
    }
    // Overlapping clips keep all events (no de-duplication).
    snapshot.events.sort_by_key(|e| (e.pos, e.channel));
    snapshot
}

/// Longest tail let ring after the arrangement end in realtime playback.
pub const MAX_END_TAIL_SECONDS: f32 = 10.0;

pub fn samples_per_tick(tempo_bpm: f64, sample_rate: u32) -> f64 {
    f64::from(sample_rate) * 60.0 / (tempo_bpm * f64::from(jdance_domain::PPQ))
}

/// Places the project's audio clips at `sample_rate`; returns the end of
/// the last one in samples. Clips whose file is not loaded are skipped.
fn compile_audio_clips(
    project: &Project,
    sample_rate: u32,
    samples: &impl Fn(&SampleRef) -> Option<Arc<SampleBuffer>>,
    out: &mut Vec<AudioClipRt>,
) -> u64 {
    let pl = &project.playlist;
    let tb = f64::from(project.timebase_rate.max(1));
    let sr = f64::from(sample_rate);
    let fade = (jdance_domain::EDGE_FADE_SECONDS * sr).round() as u64;
    let mut end = 0;
    let channel_of = |id| {
        project
            .channels
            .iter()
            .take(MAX_CHANNELS)
            .position(|c| c.id == id)
    };
    for c in &pl.audio_clips {
        let Some(asset) = project.clip_asset(c) else {
            continue;
        };
        let to_dev = |frames: u64, rate: f64| (frames as f64 * sr / rate).round() as u64;
        let start = to_dev(c.start, tb);
        let len = to_dev(c.length(asset, project.timebase_rate), tb);
        end = end.max(start + len);
        if !pl.admits_audio(c) || len == 0 {
            continue;
        }
        let Some(buffer) = samples(&SampleRef {
            path: asset.path.clone(),
        })
        .filter(|b| b.sample_rate == sample_rate) else {
            continue;
        };
        let Some(channel) = channel_of(c.channel) else {
            continue;
        };
        let bound = |f: u64| f.max(fade).min(len / 2);
        out.push(AudioClipRt {
            id: c.id,
            buffer,
            channel,
            start,
            end: start + len,
            source: to_dev(c.source_in, f64::from(asset.sample_rate.max(1))),
            gain: c.gain,
            fade_in: bound(to_dev(c.fade_in, tb)),
            fade_out: bound(to_dev(c.fade_out, tb)),
            auto_gain: None,
        });
    }
    out.sort_by_key(|c| c.start);
    end
}

/// Adds the pattern's active steps and notes of the repetition starting
/// at `base` ticks (may be negative when a clip starts mid-pattern),
/// keeping only events inside `window` ticks; gates end at the
/// repetition's or the window's end.
fn push_pattern_events(
    project: &Project,
    pattern: &jdance_domain::Pattern,
    base: i64,
    window: (u64, u64),
    to_sample: &impl Fn(u64) -> u64,
    out: &mut Vec<StepEvent>,
) {
    let (lo, hi) = (window.0 as i64, window.1 as i64);
    let rep_end = (base + pattern.length_ticks() as i64).min(hi);
    let inside = |t: i64| t >= lo && t < rep_end;
    for row in &pattern.rows {
        let Some(ch_index) = project
            .channel_index(row.channel)
            .filter(|i| *i < MAX_CHANNELS)
        else {
            continue;
        };
        if project.channels[ch_index].muted {
            continue;
        }
        for (i, step) in row
            .steps
            .iter()
            .enumerate()
            .take(pattern.length_steps as usize)
        {
            let tick = base + i as i64 * i64::from(TICKS_PER_STEP);
            if !step.on || !inside(tick) {
                continue;
            }
            let pos = to_sample(tick as u64);
            // Instruments hold a step for one step; samplers ignore it.
            let gate_end = to_sample((tick + i64::from(TICKS_PER_STEP)).min(hi) as u64);
            out.push(StepEvent {
                pos,
                channel: ch_index,
                velocity: step.velocity,
                key: ROOT_KEY,
                length: gate_end.saturating_sub(pos).max(1),
            });
        }
        // Notes start inside the pattern; gated notes end at the
        // repetition's (or the clip's) end at the latest.
        for n in &row.notes {
            let start = base + i64::from(n.start);
            if !inside(start) {
                continue;
            }
            let pos = to_sample(start as u64);
            let end = to_sample((start + i64::from(n.length)).min(rep_end) as u64);
            out.push(StepEvent {
                pos,
                channel: ch_index,
                velocity: n.velocity,
                key: n.key,
                length: end.saturating_sub(pos).max(1),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{
        Channel, ChannelSource, Effect, EffectKind, Insert, InsertId, MixerTrack, Send, Sidechain,
        Tap,
    };

    #[test]
    fn step_positions_at_120_bpm() {
        // 120 BPM at 48 kHz: a sixteenth is 6000 samples.
        assert_eq!(tick_to_sample(240, 120.0, 48_000), 6000);
        assert_eq!(tick_to_sample(16 * 240, 120.0, 48_000), 96_000);
    }

    #[test]
    fn compile_orders_events_and_skips_muted() {
        let mut p = Project::new();
        p.tempo_bpm = 120.0;
        let a = ChannelId(p.ids.next());
        let b = ChannelId(p.ids.next());
        let mut cb = Channel::new_sampler(b, "B");
        cb.muted = true;
        p.channels.push(Channel::new_sampler(a, "A"));
        p.channels.push(cb);
        let pat = &mut p.patterns[0];
        pat.row_mut(a).steps[4].on = true;
        pat.row_mut(a).steps[0].on = true;
        pat.row_mut(b).steps[0].on = true;
        let s = compile(&p, 48_000, |_| None);
        assert_eq!(s.loop_len, 96_000);
        let pos: Vec<u64> = s.events.iter().map(|e| e.pos).collect();
        assert_eq!(pos, [0, 24_000]);
        assert_eq!(s.channels[1].gain.l.target(), 0.0);
    }

    #[test]
    fn sample_with_wrong_rate_is_ignored() {
        let mut p = Project::new();
        let a = ChannelId(p.ids.next());
        let mut c = Channel::new_sampler(a, "A");
        c.source = ChannelSource::Sampler {
            sample: Some(SampleRef {
                path: "x.wav".into(),
            }),
        };
        p.channels.push(c);
        let buf = Arc::new(SampleBuffer::from_interleaved(44_100, 1, &[1.0]));
        let s = compile(&p, 48_000, |_| Some(Arc::clone(&buf)));
        assert!(s.channels[0].sample.is_none());
    }

    fn lookahead(ms: f32) -> Effect {
        Effect::new(EffectKind::Compressor)
            .with("ratio", 1.0)
            .with("lookahead", ms)
    }

    #[test]
    fn pdc_amounts_for_parallel_send_and_sidechain() {
        // 48 kHz: 1 ms lookahead = 48 samples latency.
        let mut p = Project::new();
        let mut a = MixerTrack::new(TrackId(1), "A");
        a.inserts.push(Insert {
            id: InsertId(10),
            bypass: false,
            effect: lookahead(1.0),
            sidechain: None,
        });
        a.sends.push(Send {
            id: SendId(11),
            dest: TrackId(3),
            level: 1.0,
            pre_fader: false,
        });
        let b = MixerTrack::new(TrackId(2), "B");
        let mut c = MixerTrack::new(TrackId(3), "Return");
        // Sidechain from B (latency 0) into a 2 ms lookahead compressor.
        c.inserts.push(Insert {
            id: InsertId(12),
            bypass: false,
            effect: lookahead(2.0),
            sidechain: Some(Sidechain {
                source: TrackId(2),
                tap: Tap::PreFader,
            }),
        });
        p.mixer.tracks = vec![a, b, c];
        assert!(p.validate_routing().is_ok());
        let s = compile(&p, 48_000, |_| None);
        let t = |id: u64| s.tracks.iter().find(|t| t.id == TrackId(id)).unwrap();
        assert_eq!(t(1).latency, 48);
        assert_eq!(t(2).latency, 0);
        // Return gets A's send at 48 → its compressor's sidechain from B
        // (0) is delayed by 48; then +96 lookahead.
        let ret = t(3);
        assert_eq!(ret.inserts[0].sidechain.as_ref().unwrap().delay.len(), 48);
        assert_eq!(ret.inserts[0].pre_delay.len(), 0);
        assert_eq!(ret.latency, 48 + 96);
        // Master aligns A (48), B (0) and Return (144): total 144.
        assert_eq!(s.latency, 144);
        assert_eq!(t(1).main_out.as_ref().unwrap().1.len(), 96);
        assert_eq!(t(2).main_out.as_ref().unwrap().1.len(), 144);
        assert_eq!(t(3).main_out.as_ref().unwrap().1.len(), 0);
        assert_eq!(t(1).sends[0].delay.len(), 0);
        assert_eq!(s.tracks.last().unwrap().id, TrackId::MASTER);
    }

    #[test]
    fn late_sidechain_delays_main_path() {
        let mut p = Project::new();
        let mut kick = MixerTrack::new(TrackId(1), "Kick");
        kick.inserts.push(Insert {
            id: InsertId(10),
            bypass: false,
            effect: lookahead(1.0),
            sidechain: None,
        });
        let mut bass = MixerTrack::new(TrackId(2), "Bass");
        bass.inserts.push(Insert {
            id: InsertId(11),
            bypass: false,
            effect: lookahead(0.0),
            sidechain: Some(Sidechain {
                source: TrackId(1),
                tap: Tap::PreFader,
            }),
        });
        p.mixer.tracks = vec![kick, bass];
        let s = compile(&p, 48_000, |_| None);
        let bass = s.tracks.iter().find(|t| t.id == TrackId(2)).unwrap();
        assert_eq!(bass.inserts[0].pre_delay.len(), 48);
        assert_eq!(bass.inserts[0].sidechain.as_ref().unwrap().delay.len(), 0);
        assert_eq!(bass.latency, 48);
    }
}
