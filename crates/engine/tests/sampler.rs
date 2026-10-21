// SPDX-License-Identifier: GPL-3.0-or-later
//! Sampler channel settings in the engine (spec "DAW UI clean room", §11,
//! §12): markers, root key, transposition, gain, loop with note length,
//! envelope, mono and polyphony.

use jdance_domain::{Channel, Note, PlaybackMode, ROOT_KEY, SamplerSettings, TICKS_PER_STEP};
use jdance_dsp::SampleBuffer;
use jdance_engine::{PlayMode, compile_with, create_engine, render_offline};
use jdance_test_support::step_project;
use std::sync::Arc;

const SR: u32 = 48_000;
/// 120 BPM: one step (240 ticks) = 6000 samples.
const STEP: usize = 6_000;
const LEN: usize = 4_000;
const PAN: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Frame k of the ramp holds (k + 1) / LEN, so every frame is non-zero.
fn ramp() -> Arc<SampleBuffer> {
    let data: Vec<f32> = (0..LEN).map(|i| (i + 1) as f32 / LEN as f32).collect();
    Arc::new(SampleBuffer::from_interleaved(SR, 1, &data))
}

fn note(step: u32, steps: u32, key: u8) -> Note {
    Note {
        start: step * TICKS_PER_STEP,
        length: steps * TICKS_PER_STEP,
        key,
        velocity: 1.0,
    }
}

fn render(edit: impl FnOnce(&mut Channel), notes: Vec<Note>, frames: usize) -> Vec<f32> {
    let mut p = step_project(120.0, &[&[]]);
    edit(&mut p.channels[0]);
    p.channels[0].pan = 0.0;
    p.channels[0].volume = 1.0;
    let ch = p.channels[0].id;
    let pid = p.patterns[0].id;
    p.patterns[0].row_mut(ch).notes = notes;
    let s = ramp();
    let snap = compile_with(&p, SR, PlayMode::Pattern(pid), |_| Some(Arc::clone(&s)));
    render_offline(snap, frames, || 512)
        .left
        .into_iter()
        .map(|v| v / PAN)
        .collect()
}

fn frame(k: usize) -> f32 {
    (k + 1) as f32 / LEN as f32
}

#[test]
fn markers_root_key_pitch_and_gain() {
    let out = render(
        |c| {
            c.sampler.start = 0.25;
            c.sampler.end = 0.5;
        },
        vec![note(0, 1, ROOT_KEY)],
        STEP,
    );
    assert!((out[0] - frame(1_000)).abs() < 1e-6, "starts at the marker");
    assert!(
        out[999] > 0.0 && out[1_000] == 0.0,
        "ends at the end marker"
    );

    // Root key B4 (59): C5 plays a semitone up.
    let up = render(
        |c| c.sampler.root_key = 59,
        vec![note(0, 1, ROOT_KEY)],
        STEP,
    );
    let rate = jdance_dsp::key_rate(60, 59) as f32;
    assert!((up[100] - frame(0) - (100.0 * rate) / LEN as f32).abs() < 1e-4);

    // Channel pitch +12 doubles the speed; +6 dB doubles the level.
    let out = render(
        |c| {
            c.pitch = 12;
            c.sampler.gain_db = 20.0 * 2f32.log10();
        },
        vec![note(0, 1, ROOT_KEY)],
        STEP,
    );
    assert!((out[100] - 2.0 * frame(200)).abs() < 1e-4, "{}", out[100]);
    assert_eq!(out[LEN / 2 + 10], 0.0, "twice as fast: half as long");
}

#[test]
fn loop_mode_repeats_until_note_off_and_envelope_shapes() {
    let out = render(
        |c| {
            c.sampler.mode = PlaybackMode::Loop;
            c.sampler.loop_start = 0.5;
            c.sampler.loop_end = 0.75;
        },
        vec![note(0, 2, ROOT_KEY)],
        3 * STEP,
    );
    // Plays 0..3000, then loops 2000..3000 until the note ends (12 000).
    assert!((out[2_999] - frame(2_999)).abs() < 1e-6);
    assert!(
        (out[3_000] - frame(2_000)).abs() < 1e-6,
        "wraps to the loop start"
    );
    assert!(out[11_000] > 0.0, "still looping while held");
    assert!(out[12_500] == 0.0, "released after the note");

    let env = render(
        |c| {
            c.sampler.envelope = true;
            c.sampler.attack_ms = 10.0;
            c.sampler.sustain = 1.0;
            c.sampler.release_ms = 0.0;
        },
        vec![note(0, 1, ROOT_KEY)],
        STEP,
    );
    // 10 ms attack = 480 samples: half way at 240.
    assert!(
        (env[239] / frame(239) - 0.5).abs() < 0.01,
        "{}",
        env[239] / frame(239)
    );
    assert!(
        (env[1_000] - frame(1_000)).abs() < 1e-6,
        "full level after the attack"
    );
}

#[test]
fn mono_and_polyphony_limit_voices() {
    // Two overlapping notes: the second starts at step 0 + 100 ticks.
    let late = Note {
        start: 100,
        ..note(0, 4, ROOT_KEY)
    };
    let both = render(|_| {}, vec![note(0, 4, ROOT_KEY), late], STEP);
    let mono = render(|c| c.mono = true, vec![note(0, 4, ROOT_KEY), late], STEP);
    let poly1 = render(|c| c.polyphony = 1, vec![note(0, 4, ROOT_KEY), late], STEP);
    let second = 100 * 25; // ticks → samples at 120 BPM / 48 kHz
    let k = second + 500;
    assert!((both[k] - frame(k) - frame(500)).abs() < 1e-5, "both sound");
    assert!(
        (mono[k] - frame(500)).abs() < 1e-5,
        "mono: only the new note"
    );
    assert_eq!(mono, poly1, "polyphony 1 behaves like mono here");
}

#[test]
fn default_settings_round_trip() {
    let s = SamplerSettings::default();
    assert!(s.validate().is_ok());
    assert!(s.is_default());
    let bad = SamplerSettings {
        start: 0.6,
        end: 0.5,
        ..s
    };
    assert!(bad.validate().is_err());
}

#[test]
fn live_notes_one_shot_and_gated() {
    let live = |gated: bool| {
        let mut p = step_project(120.0, &[&[]]);
        p.channels[0].pan = 0.0;
        p.channels[0].volume = 1.0;
        if gated {
            p.channels[0].sampler.mode = PlaybackMode::Loop;
        }
        let ch = p.channels[0].id;
        let s = ramp();
        let snap = compile_with(&p, SR, PlayMode::Pattern(p.patterns[0].id), |_| {
            Some(Arc::clone(&s))
        });
        let (mut h, mut e) = create_engine(SR);
        h.set_snapshot(snap).unwrap();
        let (mut l, mut r) = (vec![0.0; 1_000], vec![0.0; 1_000]);
        e.render(&mut l, &mut r);
        h.live_note(ch, ROOT_KEY, 1.0).unwrap();
        e.render(&mut l, &mut r);
        let first = l[999] / PAN;
        h.live_note_off(ch, ROOT_KEY).unwrap();
        e.render(&mut l, &mut r);
        (first, l[999] / PAN)
    };
    let (held, after) = live(false);
    assert!((held - frame(999)).abs() < 1e-6, "plays while stopped");
    assert!(
        (after - frame(1_999)).abs() < 1e-6,
        "one-shot plays on after note-off"
    );
    let (held, after) = live(true);
    assert!(held > 0.0);
    assert_eq!(after, 0.0, "a gated note stops at note-off");
}
