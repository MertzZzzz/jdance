// SPDX-License-Identifier: GPL-3.0-or-later
//! MIX-060 stress project: 32 tracks, 8 sends, 4 sidechains, 64 channels
//! with only built-in effects. Measures render time against the callback
//! budget at 48 kHz / 256 frames.
//!
//! Machine-dependent, so ignored by default. Run on the reference machine:
//! `cargo test --release -p jdance-engine --test stress -- --ignored --nocapture`

use jdance_domain::{
    Channel, ChannelId, ChannelSource, CompressorParams, Effect, Insert, InsertId, MixerTrack,
    Project, SampleRef, Send, SendId, Sidechain, Tap, TrackId,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{compile, create_engine};
use std::sync::Arc;
use std::time::Instant;

const SR: u32 = 48_000;
const BLOCK: usize = 256;

fn stress_project() -> Project {
    let mut p = Project::new();
    p.tempo_bpm = 174.0;
    let mut next = 10_000u64;
    let mut id = || {
        next += 1;
        next
    };
    let comp = |sc: Option<Sidechain>, id: u64| Insert {
        id: InsertId(id),
        bypass: false,
        effect: Effect::Compressor(CompressorParams {
            lookahead_ms: 1.0,
            ..Default::default()
        }),
        sidechain: sc,
    };
    let track_ids: Vec<TrackId> = (0..32).map(|_| TrackId(id())).collect();
    for (i, t) in track_ids.iter().enumerate() {
        let mut track = MixerTrack::new(*t, format!("T{i}"));
        track.inserts.push(comp(None, id()));
        // Tracks 0..24 feed buses 24..32; buses go to the master.
        if i < 24 {
            track.main_out = track_ids[24 + i % 8];
        }
        if i < 8 {
            track.sends.push(Send {
                id: SendId(id()),
                dest: track_ids[31],
                level: 0.3,
                pre_fader: false,
            });
        }
        if (12..16).contains(&i) {
            track.inserts.push(comp(
                Some(Sidechain {
                    source: track_ids[i - 12],
                    tap: Tap::PreFader,
                }),
                id(),
            ));
        }
        p.mixer.tracks.push(track);
    }
    for i in 0..64 {
        let cid = ChannelId(id());
        let mut c = Channel::new_sampler(cid, format!("C{i}"));
        c.track = track_ids[i % 24];
        c.source = ChannelSource::Sampler {
            sample: Some(SampleRef {
                path: "s.wav".into(),
            }),
        };
        p.channels.push(c);
        let row = p.patterns[0].row_mut(cid);
        for s in 0..16 {
            row.steps[s].on = (s + i) % 2 == 0;
        }
    }
    p.validate_routing().expect("stress routing is valid");
    p
}

#[test]
#[ignore = "performance measurement; run explicitly on the reference machine"]
fn stress_project_fits_budget() {
    let sample = Arc::new(SampleBuffer::from_interleaved(
        SR,
        1,
        &(0..SR as usize / 2)
            .map(|i| ((i % 100) as f32 / 100.0) - 0.5)
            .collect::<Vec<_>>(),
    ));
    let snapshot = compile(&stress_project(), SR, |_| Some(Arc::clone(&sample)));
    let (mut h, mut proc) = create_engine(SR);
    h.set_snapshot(snapshot).unwrap();
    h.play().unwrap();
    let mut l = vec![0.0; BLOCK];
    let mut r = vec![0.0; BLOCK];
    // Warm up, then measure 10 s of audio.
    for _ in 0..200 {
        proc.render(&mut l, &mut r);
    }
    let blocks = 10 * SR as usize / BLOCK;
    let mut times = Vec::with_capacity(blocks);
    for _ in 0..blocks {
        let t = Instant::now();
        proc.render(&mut l, &mut r);
        times.push(t.elapsed().as_secs_f64());
    }
    times.sort_by(f64::total_cmp);
    let budget = BLOCK as f64 / f64::from(SR);
    let p50 = times[times.len() / 2] / budget;
    let p99 = times[times.len() * 99 / 100] / budget;
    let max = times[times.len() - 1] / budget;
    println!(
        "stress-mvp @48k/{BLOCK}: p50 {:.1}%  p99 {:.1}%  max {:.1}% of callback budget",
        p50 * 100.0,
        p99 * 100.0,
        max * 100.0
    );
    assert!(p99 < 0.5, "MIX-060: p99 must stay under 50% of the budget");
}
