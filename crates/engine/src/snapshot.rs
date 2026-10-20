// SPDX-License-Identifier: GPL-3.0-or-later
//! Compilation of the document into a realtime snapshot.
//!
//! A snapshot holds both the structure (events, routing in processing
//! order, delay-compensation amounts) and the runtime state that must
//! survive recompilation: smoothed gains, delay-line contents and effect
//! state. [`PlaybackSnapshot::take_state_from`] moves that state from the
//! previous snapshot by object ID without allocating, so edits do not
//! cause clicks or reset delay lines.

use jdance_domain::{
    ChannelId, Effect, InsertId, MAX_CHANNELS, MixerTrack, Project, SampleRef, SendId, Tap, TrackId,
};
use jdance_dsp::{
    Compressor, CompressorSettings, DelayLine, SampleBuffer, Smoothed, StereoGain, balance_gains,
    pan_gains,
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

#[derive(Debug, Clone)]
pub struct PlaybackSnapshot {
    pub sample_rate: u32,
    /// Pattern loop length in samples; `0` means nothing to play.
    pub loop_len: u64,
    /// Step triggers sorted by `pos`, all `< loop_len`.
    pub events: Vec<StepEvent>,
    pub channels: Vec<ChannelRt>,
    /// Mixer tracks in processing order; the master is last.
    pub tracks: Vec<TrackRt>,
    /// Total output latency at the master, in samples (MIX-034).
    pub latency: u32,
    /// Some edge needed more than [`MAX_PDC_SAMPLES`] of compensation.
    pub pdc_clamped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepEvent {
    /// Sample position inside the loop.
    pub pos: u64,
    /// Index into [`PlaybackSnapshot::channels`].
    pub channel: usize,
    pub velocity: f32,
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
pub struct InsertRt {
    pub id: InsertId,
    pub bypass: bool,
    /// Delays the main signal before the effect when its sidechain
    /// arrives later than the main signal (MIX-030).
    pub pre_delay: DelayLine,
    pub effect: EffectRt,
    pub sidechain: Option<SidechainRt>,
}

#[derive(Debug, Clone)]
pub struct SidechainRt {
    /// Index of the source track (always earlier in processing order).
    pub source: usize,
    pub tap: Tap,
    pub delay: DelayLine,
}

#[derive(Debug, Clone)]
pub struct SendRt {
    pub id: SendId,
    pub dest: usize,
    pub pre_fader: bool,
    pub gain: Smoothed,
    pub delay: DelayLine,
}

#[derive(Debug, Clone)]
pub enum EffectRt {
    Compressor(Compressor),
}

impl EffectRt {
    fn new(effect: &Effect, sample_rate: u32) -> Self {
        match effect {
            Effect::Compressor(p) => EffectRt::Compressor(Compressor::new(
                &CompressorSettings {
                    threshold_db: p.threshold_db,
                    ratio: p.ratio,
                    attack_ms: p.attack_ms,
                    release_ms: p.release_ms,
                    makeup_db: p.makeup_db,
                    lookahead_ms: p.lookahead_ms,
                },
                sample_rate,
            )),
        }
    }

    pub fn latency(&self) -> u32 {
        match self {
            EffectRt::Compressor(c) => c.latency() as u32,
        }
    }

    fn take_state(&mut self, old: &mut EffectRt) {
        match (self, old) {
            (EffectRt::Compressor(new), EffectRt::Compressor(old)) => new.take_state(old),
        }
    }
}

impl PlaybackSnapshot {
    /// A snapshot that plays nothing and outputs silence.
    pub fn empty(sample_rate: u32) -> Self {
        compile(&Project::new(), sample_rate, |_| None)
    }

    /// Latency of each track's output, by ID (for the UI, MIX-034).
    pub fn track_latencies(&self) -> Vec<(TrackId, u32)> {
        self.tracks.iter().map(|t| (t.id, t.latency)).collect()
    }

    /// Moves runtime state (gain positions, delay contents, effect state)
    /// from `old` into `self` for objects present in both. No allocation:
    /// buffers are swapped, so `old` ends up holding `self`'s fresh ones.
    pub fn take_state_from(&mut self, old: &mut PlaybackSnapshot) {
        for ch in &mut self.channels {
            if let Some(o) = old.channels.iter_mut().find(|o| o.id == ch.id) {
                ch.gain.continue_from(&o.gain);
                ch.delay.take_state(&mut o.delay);
            }
        }
        for t in &mut self.tracks {
            let Some(o) = old.tracks.iter_mut().find(|o| o.id == t.id) else {
                continue;
            };
            t.fader.continue_from(&o.fader);
            t.mute.continue_from(&o.mute);
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
                let Some(oi) = o.inserts.iter_mut().find(|oi| oi.id == i.id) else {
                    continue;
                };
                i.pre_delay.take_state(&mut oi.pre_delay);
                i.effect.take_state(&mut oi.effect);
                if let (Some(sc), Some(osc)) = (&mut i.sidechain, &mut oi.sidechain) {
                    sc.delay.take_state(&mut osc.delay);
                }
            }
        }
    }
}

/// Sample position of a tick at a constant tempo. Uses absolute positions
/// so results never depend on block boundaries (RT-022).
pub fn tick_to_sample(tick: u64, tempo_bpm: f64, sample_rate: u32) -> u64 {
    let samples_per_tick =
        f64::from(sample_rate) * 60.0 / (tempo_bpm * f64::from(jdance_domain::PPQ));
    (tick as f64 * samples_per_tick).round() as u64
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
    let mut in_lat = vec![0u32; order.len()];
    let mut out_lat = vec![0u32; order.len()];
    let mut pdc = Pdc { clamped: false };
    let mut tracks: Vec<TrackRt> = Vec::with_capacity(order.len());

    for (ti, id) in order.iter().enumerate() {
        let t = mixer.track(*id).expect("ordered track exists");
        let mut acc = in_lat[ti];
        let mut inserts = Vec::with_capacity(t.inserts.len());
        for ins in &t.inserts {
            let effect = EffectRt::new(&ins.effect, sample_rate);
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
            inserts.push(InsertRt {
                id: ins.id,
                bypass: ins.bypass,
                pre_delay: pdc.line(pre),
                effect,
                sidechain,
            });
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
            mute: Smoothed::new(if t.muted { 0.0 } else { 1.0 }, ramp),
            sends: Vec::new(),
            main_out: None,
            latency: acc,
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
        .map(|c| {
            let track = track_of(c.track);
            let (l, r) = pan_gains(c.pan);
            let g = if c.muted { 0.0 } else { c.volume };
            ChannelRt {
                id: c.id,
                sample: c
                    .sample()
                    .and_then(&samples)
                    .filter(|s| s.sample_rate == sample_rate),
                track,
                gain: StereoGain::new((l * g, r * g), ramp),
                delay: pdc.line(in_lat[track]),
            }
        })
        .collect();

    let latency = out_lat.last().copied().unwrap_or(0);
    let mut snapshot = PlaybackSnapshot {
        sample_rate,
        loop_len: 0,
        events: Vec::new(),
        channels,
        tracks,
        latency,
        pdc_clamped: pdc.clamped,
    };

    let Some(pattern) = project.active_pattern() else {
        return snapshot;
    };
    snapshot.loop_len = tick_to_sample(pattern.length_ticks(), project.tempo_bpm, sample_rate);
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
            if !step.on {
                continue;
            }
            let tick = i as u64 * u64::from(jdance_domain::TICKS_PER_STEP);
            let pos = tick_to_sample(tick, project.tempo_bpm, sample_rate);
            if pos < snapshot.loop_len {
                snapshot.events.push(StepEvent {
                    pos,
                    channel: ch_index,
                    velocity: step.velocity,
                });
            }
        }
    }
    snapshot.events.sort_by_key(|e| (e.pos, e.channel));
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{
        Channel, ChannelSource, CompressorParams, Insert, MixerTrack, Send, Sidechain,
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
        Effect::Compressor(CompressorParams {
            ratio: 1.0,
            lookahead_ms: ms,
            ..Default::default()
        })
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
