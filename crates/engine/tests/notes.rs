// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano-roll notes: timing, pitch and gate boundaries.

use jdance_domain::{Note, PatternId, ROOT_KEY, TICKS_PER_STEP};
use jdance_dsp::SampleBuffer;
use jdance_engine::{PlayMode, compile_with, render_offline};
use jdance_test_support::step_project;
use std::sync::Arc;

const SR: u32 = 48_000;
/// 120 BPM: one step (240 ticks) = 6000 samples.
const STEP: usize = 6_000;

fn ramp() -> Arc<SampleBuffer> {
    let data: Vec<f32> = (0..4_000).map(|i| i as f32 / 4_000.0).collect();
    Arc::new(SampleBuffer::from_interleaved(SR, 1, &data))
}

fn note(step: u32, key: u8) -> Note {
    Note {
        id: 0,
        start: step * TICKS_PER_STEP,
        length: TICKS_PER_STEP,
        key,
        velocity: 1.0,
    }
}

fn render(notes: Vec<Note>, frames: usize) -> Vec<f32> {
    let mut p = step_project(120.0, &[&[]]);
    p.channels[0].pan = 0.0;
    p.channels[0].volume = 1.0;
    let ch = p.channels[0].id;
    let pid: PatternId = p.patterns[0].id;
    p.patterns[0].row_mut(ch).notes = notes;
    p.validate_arrangement().unwrap();
    let s = ramp();
    let snap = compile_with(&p, SR, PlayMode::Pattern(pid), |_| Some(Arc::clone(&s)));
    render_offline(snap, frames, || 512).left
}

#[test]
fn notes_start_on_time_and_pitch_changes_speed() {
    let out = render(vec![note(1, ROOT_KEY), note(4, ROOT_KEY + 12)], 6 * STEP);
    let pan = 0.5f32.sqrt();
    // Root key: frame k of the ramp is k/4000.
    let at = |i: usize| out[i] / pan;
    assert_eq!(at(STEP - 1), 0.0);
    assert!((at(STEP + 100) - 100.0 / 4_000.0).abs() < 1e-6);
    // An octave up reads two frames per output frame.
    assert!((at(4 * STEP + 100) - 200.0 / 4_000.0).abs() < 1e-6);
    // ... and ends after half the sample length.
    assert_eq!(at(4 * STEP + 2_000), 0.0);
}

#[test]
fn notes_after_the_pattern_end_do_not_play() {
    // Default pattern: 16 steps; a note on step 20 is outside.
    let out = render(vec![note(20, ROOT_KEY)], 16 * STEP);
    assert!(out.iter().all(|v| *v == 0.0));
}

#[test]
fn gate_is_cut_at_the_repetition_end() {
    let mut p = step_project(120.0, &[&[]]);
    let ch = p.channels[0].id;
    let long = Note {
        id: 0,
        start: 15 * TICKS_PER_STEP,
        length: 8 * TICKS_PER_STEP,
        key: 64,
        velocity: 0.5,
    };
    p.patterns[0].row_mut(ch).notes = vec![long];
    let pid = p.patterns[0].id;
    let s = ramp();
    let snap = compile_with(&p, SR, PlayMode::Pattern(pid), |_| Some(Arc::clone(&s)));
    let e = snap.events.iter().find(|e| e.key == 64).unwrap();
    assert_eq!(e.pos, 15 * STEP as u64);
    assert_eq!(e.length, STEP as u64, "cut at the pattern end");
    assert_eq!(e.velocity, 0.5);
}
