// SPDX-License-Identifier: GPL-3.0-or-later
//! Realtime side of the engine.
//!
//! `render` obeys RT rules (section 8): no allocation or deallocation, no
//! locks, no I/O, bounded work. All buffers are allocated in `new`.
//!
//! Signal flow per block (MIX-010):
//! voices → channel buffers → channel gain → PDC delay → track inputs;
//! then each track in processing order: inserts → pre-fader tap → fader
//! → post-fader tap → sends / mute → main out.

use crate::snapshot::{EffectRt, TrackRt};
use crate::{
    Command, EngineFeedback, Garbage, MAX_BLOCK, MAX_COMMANDS_PER_RENDER, MAX_VOICES,
    PlaybackSnapshot,
};
use jdance_domain::{ChannelId, MAX_CHANNELS, MAX_TRACKS, Tap};
use jdance_dsp::SamplerVoice;
use rtrb::{Consumer, Producer};
use std::sync::Arc;
use std::time::Instant;

/// Tracks including the master.
const MAX_BUSES: usize = MAX_TRACKS + 1;

#[derive(Debug, Clone, Copy)]
struct Voice {
    inner: SamplerVoice,
    channel: ChannelId,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            inner: SamplerVoice::default(),
            channel: ChannelId(0),
        }
    }
}

/// `count` stereo buffers of `MAX_BLOCK` frames in two flat allocations.
#[derive(Debug)]
struct Buses {
    l: Box<[f32]>,
    r: Box<[f32]>,
}

impl Buses {
    fn new(count: usize) -> Self {
        Self {
            l: vec![0.0; count * MAX_BLOCK].into_boxed_slice(),
            r: vec![0.0; count * MAX_BLOCK].into_boxed_slice(),
        }
    }

    #[inline]
    fn get(&self, i: usize, n: usize) -> (&[f32], &[f32]) {
        let s = i * MAX_BLOCK;
        (&self.l[s..s + n], &self.r[s..s + n])
    }

    #[inline]
    fn get_mut(&mut self, i: usize, n: usize) -> (&mut [f32], &mut [f32]) {
        let s = i * MAX_BLOCK;
        (&mut self.l[s..s + n], &mut self.r[s..s + n])
    }

    fn zero(&mut self, count: usize, n: usize) {
        for i in 0..count {
            let (l, r) = self.get_mut(i, n);
            l.fill(0.0);
            r.fill(0.0);
        }
    }
}

#[inline]
fn add_into(dst: (&mut [f32], &mut [f32]), src: (&[f32], &[f32])) {
    for (d, s) in dst.0.iter_mut().zip(src.0) {
        *d += *s;
    }
    for (d, s) in dst.1.iter_mut().zip(src.1) {
        *d += *s;
    }
}

#[inline]
fn copy_into(dst: (&mut [f32], &mut [f32]), src: (&[f32], &[f32])) {
    dst.0.copy_from_slice(src.0);
    dst.1.copy_from_slice(src.1);
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
    channels: Buses,
    inputs: Buses,
    pre_taps: Buses,
    post_taps: Buses,
    /// [0] send/main-out scratch, [1] sidechain, [2] block output.
    scratch: Buses,
    mute_env: Box<[f32]>,
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
            channels: Buses::new(MAX_CHANNELS),
            inputs: Buses::new(MAX_BUSES),
            pre_taps: Buses::new(MAX_BUSES),
            post_taps: Buses::new(MAX_BUSES),
            scratch: Buses::new(3),
            mute_env: vec![0.0; MAX_BLOCK].into_boxed_slice(),
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
            let (ol, or) = self.scratch.get(2, n);
            left[done..done + n].copy_from_slice(ol);
            right[done..done + n].copy_from_slice(or);
            done += n;
        }
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

    fn swap_snapshot(&mut self, mut new: Box<PlaybackSnapshot>) {
        if let Some(old) = &mut self.snapshot {
            // Keep the musical position when the loop length changes.
            if old.loop_len > 0 && new.loop_len > 0 && old.loop_len != new.loop_len {
                let p = u128::from(self.pos) * u128::from(new.loop_len) / u128::from(old.loop_len);
                self.pos = p as u64;
            }
            new.take_state_from(old);
        }
        if new.loop_len == 0 || self.pos >= new.loop_len {
            self.pos = 0;
        }
        // Re-target voices: channels may have moved, been removed or
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
        let Self {
            snapshot,
            voices,
            channels,
            inputs,
            pre_taps,
            post_taps,
            scratch,
            mute_env,
            playing,
            pos,
            voice_serial,
            feedback,
            ..
        } = self;
        {
            let (out_l, out_r) = scratch.get_mut(2, n);
            out_l.fill(0.0);
            out_r.fill(0.0);
        }
        let Some(snap) = snapshot.as_deref_mut() else {
            return;
        };
        let n_ch = snap.channels.len();
        let n_tr = snap.tracks.len();
        channels.zero(n_ch, n);
        inputs.zero(n_tr, n);

        // 1. Sequencer events and voices into per-channel buffers.
        let mut cursor = 0;
        if *playing && snap.loop_len > 0 {
            let mut done = 0;
            let mut p = *pos;
            while done < n {
                let seg_end = snap.loop_len.min(p + (n - done) as u64);
                let first = snap.events.partition_point(|e| e.pos < p);
                let mut k = first;
                while let Some(e) = snap.events.get(k).copied().filter(|e| e.pos < seg_end) {
                    let offset = done + (e.pos - p) as usize;
                    render_voices(snap, voices, channels, cursor, offset);
                    cursor = offset;
                    trigger(snap, voices, voice_serial, feedback, &e);
                    k += 1;
                }
                done += (seg_end - p) as usize;
                p = if seg_end == snap.loop_len { 0 } else { seg_end };
            }
            *pos = p;
        }
        render_voices(snap, voices, channels, cursor, n);

        // 2. Channel gain and compensation, summed into track inputs.
        for (ci, ch) in snap.channels.iter_mut().enumerate() {
            {
                let (l, r) = channels.get_mut(ci, n);
                ch.gain.apply(l, r);
                ch.delay.process(l, r);
            }
            add_into(inputs.get_mut(ch.track, n), channels.get(ci, n));
        }

        // 3. Tracks in processing order; destinations always come later.
        for (ti, track) in snap.tracks.iter_mut().enumerate() {
            process_inserts(track, ti, inputs, pre_taps, post_taps, scratch, n);
            copy_into(pre_taps.get_mut(ti, n), inputs.get(ti, n));
            {
                let (l, r) = inputs.get_mut(ti, n);
                track.fader.apply(l, r);
            }
            copy_into(post_taps.get_mut(ti, n), inputs.get(ti, n));
            track.mute.fill(&mut mute_env[..n]);

            for send in &mut track.sends {
                let (src_l, src_r) = if send.pre_fader {
                    pre_taps.get(ti, n)
                } else {
                    post_taps.get(ti, n)
                };
                {
                    let (tl, tr) = scratch.get_mut(0, n);
                    for i in 0..n {
                        let g = send.gain.next_value();
                        let m = if send.pre_fader { 1.0 } else { mute_env[i] };
                        tl[i] = src_l[i] * g * m;
                        tr[i] = src_r[i] * g * m;
                    }
                    send.delay.process(tl, tr);
                }
                add_into(inputs.get_mut(send.dest, n), scratch.get(0, n));
            }

            let (src_l, src_r) = post_taps.get(ti, n);
            match &mut track.main_out {
                Some((dest, delay)) => {
                    {
                        let (tl, tr) = scratch.get_mut(0, n);
                        for i in 0..n {
                            tl[i] = src_l[i] * mute_env[i];
                            tr[i] = src_r[i] * mute_env[i];
                        }
                        delay.process(tl, tr);
                    }
                    add_into(inputs.get_mut(*dest, n), scratch.get(0, n));
                }
                None => {
                    // The master writes the block output.
                    let (ol, or) = scratch.get_mut(2, n);
                    for i in 0..n {
                        ol[i] = src_l[i] * mute_env[i];
                        or[i] = src_r[i] * mute_env[i];
                    }
                }
            }
        }
    }
}

/// Runs the insert chain of track `ti` in place on its input buffer.
fn process_inserts(
    track: &mut TrackRt,
    ti: usize,
    inputs: &mut Buses,
    pre_taps: &Buses,
    post_taps: &Buses,
    scratch: &mut Buses,
    n: usize,
) {
    for ins in &mut track.inserts {
        let has_sidechain = match &mut ins.sidechain {
            Some(sc) => {
                let (src_l, src_r) = match sc.tap {
                    Tap::PreFader => pre_taps.get(sc.source, n),
                    Tap::PostFader => post_taps.get(sc.source, n),
                };
                let (sl, sr) = scratch.get_mut(1, n);
                sl.copy_from_slice(src_l);
                sr.copy_from_slice(src_r);
                sc.delay.process(sl, sr);
                true
            }
            None => false,
        };
        let (l, r) = inputs.get_mut(ti, n);
        ins.pre_delay.process(l, r);
        let sidechain = has_sidechain.then(|| scratch.get(1, n));
        match &mut ins.effect {
            EffectRt::Compressor(c) => c.process(l, r, sidechain, ins.bypass),
        }
    }
}

fn render_voices(
    snap: &PlaybackSnapshot,
    voices: &mut [Voice],
    channels: &mut Buses,
    from: usize,
    to: usize,
) {
    if from >= to {
        return;
    }
    for v in voices.iter_mut().filter(|v| v.inner.is_active()) {
        let slot = v.inner.slot;
        match snap.channels.get(slot).and_then(|c| c.sample.as_deref()) {
            Some(sample) => {
                let (l, r) = channels.get_mut(slot, to);
                v.inner.render_add(sample, &mut l[from..], &mut r[from..]);
            }
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
    v.inner.start(e.channel, *serial, e.velocity, e.velocity);
}
