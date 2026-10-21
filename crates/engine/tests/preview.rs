// SPDX-License-Identifier: GPL-3.0-or-later
//! Browser preview (spec "DAW UI clean room", §4): plays without a
//! project or transport, is replaced or stopped by the next command, and
//! its buffers are released off the audio thread.

use jdance_dsp::SampleBuffer;
use jdance_engine::create_engine;
use jdance_rt_check::{GuardedAllocator, reset_violations, rt_section, violations};
use std::sync::Arc;

#[global_allocator]
static ALLOC: GuardedAllocator = GuardedAllocator;

#[test]
fn preview_plays_without_transport_and_is_released_off_thread() {
    let (mut h, mut e) = create_engine(48_000);
    let s = Arc::new(SampleBuffer::from_interleaved(48_000, 1, &[1.0; 300]));
    h.preview(Some(Arc::clone(&s))).unwrap();
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    reset_violations();
    rt_section(|| e.render(&mut l, &mut r));
    assert_eq!(violations(), 0);
    assert_eq!(l[0], 0.5, "−6 dB preview level");
    assert!(!h.feedback().read().playing, "the transport stays stopped");
    rt_section(|| e.render(&mut l, &mut r));
    assert_eq!(l[43], 0.5, "300 frames: 44 left in the second block");
    assert_eq!(l[44], 0.0, "plays once");
    // Stopping hands the buffer back; the handle drops it.
    h.preview(None).unwrap();
    rt_section(|| e.render(&mut l, &mut r));
    assert!(l.iter().all(|v| *v == 0.0));
    assert_eq!(Arc::strong_count(&s), 2, "still queued as garbage");
    h.collect_garbage();
    assert_eq!(Arc::strong_count(&s), 1);
}
