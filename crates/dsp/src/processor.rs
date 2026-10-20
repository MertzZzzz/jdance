// SPDX-License-Identifier: GPL-3.0-or-later
//! Common lifecycle of built-in effect processors.
//!
//! - `prepare` allocates and is called off the audio thread;
//! - `set_param`, `process_block`, `reset_step` are realtime-safe: no
//!   allocation, no locks, time proportional to the work requested;
//! - parameter indices follow the order of the effect's schema in
//!   `jdance-domain` (each processor exports `PARAM_IDS` for checking).

/// How long an effect keeps producing output after its input stops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tail {
    None,
    /// Estimated time until the output falls below −60 dB.
    Seconds(f32),
}

impl Tail {
    pub fn seconds(self) -> f32 {
        match self {
            Tail::None => 0.0,
            Tail::Seconds(s) => s,
        }
    }
}

/// A parameter change at a frame offset within the block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamEvent {
    pub frame: u32,
    pub index: u16,
    pub value: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessContext<'a> {
    /// External sidechain input for this block, if connected.
    pub sidechain: Option<(&'a [f32], &'a [f32])>,
    pub tempo_bpm: f64,
}

impl ProcessContext<'_> {
    pub fn plain(tempo_bpm: f64) -> Self {
        Self {
            sidechain: None,
            tempo_bpm,
        }
    }
}

pub trait Processor {
    /// Allocates buffers for `sample_rate`; not realtime-safe.
    fn prepare(&mut self, sample_rate: u32, max_block: usize);
    /// Sets a parameter (already validated). Smoothed parameters ramp
    /// from their current value. Realtime-safe.
    fn set_param(&mut self, index: usize, value: f32);
    /// Jumps all smoothed parameters to their targets (after initial
    /// `set_param` calls, before the first block).
    fn settle(&mut self);
    /// Processes one block in place. Realtime-safe.
    fn process_block(&mut self, l: &mut [f32], r: &mut [f32], ctx: &ProcessContext);
    /// Clears internal state, at most `max_samples` buffer samples per
    /// call so large buffers are cleared over several blocks. Returns
    /// `true` once the state is clean.
    fn reset_step(&mut self, max_samples: usize) -> bool;
    fn latency_frames(&self) -> u32;
    fn tail(&self) -> Tail;
}

/// Processes a block, applying `events` (sorted by frame) at their
/// offsets. Realtime-safe.
pub fn process_with_events<P: Processor + ?Sized>(
    p: &mut P,
    l: &mut [f32],
    r: &mut [f32],
    events: &[ParamEvent],
    ctx: &ProcessContext,
) {
    let n = l.len().min(r.len());
    let mut start = 0;
    let mut k = 0;
    while start < n {
        while let Some(e) = events.get(k).filter(|e| e.frame as usize <= start) {
            p.set_param(usize::from(e.index), e.value);
            k += 1;
        }
        let end = events
            .get(k)
            .map_or(n, |e| (e.frame as usize).clamp(start + 1, n));
        let sc = ctx.sidechain.map(|(a, b)| (&a[start..end], &b[start..end]));
        let sub = ProcessContext {
            sidechain: sc,
            tempo_bpm: ctx.tempo_bpm,
        };
        p.process_block(&mut l[start..end], &mut r[start..end], &sub);
        start = end;
    }
}

/// Clears `buf` from `*cursor` by at most `budget` samples; returns the
/// number of samples cleared. Helper for bounded resets.
pub(crate) fn clear_chunk(buf: &mut [f32], cursor: &mut usize, budget: usize) -> usize {
    let end = (*cursor + budget).min(buf.len());
    let n = end - *cursor;
    buf[*cursor..end].fill(0.0);
    *cursor = end;
    n
}

/// One-pole coefficient for a time constant; `0` ms means instant.
pub(crate) fn one_pole(ms: f32, sample_rate: u32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * sample_rate as f32)).exp()
    }
}

/// Ramp length in samples for the schema's 20 ms smoothing.
pub(crate) fn ramp_samples(sample_rate: u32) -> u32 {
    (sample_rate / 50).max(1)
}
