// SPDX-License-Identifier: GPL-3.0-or-later
//! Automation in the engine (AUT-020/021/030/033, AUT-070): volume, pan,
//! sends, built-in parameters, bypass and plugin parameters; song mode
//! only; block-size independence; no allocation.

use jdance_domain::{
    AutoPoint, AutomationId, AutomationLane, AutomationTarget, Channel, ChannelId, ChannelSource,
    Curve, Effect, EffectKind, Insert, InsertId, LaneId, MixerTrack, PatternClip, PluginSlot,
    PluginTarget, Project, SampleRef, Send, SendId, TrackId,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{PlayMode, PlaybackSnapshot, PluginRt, compile_full, render_offline};
use jdance_plugin_api::{PluginProcessor, Transport};
use jdance_rt_check::{GuardedAllocator, reset_violations, rt_section, violations};
use std::sync::{Arc, Mutex};

#[global_allocator]
static ALLOC: GuardedAllocator = GuardedAllocator;

const SR: u32 = 48_000;
/// 120 BPM: a bar (3840 ticks) = 96 000 samples, 25 samples per tick.
const BAR: usize = 96_000;
const TRACK: TrackId = TrackId(10);
const RET: TrackId = TrackId(11);

fn pt(tick: u64, value: f32, curve: Curve) -> AutoPoint {
    AutoPoint {
        tick,
        value,
        curve,
        tension: 0.0,
    }
}

/// A DC channel (1.0, centre) on track 10, playing from 0 in song mode.
fn project() -> Project {
    let mut p = Project::new();
    p.tempo_bpm = 120.0;
    for _ in 0..1000 {
        p.ids.next();
    }
    p.mixer.tracks.push(MixerTrack::new(TRACK, "A"));
    p.mixer.tracks.push(MixerTrack::new(RET, "Return"));
    let id = ChannelId(500);
    let mut c = Channel::new_sampler(id, "dc");
    c.volume = 1.0;
    c.track = TRACK;
    c.source = ChannelSource::Sampler {
        sample: Some(SampleRef {
            path: "dc.wav".into(),
        }),
    };
    p.channels.push(c);
    p.patterns[0].row_mut(id).steps[0].on = true;
    p.patterns[0].row_mut(id).steps[0].velocity = 1.0;
    let clip = PatternClip::repeating(
        jdance_domain::ClipId(p.ids.next()),
        p.playlist.lanes[0].id,
        &p.patterns[0],
        0,
        1,
    );
    p.playlist.clips.push(clip);
    let _: LaneId = p.playlist.lanes[0].id;
    p
}

/// A source with one clip over its whole length (16 bars) from 0.
fn lane(p: &mut Project, target: AutomationTarget, points: Vec<AutoPoint>) -> AutomationId {
    let id = source(p, target, points);
    place(p, id, 0, 0, 16 * 3840);
    id
}

fn source(p: &mut Project, target: AutomationTarget, points: Vec<AutoPoint>) -> AutomationId {
    let id = AutomationId(p.ids.next());
    p.automation.push(AutomationLane {
        id,
        name: "A".into(),
        target,
        length: 16 * 3840,
        points,
    });
    id
}

fn place(p: &mut Project, source: AutomationId, start: u64, offset: u64, duration: u64) {
    let clip = jdance_domain::AutomationClip {
        id: jdance_domain::ClipId(p.ids.next()),
        lane: p.playlist.lanes[1].id,
        source,
        start,
        offset,
        duration,
        muted: false,
    };
    p.playlist.automation_clips.push(clip);
    p.validate_arrangement().unwrap();
}

fn compile(p: &Project, mode: PlayMode, plugin: Option<PluginRt>) -> PlaybackSnapshot {
    let dc = Arc::new(SampleBuffer::from_interleaved(SR, 1, &vec![1.0; 2 * BAR]));
    let mut plugin = plugin;
    compile_full(p, SR, mode, |_| Some(Arc::clone(&dc)), &mut |_, _| {
        plugin.take().unwrap_or_else(PluginRt::missing)
    })
}

const PAN: f32 = std::f32::consts::FRAC_1_SQRT_2;

#[test]
fn volume_follows_the_envelope_within_32_samples() {
    let mut p = project();
    // 1.0 at 0 (0.8 · 1.25) falling linearly to 0 at one bar.
    lane(
        &mut p,
        AutomationTarget::TrackVolume(TRACK),
        vec![pt(0, 0.8, Curve::Linear), pt(3840, 0.0, Curve::Linear)],
    );
    let out = render_offline(compile(&p, PlayMode::Song, None), BAR / 2, || 700).left;
    let env = |k: usize| 1.0 - k as f32 / BAR as f32;
    for k in (31..BAR / 2).step_by(32) {
        assert!(
            (out[k] - PAN * env(k)).abs() < 1e-4,
            "k={k}: {} vs {}",
            out[k],
            PAN * env(k)
        );
    }
    // Pattern mode ignores automation (ADR-014).
    let pattern = render_offline(
        compile(&p, PlayMode::Pattern(p.patterns[0].id), None),
        4_096,
        || 512,
    )
    .left;
    assert!((pattern[4_000] - PAN).abs() < 1e-6);
}

#[test]
fn automation_acts_only_inside_its_clips() {
    let mut p = project();
    // Volume 0.4 (0.5 · 1.25 = 0.625) on source ticks 0..1920, then 0.8.
    let id = source(
        &mut p,
        AutomationTarget::TrackVolume(TRACK),
        vec![pt(0, 0.4, Curve::Hold), pt(1920, 0.8, Curve::Hold)],
    );
    // Source from tick 960 placed at a quarter bar for half a bar.
    place(&mut p, id, 960, 960, 1920);
    let out = render_offline(compile(&p, PlayMode::Song, None), BAR, || 256).left;
    let q = BAR / 4;
    assert!((out[q - 64] - PAN).abs() < 1e-5, "before: manual 1.0");
    assert!((out[q + 64] - PAN * 0.5).abs() < 1e-5, "{}", out[q + 64]);
    assert!(
        (out[2 * q + 64] - PAN).abs() < 1e-5,
        "source 1920: 0.8 · 1.25"
    );
    assert!((out[3 * q + 64] - PAN).abs() < 1e-5, "after: manual again");
    // A muted clip leaves the manual value.
    p.playlist.automation_clips[0].muted = true;
    let muted = render_offline(compile(&p, PlayMode::Song, None), BAR, || 256).left;
    assert!((muted[q + 64] - PAN).abs() < 1e-5);
}

#[test]
fn audio_clip_gain_is_automated() {
    let mut p = project();
    p.playlist.clips.clear();
    let asset = jdance_domain::AudioAsset {
        id: jdance_domain::AssetId(p.ids.next()),
        name: "dc".into(),
        path: "dc.wav".into(),
        hash: 0,
        size: 0,
        sample_rate: SR,
        channels: 1,
        frames: 2 * BAR as u64,
    };
    let clip = jdance_domain::AudioClip {
        id: jdance_domain::ClipId(p.ids.next()),
        lane: p.playlist.lanes[0].id,
        channel: jdance_domain::ChannelId(p.ids.next()),
        start: 0,
        source_in: 0,
        source_out: 2 * BAR as u64,
        gain: 1.0,
        muted: false,
        fade_in: 0,
        fade_out: 0,
        speed: 1.0,
    };
    p.channels.push(jdance_domain::Channel::new_audio(
        clip.channel,
        "dc",
        asset.id,
    ));
    p.assets.push(asset);
    p.playlist.audio_clips.push(clip);
    lane(
        &mut p,
        AutomationTarget::AudioClipGain(clip.id),
        vec![pt(0, 0.4, Curve::Hold)],
    );
    let out = render_offline(compile(&p, PlayMode::Song, None), BAR, || 512).left;
    // The lane bus is the master: unity, centre pan law of the master.
    let plain = {
        p.automation.clear();
        p.playlist.automation_clips.clear();
        render_offline(compile(&p, PlayMode::Song, None), BAR, || 512).left
    };
    assert!(plain[BAR / 2] > 0.1);
    assert!(
        (out[BAR / 2] - plain[BAR / 2] * 0.5).abs() < 1e-5,
        "{} vs {}",
        out[BAR / 2],
        plain[BAR / 2]
    );
}

#[test]
fn hold_jumps_complete_within_32_samples() {
    let mut p = project();
    lane(
        &mut p,
        AutomationTarget::TrackVolume(TRACK),
        vec![pt(0, 0.8, Curve::Hold), pt(1920, 0.0, Curve::Hold)],
    );
    let out = render_offline(compile(&p, PlayMode::Song, None), BAR, || 333).left;
    let jump = BAR / 2;
    assert!((out[jump - 33] - PAN).abs() < 1e-6, "before the jump");
    assert!(out[jump + 32].abs() < 1e-6, "silent within 32 samples");
}

#[test]
fn pan_and_send_level_are_automated() {
    let mut p = project();
    p.mixer.tracks[0].sends.push(Send {
        id: SendId(20),
        dest: RET,
        level: 0.0,
        pre_fader: true,
    });
    p.mixer.tracks[1].muted = false;
    // Pan hard left; send level 1.0 (0.5 · 2.0).
    lane(
        &mut p,
        AutomationTarget::TrackPan(TRACK),
        vec![pt(0, 0.0, Curve::Hold)],
    );
    lane(
        &mut p,
        AutomationTarget::SendLevel(SendId(20)),
        vec![pt(0, 0.5, Curve::Hold)],
    );
    let out = render_offline(compile(&p, PlayMode::Song, None), 4_096, || 512);
    // Chased at the start (AUT-033): no ramp from the project values.
    assert!((out.left[0] - 2.0 * PAN).abs() < 1e-5, "{}", out.left[0]);
    assert!((out.right[0] - PAN).abs() < 1e-5, "{}", out.right[0]);
    // Left: fader path (hard left, gain 1) + pre-fader send (centre).
    assert!(
        (out.left[4_000] - 2.0 * PAN).abs() < 1e-5,
        "{}",
        out.left[4_000]
    );
    assert!(
        (out.right[4_000] - PAN).abs() < 1e-5,
        "{}",
        out.right[4_000]
    );
}

#[test]
fn builtin_parameter_and_bypass_are_automated() {
    let mut p = project();
    // A compressor at ratio 1 is a pure makeup gain stage.
    let comp = Effect::new(EffectKind::Compressor)
        .with("ratio", 1.0)
        .with("makeup", 0.0);
    p.mixer.tracks[0].inserts.push(Insert {
        id: InsertId(30),
        bypass: false,
        wet: 1.0,
        effect: comp,
        sidechain: None,
    });
    // makeup 0..24 dB linear: 0.25 → 6 dB.
    lane(
        &mut p,
        AutomationTarget::EffectParam {
            insert: InsertId(30),
            param: "makeup".into(),
        },
        vec![pt(0, 0.25, Curve::Hold)],
    );
    let out = render_offline(compile(&p, PlayMode::Song, None), 20_000, || 512).left;
    let gain = out[19_000] / PAN;
    assert!((jdance_dsp::gain_to_db(gain) - 6.0).abs() < 0.05, "{gain}");

    // Bypass automated on: the makeup is gone.
    lane(
        &mut p,
        AutomationTarget::InsertBypass(InsertId(30)),
        vec![pt(0, 1.0, Curve::Hold)],
    );
    let out = render_offline(compile(&p, PlayMode::Song, None), 20_000, || 512).left;
    assert!((out[19_000] - PAN).abs() < 1e-5, "{}", out[19_000]);
}

/// Queued (id, offset, value) points of one block.
type Points = Vec<(u32, u32, f64)>;

/// Records the queued parameter points per block.
struct ParamLog(Arc<Mutex<Vec<Points>>>, Points);

impl PluginProcessor for ParamLog {
    fn process(
        &mut self,
        _: &mut [f32],
        _: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &Transport,
    ) {
        let block = std::mem::take(&mut self.1);
        if let Ok(mut l) = self.0.lock() {
            l.push(block);
        }
    }

    fn queue_param(&mut self, id: u32, offset: u32, value: f64) {
        self.1.push((id, offset, value));
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn has_sidechain_input(&self) -> bool {
        false
    }
}

#[test]
fn plugin_parameters_get_chase_breakpoints_and_intermediate_points() {
    let mut p = project();
    p.mixer.tracks[0].inserts.push(Insert {
        id: InsertId(31),
        bypass: false,
        wet: 1.0,
        effect: Effect::plugin(PluginSlot {
            format: "vst3".into(),
            class_id: "0".repeat(32),
            name: "Log".into(),
            vendor: "t".into(),
            version: "1".into(),
            category: "Fx".into(),
            component_state: Vec::new(),
            controller_state: Vec::new(),
        }),
        sidechain: None,
    });
    // Breakpoint at tick 10 = sample 250.
    lane(
        &mut p,
        AutomationTarget::PluginParam {
            target: PluginTarget::Insert(InsertId(31)),
            param: 7,
        },
        vec![pt(0, 0.1, Curve::Linear), pt(10, 0.9, Curve::Linear)],
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let plugin = PluginRt::fresh(Box::new(ParamLog(Arc::clone(&log), Vec::new())));
    render_offline(compile(&p, PlayMode::Song, Some(plugin)), 1_024, || 512);
    let blocks = log.lock().unwrap();
    let first = &blocks[0];
    assert_eq!(first[0], (7, 0, f64::from(0.1f32)), "chase at offset 0");
    assert!(
        first.iter().any(|&(_, o, _)| o == 64),
        "intermediate every 64"
    );
    assert!(
        first
            .iter()
            .any(|&(_, o, v)| o == 250 && (v - 0.9).abs() < 1e-6),
        "breakpoint at its exact offset: {first:?}"
    );
    assert!(
        blocks[1].iter().any(|&(_, o, _)| o == 0),
        "chase every block"
    );
}

#[test]
fn automated_rendering_is_block_size_independent() {
    let mut p = project();
    lane(
        &mut p,
        AutomationTarget::TrackVolume(TRACK),
        vec![
            pt(0, 0.1, Curve::Curve),
            pt(1000, 0.9, Curve::Hold),
            pt(2000, 0.3, Curve::Linear),
            pt(3840, 0.6, Curve::Linear),
        ],
    );
    let a = render_offline(compile(&p, PlayMode::Song, None), BAR, || 1024);
    let mut sizes = [7usize, 333, 64, 1, 900].into_iter().cycle();
    let b = render_offline(compile(&p, PlayMode::Song, None), BAR, || {
        sizes.next().unwrap()
    });
    assert_eq!(a, b);
}

#[test]
fn applying_automation_does_not_allocate() {
    let mut p = project();
    lane(
        &mut p,
        AutomationTarget::TrackVolume(TRACK),
        vec![pt(0, 0.1, Curve::Linear), pt(3840, 0.9, Curve::Linear)],
    );
    let snap = compile(&p, PlayMode::Song, None);
    let (mut h, mut e) = jdance_engine::create_engine(SR);
    h.set_snapshot(snap).unwrap();
    h.play().unwrap();
    let mut l = vec![0.0; 1024];
    let mut r = vec![0.0; 1024];
    e.render(&mut l, &mut r);
    reset_violations();
    rt_section(|| {
        for _ in 0..100 {
            e.render(&mut l, &mut r);
        }
    });
    assert_eq!(violations(), 0);
}

#[test]
fn golden_volume_pan_and_send_automation() {
    // AUT-051, AUT-070: one bar at a low rate keeps the file small.
    let sr = 22_050;
    let mut p = project();
    for k in (0..16).step_by(2) {
        let s = &mut p.patterns[0].row_mut(ChannelId(500)).steps[k];
        s.on = true;
        s.velocity = 1.0;
    }
    p.mixer.tracks[0].sends.push(Send {
        id: SendId(20),
        dest: RET,
        level: 0.0,
        pre_fader: false,
    });
    p.mixer.tracks[1].pan = 1.0;
    lane(
        &mut p,
        AutomationTarget::TrackVolume(TRACK),
        vec![
            AutoPoint {
                tension: 0.6,
                ..pt(0, 0.8, Curve::Curve)
            },
            pt(1920, 0.2, Curve::Linear),
            pt(3840, 0.7, Curve::Linear),
        ],
    );
    lane(
        &mut p,
        AutomationTarget::TrackPan(TRACK),
        vec![pt(0, 0.0, Curve::Linear), pt(3840, 1.0, Curve::Linear)],
    );
    lane(
        &mut p,
        AutomationTarget::SendLevel(SendId(20)),
        vec![
            pt(0, 0.0, Curve::Hold),
            pt(960, 0.5, Curve::Hold),
            pt(2880, 0.25, Curve::Hold),
        ],
    );
    let click = Arc::new(jdance_test_support::click(sr, 2_000));
    let snap = compile_full(
        &p,
        sr,
        PlayMode::Song,
        |_| Some(Arc::clone(&click)),
        &mut |_, _| PluginRt::missing(),
    );
    let frames = (f64::from(sr) * 2.0) as usize;
    let out = render_offline(snap, frames, || 256);
    // Tolerance as in `golden_four_on_floor` (libm in the pan law).
    jdance_test_support::check_golden(
        "automation_volume_pan_send_22k",
        sr,
        &out.left,
        &out.right,
        1e-6,
    );
}
