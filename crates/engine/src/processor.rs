// SPDX-License-Identifier: GPL-3.0-or-later
//! Realtime side of the engine.
//!
//! `render` obeys RT rules (section 8): no allocation or deallocation, no
//! locks, no I/O, bounded work. All buffers are allocated in `new`.
//!
//! Signal flow per block (spec "Mixer signal flow", MIX-010):
//! voices → channel buffers → channel gain → PDC delay → bus input sum;
//! each bus in processing order: inserts → structural fade → pre-fader
//! tap → gain/balance → post-fader tap → mute/solo → bus meter → main out
//! (and sends); the master additionally: safety limiter → output meter.

use crate::EffectRt;
use crate::automation::{AUTOMATION_STEP, AutomationRt, BlockTimeline, PLUGIN_POINT_STEP};
use crate::metronome::Metronome;
use crate::snapshot::TrackRt;
use crate::{
    Command, EngineFeedback, Garbage, MAX_BLOCK, MAX_COMMANDS_PER_RENDER, MAX_VOICES,
    PlaybackSnapshot,
};
use jdance_domain::{ChannelId, MAX_CHANNELS, MAX_TRACKS, PPQ, Tap};
use jdance_dsp::{ParamEvent, balance_gains};
use jdance_dsp::{SamplerVoice, Smoothed};
use jdance_plugin_api::Transport;
use rtrb::{Consumer, Producer};
use std::sync::Arc;
use std::time::Instant;

/// Tracks including the master.
const MAX_BUSES: usize = MAX_TRACKS + 1;

/// Browser preview level (−6 dB).
const PREVIEW_GAIN: f32 = 0.5;

/// Short release for voices on Stop, seek, mute and loop wrap, ≈ 5 ms.
fn release_len(sample_rate: u32) -> u32 {
    (sample_rate / 200).max(1)
}

#[derive(Debug, Clone)]
struct Voice {
    inner: SamplerVoice,
    channel: ChannelId,
    /// Key of a held live note (released by its note-off).
    live: Option<u8>,
    /// The sample this voice started with, kept when its channel got
    /// another one: the voice plays it to its end (spec 0.2.5 §3, no
    /// click); it goes to the garbage ring when the voice is done.
    held: Option<Arc<jdance_dsp::SampleBuffer>>,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            inner: SamplerVoice::default(),
            channel: ChannelId(0),
            live: None,
            held: None,
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

#[inline]
fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Parameter events per insert per block: 64 parameters at 32-sample
/// resolution.
const MAX_PARAM_EVENTS: usize = (MAX_BLOCK / AUTOMATION_STEP + 1) * 64;

/// Owned by the audio callback (or by an offline renderer).
#[derive(Debug)]
pub struct EngineProcessor {
    sample_rate: u32,
    commands: Consumer<Command>,
    garbage: Producer<Garbage>,
    feedback: Arc<EngineFeedback>,
    snapshot: Option<Box<PlaybackSnapshot>>,
    /// New snapshot waiting for structural fades to finish.
    pending: Option<Box<PlaybackSnapshot>>,
    voices: Box<[Voice]>,
    channels: Buses,
    inputs: Buses,
    pre_taps: Buses,
    post_taps: Buses,
    /// [0] send/main-out scratch, [1] sidechain, [2] block output.
    scratch: Buses,
    dry: Buses,
    env: Box<[f32]>,
    /// Output fade used by Stop.
    out_fade: Smoothed,
    stopping: bool,
    play_after_stop: bool,
    /// Count-in beats to start once a running Stop finishes (Record while
    /// playing restarts from the caret, spec 0.2.3 REC-011).
    count_after_stop: u32,
    playing: bool,
    /// Position inside the loop, in samples.
    pos: u64,
    /// Where Stop and the arrangement end return to: the last seek
    /// target, i.e. the Playlist caret (spec 0.2.3 CUR-004).
    home: u64,
    /// Where the current playback started; playback that starts at or
    /// after the arrangement end does not stop there (CUR-006).
    play_from: u64,
    /// Ignore the arrangement end (recording, CUR-006).
    hold_end: bool,
    voice_serial: u64,
    /// Automation was applied in the previous block.
    automating: bool,
    /// Parameter events for one insert, built per block.
    param_events: Vec<ParamEvent>,
    /// Browser preview: the sample and its voice.
    preview: Option<(Arc<jdance_dsp::SampleBuffer>, SamplerVoice)>,
    /// Metronome and count-in (spec 0.2.3 MET-*).
    metronome: Metronome,
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
            pending: None,
            voices: vec![Voice::default(); MAX_VOICES].into_boxed_slice(),
            channels: Buses::new(MAX_CHANNELS),
            inputs: Buses::new(MAX_BUSES),
            pre_taps: Buses::new(MAX_BUSES),
            post_taps: Buses::new(MAX_BUSES),
            scratch: Buses::new(3),
            dry: Buses::new(1),
            env: vec![0.0; MAX_BLOCK].into_boxed_slice(),
            out_fade: Smoothed::new(1.0, release_len(sample_rate)),
            stopping: false,
            play_after_stop: false,
            count_after_stop: 0,
            playing: false,
            pos: 0,
            home: 0,
            play_from: 0,
            hold_end: false,
            voice_serial: 0,
            automating: false,
            param_events: Vec::with_capacity(MAX_PARAM_EVENTS),
            preview: None,
            metronome: Metronome::new(sample_rate),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Renders `left.len()` frames (any length; RT-020) into the outputs,
    /// overwriting them. `left` and `right` must have equal length.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.render_with_input(left, right, &[], 0);
    }

    /// [`Self::render`] with the live hardware input of the same frames:
    /// `input` holds `channels` interleaved channels per frame (mixer track
    /// inputs, spec 0.2.3 INP-004). Missing frames are silence.
    pub fn render_with_input(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        input: &[f32],
        channels: usize,
    ) {
        let started = Instant::now();
        let frames = left.len().min(right.len());
        self.drain_commands();
        self.release_held_samples();

        let mut done = 0;
        while done < frames {
            self.block_boundary();
            let mut n = (frames - done).min(MAX_BLOCK);
            if self.metronome.counting() && !self.playing {
                // The transport starts exactly where the count-in ends
                // (MET-023).
                match self.beat_samples() {
                    Some(beat) => {
                        let left = self.metronome.count_remaining(beat).max(1);
                        n = n.min(left as usize);
                    }
                    None => self.metronome.cancel_count_in(),
                }
            }
            let live = match channels {
                0 => &[][..],
                ch => input.get(done * ch..(done + n) * ch).unwrap_or(&[]),
            };
            self.render_block(n, live, channels);
            let (ol, or) = self.scratch.get(2, n);
            left[done..done + n].copy_from_slice(ol);
            right[done..done + n].copy_from_slice(or);
            done += n;
        }
        left[frames..].fill(0.0);
        right[frames..].fill(0.0);
        if let Some((sample, voice)) = &mut self.preview {
            voice.render_add(sample, &mut left[..frames], &mut right[..frames]);
        }

        let loop_len = self.snapshot.as_ref().map_or(0, |s| s.loop_len);
        self.feedback
            .publish_transport(self.playing, self.pos, loop_len);
        self.feedback.publish_count_in(self.metronome.count_left());
        let active = self.voices.iter().filter(|v| v.inner.is_active()).count();
        self.feedback.publish_voices(active as u32);
        if frames > 0 {
            let budget = frames as f64 / f64::from(self.sample_rate);
            let load = started.elapsed().as_secs_f64() / budget;
            self.feedback.note_render(load as f32);
        }
    }

    /// Work done between blocks: completing a pending structural swap and
    /// a pending Stop.
    fn block_boundary(&mut self) {
        if self.pending.is_some()
            && self
                .snapshot
                .as_ref()
                .is_none_or(|s| s.structural_fade_done())
            && self.garbage.slots() > 0
        {
            let new = self.pending.take().expect("checked");
            self.swap_snapshot(new);
        }
        if self.stopping && !self.out_fade.is_ramping() {
            self.finish_stop();
        }
    }

    /// Samples per beat of the project signature at the current tempo.
    fn beat_samples(&self) -> Option<f64> {
        let s = self.snapshot.as_deref()?;
        Some(s.samples_per_tick * f64::from(s.time_signature.beat_ticks()))
    }

    fn release_voices(&mut self) {
        let len = release_len(self.sample_rate);
        for v in self.voices.iter_mut() {
            v.inner.release(len);
        }
        self.instruments_off();
    }

    /// Note-off for every sounding instrument note, at the next block
    /// start (PLG-033).
    fn instruments_off(&mut self) {
        if let Some(s) = &mut self.snapshot {
            for ch in &mut s.channels {
                if let Some(inst) = &mut ch.instrument {
                    inst.all_off(0);
                }
            }
        }
    }

    fn finish_stop(&mut self) {
        self.stopping = false;
        for v in self.voices.iter_mut() {
            v.inner.stop();
        }
        self.playing = self.play_after_stop;
        self.play_after_stop = false;
        self.pos = self.home;
        self.play_from = self.home;
        let beats = std::mem::take(&mut self.count_after_stop);
        if beats > 0 {
            self.metronome.start_count_in(beats);
        }
        if let Some(s) = &mut self.snapshot {
            for t in &mut s.tracks {
                for i in &mut t.inserts {
                    i.force_reset = true;
                }
            }
            if let Some(l) = &mut s.limiter {
                l.reset();
            }
        }
        // The output is silent now; fade back in for the next playback.
        self.out_fade.set_target(1.0);
    }

    fn drain_commands(&mut self) {
        for _ in 0..MAX_COMMANDS_PER_RENDER {
            let Ok(next) = self.commands.peek() else {
                break;
            };
            if matches!(next, Command::Preview(_)) && self.garbage.slots() == 0 {
                break;
            }
            if matches!(next, Command::SetSnapshot(_))
                && (self.garbage.slots() == 0 || self.pending.is_some())
            {
                // Never drop a snapshot here, and finish a pending
                // structural swap first.
                self.feedback.note_deferred_snapshot();
                break;
            }
            let Ok(cmd) = self.commands.pop() else { break };
            match cmd {
                Command::Play => {
                    if self.stopping {
                        self.play_after_stop = true;
                    } else {
                        if !self.playing {
                            self.play_from = self.pos;
                        }
                        self.playing = true;
                    }
                }
                Command::HoldEnd(hold) => self.hold_end = hold,
                Command::Metronome { on, gain } => {
                    self.metronome.on = on;
                    self.metronome.gain = gain;
                }
                Command::CountIn(beats) if self.stopping => {
                    if beats == 0 {
                        self.play_after_stop = true;
                    } else {
                        self.count_after_stop = beats;
                    }
                }
                Command::CountIn(beats) => {
                    if !self.playing && self.snapshot.is_some() {
                        if beats == 0 {
                            self.play_from = self.pos;
                            self.playing = true;
                        } else {
                            self.metronome.start_count_in(beats);
                        }
                    }
                }
                Command::Stop => {
                    self.metronome.cancel_count_in();
                    self.count_after_stop = 0;
                    self.stopping = true;
                    self.play_after_stop = false;
                    self.out_fade.set_target(0.0);
                    self.instruments_off();
                }
                Command::Preview(sample) => {
                    let new = sample.map(|s| {
                        let mut v = SamplerVoice::default();
                        v.start(0, 0, PREVIEW_GAIN, PREVIEW_GAIN);
                        (s, v)
                    });
                    if let Some((old, _)) = std::mem::replace(&mut self.preview, new) {
                        // Checked above: there is room.
                        let _ = self.garbage.push(Garbage::Sample(old));
                    }
                }
                Command::LiveNote {
                    channel,
                    key,
                    velocity,
                } => self.live_note(channel, key, Some(velocity)),
                Command::LiveNoteOff { channel, key } => self.live_note(channel, key, None),
                Command::Panic => {
                    for v in self.voices.iter_mut() {
                        v.inner.stop();
                    }
                    self.instruments_off();
                }
                Command::Seek(p) => {
                    // Notes that started before the new position are not
                    // restored (MVP limitation); sounding voices fade.
                    self.release_voices();
                    // Any position, past the arrangement end too (CUR-002).
                    self.pos = match self.snapshot.as_deref() {
                        Some(s) => match s.loop_range {
                            Some((0, len)) if s.end == len => p % len.max(1),
                            _ => p,
                        },
                        None => p,
                    };
                    self.home = self.pos;
                    self.play_from = self.pos;
                }
                Command::SetParam {
                    insert,
                    index,
                    value,
                } => {
                    for s in [&mut self.snapshot, &mut self.pending]
                        .into_iter()
                        .flatten()
                    {
                        if let Some(i) = s
                            .tracks
                            .iter_mut()
                            .flat_map(|t| t.inserts.iter_mut())
                            .find(|i| i.id == insert)
                        {
                            i.effect.processor().set_param(usize::from(index), value);
                        }
                    }
                }
                Command::SetSnapshot(mut new) => {
                    let fade = match &mut self.snapshot {
                        Some(old) => old.begin_structural_fade(&mut new),
                        None => false,
                    };
                    if fade {
                        self.pending = Some(new);
                    } else {
                        self.swap_snapshot(new);
                    }
                }
            }
        }
    }

    /// Note-on (`Some(velocity)`) or note-off of a live note at the next
    /// block start.
    fn live_note(&mut self, channel: ChannelId, key: u8, velocity: Option<f32>) {
        let Some(snap) = self.snapshot.as_deref_mut() else {
            return;
        };
        let Some(index) = snap.channels.iter().position(|c| c.id == channel) else {
            return;
        };
        let ch = &mut snap.channels[index];
        if let Some(inst) = &mut ch.instrument {
            let key = transpose(key, ch.pitch);
            match velocity {
                Some(v) => {
                    inst.make_room(0, ch.mono, ch.polyphony);
                    inst.live_on(key, v);
                }
                None => inst.live_off(key),
            }
            return;
        }
        // One-shot samples play to their end; gated ones stop at note-off.
        let gated = ch.sampler.gated;
        match velocity {
            Some(velocity) => {
                let e = crate::StepEvent {
                    pos: 0,
                    channel: index,
                    velocity,
                    key,
                    length: u64::MAX,
                };
                if let Some(i) = trigger(
                    snap,
                    &mut self.voices,
                    &mut self.voice_serial,
                    &self.feedback,
                    &e,
                ) {
                    self.voices[i].live = Some(key);
                }
            }
            None => {
                for v in self.voices.iter_mut() {
                    if v.live == Some(key) && v.channel == channel && v.inner.is_active() {
                        v.live = None;
                        if gated {
                            v.inner.note_off();
                        }
                    }
                }
            }
        }
    }

    fn swap_snapshot(&mut self, mut new: Box<PlaybackSnapshot>) {
        // The new snapshot's targets start at project values: chase the
        // envelopes again in the next block.
        self.automating = false;
        if let Some(old) = &mut self.snapshot {
            // Keep the musical position (in ticks) when the tempo changes.
            if old.samples_per_tick != new.samples_per_tick {
                let ticks = self.pos as f64 / old.samples_per_tick;
                self.pos = (ticks * new.samples_per_tick).round() as u64;
            }
            new.take_state_from(old);
        }
        match new.loop_range {
            Some((a, b)) if new.end == b && a == 0 => {
                // Pattern mode: stay inside the pattern.
                if self.pos >= b {
                    self.pos %= b.max(1);
                }
            }
            _ => {
                if self.pos > new.end + new.end_tail {
                    self.pos = 0;
                }
            }
        }
        // Re-target voices: channels may have moved, been removed, muted
        // or changed sample.
        let release = release_len(self.sample_rate);
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
                (Some((i, c)), Some(old)) => {
                    // Another sample: this voice keeps playing the one it
                    // started with (an Arc clone, no allocation).
                    if v.held.is_none() && !c.sample.as_ref().is_some_and(|s| Arc::ptr_eq(s, old)) {
                        v.held = Some(Arc::clone(old));
                    }
                    v.inner.slot = i;
                    if c.muted {
                        v.inner.release(release);
                    }
                }
                _ => v.inner.stop(),
            }
        }
        if let Some(old) = self.snapshot.replace(new) {
            // Space was checked by the caller.
            let _ = self.garbage.push(Garbage::Snapshot(old));
        }
    }

    /// Samples kept by voices that have ended go to the garbage ring
    /// (spec 0.2.5 §3); with the ring full they wait for the next block.
    fn release_held_samples(&mut self) {
        for v in self.voices.iter_mut() {
            if !v.inner.is_active()
                && v.held.is_some()
                && self.garbage.slots() > 0
                && let Some(s) = v.held.take()
            {
                let _ = self.garbage.push(Garbage::Sample(s));
            }
        }
    }

    fn render_block(&mut self, n: usize, live: &[f32], live_channels: usize) {
        let release = release_len(self.sample_rate);
        let Self {
            snapshot,
            voices,
            channels,
            inputs,
            pre_taps,
            post_taps,
            scratch,
            dry,
            env,
            out_fade,
            playing,
            pos,
            home,
            play_from,
            hold_end,
            voice_serial,
            feedback,
            stopping,
            automating,
            param_events,
            metronome,
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
        // Live inputs of monitored tracks, before the inserts (INP-004).
        if !live.is_empty() {
            for (ti, t) in snap.tracks.iter().enumerate() {
                let Some((first, stereo)) = t.live_input else {
                    continue;
                };
                if first >= live_channels {
                    continue;
                }
                let second = if stereo && first + 1 < live_channels {
                    first + 1
                } else {
                    first
                };
                let (l, r) = inputs.get_mut(ti, n);
                for (i, frame) in live.chunks_exact(live_channels).take(n).enumerate() {
                    l[i] += frame[first];
                    r[i] += frame[second];
                }
            }
        }
        // Transport for plugins at the block start (PLG-034). Wraps inside
        // the block are not reported; the next block starts right.
        let quarters = |samples: u64| samples as f64 / snap.samples_per_tick / f64::from(PPQ);
        let transport = Transport {
            playing: *playing && !*stopping,
            tempo_bpm: snap.tempo_bpm,
            time_sig: (
                snap.time_signature.numerator,
                snap.time_signature.denominator,
            ),
            project_time_samples: *pos as i64,
            project_time_quarters: quarters(*pos),
            cycle: snap.loop_range.map(|(a, b)| (quarters(a), quarters(b))),
        };

        // Automation applies in song playback (ADR-014); when it stops,
        // targets return to their project values.
        let auto = snap.song && *playing && !*stopping && !snap.automation.is_empty();
        let restore = *automating && !auto;
        // First automated block: targets jump to the envelope (chase).
        let starting = auto && !*automating;
        *automating = auto;
        let mut timeline = BlockTimeline::new(*pos);

        // 1. Sequencer events and voices into per-channel buffers. No new
        //    events while stopping.
        let mut cursor = 0;
        let mut reached_end = false;
        if *playing && !*stopping {
            let mut done = 0;
            let mut p = *pos;
            while done < n {
                let remaining = (n - done) as u64;
                // Wrap only when approaching the loop end from inside.
                let wrap = snap.loop_range.filter(|(_, b)| p < *b);
                let seg_end = match wrap {
                    Some((_, b)) => b.min(p + remaining),
                    None => p + remaining,
                };
                // Instrument gates ending in this segment.
                for ch in snap.channels.iter_mut() {
                    if let Some(inst) = &mut ch.instrument {
                        inst.gates(p, seg_end, done);
                    }
                }
                let first = snap.events.partition_point(|e| e.pos < p);
                let mut k = first;
                while let Some(e) = snap.events.get(k).copied().filter(|e| e.pos < seg_end) {
                    let offset = done + (e.pos - p) as usize;
                    k += 1;
                    if let Some(c) = snap.channels.get_mut(e.channel)
                        && let Some(inst) = c.instrument.as_mut()
                    {
                        inst.make_room(offset, c.mono, c.polyphony);
                        let key = transpose(e.key, c.pitch);
                        inst.note_on(offset, e.pos, key, e.velocity, e.length);
                        continue;
                    }
                    render_voices(snap, voices, channels, cursor, offset);
                    cursor = offset;
                    trigger(snap, voices, voice_serial, feedback, &e);
                }
                // Audio clips of this segment into their channels; the
                // channel's volume, pan, mute and delay compensation follow.
                let seg = (seg_end - p) as usize;
                for c in &snap.audio_clips {
                    let (l, r) = channels.get_mut(c.channel, done + seg);
                    c.render_add(p, &mut l[done..], &mut r[done..], &snap.automation);
                }
                done += seg;
                match wrap {
                    Some((a, b)) if seg_end == b => {
                        // Loop wrap: instrument voices end with a short
                        // release; effect tails continue.
                        render_voices(snap, voices, channels, cursor, done);
                        cursor = done;
                        for v in voices.iter_mut() {
                            v.inner.release(release);
                        }
                        for ch in snap.channels.iter_mut() {
                            if let Some(inst) = &mut ch.instrument {
                                inst.all_off(done);
                            }
                        }
                        p = a;
                        if done < n {
                            timeline.push(done, a);
                        }
                    }
                    _ => p = seg_end,
                }
            }
            *pos = p;
            // Arrangement end (no loop around it): after the tail, stop. An
            // empty arrangement has no end and keeps the transport running.
            reached_end = snap.end > 0
                && !*hold_end
                && *play_from < snap.end
                && snap.loop_range.is_none_or(|(_, b)| p > b)
                && p >= snap.end + snap.end_tail;
        }
        render_voices(snap, voices, channels, cursor, n);
        // Instruments render whole blocks, also while stopped (tails).
        let automation = &snap.automation;
        for (ci, ch) in snap.channels.iter_mut().enumerate() {
            if let Some(inst) = &mut ch.instrument {
                if auto {
                    let lanes = std::mem::take(&mut inst.auto_plugin);
                    queue_plugin_points(&lanes, automation, &timeline, n, |id, o, v| {
                        inst.plugin.queue_param(id, o, v);
                    });
                    inst.auto_plugin = lanes;
                }
                let (l, r) = channels.get_mut(ci, n);
                inst.render(l, r, &transport);
            }
        }

        // 2. Channel gain and compensation, summed into bus inputs.
        for (ci, ch) in snap.channels.iter_mut().enumerate() {
            {
                let (l, r) = channels.get_mut(ci, n);
                ch.gain.apply(l, r);
                ch.delay.process(l, r);
            }
            add_into(inputs.get_mut(ch.track, n), channels.get(ci, n));
        }

        // 3. Buses in processing order; destinations always come later.
        let meters = &feedback.meters;
        for (ti, track) in snap.tracks.iter_mut().enumerate() {
            let auto_ctx = AutoCtx {
                automation,
                active: auto,
                starting,
                restore,
                timeline: &timeline,
            };
            process_inserts(
                track,
                ti,
                inputs,
                pre_taps,
                post_taps,
                scratch,
                dry,
                &transport,
                n,
                &auto_ctx,
                param_events,
            );
            if track.struct_fade.is_ramping() || track.struct_fade.current() != 1.0 {
                track.struct_fade.fill(&mut env[..n]);
                let (l, r) = inputs.get_mut(ti, n);
                for i in 0..n {
                    l[i] *= env[i];
                    r[i] *= env[i];
                }
            }
            copy_into(pre_taps.get_mut(ti, n), inputs.get(ti, n));
            {
                let (l, r) = inputs.get_mut(ti, n);
                let automated = track.auto_volume.is_some() || track.auto_pan.is_some();
                if auto && automated {
                    // Ramps between envelope values at 32-sample steps of
                    // the timeline (AUT-021), so the result does not depend
                    // on block sizes.
                    let mut o = 0;
                    while o < n {
                        let at = timeline.pos_at(o);
                        let step = AUTOMATION_STEP as u64;
                        let to_boundary = (step - at % step) as usize;
                        let len = (n - o).min(to_boundary);
                        let gains = |pos: u64| {
                            let value = |lane: Option<usize>, base: f32| {
                                lane.and_then(|i| automation[i].plain_at(pos))
                                    .unwrap_or(base)
                            };
                            let vol = value(track.auto_volume, track.base_volume);
                            let (gl, gr) = balance_gains(value(track.auto_pan, track.base_pan));
                            (gl * vol, gr * vol)
                        };
                        if o == 0 && starting {
                            // Chase (AUT-033): the first sample already has
                            // the envelope's value.
                            let (gl, gr) = gains(at);
                            track.fader.l.reset(gl);
                            track.fader.r.reset(gr);
                        }
                        if at.is_multiple_of(step) || (o == 0 && starting) {
                            let (gl, gr) = gains(at + to_boundary as u64 - 1);
                            track.fader.l.set_target_over(gl, to_boundary as u32);
                            track.fader.r.set_target_over(gr, to_boundary as u32);
                        }
                        track.fader.apply(&mut l[o..o + len], &mut r[o..o + len]);
                        o += len;
                    }
                } else {
                    if restore && automated {
                        let (gl, gr) = balance_gains(track.base_pan);
                        track.fader.l.set_target(gl * track.base_volume);
                        track.fader.r.set_target(gr * track.base_volume);
                    }
                    track.fader.apply(l, r);
                }
            }
            copy_into(post_taps.get_mut(ti, n), inputs.get(ti, n));
            track.mute.fill(&mut env[..n]);

            for send in &mut track.sends {
                let (src_l, src_r) = if send.pre_fader {
                    pre_taps.get(ti, n)
                } else {
                    post_taps.get(ti, n)
                };
                let send_lane = send.auto_level.filter(|_| auto);
                if restore && send.auto_level.is_some() {
                    send.gain.set_target(send.base_level);
                }
                {
                    let (tl, tr) = scratch.get_mut(0, n);
                    for i in 0..n {
                        if let Some(lane) = send_lane {
                            let at = timeline.pos_at(i);
                            let step = AUTOMATION_STEP as u64;
                            let value =
                                |pos| automation[lane].plain_at(pos).unwrap_or(send.base_level);
                            if i == 0 && starting {
                                send.gain.reset(value(at));
                            }
                            if at.is_multiple_of(step) || (i == 0 && starting) {
                                let to_boundary = step - at % step;
                                send.gain.set_target_over(
                                    value(at + to_boundary - 1),
                                    to_boundary as u32,
                                );
                            }
                        }
                        let g = send.gain.next_value();
                        let m = if send.pre_fader { 1.0 } else { env[i] };
                        tl[i] = src_l[i] * g * m;
                        tr[i] = src_r[i] * g * m;
                    }
                    send.delay.process(tl, tr);
                }
                add_into(inputs.get_mut(send.dest, n), scratch.get(0, n));
            }

            let (src_l, src_r) = post_taps.get(ti, n);
            let out_index = if track.main_out.is_some() { 0 } else { 2 };
            {
                let (tl, tr) = scratch.get_mut(out_index, n);
                for i in 0..n {
                    tl[i] = src_l[i] * env[i];
                    tr[i] = src_r[i] * env[i];
                }
                // Bus meter: after mute/solo, before compensation delay.
                if let Some(slot) = meters.buses.get(ti) {
                    slot.publish(track.id.0, peak(tl), peak(tr));
                }
            }
            if let Some((dest, delay)) = &mut track.main_out {
                let (tl, tr) = scratch.get_mut(0, n);
                delay.process(tl, tr);
                add_into(inputs.get_mut(*dest, n), scratch.get(0, n));
            }
        }

        // 4. Master output stage: Stop fade, safety limiter, output meter.
        let (ol, or) = scratch.get_mut(2, n);
        let fading = out_fade.is_ramping() || out_fade.current() != 1.0;
        if fading {
            out_fade.fill(&mut env[..n]);
            for i in 0..n {
                ol[i] *= env[i];
                or[i] *= env[i];
            }
        }
        if let Some(limiter) = &mut snap.limiter {
            limiter.process(ol, or);
            meters.publish_limiter(limiter.take_min_gain());
        }
        meters.output.publish(u64::MAX, peak(ol), peak(or));
        // 5. Metronome after the master (MET-012), faded by Stop.
        let beat = snap.samples_per_tick * f64::from(snap.time_signature.beat_ticks());
        let per_bar = u32::from(snap.time_signature.numerator);
        let mut count_done = false;
        if *playing && !*stopping {
            metronome.schedule_playing(&timeline, n, beat, per_bar);
        } else if !*playing && !*stopping {
            count_done = metronome.schedule_count_in(n, beat, per_bar);
        }
        metronome.render(ol, or, fading.then_some(&env[..n]));
        if count_done {
            *play_from = *pos;
            *playing = true;
        }
        if reached_end {
            *playing = false;
            *pos = *home;
            *play_from = *home;
        }
    }
}

/// Automation state for one block.
struct AutoCtx<'a> {
    automation: &'a [AutomationRt],
    /// Envelopes apply in this block.
    active: bool,
    /// First automated block (chase at offset 0).
    starting: bool,
    /// Automation just stopped: return to project values.
    restore: bool,
    timeline: &'a BlockTimeline,
}

/// Queues plugin automation: a chase point at offset 0 (AUT-033), points
/// every 64 samples and every envelope breakpoint at its exact offset
/// (AUT-030).
fn queue_plugin_points(
    lanes: &[(u32, usize)],
    automation: &[AutomationRt],
    timeline: &BlockTimeline,
    n: usize,
    mut queue: impl FnMut(u32, u32, f64),
) {
    for &(id, lane) in lanes {
        let a = &automation[lane];
        let mut o = 0;
        while o < n {
            if let Some(v) = a.normalized_at(timeline.pos_at(o)) {
                queue(id, o as u32, f64::from(v));
            }
            o += PLUGIN_POINT_STEP;
        }
        // The plugin interpolates linearly between points, so a jump (a
        // loop wrap, or a point after a hold) gets the old value one
        // sample earlier.
        let mut step = |offset: usize, before: Option<f32>, after: f32| {
            if offset > 0
                && let Some(b) = before
            {
                queue(id, offset as u32 - 1, f64::from(b));
            }
            queue(id, offset as u32, f64::from(after));
        };
        for (k, (seg_offset, seg_pos, len)) in timeline.segments(n).enumerate() {
            if k > 0
                && seg_offset > 0
                && let Some(v) = a.normalized_at(seg_pos)
            {
                // Loop wrap (AUT-033).
                step(
                    seg_offset,
                    a.normalized_at(timeline.pos_at(seg_offset - 1)),
                    v,
                );
            }
            for (pos, v) in a.breakpoints(seg_pos, seg_pos + len as u64) {
                let offset = seg_offset + (pos - seg_pos) as usize;
                let before = (pos > seg_pos).then(|| a.normalized_at(pos - 1)).flatten();
                step(offset, before, v);
            }
        }
    }
}

/// Runs the insert chain of bus `ti` in place on its input buffer.
#[allow(clippy::too_many_arguments)]
fn process_inserts(
    track: &mut TrackRt,
    ti: usize,
    inputs: &mut Buses,
    pre_taps: &Buses,
    post_taps: &Buses,
    scratch: &mut Buses,
    dry: &mut Buses,
    transport: &Transport,
    n: usize,
    auto: &AutoCtx,
    events: &mut Vec<ParamEvent>,
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
        events.clear();
        if let Some((lane, base)) = ins.auto_bypass {
            let bypass = if auto.active {
                auto.automation[lane]
                    .plain_at(auto.timeline.pos_at(0))
                    .map_or(base, |v| v >= 0.5)
            } else {
                base
            };
            if auto.active || auto.restore {
                ins.mix.set_target(if bypass { 0.0 } else { 1.0 });
            }
        }
        if auto.active && !ins.auto_params.is_empty() {
            // Events at 32-sample steps of the timeline (block-size
            // independent), plus a chase at offset 0 when automation starts.
            for o in 0..n {
                let at = auto.timeline.pos_at(o);
                if !at.is_multiple_of(AUTOMATION_STEP as u64) && !(o == 0 && auto.starting) {
                    continue;
                }
                for &(index, lane, base) in &ins.auto_params {
                    // Outside the clips: back to the manual value.
                    let v = auto.automation[lane].plain_at(at).unwrap_or(base);
                    if events.len() < events.capacity() {
                        events.push(ParamEvent {
                            frame: o as u32,
                            index,
                            value: v,
                        });
                    }
                }
            }
        } else if auto.restore {
            for &(index, _, base) in &ins.auto_params {
                if events.len() < events.capacity() {
                    events.push(ParamEvent {
                        frame: 0,
                        index,
                        value: base,
                    });
                }
            }
        }
        if auto.active
            && !ins.auto_plugin.is_empty()
            && let EffectRt::Plugin(p) = &mut ins.effect
        {
            queue_plugin_points(
                &ins.auto_plugin,
                auto.automation,
                auto.timeline,
                n,
                |id, o, v| {
                    p.queue_param(id, o, v);
                },
            );
        }
        let (l, r) = inputs.get_mut(ti, n);
        let sidechain = has_sidechain.then(|| scratch.get(1, n));
        ins.process(l, r, dry.get_mut(0, n), sidechain, transport, events);
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
        let sample = v
            .held
            .as_deref()
            .or_else(|| snap.channels.get(slot).and_then(|c| c.sample.as_deref()));
        match sample {
            Some(sample) => {
                let (l, r) = channels.get_mut(slot, to);
                v.inner.render_add(sample, &mut l[from..], &mut r[from..]);
            }
            None => v.inner.stop(),
        }
    }
}

/// `key` moved by `semitones`, kept in the MIDI range.
fn transpose(key: u8, semitones: i32) -> u8 {
    (i32::from(key) + semitones).clamp(0, 127) as u8
}

fn trigger(
    snap: &PlaybackSnapshot,
    voices: &mut [Voice],
    serial: &mut u64,
    feedback: &EngineFeedback,
    e: &crate::StepEvent,
) -> Option<usize> {
    let ch = snap.channels.get(e.channel)?;
    ch.sample.as_ref()?;
    let fade = release_len(snap.sample_rate);
    // Voice limits of the channel: mono cuts every voice, a polyphony
    // limit the oldest ones (short fades, no clicks).
    let mine =
        |v: &Voice| v.inner.is_active() && !v.inner.is_releasing() && v.inner.slot == e.channel;
    if ch.mono {
        for v in voices.iter_mut().filter(|v| mine(v)) {
            v.inner.release(fade);
        }
    } else if ch.polyphony > 0 {
        let mut held = voices.iter().filter(|v| mine(v)).count();
        while held >= usize::from(ch.polyphony) {
            if let Some(v) = voices
                .iter_mut()
                .filter(|v| mine(v))
                .min_by_key(|v| v.inner.started_at)
            {
                v.inner.release(fade);
            }
            held -= 1;
        }
    }
    // A voice still holding a replaced sample is not reused: its sample
    // must leave through the garbage ring, not be freed here.
    let index = match voices
        .iter()
        .position(|v| !v.inner.is_active() && v.held.is_none())
    {
        Some(i) => i,
        None => {
            feedback.note_steal();
            voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.held.is_none())
                .min_by_key(|(_, v)| v.inner.started_at)
                .map(|(i, _)| i)?
        }
    };
    *serial += 1;
    let v = &mut voices[index];
    v.channel = ch.id;
    v.live = None;
    let s = &ch.sampler;
    let mut playback = s.playback;
    if s.gated {
        playback.gate = Some(e.length.max(1));
    }
    let gain = e.velocity * s.gain;
    v.inner.start_with(
        e.channel,
        *serial,
        gain,
        gain,
        jdance_dsp::key_rate(transpose(e.key, ch.pitch), s.root_key),
        playback,
    );
    Some(index)
}
