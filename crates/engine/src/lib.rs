// SPDX-License-Identifier: GPL-3.0-or-later
//! Playback engine.
//!
//! Threading model (ARCH-003, ARCH-004):
//! - the non-realtime side owns an [`EngineHandle`], compiles the document
//!   into an immutable [`PlaybackSnapshot`] and sends it, together with
//!   transport commands, through a bounded wait-free SPSC queue;
//! - the realtime side owns the [`EngineProcessor`] and calls
//!   [`EngineProcessor::render`] from the audio callback;
//! - replaced snapshots travel back through a second SPSC queue and are
//!   dropped by [`EngineHandle::collect_garbage`], never on the audio thread;
//! - state for the UI is published through atomics in [`EngineFeedback`].
//!
//! The processor does not depend on any audio device: offline rendering
//! drives the very same code ([`render_offline`]).

mod automation;
mod effect_rt;
mod feedback;
mod metronome;
mod offline;
mod param_diff;
mod processor;
mod snapshot;

pub use effect_rt::{EffectRt, InsertRt, InstrumentRt, PluginRt, RESET_BUDGET};
pub use feedback::{EngineFeedback, FeedbackReading, MeterSlot, Meters};
pub use metronome::DEFAULT_METRONOME_GAIN;
pub use offline::{OfflineRender, render_offline, render_offline_with};
pub use param_diff::{ParamChange, param_only_changes};
pub use processor::EngineProcessor;
pub use snapshot::{
    ChannelRt, MAX_END_TAIL_SECONDS, PlayMode, PlaybackSnapshot, StepEvent, compile, compile_full,
    compile_with, samples_per_tick, tick_to_sample,
};

use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use thiserror::Error;

/// Largest block the processor renders at once; longer callbacks are split
/// (RT-020).
pub const MAX_BLOCK: usize = 1024;
/// Global voice limit; the oldest voice is stolen when exceeded (RT-011).
pub const MAX_VOICES: usize = 128;
/// Capacity of the UI → engine command queue (RT-011).
pub const COMMAND_QUEUE_CAPACITY: usize = 256;
/// Commands drained per `render` call, bounding callback work (RT-011).
pub const MAX_COMMANDS_PER_RENDER: usize = 64;

#[derive(Debug)]
pub(crate) enum Command {
    Play,
    /// Fades the output out, then clears voices and effect state.
    Stop,
    /// Silences all voices immediately.
    Panic,
    /// Jumps to a sample position, which also becomes the position Stop
    /// returns to; sounding voices get a short release.
    Seek(u64),
    /// Keep playing past the arrangement end (recording).
    HoldEnd(bool),
    Metronome {
        on: bool,
        gain: f32,
    },
    /// Clicks this many beats while stopped, then plays (MET-021); 0
    /// plays at once.
    CountIn(u32),
    SetSnapshot(Box<PlaybackSnapshot>),
    /// Changes one effect parameter without recompiling (smoothed).
    SetParam {
        insert: jdance_domain::InsertId,
        index: u16,
        value: f32,
    },
    /// Plays a sample once on the preview bus (after the master); `None`
    /// stops. Not part of the project or the transport.
    Preview(Option<Arc<jdance_dsp::SampleBuffer>>),
    /// A held note on a channel outside the sequencer (keyboard, MIDI
    /// input); sounds while stopped too.
    LiveNote {
        channel: jdance_domain::ChannelId,
        key: u8,
        velocity: f32,
    },
    LiveNoteOff {
        channel: jdance_domain::ChannelId,
        key: u8,
    },
}

/// Objects handed back to the non-realtime side for destruction.
#[derive(Debug)]
pub(crate) enum Garbage {
    Snapshot(#[allow(dead_code)] Box<PlaybackSnapshot>),
    Sample(#[allow(dead_code)] Arc<jdance_dsp::SampleBuffer>),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EngineError {
    /// The command queue is full; the command was dropped (RT-011).
    #[error("engine command queue is full")]
    QueueFull,
}

/// Non-realtime control endpoint of the engine.
#[derive(Debug)]
pub struct EngineHandle {
    commands: Producer<Command>,
    garbage: Consumer<Garbage>,
    feedback: Arc<EngineFeedback>,
    sample_rate: u32,
}

/// Creates a connected handle/processor pair for `sample_rate`.
pub fn create_engine(sample_rate: u32) -> (EngineHandle, EngineProcessor) {
    let (cmd_tx, cmd_rx) = RingBuffer::new(COMMAND_QUEUE_CAPACITY);
    // Every command yields at most one garbage item, so a garbage queue at
    // least as large as the command queue never blocks the processor for
    // long; the processor still checks for space before taking a snapshot.
    let (gc_tx, gc_rx) = RingBuffer::new(COMMAND_QUEUE_CAPACITY);
    let feedback = Arc::new(EngineFeedback::default());
    let processor = EngineProcessor::new(sample_rate, cmd_rx, gc_tx, Arc::clone(&feedback));
    let handle = EngineHandle {
        commands: cmd_tx,
        garbage: gc_rx,
        feedback,
        sample_rate,
    };
    (handle, processor)
}

impl EngineHandle {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn send(&mut self, cmd: Command) -> Result<(), EngineError> {
        self.collect_garbage();
        self.commands.push(cmd).map_err(|_| {
            self.feedback.note_dropped_command();
            EngineError::QueueFull
        })
    }

    pub fn play(&mut self) -> Result<(), EngineError> {
        self.send(Command::Play)
    }

    pub fn stop(&mut self) -> Result<(), EngineError> {
        self.send(Command::Stop)
    }

    pub fn panic(&mut self) -> Result<(), EngineError> {
        self.send(Command::Panic)
    }

    /// Jumps to `position` (samples); Stop returns here (the Playlist
    /// caret). Notes that started before the new position are not restored
    /// (MVP limitation).
    pub fn seek(&mut self, position: u64) -> Result<(), EngineError> {
        self.send(Command::Seek(position))
    }

    /// While set, the transport does not stop at the arrangement end
    /// (recording, spec 0.2.3 CUR-006).
    pub fn hold_end(&mut self, hold: bool) -> Result<(), EngineError> {
        self.send(Command::HoldEnd(hold))
    }

    /// Metronome on/off and level (linear), MET-002/003.
    pub fn set_metronome(&mut self, on: bool, gain: f32) -> Result<(), EngineError> {
        self.send(Command::Metronome {
            on,
            gain: gain.clamp(0.0, 1.0),
        })
    }

    /// From a stop: clicks `beats` beats, then starts playing on the next
    /// beat (MET-021, MET-023). Stop cancels it.
    pub fn count_in(&mut self, beats: u32) -> Result<(), EngineError> {
        self.send(Command::CountIn(beats))
    }

    /// Changes an effect parameter in place, smoothed by the effect.
    /// Parameters that change latency must go through a recompile.
    pub fn set_param(
        &mut self,
        insert: jdance_domain::InsertId,
        index: u16,
        value: f32,
    ) -> Result<(), EngineError> {
        self.send(Command::SetParam {
            insert,
            index,
            value,
        })
    }

    /// Plays `sample` (at the engine rate) on the preview bus, replacing
    /// any preview; `None` stops the preview.
    pub fn preview(
        &mut self,
        sample: Option<Arc<jdance_dsp::SampleBuffer>>,
    ) -> Result<(), EngineError> {
        debug_assert!(
            sample
                .as_ref()
                .is_none_or(|s| s.sample_rate == self.sample_rate)
        );
        self.send(Command::Preview(sample))
    }

    /// Starts a held note on `channel` (channel pitch applies); one-shot
    /// samples play to their end, gated ones until [`Self::live_note_off`].
    pub fn live_note(
        &mut self,
        channel: jdance_domain::ChannelId,
        key: u8,
        velocity: f32,
    ) -> Result<(), EngineError> {
        self.send(Command::LiveNote {
            channel,
            key,
            velocity: velocity.clamp(0.0, 1.0),
        })
    }

    pub fn live_note_off(
        &mut self,
        channel: jdance_domain::ChannelId,
        key: u8,
    ) -> Result<(), EngineError> {
        self.send(Command::LiveNoteOff { channel, key })
    }

    /// Replaces the playing snapshot. Its sample rate must match the engine.
    pub fn set_snapshot(&mut self, snapshot: PlaybackSnapshot) -> Result<(), EngineError> {
        debug_assert_eq!(snapshot.sample_rate, self.sample_rate);
        self.send(Command::SetSnapshot(Box::new(snapshot)))
    }

    /// Drops objects returned by the processor. Returns how many.
    pub fn collect_garbage(&mut self) -> usize {
        let mut n = 0;
        while let Ok(g) = self.garbage.pop() {
            drop(g);
            n += 1;
        }
        n
    }

    pub fn feedback(&self) -> &EngineFeedback {
        &self.feedback
    }

    pub fn feedback_arc(&self) -> Arc<EngineFeedback> {
        Arc::clone(&self.feedback)
    }
}
