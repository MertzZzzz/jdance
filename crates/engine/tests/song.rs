// SPDX-License-Identifier: GPL-3.0-or-later
//! Song (playlist) playback: spec "Playlist" playback rules.

use jdance_domain::{
    Channel, ChannelId, ChannelSource, ClipId, LaneId, PatternClip, PatternId, Project, SampleRef,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{
    EngineHandle, EngineProcessor, PlayMode, PlaybackSnapshot, compile_with, create_engine,
    render_offline,
};
use proptest::prelude::*;
use std::sync::Arc;

const SR: u32 = 48_000;
/// 120 BPM at 48 kHz: 25 samples per tick, a 16-step bar is 96 000.
const BAR: u64 = 96_000;
const BAR_TICKS: u64 = 3_840;

struct Song {
    p: Project,
    sample: Arc<SampleBuffer>,
    pat: PatternId,
    next: u64,
}

impl Song {
    /// One channel with a short click on step 0 of the first pattern.
    fn new() -> Self {
        let mut p = Project::new();
        p.tempo_bpm = 120.0;
        let ch = ChannelId(p.ids.next());
        let mut c = Channel::new_sampler(ch, "c");
        c.volume = 1.0;
        c.source = ChannelSource::Sampler {
            sample: Some(SampleRef { path: "s".into() }),
        };
        p.channels.push(c);
        let pat = p.patterns[0].id;
        p.patterns[0].row_mut(ch).steps[0].on = true;
        p.patterns[0].row_mut(ch).steps[0].velocity = 1.0;
        let mut click = vec![0.0; 100];
        click[0] = 1.0;
        Self {
            p,
            sample: Arc::new(SampleBuffer::from_interleaved(SR, 1, &click)),
            pat,
            next: 10_000,
        }
    }

    fn lane(&self, i: usize) -> LaneId {
        self.p.playlist.lanes[i].id
    }

    fn clip(&mut self, lane: usize, start_bar: u64, repeat: u32) -> ClipId {
        self.next += 1;
        let id = ClipId(self.next);
        let pat = self.p.pattern(self.pat).unwrap();
        let clip = PatternClip::repeating(id, self.lane(lane), pat, start_bar * BAR_TICKS, repeat);
        self.p.playlist.clips.push(clip);
        id
    }

    fn snapshot(&self) -> PlaybackSnapshot {
        self.p.validate_arrangement().unwrap();
        let s = Arc::clone(&self.sample);
        compile_with(&self.p, SR, PlayMode::Song, move |_| Some(Arc::clone(&s)))
    }

    fn engine(&self) -> (EngineHandle, EngineProcessor) {
        let (mut h, p) = create_engine(SR);
        h.set_snapshot(self.snapshot()).unwrap();
        h.play().unwrap();
        (h, p)
    }

    /// Positions where a click starts (the click is a single 1.0 sample
    /// scaled by the channel pan law).
    fn hits(&self, frames: usize) -> Vec<(usize, f32)> {
        let out = render_offline(self.snapshot(), frames, || 1000);
        out.left
            .iter()
            .enumerate()
            .filter(|(_, v)| v.abs() > 1e-6)
            .map(|(i, v)| (i, *v))
            .collect()
    }
}

fn positions(h: &[(usize, f32)]) -> Vec<usize> {
    h.iter().map(|(i, _)| *i).collect()
}

#[test]
fn clips_play_at_their_position_and_repeat_in_local_time() {
    let mut s = Song::new();
    s.clip(0, 1, 2); // bars 1 and 2
    assert_eq!(
        positions(&s.hits(4 * BAR as usize)),
        [BAR as usize, 2 * BAR as usize]
    );
    assert_eq!(s.snapshot().end, 3 * BAR);
}

#[test]
fn overlapping_clips_both_play_without_deduplication() {
    let mut s = Song::new();
    s.clip(0, 0, 1);
    s.clip(1, 0, 1);
    s.clip(1, 0, 1); // same lane, same time, same pattern
    let hits = s.hits(BAR as usize);
    assert_eq!(positions(&hits), [0]);
    let single = {
        let mut one = Song::new();
        one.clip(0, 0, 1);
        one.hits(BAR as usize)[0].1
    };
    assert!(
        (hits[0].1 - 3.0 * single).abs() < 1e-5,
        "three independent voices"
    );
}

#[test]
fn lane_and_clip_mute_solo_gate_events() {
    let mut s = Song::new();
    s.clip(0, 0, 1);
    let b = s.clip(1, 1, 1);
    s.clip(2, 2, 1);
    s.p.playlist.lanes[1].solo = true;
    assert_eq!(
        positions(&s.hits(4 * BAR as usize)),
        [BAR as usize],
        "only solo lanes"
    );
    s.p.playlist.clip_mut(b).unwrap().muted = true;
    assert!(
        s.hits(4 * BAR as usize).is_empty(),
        "clip mute beats lane solo"
    );
    s.p.playlist.lanes[1].solo = false;
    s.p.playlist.lanes[0].muted = true;
    assert_eq!(positions(&s.hits(4 * BAR as usize)), [2 * BAR as usize]);
    // Muted clips still define the arrangement end.
    assert_eq!(s.snapshot().end, 3 * BAR);
}

#[test]
fn transport_stops_after_arrangement_end_and_tail() {
    let mut s = Song::new();
    s.clip(0, 0, 1);
    let (h, mut p) = s.engine();
    let mut l = vec![0.0; 1000];
    let mut r = vec![0.0; 1000];
    for _ in 0..(BAR as usize / 1000 - 1) {
        p.render(&mut l, &mut r);
    }
    assert!(h.feedback().read().playing, "still inside the arrangement");
    for _ in 0..20 {
        p.render(&mut l, &mut r);
    }
    let f = h.feedback().read();
    assert!(!f.playing, "stopped after end + tail");
    assert_eq!(f.position, 0);
}

#[test]
fn global_loop_wraps_and_releases_voices() {
    let mut s = Song::new();
    s.clip(0, 0, 4);
    // Long sample so the wrap release is visible.
    s.sample = Arc::new(SampleBuffer::from_interleaved(
        SR,
        1,
        &vec![0.5; 3 * BAR as usize],
    ));
    s.p.playlist.loop_range = Some((BAR_TICKS, 3 * BAR_TICKS));
    s.p.playlist.loop_enabled = true;
    let out = render_offline(s.snapshot(), 5 * BAR as usize + 1_000, || 777);
    let l = &out.left;
    // Each trigger starts a voice holding `v`. Before the wrap at 3·BAR the
    // voices from 0, BAR and 2·BAR sound (3v); at the wrap they get a 5 ms
    // release and the bar-1 event triggers again (v).
    let release = (SR / 200) as usize;
    let v = l[10];
    assert!((l[3 * BAR as usize - 1] - 3.0 * v).abs() < 1e-5);
    assert!((l[3 * BAR as usize + release + 5] - v).abs() < 1e-5);
    // Released voices fade; the only jumps are note onsets (≤ v).
    let step = l
        .windows(2)
        .skip(1)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(step <= v * 1.01, "step {step}");
    // And the loop keeps going: bar 1 again at 5·BAR.
    assert!((l[5 * BAR as usize + release + 5] - v).abs() < 1e-5);
}

#[test]
fn seek_starts_events_from_the_new_position_only() {
    let mut s = Song::new();
    s.clip(0, 0, 3);
    let (mut h, mut p) = s.engine();
    h.seek(BAR + BAR / 2).unwrap();
    let mut l = vec![0.0; BAR as usize];
    let mut r = vec![0.0; BAR as usize];
    p.render(&mut l, &mut r);
    let hits: Vec<usize> = l
        .iter()
        .enumerate()
        .filter(|(_, v)| v.abs() > 1e-6)
        .map(|(i, _)| i)
        .collect();
    // The bar-1 note (before the seek point) is not restored; the bar-2
    // note plays BAR/2 later.
    assert_eq!(hits, [BAR as usize / 2]);
}

#[test]
fn pattern_length_change_keeps_the_clip_length_and_repeats_more() {
    // Spec 0.2.0 §4: a clip stores its duration; the pattern definition
    // changing does not move the arrangement.
    let mut s = Song::new();
    s.clip(0, 0, 3);
    s.p.patterns[0].length_steps = 8;
    for row in &mut s.p.patterns[0].rows {
        row.steps.truncate(8);
    }
    assert_eq!(s.snapshot().end, 3 * BAR);
    assert_eq!(
        positions(&s.hits(2 * BAR as usize)),
        [0, BAR as usize / 2, BAR as usize, 3 * BAR as usize / 2]
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// RT-022 in song mode, including the loop wrap.
    #[test]
    fn song_is_block_size_independent(sizes in prop::collection::vec(1usize..=2048, 1..32)) {
        let mut s = Song::new();
        s.clip(0, 0, 2);
        s.clip(1, 1, 3);
        s.p.playlist.loop_range = Some((BAR_TICKS / 2, 3 * BAR_TICKS));
        s.p.playlist.loop_enabled = true;
        let reference = render_offline(s.snapshot(), 4 * BAR as usize, || 512);
        let mut it = sizes.iter().cycle();
        let out = render_offline(s.snapshot(), 4 * BAR as usize, || *it.next().unwrap());
        prop_assert_eq!(out.left, reference.left);
    }
}

#[test]
fn empty_arrangement_keeps_transport_running() {
    let s = Song::new();
    let (h, mut p) = s.engine();
    let mut l = vec![0.0; 1000];
    let mut r = vec![0.0; 1000];
    for _ in 0..10 {
        p.render(&mut l, &mut r);
    }
    assert!(h.feedback().read().playing);
}
