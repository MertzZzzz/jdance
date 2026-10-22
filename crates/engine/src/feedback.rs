// SPDX-License-Identifier: GPL-3.0-or-later
//! Realtime → UI state, published through relaxed atomics.
//!
//! Every field is written by the audio thread with plain atomic stores or
//! `fetch_*` operations: wait-free, allocation-free, O(1).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

/// Peak meter for one bus: written by the audio thread with `fetch_max`
/// on the `f32` bits (monotonic for non-negative values), read and reset
/// by the UI.
#[derive(Debug, Default)]
pub struct MeterSlot {
    /// Track ID the slot currently reports; `u64::MAX` when unused.
    id: AtomicU64,
    peak_l: AtomicU32,
    peak_r: AtomicU32,
}

impl MeterSlot {
    #[inline]
    pub(crate) fn publish(&self, id: u64, peak_l: f32, peak_r: f32) {
        self.id.store(id, Relaxed);
        self.peak_l.fetch_max(peak_l.max(0.0).to_bits(), Relaxed);
        self.peak_r.fetch_max(peak_r.max(0.0).to_bits(), Relaxed);
    }

    /// `(track id, peak left, peak right)` since the last read.
    pub fn take(&self) -> (u64, f32, f32) {
        (
            self.id.load(Relaxed),
            f32::from_bits(self.peak_l.swap(0, Relaxed)),
            f32::from_bits(self.peak_r.swap(0, Relaxed)),
        )
    }
}

/// Bus meters (one per track incl. master, post mute) plus the master
/// output after the limiter and the limiter's gain reduction.
#[derive(Debug)]
pub struct Meters {
    pub buses: Box<[MeterSlot]>,
    pub output: MeterSlot,
    /// Lowest limiter gain since the last read, as `f32` bits (1.0 = none).
    limiter_gain: AtomicU32,
}

impl Default for Meters {
    fn default() -> Self {
        Self {
            buses: (0..jdance_domain::MAX_TRACKS + 1)
                .map(|_| MeterSlot {
                    id: AtomicU64::new(u64::MAX),
                    ..Default::default()
                })
                .collect(),
            output: MeterSlot::default(),
            limiter_gain: AtomicU32::new(1.0f32.to_bits()),
        }
    }
}

impl Meters {
    #[inline]
    pub(crate) fn publish_limiter(&self, min_gain: f32) {
        // Lower gain = more reduction: keep the minimum.
        self.limiter_gain
            .fetch_min(min_gain.clamp(0.0, 1.0).to_bits(), Relaxed);
    }

    /// Lowest limiter gain since the last read (1.0 = no reduction).
    pub fn take_limiter_gain(&self) -> f32 {
        f32::from_bits(self.limiter_gain.swap(1.0f32.to_bits(), Relaxed))
    }
}

#[derive(Debug, Default)]
pub struct EngineFeedback {
    pub meters: Meters,
    playing: AtomicBool,
    position: AtomicU64,
    /// Count-in beats still to come (MET-022).
    count_in: AtomicU32,
    loop_len: AtomicU64,
    active_voices: AtomicU32,
    voice_steals: AtomicU64,
    renders: AtomicU64,
    /// Peak callback load since the last reading, as `f32` bits.
    peak_load: AtomicU32,
    /// Blocks that took longer to render than their audio duration.
    overloads: AtomicU64,
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
    /// Count-in beats still to come; 0 = none.
    pub count_in: u32,
    pub active_voices: u32,
    pub voice_steals: u64,
    pub renders: u64,
    /// Peak render time / audio duration of the block since last reading.
    pub peak_load: f32,
    /// Blocks rendered slower than real time since the engine started
    /// (each one likely caused a dropout).
    pub overloads: u64,
    pub dropped_commands: u64,
    pub deferred_snapshots: u64,
}

impl EngineFeedback {
    /// The transport is rolling (not stopped, not counting in). Unlike
    /// [`Self::read`] this has no side effects.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Relaxed)
    }

    pub(crate) fn publish_count_in(&self, beats: u32) {
        self.count_in.store(beats, Relaxed);
    }

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
        if load > 1.0 {
            self.overloads.fetch_add(1, Relaxed);
        }
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
            count_in: self.count_in.load(Relaxed),
            active_voices: self.active_voices.load(Relaxed),
            voice_steals: self.voice_steals.load(Relaxed),
            renders: self.renders.load(Relaxed),
            peak_load: f32::from_bits(self.peak_load.swap(0, Relaxed)),
            overloads: self.overloads.load(Relaxed),
            dropped_commands: self.dropped_commands.load(Relaxed),
            deferred_snapshots: self.deferred_snapshots.load(Relaxed),
        }
    }
}
