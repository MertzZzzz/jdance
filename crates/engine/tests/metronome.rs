// SPDX-License-Identifier: GPL-3.0-or-later
//! Metronome and count-in (spec 0.2.3 MET-001, MET-011, MET-012, MET-021,
//! MET-023).

use jdance_domain::{Project, TimeSignature};
use jdance_engine::{PlayMode, compile_with, create_engine, render_offline};

const SR: u32 = 48_000;

/// 120 BPM: a quarter note is 24 000 samples.
fn project(sig: TimeSignature) -> Project {
    let mut p = Project::new();
    p.tempo_bpm = 120.0;
    p.time_signature = sig;
    p
}

fn snapshot(p: &Project, mode: PlayMode) -> jdance_engine::PlaybackSnapshot {
    compile_with(p, SR, mode, |_| None)
}

/// Click onsets: the first non-silent sample after a gap, minus one (a
/// click starts at sin(0) = 0).
fn onsets(l: &[f32]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut quiet = 1_000;
    for (i, v) in l.iter().enumerate() {
        if v.abs() > 1e-6 {
            if quiet > 500 {
                out.push(i - 1);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    out
}

fn render(p: &mut jdance_engine::EngineProcessor, frames: usize, block: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    let mut l = vec![0.0; block];
    let mut r = vec![0.0; block];
    while out.len() < frames {
        p.render(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out.truncate(frames);
    out
}

#[test]
fn clicks_fall_on_beats_of_the_signature_in_song_and_pattern_mode() {
    for (sig, beat) in [
        (TimeSignature::COMMON, 24_000),
        (TimeSignature::new(3, 4), 24_000),
        (TimeSignature::new(7, 8), 12_000),
    ] {
        let p = project(sig);
        for mode in [PlayMode::Song, PlayMode::Pattern(p.patterns[0].id)] {
            let (mut h, mut proc_) = create_engine(SR);
            h.set_snapshot(snapshot(&p, mode)).unwrap();
            h.set_metronome(true, 0.5).unwrap();
            h.play().unwrap();
            // Odd block size: clicks must not depend on block edges.
            let l = render(&mut proc_, 8 * beat + 10, 333);
            let want: Vec<usize> = (0..=8).map(|k| k * beat).collect();
            let got = onsets(&l);
            // Pattern mode wraps at the pattern end; beats keep counting
            // from its start.
            if mode == PlayMode::Song {
                assert_eq!(got, want, "{sig}");
            } else {
                assert!(got.iter().all(|o| o % beat == 0), "{sig}: {got:?}");
            }
            let peak = |at: usize| l[at..at + 500].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let n = usize::from(sig.numerator);
            assert!(peak(0) > peak(beat) * 1.3, "{sig}: accent on the bar");
            if mode == PlayMode::Song {
                assert!((peak(n * beat) - peak(0)).abs() < 1e-3, "{sig}: next bar");
            }
        }
    }
}

#[test]
fn metronome_is_off_by_default_and_never_in_offline_renders() {
    let p = project(TimeSignature::COMMON);
    let out = render_offline(snapshot(&p, PlayMode::Song), 100_000, || 1);
    assert!(out.left.iter().all(|v| *v == 0.0));
    let (mut h, mut proc_) = create_engine(SR);
    h.set_snapshot(snapshot(&p, PlayMode::Song)).unwrap();
    h.play().unwrap();
    assert!(render(&mut proc_, 50_000, 512).iter().all(|v| *v == 0.0));
}

#[test]
fn count_in_clicks_then_starts_the_transport_on_the_beat() {
    let p = project(TimeSignature::new(3, 4));
    let (mut h, mut proc_) = create_engine(SR);
    h.set_snapshot(snapshot(&p, PlayMode::Song)).unwrap();
    h.seek(1_000).unwrap();
    // The metronome is off: the count-in clicks anyway (MET-021).
    h.count_in(12).unwrap();
    let beat = 24_000;
    let l = render(&mut proc_, 12 * beat - 480, 480);
    let f = h.feedback().read();
    assert!(!f.playing, "still counting");
    assert_eq!(f.position, 1_000, "the caret holds during the count-in");
    assert_eq!(f.count_in, 0, "the last beat has clicked");
    assert_eq!(onsets(&l), (0..12).map(|k| k * beat).collect::<Vec<_>>());
    // The next 480 samples end the count-in exactly, the 480 after play.
    render(&mut proc_, 960, 960);
    let f = h.feedback().read();
    assert!(f.playing);
    assert_eq!(f.position, 1_000 + 480);
}

#[test]
fn stop_cancels_the_count_in() {
    let p = project(TimeSignature::COMMON);
    let (mut h, mut proc_) = create_engine(SR);
    h.set_snapshot(snapshot(&p, PlayMode::Song)).unwrap();
    h.count_in(4).unwrap();
    render(&mut proc_, 30_000, 512);
    assert!(h.feedback().read().count_in > 0);
    h.stop().unwrap();
    render(&mut proc_, 200_000, 512);
    let f = h.feedback().read();
    assert!(!f.playing);
    assert_eq!(f.count_in, 0);
}
