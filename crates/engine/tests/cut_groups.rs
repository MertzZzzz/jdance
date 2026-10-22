// SPDX-License-Identifier: GPL-3.0-or-later
//! Cut groups of the advanced sampler (spec 0.2.5 §4): a closed hat with
//! Cut 1 cuts an open hat with By 1; Cut self cuts the channel's own
//! previous note.

use jdance_domain::{Channel, ChannelId, ChannelSource, Project, SampleRef};
use jdance_dsp::SampleBuffer;
use jdance_engine::{PlayMode, compile_with, create_engine};
use std::sync::Arc;

const SR: u32 = 48_000;

fn channel(p: &mut Project, sample: &str, cut: u8, cut_by: u8) -> ChannelId {
    let id = ChannelId(p.ids.next());
    let mut c = Channel::new_sampler(id, sample);
    c.volume = 1.0;
    c.pan = 0.0;
    c.source = ChannelSource::Sampler {
        sample: Some(SampleRef {
            path: sample.into(),
        }),
    };
    c.sampler.cut = cut;
    c.sampler.cut_by = cut_by;
    p.channels.push(c);
    id
}

/// A constant level, so the output shows which sample sounds.
fn level(v: f32) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer::from_interleaved(SR, 1, &vec![v; SR as usize]))
}

fn render_blocks(proc: &mut jdance_engine::EngineProcessor, n: usize) -> f32 {
    let mut l = vec![0.0; 256];
    let mut r = vec![0.0; 256];
    for _ in 0..n {
        proc.render(&mut l, &mut r);
    }
    l[255]
}

#[test]
fn a_closed_hat_cuts_the_open_hat() {
    let mut p = Project::new();
    let open = channel(&mut p, "open", 0, 1);
    let closed = channel(&mut p, "closed", 1, 0);
    let (o, c) = (level(0.25), level(0.5));
    let samples = move |s: &SampleRef| {
        Some(if s.path == "open" {
            o.clone()
        } else {
            c.clone()
        })
    };
    let (mut h, mut proc) = create_engine(SR);
    h.set_snapshot(compile_with(&p, SR, PlayMode::Song, samples))
        .unwrap();
    render_blocks(&mut proc, 1);
    h.live_note(open, 60, 1.0).unwrap();
    let alone = render_blocks(&mut proc, 1);
    assert!(alone > 0.1, "the open hat sounds: {alone}");
    h.live_note(closed, 60, 1.0).unwrap();
    // After the short release fade only the closed hat is left.
    let closed_alone = render_blocks(&mut proc, 20);
    assert!(
        closed_alone > 0.1 && closed_alone < alone * 2.5,
        "open hat cut: {closed_alone} (open alone {alone})"
    );
    // The open hat does not cut the closed one.
    h.live_note(open, 60, 1.0).unwrap();
    let both = render_blocks(&mut proc, 20);
    assert!(both > closed_alone * 1.2, "both sound: {both}");
}

#[test]
fn cut_self_cuts_the_previous_note() {
    let mut p = Project::new();
    let ch = channel(&mut p, "a", 3, 3);
    let a = level(0.25);
    let (mut h, mut proc) = create_engine(SR);
    h.set_snapshot(compile_with(&p, SR, PlayMode::Song, move |_| {
        Some(a.clone())
    }))
    .unwrap();
    render_blocks(&mut proc, 1);
    h.live_note(ch, 60, 1.0).unwrap();
    let one = render_blocks(&mut proc, 1);
    h.live_note(ch, 60, 1.0).unwrap();
    let still_one = render_blocks(&mut proc, 20);
    assert!(
        (still_one - one).abs() < 0.02,
        "one voice at a time: {one} then {still_one}"
    );
}
