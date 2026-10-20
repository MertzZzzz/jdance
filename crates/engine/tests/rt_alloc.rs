// SPDX-License-Identifier: GPL-3.0-or-later
//! RT-001: the render path performs no heap operations, including command
//! handling and snapshot swaps.

use jdance_engine::{compile, create_engine};
use jdance_rt_check::{GuardedAllocator, reset_violations, rt_section, violations};
use jdance_test_support::{click, step_project};
use std::sync::Arc;

#[global_allocator]
static ALLOC: GuardedAllocator = GuardedAllocator;

#[test]
fn render_does_not_allocate() {
    let sr = 48_000;
    let sample = Arc::new(click(sr, 4_000));
    let all: Vec<u32> = (0..16).collect();
    let busy: Vec<&[u32]> = vec![&all; 12];
    let sparse: Vec<&[u32]> = vec![&[0, 8]; 3];
    let fast = compile(&step_project(140.0, &busy), sr, |_| {
        Some(Arc::clone(&sample))
    });
    let slow = compile(&step_project(90.0, &sparse), sr, |_| {
        Some(Arc::clone(&sample))
    });

    let (mut handle, mut processor) = create_engine(sr);
    let mut l = vec![0.0f32; 4096];
    let mut r = vec![0.0f32; 4096];
    handle.set_snapshot(fast.clone()).unwrap();
    handle.play().unwrap();

    reset_violations();
    for i in 0..400 {
        // Swap snapshots (tempo change) and stop/start while rendering.
        match i % 50 {
            10 => handle.set_snapshot(slow.clone()).unwrap(),
            30 => handle.set_snapshot(fast.clone()).unwrap(),
            40 => handle.stop().unwrap(),
            41 => handle.play().unwrap(),
            45 => handle.panic().unwrap(),
            _ => {}
        }
        let n = [64, 1, 333, 1024, 4096][i % 5];
        rt_section(|| processor.render(&mut l[..n], &mut r[..n]));
        handle.collect_garbage();
    }
    assert_eq!(violations(), 0, "heap operations inside render");
    assert!(handle.feedback().read().renders >= 400);
}
