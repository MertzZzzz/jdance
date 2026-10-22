// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin inserts in the engine with a fake plugin processor: delay
//! compensation (MIX-030, PLG-050), instance hand-over between snapshots,
//! release off the audio thread, placeholders (PLG-072), transport
//! (PLG-034) and RT-001.

use jdance_domain::{
    Channel, ChannelId, ChannelSource, Effect, Insert, InsertId, MixerTrack, PluginSlot, Project,
    SampleRef, TrackId,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{
    PlayMode, PlaybackSnapshot, PluginRt, compile_full, create_engine, render_offline,
};
use jdance_plugin_api::{PluginProcessor, Transport};
use jdance_rt_check::{GuardedAllocator, reset_violations, rt_section, violations};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[global_allocator]
static ALLOC: GuardedAllocator = GuardedAllocator;

const SR: u32 = 48_000;
const LATENCY: usize = 100;

#[derive(Default)]
struct Probe {
    dropped: AtomicUsize,
    blocks: AtomicUsize,
    last: Mutex<Option<Transport>>,
}

/// Delays by `LATENCY` and halves the signal; reports to a probe.
struct FakeDelay {
    line: Vec<[f32; 2]>,
    pos: usize,
    probe: Arc<Probe>,
}

impl FakeDelay {
    fn boxed(probe: &Arc<Probe>) -> Box<dyn PluginProcessor> {
        Box::new(Self {
            line: vec![[0.0; 2]; LATENCY],
            pos: 0,
            probe: Arc::clone(probe),
        })
    }
}

impl PluginProcessor for FakeDelay {
    fn process(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        _sidechain: Option<(&[f32], &[f32])>,
        transport: &Transport,
    ) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let out = self.line[self.pos];
            self.line[self.pos] = [*l, *r];
            self.pos = (self.pos + 1) % LATENCY;
            *l = out[0] * 0.5;
            *r = out[1] * 0.5;
        }
        self.probe.blocks.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut t) = self.probe.last.try_lock() {
            *t = Some(*transport);
        }
    }

    fn latency_samples(&self) -> u32 {
        LATENCY as u32
    }

    fn has_sidechain_input(&self) -> bool {
        false
    }
}

impl Drop for FakeDelay {
    fn drop(&mut self) {
        self.probe.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

fn slot() -> PluginSlot {
    PluginSlot {
        format: "vst3".into(),
        class_id: "4A445446584500000000000000000001".into(),
        name: "Fake Delay".into(),
        vendor: "tests".into(),
        version: "1".into(),
        category: "Fx".into(),
        component_state: vec![1, 2, 3],
        controller_state: Vec::new(),
    }
}

const PLUGIN_INSERT: InsertId = InsertId(10);

/// Channel A → track 1 (plugin) and channel B → track 2 (no plugin),
/// both with an impulse on step 0.
fn project(with_plugin: bool) -> (Project, Arc<SampleBuffer>) {
    let mut p = Project::new();
    p.tempo_bpm = 120.0;
    for _ in 0..1000 {
        p.ids.next();
    }
    let mut a = MixerTrack::new(TrackId(1), "A");
    if with_plugin {
        a.inserts.push(Insert {
            id: PLUGIN_INSERT,
            bypass: false,
            wet: 1.0,
            effect: Effect::plugin(slot()),
            sidechain: None,
        });
    }
    p.mixer.tracks.push(a);
    p.mixer.tracks.push(MixerTrack::new(TrackId(2), "B"));
    for (i, track) in [TrackId(1), TrackId(2)].into_iter().enumerate() {
        let id = ChannelId(500 + i as u64);
        let mut c = Channel::new_sampler(id, "c");
        c.volume = 1.0;
        c.track = track;
        c.source = ChannelSource::Sampler {
            sample: Some(SampleRef {
                path: "imp.wav".into(),
            }),
        };
        p.channels.push(c);
        let step = &mut p.patterns[0].row_mut(id).steps[0];
        step.on = true;
        step.velocity = 1.0;
    }
    p.validate_routing().unwrap();
    let mut imp = vec![0.0; 64];
    imp[0] = 1.0;
    (p, Arc::new(SampleBuffer::from_interleaved(SR, 1, &imp)))
}

fn compile(
    p: &Project,
    s: &Arc<SampleBuffer>,
    plugin: impl FnOnce() -> PluginRt,
) -> PlaybackSnapshot {
    let mut plugin = Some(plugin);
    let first = p.patterns[0].id;
    compile_full(
        p,
        SR,
        PlayMode::Pattern(first),
        |_| Some(Arc::clone(s)),
        &mut |id, sl| {
            assert_eq!(
                (id, sl.name.as_str()),
                (
                    jdance_domain::PluginTarget::Insert(PLUGIN_INSERT),
                    "Fake Delay"
                )
            );
            plugin.take().map_or_else(PluginRt::missing, |f| f())
        },
    )
}

fn nonzero(buf: &[f32]) -> Vec<usize> {
    (0..buf.len()).filter(|&i| buf[i].abs() > 1e-9).collect()
}

#[test]
fn plugin_latency_is_compensated_on_parallel_paths() {
    let probe = Arc::new(Probe::default());
    let (p, s) = project(true);
    let snap = compile(&p, &s, || PluginRt::fresh(FakeDelay::boxed(&probe)));
    assert_eq!(snap.latency as usize, LATENCY);
    let pan = 0.5f32.sqrt();
    let out = render_offline(snap, 400, || 64);
    assert_eq!(nonzero(&out.left), [LATENCY]);
    // Plugin path at half gain plus the dry path, both at the pan law.
    assert!(
        (out.left[LATENCY] - 1.5 * pan).abs() < 1e-6,
        "{}",
        out.left[LATENCY]
    );
}

#[test]
fn missing_plugin_passes_audio_through() {
    let (p, s) = project(true);
    let snap = compile(&p, &s, PluginRt::missing);
    assert_eq!(snap.latency, 0);
    let out = render_offline(snap, 200, || 64);
    let pan = 0.5f32.sqrt();
    assert_eq!(nonzero(&out.left), [0]);
    assert!((out.left[0] - 2.0 * pan).abs() < 1e-6);
}

#[test]
fn instance_moves_between_snapshots_and_is_dropped_off_the_audio_thread() {
    let probe = Arc::new(Probe::default());
    let (p, s) = project(true);
    let (mut h, mut proc_) = create_engine(SR);
    let mut l = vec![0.0; 256];
    let mut r = vec![0.0; 256];
    h.set_snapshot(compile(&p, &s, || {
        PluginRt::fresh(FakeDelay::boxed(&probe))
    }))
    .unwrap();
    h.play().unwrap();
    proc_.render(&mut l, &mut r);
    // A recompile (tempo change) carries the same instance over.
    let mut p2 = p.clone();
    p2.tempo_bpm = 100.0;
    h.set_snapshot(compile(&p2, &s, || PluginRt::carried(LATENCY as u32)))
        .unwrap();
    proc_.render(&mut l, &mut r);
    h.collect_garbage();
    assert_eq!(probe.dropped.load(Ordering::Relaxed), 0, "instance kept");
    let before = probe.blocks.load(Ordering::Relaxed);
    proc_.render(&mut l, &mut r);
    assert!(
        probe.blocks.load(Ordering::Relaxed) > before,
        "still processing"
    );

    // Removing the insert returns the instance as garbage.
    let (p3, _) = project(false);
    h.set_snapshot(compile(&p3, &s, PluginRt::missing)).unwrap();
    for _ in 0..8 {
        proc_.render(&mut l, &mut r);
    }
    assert_eq!(
        probe.dropped.load(Ordering::Relaxed),
        0,
        "not dropped in render"
    );
    h.collect_garbage();
    assert_eq!(probe.dropped.load(Ordering::Relaxed), 1);
}

#[test]
fn plugin_sees_transport_at_block_start() {
    let probe = Arc::new(Probe::default());
    let (p, s) = project(true);
    let (mut h, mut proc_) = create_engine(SR);
    h.set_snapshot(compile(&p, &s, || {
        PluginRt::fresh(FakeDelay::boxed(&probe))
    }))
    .unwrap();
    let mut l = vec![0.0; 480];
    let mut r = vec![0.0; 480];
    proc_.render(&mut l, &mut r);
    let t = probe.last.lock().unwrap().unwrap();
    assert!(!t.playing);
    h.play().unwrap();
    for _ in 0..3 {
        proc_.render(&mut l, &mut r);
    }
    let t = probe.last.lock().unwrap().unwrap();
    assert!(t.playing);
    assert_eq!(t.tempo_bpm, 120.0);
    assert_eq!(t.project_time_samples, 2 * 480);
    // 120 BPM: 24 000 samples per quarter.
    assert!((t.project_time_quarters - 960.0 / 24_000.0).abs() < 1e-9);
    assert_eq!(t.cycle, Some((0.0, 4.0)), "pattern loop: one bar");
}

#[test]
fn rendering_with_plugins_does_not_allocate() {
    let probe = Arc::new(Probe::default());
    let (p, s) = project(true);
    let (mut h, mut proc_) = create_engine(SR);
    h.set_snapshot(compile(&p, &s, || {
        PluginRt::fresh(FakeDelay::boxed(&probe))
    }))
    .unwrap();
    h.play().unwrap();
    let mut l = vec![0.0; 1024];
    let mut r = vec![0.0; 1024];
    proc_.render(&mut l, &mut r);
    let next = compile(&p, &s, || PluginRt::carried(LATENCY as u32));
    h.set_snapshot(next).unwrap();
    reset_violations();
    rt_section(|| {
        for _ in 0..50 {
            proc_.render(&mut l, &mut r);
        }
    });
    assert_eq!(violations(), 0);
    h.collect_garbage();
}
