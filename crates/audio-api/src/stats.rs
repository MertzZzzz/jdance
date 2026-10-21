// SPDX-License-Identifier: GPL-3.0-or-later
//! Stream diagnostics shared between the device callback and the UI.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

/// Written by the audio callback with relaxed atomics (wait-free, O(1)).
/// `error` is written only from the backend's error callback, which is not
/// the realtime render thread.
#[derive(Debug)]
pub struct StreamStats {
    callbacks: AtomicU64,
    min_frames: AtomicU32,
    max_frames: AtomicU32,
    last_frames: AtomicU32,
    /// Callbacks whose length exceeded the adapter's preallocated buffers.
    oversized: AtomicU64,
    panicked: AtomicBool,
    error: Mutex<Option<String>>,
}

impl Default for StreamStats {
    fn default() -> Self {
        Self {
            callbacks: AtomicU64::new(0),
            min_frames: AtomicU32::new(u32::MAX),
            max_frames: AtomicU32::new(0),
            last_frames: AtomicU32::new(0),
            oversized: AtomicU64::new(0),
            panicked: AtomicBool::new(false),
            error: Mutex::new(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatsReading {
    pub callbacks: u64,
    /// Observed callback sizes; `None` before the first callback.
    pub min_frames: Option<u32>,
    pub max_frames: Option<u32>,
    pub last_frames: Option<u32>,
    pub oversized: u64,
    pub panicked: bool,
    pub error: Option<String>,
}

impl StreamStats {
    /// Called once per device callback. Realtime-safe.
    #[inline]
    pub fn note_callback(&self, frames: u32) {
        self.callbacks.fetch_add(1, Relaxed);
        self.min_frames.fetch_min(frames, Relaxed);
        self.max_frames.fetch_max(frames, Relaxed);
        self.last_frames.store(frames, Relaxed);
    }

    #[inline]
    pub fn note_oversized(&self) {
        self.oversized.fetch_add(1, Relaxed);
    }

    /// The render callback panicked; the adapter outputs silence from now
    /// on (RT-031). Realtime-safe.
    #[inline]
    pub fn note_panic(&self) {
        self.panicked.store(true, Relaxed);
    }

    pub fn is_panicked(&self) -> bool {
        self.panicked.load(Relaxed)
    }

    /// Not realtime-safe: call only from non-render threads.
    pub fn set_error(&self, message: String) {
        if let Ok(mut e) = self.error.lock() {
            e.get_or_insert(message);
        }
    }

    pub fn read(&self) -> StatsReading {
        let callbacks = self.callbacks.load(Relaxed);
        let seen = callbacks > 0;
        StatsReading {
            callbacks,
            min_frames: seen.then(|| self.min_frames.load(Relaxed)),
            max_frames: seen.then(|| self.max_frames.load(Relaxed)),
            last_frames: seen.then(|| self.last_frames.load(Relaxed)),
            oversized: self.oversized.load(Relaxed),
            panicked: self.panicked.load(Relaxed),
            error: self.error.lock().ok().and_then(|e| e.clone()),
        }
    }
}
