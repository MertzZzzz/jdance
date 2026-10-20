// SPDX-License-Identifier: GPL-3.0-or-later
use jdance_rt_check::{GuardedAllocator, outside_rt, reset_violations, rt_section, violations};

#[global_allocator]
static ALLOC: GuardedAllocator = GuardedAllocator;

// One test function: the counter is process-wide and tests run in parallel.
#[test]
fn guard_counts_only_inside_rt_sections() {
    reset_violations();
    let v: Vec<u8> = Vec::with_capacity(16);
    assert_eq!(violations(), 0);

    let mut buf = [0.0f32; 64];
    rt_section(|| {
        for (i, s) in buf.iter_mut().enumerate() {
            *s = i as f32;
        }
    });
    assert_eq!(violations(), 0);

    rt_section(|| {
        let leaked = vec![1u8; 8];
        std::hint::black_box(&leaked);
    });
    // One allocation and one deallocation.
    assert_eq!(violations(), 2);

    reset_violations();
    rt_section(|| outside_rt(|| std::hint::black_box(vec![0u8; 4])));
    assert_eq!(violations(), 0);
    drop(v);
}
