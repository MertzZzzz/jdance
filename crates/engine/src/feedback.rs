// SPDX-License-Identifier: GPL-3.0-or-later
//! Realtime → UI state, published through relaxed atomics.
//!
//! Every field is written by the audio thread with plain atomic stores or
//! `fetch_*` operations: wait-free, allocation-free, O(1).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

#[derive(Debug, Default)]
pub struct EngineFeedback {
    playing: AtomicBool,
    position: AtomicU64,
    loop_len: AtomicU64,
    active_voices: AtomicU32,
    voice_steals: AtomicU64,
    renders: AtomicU64,
    /// Peak callback load since the last reading, as `f32` bits.
    peak_load: AtomicU32,
    dropped_commands: AtomicU64,
    deferred_snapshots: AtomicU64,
}

/// Plain copy of [`EngineFeedback`] for display.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FeedbackReading {
    pub playing: bool,
    /// Playhead position inside the pattern loop, in samples.
    pub position: u64,
    pub loop_len: u64,
    pub active_voices: u32,
    pub voice_steals: u64,
    pub renders: u64,
    /// Peak render time / audio duration of the block since last reading.
    pub peak_load: f32,
    pub dropped_commands: u64,
    pub deferred_snapshots: u64,
}

impl EngineFeedback {
    pub(crate) fn publish_transport(&self, playing: bool, position: u64, loop_len: u64) {
        self.playing.store(playing, Relaxed);
        self.position.store(position, Relaxed);
        self.loop_len.store(loop_len, Relaxed);
    }

    pub(crate) fn publish_voices(&self, active: u32) {
        self.active_voices.store(active, Relaxed);
    }

    pub(crate) fn note_steal(&self) {
        self.voice_steals.fetch_add(1, Relaxed);
    }

    pub(crate) fn note_render(&self, load: f32) {
        self.renders.fetch_add(1, Relaxed);
        let mut cur = self.peak_load.load(Relaxed);
        while f32::from_bits(cur) < load {
            match self
                .peak_load
                .compare_exchange_weak(cur, load.to_bits(), Relaxed, Relaxed)
            {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    pub(crate) fn note_dropped_command(&self) {
        self.dropped_commands.fetch_add(1, Relaxed);
    }

    pub(crate) fn note_deferred_snapshot(&self) {
        self.deferred_snapshots.fetch_add(1, Relaxed);
    }

    /// Reads all values; resets the peak load.
    pub fn read(&self) -> FeedbackReading {
        FeedbackReading {
            playing: self.playing.load(Relaxed),
            position: self.position.load(Relaxed),
            loop_len: self.loop_len.load(Relaxed),
            active_voices: self.active_voices.load(Relaxed),
            voice_steals: self.voice_steals.load(Relaxed),
            renders: self.renders.load(Relaxed),
            peak_load: f32::from_bits(self.peak_load.swap(0, Relaxed)),
            dropped_commands: self.dropped_commands.load(Relaxed),
            deferred_snapshots: self.deferred_snapshots.load(Relaxed),
        }
    }
}
