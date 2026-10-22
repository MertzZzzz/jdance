// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer track inputs (spec 0.2.3 INP-003/004/006, MON-001/003).

use jdance_domain::{MixerTrack, Project, TrackId, TrackInput};
use jdance_engine::{PlayMode, compile_with, create_engine, render_offline};

const SR: u32 = 48_000;
const N: usize = 4_800;

fn project(input: Option<TrackInput>, monitor: bool, muted: bool) -> Project {
    let mut p = Project::new();
    let mut t = MixerTrack::new(TrackId(p.ids.next()), "Voice");
    t.input = input;
    t.monitor = monitor;
    t.muted = muted;
    p.mixer.tracks.push(t);
    p
}

/// Steady output after the fader ramps, for 3 device channels holding
/// 0.1, 0.2 and 0.3.
fn output(p: &Project) -> (f32, f32) {
    let (mut h, mut proc_) = create_engine(SR);
    h.set_snapshot(compile_with(p, SR, PlayMode::Song, |_| None))
        .unwrap();
    let input: Vec<f32> = (0..N).flat_map(|_| [0.1, 0.2, 0.3]).collect();
    let (mut l, mut r) = (vec![0.0; N], vec![0.0; N]);
    for _ in 0..3 {
        proc_.render_with_input(&mut l, &mut r, &input, 3);
    }
    (l[N - 1], r[N - 1])
}

#[test]
fn a_monitored_input_sounds_through_the_track() {
    let (l, r) = output(&project(Some(TrackInput::Stereo(1)), true, false));
    // Centered pan keeps the level (ACH-012 law): 0.2 left, 0.3 right.
    assert!((l - 0.2).abs() < 1e-3 && (r - 0.3).abs() < 1e-3, "{l} {r}");
    // Mono feeds both sides the same (INP-003).
    let (l, r) = output(&project(Some(TrackInput::Mono(0)), true, false));
    assert!((l - 0.1).abs() < 1e-3 && (r - 0.1).abs() < 1e-3, "{l} {r}");
}

#[test]
fn unmonitored_muted_or_missing_inputs_are_silent() {
    assert_eq!(
        output(&project(Some(TrackInput::Mono(0)), false, false)),
        (0.0, 0.0)
    );
    let (l, _) = output(&project(Some(TrackInput::Mono(0)), true, true));
    assert!(l.abs() < 1e-6, "mute silences the input (MON-003)");
    // A channel the device does not have: silence, the rest works.
    assert_eq!(
        output(&project(Some(TrackInput::Mono(7)), true, false)),
        (0.0, 0.0)
    );
}

#[test]
fn offline_renders_never_hear_the_input() {
    let p = project(Some(TrackInput::Mono(0)), true, false);
    let out = render_offline(compile_with(&p, SR, PlayMode::Song, |_| None), N, || 1);
    assert!(out.left.iter().all(|v| *v == 0.0), "INP-006");
}
