// SPDX-License-Identifier: GPL-3.0-or-later
//! Realtime side of the engine.
//!
//! `render` obeys RT rules (section 8): no allocation or deallocation, no
//! locks, no I/O, bounded work. All buffers are allocated in `new`.

use crate::{
    Command, EngineFeedback, Garbage, MAX_BLOCK, MAX_COMMANDS_PER_RENDER, MAX_VOICES,
    PlaybackSnapshot,
};
use jdance_domain::ChannelId;
use jdance_dsp::SamplerVoice;
use rtrb::{Consumer, Producer};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
struct Voice {
    inner: SamplerVoice,
    channel: ChannelId,
    velocity: f32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            inner: SamplerVoice::default(),
            channel: ChannelId(0),
            velocity: 0.0,
        }
    }
}

/// Owned by the audio callback (or by an offline renderer).
#[derive(Debug)]
pub struct EngineProcessor {
    sample_rate: u32,
    commands: Consumer<Command>,
    garbage: Producer<Garbage>,
    feedback: Arc<EngineFeedback>,
    snapshot: Option<Box<PlaybackSnapshot>>,
    voices: Box<[Voice]>,
    scratch_l: Box<[f32]>,
    scratch_r: Box<[f32]>,
    playing: bool,
    /// Position inside the loop, in samples.
    pos: u64,
    voice_serial: u64,
}

impl EngineProcessor {
    pub(crate) fn new(
        sample_rate: u32,
        commands: Consumer<Command>,
        garbage: Producer<Garbage>,
        feedback: Arc<EngineFeedback>,
    ) -> Self {
        Self {
            sample_rate,
            commands,
            garbage,
            feedback,
            snapshot: None,
            voices: vec![Voice::default(); MAX_VOICES].into_boxed_slice(),
            scratch_l: vec![0.0; MAX_BLOCK].into_boxed_slice(),
            scratch_r: vec![0.0; MAX_BLOCK].into_boxed_slice(),
            playing: false,
            pos: 0,
            voice_serial: 0,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Renders `left.len()` frames (any length; RT-020) into the outputs,
    /// overwriting them. `left` and `right` must have equal length.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let started = Instant::now();
        let frames = left.len().min(right.len());
        self.drain_commands();

        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_BLOCK);
            self.render_block(n);
            left[done..done + n].copy_from_slice(&self.scratch_l[..n]);
            right[done..done + n].copy_from_slice(&self.scratch_r[..n]);
            done += n;
        }
        // Zero any excess if the slices differ in length.
        left[frames..].fill(0.0);
        right[frames..].fill(0.0);

        let loop_len = self.snapshot.as_ref().map_or(0, |s| s.loop_len);
        self.feedback
            .publish_transport(self.playing, self.pos, loop_len);
        let active = self.voices.iter().filter(|v| v.inner.is_active()).count();
        self.feedback.publish_voices(active as u32);
        if frames > 0 {
            let budget = frames as f64 / f64::from(self.sample_rate);
            let load = started.elapsed().as_secs_f64() / budget;
            self.feedback.note_render(load as f32);
        }
    }

    fn drain_commands(&mut self) {
        for _ in 0..MAX_COMMANDS_PER_RENDER {
            let Ok(next) = self.commands.peek() else {
                break;
            };
            if matches!(next, Command::SetSnapshot(_)) && self.garbage.slots() == 0 {
                // Never drop a snapshot here: wait until the UI side has
                // collected garbage.
                self.feedback.note_deferred_snapshot();
                break;
            }
            let Ok(cmd) = self.commands.pop() else { break };
            match cmd {
                Command::Play => self.playing = true,
                Command::Stop => {
                    self.playing = false;
                    self.pos = 0;
                }
                Command::Panic => {
                    for v in self.voices.iter_mut() {
                        v.inner.stop();
                    }
                }
                Command::SetSnapshot(new) => self.swap_snapshot(new),
            }
        }
    }

    fn swap_snapshot(&mut self, new: Box<PlaybackSnapshot>) {
        // Keep the musical position when the loop length changes (tempo).
        if let Some(old) = &self.snapshot
            && old.loop_len > 0
            && new.loop_len > 0
            && old.loop_len != new.loop_len
        {
            let p = u128::from(self.pos) * u128::from(new.loop_len) / u128::from(old.loop_len);
            self.pos = p as u64;
        }
        if new.loop_len == 0 || self.pos >= new.loop_len {
            self.pos = 0;
        }
        // Re-target voices: channels may have moved, been removed, muted or
        // changed sample.
        for v in self.voices.iter_mut().filter(|v| v.inner.is_active()) {
            let old_sample = self
                .snapshot
                .as_ref()
                .and_then(|s| s.channels.get(v.inner.slot))
                .and_then(|c| c.sample.as_ref());
            let found = new
                .channels
                .iter()
                .enumerate()
                .find(|(_, c)| c.id == v.channel);
            match (found, old_sample) {
                (Some((i, c)), Some(old))
                    if c.sample.as_ref().is_some_and(|s| Arc::ptr_eq(s, old)) =>
                {
                    v.inner.slot = i;
                    v.inner
                        .set_gains(c.gain.0 * v.velocity, c.gain.1 * v.velocity);
                }
                _ => v.inner.stop(),
            }
        }
        if let Some(old) = self.snapshot.replace(new) {
            // Space was checked in `drain_commands`.
            let _ = self.garbage.push(Garbage::Snapshot(old));
        }
    }

    fn render_block(&mut self, n: usize) {
        self.scratch_l[..n].fill(0.0);
        self.scratch_r[..n].fill(0.0);
        let Some(snap) = self.snapshot.as_deref() else {
            return;
        };

        let mut cursor = 0;
        if self.playing && snap.loop_len > 0 {
            let mut done = 0;
            let mut p = self.pos;
            while done < n {
                let seg_end = snap.loop_len.min(p + (n - done) as u64);
                let first = snap.events.partition_point(|e| e.pos < p);
                for e in snap.events[first..].iter().take_while(|e| e.pos < seg_end) {
                    let offset = done + (e.pos - p) as usize;
                    render_voices(
                        snap,
                        &mut self.voices,
                        &mut self.scratch_l[cursor..offset],
                        &mut self.scratch_r[cursor..offset],
                    );
                    cursor = offset;
                    trigger(
                        snap,
                        &mut self.voices,
                        &mut self.voice_serial,
                        &self.feedback,
                        e,
                    );
                }
                done += (seg_end - p) as usize;
                p = if seg_end == snap.loop_len { 0 } else { seg_end };
            }
            self.pos = p;
        }
        render_voices(
            snap,
            &mut self.voices,
            &mut self.scratch_l[cursor..n],
            &mut self.scratch_r[cursor..n],
        );

        let (gl, gr) = snap.master_gain;
        for s in &mut self.scratch_l[..n] {
            *s *= gl;
        }
        for s in &mut self.scratch_r[..n] {
            *s *= gr;
        }
    }
}

fn render_voices(snap: &PlaybackSnapshot, voices: &mut [Voice], l: &mut [f32], r: &mut [f32]) {
    if l.is_empty() {
        return;
    }
    for v in voices.iter_mut().filter(|v| v.inner.is_active()) {
        match snap
            .channels
            .get(v.inner.slot)
            .and_then(|c| c.sample.as_deref())
        {
            Some(sample) => v.inner.render_add(sample, l, r),
            None => v.inner.stop(),
        }
    }
}

fn trigger(
    snap: &PlaybackSnapshot,
    voices: &mut [Voice],
    serial: &mut u64,
    feedback: &EngineFeedback,
    e: &crate::StepEvent,
) {
    let Some(ch) = snap.channels.get(e.channel) else {
        return;
    };
    if ch.sample.is_none() {
        return;
    }
    let index = match voices.iter().position(|v| !v.inner.is_active()) {
        Some(i) => i,
        None => {
            feedback.note_steal();
            voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.inner.started_at)
                .map(|(i, _)| i)
                .unwrap_or(0)
        }
    };
    *serial += 1;
    let v = &mut voices[index];
    v.channel = ch.id;
    v.velocity = e.velocity;
    v.inner.start(
        e.channel,
        *serial,
        ch.gain.0 * e.velocity,
        ch.gain.1 * e.velocity,
    );
}
