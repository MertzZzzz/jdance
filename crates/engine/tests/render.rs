// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline render behavior: timing, determinism across block sizes (RT-022)
//! and golden output (CI-003).

use jdance_engine::{MAX_BLOCK, PlaybackSnapshot, compile, render_offline};
use jdance_test_support::{check_golden, click, diff, step_project};
use proptest::prelude::*;
use std::sync::Arc;

const SR: u32 = 48_000;

fn snapshot(tempo: f64, patterns: &[&[u32]]) -> PlaybackSnapshot {
    snapshot_at(SR, tempo, patterns)
}

fn snapshot_at(sr: u32, tempo: f64, patterns: &[&[u32]]) -> PlaybackSnapshot {
    let project = step_project(tempo, patterns);
    let sample = Arc::new(click(sr, (sr as usize * 3) / 16));
    compile(&project, sr, |_| Some(Arc::clone(&sample)))
}

fn four_on_floor() -> PlaybackSnapshot {
    snapshot(120.0, &[&[0, 4, 8, 12], &[2, 6, 10, 14], &[4, 12, 15]])
}

#[test]
fn first_trigger_is_sample_accurate() {
    // 120 BPM, 48 kHz: step 4 starts at sample 24 000.
    let snap = snapshot(120.0, &[&[4]]);
    let out = render_offline(snap, 30_000, || 333);
    let first = out.left.iter().position(|s| *s != 0.0).unwrap();
    assert_eq!(first, 24_000);
}

#[test]
fn loop_wraps_and_retriggers() {
    let snap = snapshot(120.0, &[&[0]]);
    assert_eq!(snap.loop_len, 96_000);
    let out = render_offline(snap, 200_000, || 512);
    // The click is 9000 samples; silence before the loop restarts.
    assert!(out.left[90_000..96_000].iter().all(|s| *s == 0.0));
    assert_ne!(out.left[96_000], 0.0);
    assert_eq!(out.left[96_000], out.left[0]);
}

#[test]
fn callbacks_larger_than_max_block_are_split() {
    let a = render_offline(four_on_floor(), 50_000, || 64);
    let b = render_offline(four_on_floor(), 50_000, || MAX_BLOCK * 3 + 17);
    assert_eq!(a, b);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// RT-022: random callback sizes in [1, 2048] give bit-identical output.
    #[test]
    fn output_independent_of_block_sizes(sizes in prop::collection::vec(1usize..=2048, 1..64)) {
        let reference = render_offline(four_on_floor(), 120_000, || 64);
        let mut it = sizes.iter().cycle();
        let out = render_offline(four_on_floor(), 120_000, || *it.next().unwrap());
        prop_assert_eq!(diff(&reference.left, &out.left).max_abs, 0.0);
        prop_assert_eq!(diff(&reference.right, &out.right).max_abs, 0.0);
    }
}

#[test]
fn golden_four_on_floor() {
    // Low rate keeps the golden file small; one loop plus the wrap.
    let sr = 22_050;
    let snap = snapshot_at(sr, 120.0, &[&[0, 4, 8, 12], &[2, 6, 10, 14], &[4, 12, 15]]);
    let frames = snap.loop_len as usize + 4_000;
    let out = render_offline(snap, frames, || 256);
    // Tolerance covers libm differences in pan_gains (cos/sin) between
    // platforms; everything else is +/* only.
    check_golden("four_on_floor_120bpm_22k", sr, &out.left, &out.right, 1e-6);
}
