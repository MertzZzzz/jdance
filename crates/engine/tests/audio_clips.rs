// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio clips and pattern clip windows in song playback (spec 0.2.0 §2,
//! §4): position in project frames, source range, edge fades, seek into a
//! clip, routing through the clip's audio channel (spec 0.2.2 ACH-010),
//! overlap; pattern clip offset, duration and loop.

use jdance_domain::{
    AssetId, AudioAsset, AudioClip, Channel, ChannelId, ChannelSource, ClipId, MixerTrack,
    PatternClip, Project, SampleRef, TrackId,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{PlayMode, compile_with, create_engine, render_offline};
use std::sync::Arc;

const SR: u32 = 48_000;
/// 3 ms anti-click fade at 48 kHz.
const FADE: usize = 144;

/// A 48 kHz ramp: frame k = (k + 1) / 10 000.
fn ramp(frames: usize) -> Arc<SampleBuffer> {
    let data: Vec<f32> = (0..frames).map(|k| (k + 1) as f32 / 10_000.0).collect();
    Arc::new(SampleBuffer::from_interleaved(SR, 1, &data))
}

/// A project with one asset and its audio channel; returns the channel.
fn project_with_asset(frames: u64) -> (Project, ChannelId) {
    let mut p = Project::new();
    p.tempo_bpm = 120.0;
    let id = AssetId(p.ids.next());
    p.assets.push(AudioAsset {
        id,
        name: "ramp".into(),
        path: "ramp.wav".into(),
        hash: 0,
        size: 0,
        sample_rate: SR,
        channels: 1,
        frames,
    });
    let ch = ChannelId(p.ids.next());
    p.channels.push(Channel::new_audio(ch, "ramp", id));
    (p, ch)
}

fn clip(p: &mut Project, channel: ChannelId, start: u64, range: (u64, u64)) -> AudioClip {
    let c = AudioClip {
        id: ClipId(p.ids.next()),
        lane: p.playlist.lanes[0].id,
        channel,
        start,
        source_in: range.0,
        source_out: range.1,
        gain: 1.0,
        muted: false,
        fade_in: 0,
        fade_out: 0,
    };
    p.playlist.audio_clips.push(c);
    c
}

fn render(p: &Project, buf: &Arc<SampleBuffer>, frames: usize) -> Vec<f32> {
    p.validate_arrangement().unwrap();
    let b = Arc::clone(buf);
    let snap = compile_with(p, SR, PlayMode::Song, move |s| {
        (s.path == "ramp.wav").then(|| Arc::clone(&b))
    });
    // An audio channel at unity volume and centered balance changes
    // nothing (ACH-011).
    render_offline(snap, frames, || 512).left
}

fn frame(k: u64) -> f32 {
    (k + 1) as f32 / 10_000.0
}

#[test]
fn clip_plays_its_source_range_at_its_position_with_edge_fades() {
    let buf = ramp(10_000);
    let (mut p, a) = project_with_asset(10_000);
    clip(&mut p, a, 1_000, (2_000, 6_000));
    let out = render(&p, &buf, 6_000);
    assert!(out[..1_000].iter().all(|v| *v == 0.0), "silent before");
    // Inside: source frame 2000 + (t − 1000), at full level after the fade.
    let t = 3_000;
    assert!((out[t] - frame(2_000 + (t as u64 - 1_000))).abs() < 1e-6);
    assert!(
        out[1_000] > 0.0 && out[1_000] < frame(2_000) * 0.05,
        "fade-in"
    );
    let last = 1_000 + 4_000 - 1;
    assert!(out[last] < frame(5_999) * 0.05, "fade-out at the end");
    assert!(
        out[1_000 + 4_000..].iter().all(|v| *v == 0.0),
        "the rest of the file never plays"
    );
    assert!((out[1_000 + FADE + 10] - frame(2_000 + FADE as u64 + 10)).abs() < 1e-6);
}

#[test]
fn seek_into_a_clip_plays_the_matching_source_position() {
    let buf = ramp(48_000);
    let (mut p, a) = project_with_asset(48_000);
    clip(&mut p, a, 0, (0, 48_000));
    p.validate_arrangement().unwrap();
    let b = Arc::clone(&buf);
    let snap = compile_with(&p, SR, PlayMode::Song, move |_| Some(Arc::clone(&b)));
    let (mut h, mut e) = create_engine(SR);
    h.set_snapshot(snap).unwrap();
    h.seek(20_000).unwrap();
    h.play().unwrap();
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    e.render(&mut l, &mut r);
    assert!((l[100] - frame(20_100)).abs() < 1e-6, "{}", l[100]);
}

#[test]
fn overlapping_clips_sum_and_play_through_their_channel() {
    let buf = ramp(10_000);
    let (mut p, a) = project_with_asset(10_000);
    clip(&mut p, a, 0, (0, 4_000));
    clip(&mut p, a, 0, (0, 4_000));
    let both = render(&p, &buf, 4_000);
    assert!((both[1_000] - 2.0 * frame(1_000)).abs() < 1e-6, "summed");

    // Channel → a track at half volume → master.
    let t = TrackId(p.ids.next());
    let mut track = MixerTrack::new(t, "Bus");
    track.volume = 0.5;
    p.mixer.tracks.push(track);
    let ch = a;
    p.channel_mut(ch).unwrap().track = t;
    let routed = render(&p, &buf, 4_000);
    assert!(
        (routed[1_000] - frame(1_000)).abs() < 1e-5,
        "{}",
        routed[1_000]
    );
    p.channel_mut(ch).unwrap().track = TrackId::MASTER;

    // Channel volume, balance and mute act on every instance.
    p.channel_mut(ch).unwrap().volume = 0.25;
    let quiet = render(&p, &buf, 4_000);
    assert!((quiet[1_000] - 0.5 * frame(1_000)).abs() < 1e-6);
    let c = p.channel_mut(ch).unwrap();
    c.volume = 1.0;
    c.pan = -1.0;
    let b = Arc::clone(&buf);
    let snap = compile_with(&p, SR, PlayMode::Song, move |_| Some(Arc::clone(&b)));
    let out = render_offline(snap, 4_000, || 512);
    assert!(
        (out.left[1_000] - 2.0 * frame(1_000)).abs() < 1e-6,
        "left keeps unity"
    );
    assert!(out.right.iter().all(|v| *v == 0.0), "balance hard left");
    let c = p.channel_mut(ch).unwrap();
    c.pan = 0.0;
    c.muted = true;
    assert!(render(&p, &buf, 4_000).iter().all(|v| *v == 0.0));
    p.channel_mut(ch).unwrap().muted = false;

    // Lane mute silences its audio clips.
    p.playlist.lanes[0].muted = true;
    assert!(render(&p, &buf, 4_000).iter().all(|v| *v == 0.0));
}

#[test]
fn clips_keep_their_time_at_another_device_rate_and_tempo() {
    // Positions are project frames at 48 kHz; at 96 kHz they double.
    let (mut p, a) = project_with_asset(48_000);
    clip(&mut p, a, 24_000, (0, 48_000));
    p.tempo_bpm = 60.0;
    let rate = 96_000;
    let b = Arc::new(ramp(48_000).resampled(rate));
    let snap = compile_with(&p, rate, PlayMode::Song, move |_| Some(Arc::clone(&b)));
    let out = render_offline(snap, 60_000, || 512).left;
    assert!(out[..48_000].iter().all(|v| *v == 0.0));
    assert!(
        out[48_000 + 1_000] > 0.0,
        "starts at 0.5 s whatever the tempo"
    );
}

#[test]
fn pattern_clip_window_offset_and_loop() {
    // One channel, a click on step 0 and step 8 (of 16).
    let mut p = Project::new();
    p.tempo_bpm = 120.0;
    let ch = ChannelId(p.ids.next());
    let mut c = Channel::new_sampler(ch, "c");
    c.volume = 1.0;
    c.pan = 0.0;
    c.source = ChannelSource::Sampler {
        sample: Some(SampleRef { path: "c".into() }),
    };
    p.channels.push(c);
    for s in [0, 8] {
        p.patterns[0].row_mut(ch).steps[s].on = true;
        p.patterns[0].row_mut(ch).steps[s].velocity = 1.0;
    }
    let mut click = vec![0.0; 10];
    click[0] = 1.0;
    let click = Arc::new(SampleBuffer::from_interleaved(SR, 1, &click));
    let onsets = |p: &Project| -> Vec<usize> {
        let b = Arc::clone(&click);
        let snap = compile_with(p, SR, PlayMode::Song, move |_| Some(Arc::clone(&b)));
        let out = render_offline(snap, 4 * 96_000, || 512).left;
        (0..out.len()).filter(|&i| out[i] > 0.5).collect()
    };
    let pat = p.patterns[0].clone();
    let lane = p.playlist.lanes[0].id;
    // Starts half way into the pattern, 1.5 bars long, looped: step 8 at
    // 0, step 0 (next repetition) at half a bar, step 8 at a bar.
    let mut clip = PatternClip::repeating(ClipId(p.ids.next()), lane, &pat, 0, 1);
    clip.offset = 1_920;
    clip.duration = 3 * 1_920;
    p.playlist.clips.push(clip);
    assert_eq!(onsets(&p), [0, 48_000, 96_000]);
    // Not looped: only the rest of the pattern (step 8) plays.
    p.playlist.clips[0].looped = false;
    assert_eq!(onsets(&p), [0]);
}
