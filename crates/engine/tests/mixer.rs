// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer routing and delay compensation (MIX-030…MIX-035).
//!
//! A compressor with ratio 1 is an exact delay by its lookahead, so it is
//! used as a latency-reporting test effect: compensation is correct when
//! impulses from all paths land on the same output sample.

use jdance_domain::{
    Channel, ChannelId, ChannelSource, CompressorParams, Effect, Insert, InsertId, MixerTrack,
    Project, SampleRef, Send, SendId, Sidechain, Tap, TrackId,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{OfflineRender, PlaybackSnapshot, compile, create_engine, render_offline};
use proptest::prelude::*;
use std::sync::Arc;

const SR: u32 = 48_000;

fn delay_effect(ms: f32) -> Effect {
    Effect::Compressor(CompressorParams {
        ratio: 1.0,
        lookahead_ms: ms,
        ..Default::default()
    })
}

fn insert(id: u64, effect: Effect, sidechain: Option<Sidechain>) -> Insert {
    Insert {
        id: InsertId(id),
        bypass: false,
        effect,
        sidechain,
    }
}

struct Builder {
    p: Project,
    samples: Vec<(String, Arc<SampleBuffer>)>,
}

impl Builder {
    fn new() -> Self {
        let mut p = Project::new();
        p.tempo_bpm = 120.0;
        p.ids = Default::default();
        // Keep allocated IDs clear of the hand-picked ones below.
        for _ in 0..1000 {
            p.ids.next();
        }
        Self {
            p,
            samples: Vec::new(),
        }
    }

    fn track(&mut self, t: MixerTrack) -> &mut Self {
        self.p.mixer.tracks.push(t);
        self
    }

    /// A channel playing `sample` on step 0 into `track`, unity gain.
    fn channel(&mut self, track: TrackId, sample: Vec<f32>) -> ChannelId {
        let id = ChannelId(self.p.ids.next());
        let path = format!("{}.wav", id.0);
        let mut c = Channel::new_sampler(id, "c");
        c.volume = 1.0;
        c.track = track;
        c.source = ChannelSource::Sampler {
            sample: Some(SampleRef { path: path.clone() }),
        };
        self.p.channels.push(c);
        self.p.patterns[0].row_mut(id).steps[0].on = true;
        self.p.patterns[0].row_mut(id).steps[0].velocity = 1.0;
        self.samples.push((
            path,
            Arc::new(SampleBuffer::from_interleaved(SR, 1, &sample)),
        ));
        id
    }

    fn snapshot(&self) -> PlaybackSnapshot {
        self.p.validate_routing().expect("valid test routing");
        compile(&self.p, SR, |s| {
            self.samples
                .iter()
                .find(|(p, _)| *p == s.path)
                .map(|(_, b)| Arc::clone(b))
        })
    }

    fn render(&self, frames: usize) -> OfflineRender {
        render_offline(self.snapshot(), frames, || 256)
    }
}

fn impulse() -> Vec<f32> {
    let mut v = vec![0.0; 64];
    v[0] = 1.0;
    v
}

fn nonzero(buf: &[f32]) -> Vec<usize> {
    buf.iter()
        .enumerate()
        .filter(|(_, s)| s.abs() > 1e-9)
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn parallel_paths_are_aligned() {
    let mut b = Builder::new();
    let mut a = MixerTrack::new(TrackId(1), "A");
    a.inserts.push(insert(10, delay_effect(1.0), None));
    b.track(a).track(MixerTrack::new(TrackId(2), "B"));
    b.channel(TrackId(1), impulse());
    b.channel(TrackId(2), impulse());
    assert_eq!(b.snapshot().latency, 48);
    let out = b.render(400);
    assert_eq!(nonzero(&out.left), [48]);
    assert_eq!(nonzero(&out.right), [48]);
}

#[test]
fn send_to_return_with_latency_is_aligned() {
    let mut b = Builder::new();
    let mut a = MixerTrack::new(TrackId(1), "A");
    a.sends.push(Send {
        id: SendId(20),
        dest: TrackId(2),
        level: 0.5,
        pre_fader: false,
    });
    let mut ret = MixerTrack::new(TrackId(2), "Return");
    ret.inserts.push(insert(10, delay_effect(2.0), None));
    b.track(a).track(ret);
    b.channel(TrackId(1), impulse());
    let out = b.render(400);
    assert_eq!(nonzero(&out.left), [96]);
}

/// DC on the bass channel, an impulse on the (muted) kick channel feeding
/// the bass compressor's sidechain. Returns the first reduced sample.
fn sidechain_onset(kick_latency_ms: f32, bass_pre_latency_ms: f32) -> (usize, u32) {
    let mut b = Builder::new();
    let mut kick = MixerTrack::new(TrackId(1), "Kick");
    kick.inserts
        .push(insert(10, delay_effect(kick_latency_ms), None));
    // MIX-012: the sidechain does not depend on the source's mute.
    kick.muted = true;
    let mut bass = MixerTrack::new(TrackId(2), "Bass");
    bass.inserts
        .push(insert(11, delay_effect(bass_pre_latency_ms), None));
    bass.inserts.push(insert(
        12,
        Effect::Compressor(CompressorParams {
            threshold_db: -40.0,
            ratio: 20.0,
            attack_ms: 0.0,
            release_ms: 500.0,
            makeup_db: 0.0,
            lookahead_ms: 0.0,
        }),
        Some(Sidechain {
            source: TrackId(1),
            tap: Tap::PreFader,
        }),
    ));
    b.track(kick).track(bass);
    let mut kick_sample = vec![0.0; 2000];
    kick_sample[200] = 1.0;
    b.channel(TrackId(1), kick_sample);
    b.channel(TrackId(2), vec![0.01; 4000]);
    let latency = b.snapshot().latency;
    let out = b.render(3000);
    let steady = out.left[(latency as usize) + 100];
    let onset = out
        .left
        .iter()
        .enumerate()
        .skip(latency as usize + 1)
        .find(|(_, s)| **s < steady * 0.5)
        .map(|(i, _)| i)
        .expect("compressor reacts to the sidechain");
    (onset - latency as usize, latency)
}

#[test]
fn sidechain_is_aligned_when_source_or_main_path_has_latency() {
    // In timeline terms the kick hit is at sample 200; the bass must duck
    // exactly there regardless of which path carries latency.
    assert_eq!(sidechain_onset(0.0, 0.0), (200, 0));
    assert_eq!(sidechain_onset(1.0, 0.0), (200, 48));
    assert_eq!(sidechain_onset(0.0, 1.0), (200, 48));
    assert_eq!(sidechain_onset(2.0, 1.0), (200, 96));
}

#[test]
fn mute_silences_main_and_post_sends_but_not_pre_sends() {
    let build = |pre_fader: bool| {
        let mut b = Builder::new();
        let mut a = MixerTrack::new(TrackId(1), "A");
        a.muted = true;
        a.sends.push(Send {
            id: SendId(20),
            dest: TrackId(2),
            level: 1.0,
            pre_fader,
        });
        b.track(a).track(MixerTrack::new(TrackId(2), "Return"));
        b.channel(TrackId(1), impulse());
        b.render(200)
    };
    assert!(nonzero(&build(false).left).is_empty());
    assert_eq!(nonzero(&build(true).left), [0]);
}

#[test]
fn volume_change_is_smoothed_and_keeps_delay_state() {
    let mut b = Builder::new();
    let mut a = MixerTrack::new(TrackId(1), "A");
    a.inserts.push(insert(10, delay_effect(1.0), None));
    b.track(a);
    b.channel(TrackId(1), vec![0.5; 20_000]);
    let first = b.snapshot();
    b.p.mixer.track_mut(TrackId(1)).unwrap().volume = 0.0;
    let second = b.snapshot();

    let (mut h, mut proc) = create_engine(SR);
    h.set_snapshot(first).unwrap();
    h.play().unwrap();
    let mut l = vec![0.0; 4_000];
    let mut r = vec![0.0; 4_000];
    proc.render(&mut l[..2_000], &mut r[..2_000]);
    h.set_snapshot(second).unwrap();
    proc.render(&mut l[2_000..], &mut r[2_000..]);
    let steady = l[1_999];
    assert!(steady > 0.1);
    // No step larger than a 10 ms ramp would produce, and it reaches 0.
    let max_step = l
        .windows(2)
        .skip(100)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(max_step < steady / 400.0, "click: step {max_step}");
    assert!(l[3_999].abs() < 1e-6);
    // The delay line kept its content: the signal did not drop out at the swap.
    assert!(l[2_000] > steady * 0.9);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// MIX-035 (5): random acyclic routing with random latencies — every
    /// impulse reaches the master on the same sample, equal to the
    /// reported latency.
    #[test]
    fn random_graphs_are_aligned(
        n in 1usize..7,
        seeds in prop::collection::vec((0u32..100, 0u32..4, 0u32..100, 0u32..100), 7),
    ) {
        let mut b = Builder::new();
        let ids: Vec<TrackId> = (1..=n as u64).map(TrackId).collect();
        let mut tracks = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            let (out_pick, lat, send_pick, send_on) = seeds[i];
            let mut t = MixerTrack::new(*id, format!("T{i}"));
            // Only route to later tracks (or the master): always acyclic.
            let later = &ids[i + 1..];
            if !later.is_empty() && out_pick % 3 != 0 {
                t.main_out = later[out_pick as usize % later.len()];
            }
            if !later.is_empty() && send_on % 2 == 0 {
                t.sends.push(Send {
                    id: SendId(500 + i as u64),
                    dest: later[send_pick as usize % later.len()],
                    level: 0.5,
                    pre_fader: send_on % 4 == 0,
                });
            }
            t.inserts.push(insert(600 + i as u64, delay_effect([0.0, 0.25, 1.0, 2.0][lat as usize]), None));
            tracks.push(t);
        }
        for t in tracks {
            b.track(t);
        }
        for id in &ids {
            b.channel(*id, impulse());
        }
        let latency = b.snapshot().latency as usize;
        let out = b.render(latency + 300);
        prop_assert_eq!(nonzero(&out.left), vec![latency]);
    }
}
