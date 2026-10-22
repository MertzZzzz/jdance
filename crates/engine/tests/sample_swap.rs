// SPDX-License-Identifier: GPL-3.0-or-later
//! A channel's sample replaced while it sounds (spec 0.2.5 §3): sounding
//! voices finish the old sample, new notes take the new one, no click.

use jdance_domain::{Channel, ChannelId, ChannelSource, Project, SampleRef};
use jdance_dsp::SampleBuffer;
use jdance_engine::{PlayMode, compile_with, create_engine};
use std::sync::Arc;

const SR: u32 = 48_000;

fn project(sample: &str) -> (Project, ChannelId) {
    let mut p = Project::new();
    let ch = ChannelId(p.ids.next());
    let mut c = Channel::new_sampler(ch, "c");
    c.volume = 1.0;
    c.pan = 0.0;
    c.source = ChannelSource::Sampler {
        sample: Some(SampleRef {
            path: sample.into(),
        }),
    };
    p.channels.push(c);
    (p, ch)
}

/// A constant level, so the output shows which sample sounds.
fn level(v: f32) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer::from_interleaved(SR, 1, &vec![v; SR as usize]))
}

#[test]
fn a_sounding_voice_finishes_the_old_sample() {
    let (a, b) = (level(0.25), level(0.5));
    let samples = move |s: &SampleRef| Some(if s.path == "a" { a.clone() } else { b.clone() });
    let (pa, ch) = project("a");
    let (mut pb, _) = project("b");
    pb.channels[0].id = ch;
    let (mut h, mut proc) = create_engine(SR);
    let s = samples.clone();
    h.set_snapshot(compile_with(&pa, SR, PlayMode::Song, move |r| s(r)))
        .unwrap();
    let mut l = vec![0.0; 256];
    let mut r = vec![0.0; 256];
    proc.render(&mut l, &mut r);
    h.live_note(ch, 60, 1.0).unwrap();
    proc.render(&mut l, &mut r);
    let before = l[255];
    assert!(before > 0.1, "the note sounds: {before}");
    // The channel gets sample B mid-note.
    let s = samples.clone();
    h.set_snapshot(compile_with(&pb, SR, PlayMode::Song, move |r| s(r)))
        .unwrap();
    proc.render(&mut l, &mut r);
    assert!(
        l.iter().all(|v| (v - before).abs() < 1e-6),
        "still sample A, no jump: {} … {}",
        l[0],
        l[255]
    );
    // A new note: sample B, on top of A.
    h.live_note(ch, 62, 1.0).unwrap();
    proc.render(&mut l, &mut r);
    assert!(l[255] > before * 2.5, "A + B: {}", l[255]);
}
