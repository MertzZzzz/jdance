// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline rendering through the realtime code path, without any device.

use crate::{PlaybackSnapshot, create_engine};

/// Stereo result of an offline render.
#[derive(Debug, Clone, PartialEq)]
pub struct OfflineRender {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// Plays `snapshot` from the start for `frames` frames. `next_block`
/// supplies callback sizes, which lets tests emulate irregular devices; any
/// value is accepted (0 is treated as 1).
pub fn render_offline(
    snapshot: PlaybackSnapshot,
    frames: usize,
    mut next_block: impl FnMut() -> usize,
) -> OfflineRender {
    let sample_rate = snapshot.sample_rate;
    let (mut handle, mut processor) = create_engine(sample_rate);
    handle
        .set_snapshot(snapshot)
        .expect("fresh queue has space");
    handle.play().expect("fresh queue has space");

    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    let mut done = 0;
    while done < frames {
        let n = next_block().max(1).min(frames - done);
        processor.render(&mut left[done..done + n], &mut right[done..done + n]);
        done += n;
        handle.collect_garbage();
    }
    OfflineRender {
        sample_rate,
        left,
        right,
    }
}
