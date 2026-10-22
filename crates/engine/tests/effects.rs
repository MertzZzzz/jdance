// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in effects inside the engine: structural fades, bypass, Stop,
//! master limiter, meters and realtime/offline equivalence.

use jdance_domain::{
    Channel, ChannelId, ChannelSource, Effect, EffectKind, Insert, InsertId, MixerTrack, Project,
    SampleRef, TrackId,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{
    EngineHandle, EngineProcessor, PlaybackSnapshot, compile, create_engine, render_offline,
};
use proptest::prelude::*;
use std::sync::Arc;

const SR: u32 = 48_000;
const TRACK: TrackId = TrackId(500);

struct Fixture {
    p: Project,
    sample: Arc<SampleBuffer>,
}

impl Fixture {
    /// One channel playing `sample` on every step into a track at unity.
    fn new(sample: Vec<f32>) -> Self {
        Self::with_steps(sample, &(0..16).collect::<Vec<_>>())
    }

    /// Like [`Fixture::new`] but triggering only on `steps`.
    fn with_steps(sample: Vec<f32>, steps: &[usize]) -> Self {
        let mut p = Project::new();
        p.tempo_bpm = 120.0;
        p.mixer.tracks.push(MixerTrack::new(TRACK, "T"));
        let id = ChannelId(p.ids.next());
        let mut c = Channel::new_sampler(id, "c");
        c.volume = 1.0;
        c.track = TRACK;
        c.source = ChannelSource::Sampler {
            sample: Some(SampleRef { path: "s".into() }),
        };
        p.channels.push(c);
        for &s in steps {
            p.patterns[0].row_mut(id).steps[s].on = true;
            p.patterns[0].row_mut(id).steps[s].velocity = 1.0;
        }
        Self {
            p,
            sample: Arc::new(SampleBuffer::from_interleaved(SR, 1, &sample)),
        }
    }

    fn add(&mut self, id: u64, effect: Effect, bypass: bool) {
        self.p.mixer.track_mut(TRACK).unwrap().inserts.push(Insert {
            id: InsertId(id),
            bypass,
            wet: 1.0,
            effect,
            sidechain: None,
        });
    }

    fn snapshot(&self) -> PlaybackSnapshot {
        self.p.validate_routing().unwrap();
        let s = Arc::clone(&self.sample);
        compile(&self.p, SR, move |_| Some(Arc::clone(&s)))
    }

    fn engine(&self) -> (EngineHandle, EngineProcessor) {
        let (mut h, p) = create_engine(SR);
        h.set_snapshot(self.snapshot()).unwrap();
        h.play().unwrap();
        (h, p)
    }
}

fn render(p: &mut EngineProcessor, frames: usize, block: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    let mut l = vec![0.0; block];
    let mut r = vec![0.0; block];
    while out.len() < frames {
        let n = block.min(frames - out.len());
        p.render(&mut l[..n], &mut r[..n]);
        out.extend_from_slice(&l[..n]);
    }
    out
}

fn max_step(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

fn noise(n: usize, seed: u32, amp: f32) -> Vec<f32> {
    let mut x = seed.max(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * amp
        })
        .collect()
}

#[test]
fn limiter_holds_ceiling_and_is_otherwise_absent() {
    let mut f = Fixture::new(noise(6_000, 3, 4.0));
    let out = render_offline(f.snapshot(), 48_000, || 256);
    assert!(
        out.left.iter().any(|v| v.abs() > 1.5),
        "no hidden clipping without the limiter"
    );
    f.p.mixer.limiter.enabled = true;
    f.p.mixer.limiter.ceiling_db = -1.0;
    let ceiling = 10f32.powf(-1.0 / 20.0);
    let out = render_offline(f.snapshot(), 48_000, || 256);
    assert!(
        out.left
            .iter()
            .chain(&out.right)
            .all(|v| v.abs() <= ceiling + 1e-6)
    );
}

#[test]
fn adding_an_effect_while_playing_does_not_click() {
    // A long constant sample: any jump in the output would be a click.
    let mut f = Fixture::with_steps(vec![0.5; 200_000], &[0]);
    let (mut h, mut p) = f.engine();
    let before = render(&mut p, 10_000, 256);
    // +6 dB makeup doubles the level: without the structural fade this
    // would be a hard jump.
    f.add(
        1,
        Effect::new(EffectKind::Compressor)
            .with("threshold", 0.0)
            .with("makeup", 6.0),
        false,
    );
    h.set_snapshot(f.snapshot()).unwrap();
    let after = render(&mut p, 10_000, 256);
    h.collect_garbage();
    let all: Vec<f32> = before.into_iter().chain(after).collect();
    let level = all[5_000];
    // A 5 ms fade moves at most level/240 per sample (plus EQ ringing).
    assert!(
        max_step(&all[1_000..]) < level / 50.0,
        "step {}",
        max_step(&all[1_000..])
    );
}

#[test]
fn bypass_crossfades_without_click() {
    let mut f = Fixture::with_steps(vec![0.5; 200_000], &[0]);
    f.add(
        1,
        Effect::new(EffectKind::ParametricEq)
            .with("low_gain", 12.0)
            .with("low_freq", 2_000.0),
        false,
    );
    let (mut h, mut p) = f.engine();
    let a = render(&mut p, 10_000, 256);
    f.p.mixer.track_mut(TRACK).unwrap().inserts[0].bypass = true;
    h.set_snapshot(f.snapshot()).unwrap();
    let b = render(&mut p, 10_000, 256);
    let all: Vec<f32> = a.into_iter().chain(b).collect();
    assert!(
        max_step(&all[2_000..]) < 0.01,
        "step {}",
        max_step(&all[2_000..])
    );
    // Fully bypassed: the output is the dry level (channel pan law × master).
    let dry = all[19_000];
    assert!((dry - all[19_999]).abs() < 1e-6);
}

#[test]
fn stop_fades_out_without_click() {
    let f = Fixture::with_steps(vec![0.5; 200_000], &[0]);
    let (mut h, mut p) = f.engine();
    let playing = render(&mut p, 5_000, 256);
    let level = playing[4_999];
    h.stop().unwrap();
    let stopping = render(&mut p, 2_000, 64);
    let all: Vec<f32> = playing
        .into_iter()
        .chain(stopping.iter().copied())
        .collect();
    // 5 ms fade: at most level/240 per sample.
    assert!(
        max_step(&all[1_000..]) <= level / 200.0,
        "Stop must fade, not cut"
    );
    assert!(
        stopping[400..].iter().all(|v| *v == 0.0),
        "silent after the fade"
    );
}

#[test]
fn stop_clears_effect_state() {
    let mut f = Fixture::new(noise(3_000, 9, 0.8));
    f.add(
        1,
        Effect::new(EffectKind::Reverb)
            .with("decay", 5.0)
            .with("wet", 1.0),
        false,
    );
    f.add(
        2,
        Effect::new(EffectKind::TempoDelay)
            .with("feedback", 0.9)
            .with("wet", 0.5),
        false,
    );
    let (mut h, mut p) = f.engine();
    let playing = render(&mut p, 20_000, 256);
    assert!(playing[19_000..].iter().any(|v| v.abs() > 1e-3));
    h.stop().unwrap();
    // The reset is bounded per block and completes over several blocks;
    // afterwards playback restarts from clean state, so it reproduces
    // the very first samples of the first pass exactly.
    render(&mut p, 48_000, 256);
    h.play().unwrap();
    let again = render(&mut p, 100, 100);
    assert_eq!(again[..50], playing[..50]);
}

#[test]
fn set_param_changes_effect_without_recompile() {
    let mut f = Fixture::with_steps(vec![0.5; 200_000], &[0]);
    f.add(
        7,
        Effect::new(EffectKind::TempoDelay)
            .with("wet", 0.0)
            .with("sync", 0.0),
        false,
    );
    let (mut h, mut p) = f.engine();
    let dry = render(&mut p, 8_000, 256);
    // wet is parameter index 4 in the schema.
    h.set_param(InsertId(7), 4, 1.0).unwrap();
    let wet = render(&mut p, 30_000, 256);
    assert!(
        (dry[7_999] - wet[29_999]).abs() > 0.01,
        "wet echo replaced the dry signal"
    );
    // Around the change the 20 ms ramp keeps steps tiny (later, the first
    // echo of the constant input is a genuine step in the signal).
    assert!(
        max_step(&wet[..4_000]) < 0.002,
        "parameter change is smoothed"
    );
}

#[test]
fn meters_report_bus_and_output_peaks() {
    let mut f = Fixture::with_steps(vec![0.5; 200_000], &[0]);
    f.p.mixer.limiter.enabled = true;
    f.p.mixer.limiter.ceiling_db = -12.0;
    let (h, mut p) = f.engine();
    render(&mut p, 4_800, 256);
    let m = &h.feedback().meters;
    let bus = m
        .buses
        .iter()
        .map(|s| s.take())
        .find(|(id, _, _)| *id == TRACK.0)
        .expect("track meter");
    assert!(bus.1 > 0.3, "bus peak {}", bus.1);
    let (_, out_l, _) = m.output.take();
    assert!(out_l <= 10f32.powf(-12.0 / 20.0) + 1e-6);
    assert!(m.take_limiter_gain() < 0.9, "limiter reduction is reported");
    assert_eq!(m.take_limiter_gain(), 1.0, "read resets");
}

fn full_chain() -> Fixture {
    let mut f = Fixture::new(noise(9_000, 5, 0.6));
    f.add(
        1,
        Effect::new(EffectKind::ParametricEq).with("mid2_gain", 9.0),
        false,
    );
    f.add(
        2,
        Effect::new(EffectKind::Compressor).with("lookahead", 2.0),
        false,
    );
    f.add(
        3,
        Effect::new(EffectKind::TempoDelay).with("feedback", 0.6),
        false,
    );
    f.add(4, Effect::new(EffectKind::Reverb).with("wet", 0.4), false);
    f.p.mixer.limiter.enabled = true;
    f
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    /// Realtime and offline rendering of the full effect chain match bit
    /// for bit for any callback size sequence.
    #[test]
    fn realtime_offline_equivalence(sizes in prop::collection::vec(1usize..=2048, 1..32)) {
        let f = full_chain();
        let reference = render_offline(f.snapshot(), 60_000, || 512);
        let (_h, mut p) = f.engine();
        let mut out = Vec::new();
        let mut it = sizes.iter().cycle();
        let mut l = vec![0.0; 2048];
        let mut r = vec![0.0; 2048];
        while out.len() < 60_000 {
            let n = (*it.next().unwrap()).min(60_000 - out.len());
            p.render(&mut l[..n], &mut r[..n]);
            out.extend_from_slice(&l[..n]);
        }
        prop_assert_eq!(out, reference.left);
    }
}

#[test]
fn an_insert_at_zero_wet_renders_like_no_insert() {
    let sample = noise(4_000, 7, 0.5);
    let plain = Fixture::new(sample.clone());
    let reference = render_offline(plain.snapshot(), 24_000, || 256);
    let mut f = Fixture::new(sample);
    f.add(1, Effect::new(EffectKind::Reverb).with("wet", 1.0), false);
    f.p.mixer.track_mut(TRACK).unwrap().inserts[0].wet = 0.0;
    let dry = render_offline(f.snapshot(), 24_000, || 256);
    assert_eq!(dry.left, reference.left);
    f.p.mixer.track_mut(TRACK).unwrap().inserts[0].wet = 1.0;
    let wet = render_offline(f.snapshot(), 24_000, || 256);
    assert_ne!(wet.left, reference.left, "the reverb is heard at full wet");
}
