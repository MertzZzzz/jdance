// SPDX-License-Identifier: GPL-3.0-or-later
//! RT-001: the render path performs no heap operations, including command
//! handling and snapshot swaps.

use jdance_domain::{
    Effect, EffectKind, Insert, InsertId, MixerTrack, Project, Send, SendId, Sidechain, Tap,
    TrackId,
};
use jdance_engine::{compile, create_engine};
use jdance_rt_check::{GuardedAllocator, reset_violations, rt_section, violations};
use jdance_test_support::{click, step_project};
use std::sync::Arc;

#[global_allocator]
static ALLOC: GuardedAllocator = GuardedAllocator;

#[test]
fn render_does_not_allocate() {
    let sr = 48_000;
    let sample = Arc::new(click(sr, 4_000));
    let all: Vec<u32> = (0..16).collect();
    let busy: Vec<&[u32]> = vec![&all; 12];
    let sparse: Vec<&[u32]> = vec![&[0, 8]; 3];
    let fast_project = with_mixer(step_project(140.0, &busy), 1.0);
    let slow_project = with_mixer(step_project(90.0, &sparse), 0.3);
    let fast = || compile(&fast_project, sr, |_| Some(Arc::clone(&sample)));
    let slow = || compile(&slow_project, sr, |_| Some(Arc::clone(&sample)));
    assert!(fast().latency > 0, "graph exercises delay compensation");

    let (mut handle, mut processor) = create_engine(sr);
    let mut l = vec![0.0f32; 4096];
    let mut r = vec![0.0f32; 4096];
    handle.set_snapshot(fast()).unwrap();
    handle.play().unwrap();

    reset_violations();
    for i in 0..400 {
        // Swap snapshots (tempo change) and stop/start while rendering.
        match i % 50 {
            10 => handle.set_snapshot(slow()).unwrap(),
            30 => handle.set_snapshot(fast()).unwrap(),
            40 => handle.stop().unwrap(),
            41 => handle.play().unwrap(),
            45 => handle.panic().unwrap(),
            20 => handle.set_param(InsertId(9012), 4, 0.9).unwrap(),
            25 => handle.stop().unwrap(),
            27 => handle.play().unwrap(),
            _ => {}
        }
        let n = [64, 1, 333, 1024, 4096][i % 5];
        rt_section(|| processor.render(&mut l[..n], &mut r[..n]));
        handle.collect_garbage();
    }
    assert_eq!(violations(), 0, "heap operations inside render");
    assert!(handle.feedback().read().renders >= 400);
}

/// Routes channels through tracks with sends, a lookahead compressor and a
/// sidechain, so swaps exercise every state-transfer path.
fn with_mixer(mut p: Project, volume: f32) -> Project {
    let comp = |lookahead_ms| Effect::new(EffectKind::Compressor).with("lookahead", lookahead_ms);
    let mut drums = MixerTrack::new(TrackId(9001), "Drums");
    drums.volume = volume;
    drums.inserts.push(Insert {
        id: InsertId(9010),
        bypass: false,
        wet: 1.0,
        effect: comp(1.0),
        sidechain: None,
    });
    drums.sends.push(Send {
        id: SendId(9020),
        dest: TrackId(9003),
        level: 0.4,
        pre_fader: false,
    });
    let mut bass = MixerTrack::new(TrackId(9002), "Bass");
    bass.inserts.push(Insert {
        id: InsertId(9011),
        bypass: false,
        wet: 1.0,
        effect: comp(0.0),
        sidechain: Some(Sidechain {
            source: TrackId(9001),
            tap: Tap::PreFader,
        }),
    });
    let mut ret = MixerTrack::new(TrackId(9003), "Return");
    for (id, kind) in [
        (9012, EffectKind::TempoDelay),
        (9013, EffectKind::Reverb),
        (9014, EffectKind::ParametricEq),
    ] {
        ret.inserts.push(Insert {
            id: InsertId(id),
            bypass: volume < 0.5 && id == 9013,
            wet: 1.0,
            effect: Effect::new(kind).with(
                if kind == EffectKind::ParametricEq {
                    "mid1_gain"
                } else {
                    "wet"
                },
                0.5,
            ),
            sidechain: None,
        });
    }
    if volume < 0.5 {
        // Structural difference between the two snapshots.
        ret.inserts.swap(0, 2);
    }
    p.mixer.limiter.enabled = true;
    p.mixer.tracks = vec![drums, bass, ret];
    for (i, c) in p.channels.iter_mut().enumerate() {
        c.track = [TrackId(9001), TrackId(9002), TrackId::MASTER][i % 3];
    }
    p
}
