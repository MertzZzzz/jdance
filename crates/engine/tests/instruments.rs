// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin instruments on channels with a fake instrument: note timing,
//! gates, note-off on loop wrap and Stop (PLG-033), latency compensation.

use jdance_domain::{
    Channel, ChannelId, Note, PatternId, PluginSlot, PluginTarget, Project, TICKS_PER_STEP,
};
use jdance_engine::{PlayMode, PlaybackSnapshot, PluginRt, compile_full, create_engine};
use jdance_plugin_api::{NoteEvent, NoteKind, PluginProcessor, Transport};
use jdance_rt_check::{GuardedAllocator, reset_violations, rt_section, violations};
use std::sync::{Arc, Mutex};

#[global_allocator]
static ALLOC: GuardedAllocator = GuardedAllocator;

const SR: u32 = 48_000;

/// (absolute sample, key, note on) per event.
type EventLog = Arc<Mutex<Vec<(u64, u8, bool)>>>;
/// 120 BPM: one step = 6000 samples; a 16-step pattern = 96 000.
const STEP: u64 = 6_000;

/// Logs (absolute sample, key, on) and outputs a DC level while any note
/// sounds, delayed by `latency`.
struct FakeSynth {
    log: EventLog,
    clock: u64,
    held: Vec<u8>,
    latency: u32,
}

impl PluginProcessor for FakeSynth {
    fn process(
        &mut self,
        _: &mut [f32],
        _: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &Transport,
    ) {
    }

    fn process_instrument(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        events: &[NoteEvent],
        _transport: &Transport,
    ) {
        let mut k = 0;
        for i in 0..left.len() {
            while let Some(e) = events.get(k).filter(|e| e.offset as usize == i) {
                k += 1;
                let on = matches!(e.kind, NoteKind::On { .. });
                if let Ok(mut l) = self.log.try_lock() {
                    l.push((self.clock + i as u64, e.key, on));
                }
                if on {
                    self.held.push(e.key);
                } else {
                    self.held.retain(|x| *x != e.key);
                }
            }
            let v = if self.held.is_empty() { 0.0 } else { 1.0 };
            left[i] = v;
            right[i] = v;
        }
        self.clock += left.len() as u64;
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn has_sidechain_input(&self) -> bool {
        false
    }
}

fn slot() -> PluginSlot {
    PluginSlot {
        format: "vst3".into(),
        class_id: "4A445446584500000000000000000004".into(),
        name: "Fake Synth".into(),
        vendor: "tests".into(),
        version: "1".into(),
        category: "Instrument".into(),
        component_state: Vec::new(),
        controller_state: Vec::new(),
    }
}

fn project(steps: &[u32], notes: Vec<Note>) -> (Project, ChannelId, PatternId) {
    let mut p = Project::new();
    p.tempo_bpm = 120.0;
    let id = ChannelId(p.ids.next());
    p.channels
        .push(Channel::new_instrument(id, "Synth", slot()));
    let pid = p.patterns[0].id;
    let row = p.patterns[0].row_mut(id);
    for &s in steps {
        row.steps[s as usize].on = true;
    }
    row.notes = notes;
    (p, id, pid)
}

fn synth(log: &EventLog, latency: u32) -> PluginRt {
    PluginRt::fresh(Box::new(FakeSynth {
        log: Arc::clone(log),
        clock: 0,
        held: Vec::with_capacity(64),
        latency,
    }))
}

fn compile(p: &Project, pid: PatternId, plugin: Option<PluginRt>) -> PlaybackSnapshot {
    let mut plugin = plugin;
    compile_full(p, SR, PlayMode::Pattern(pid), |_| None, &mut |target, s| {
        assert!(matches!(target, PluginTarget::Channel(_)));
        assert_eq!(s.name, "Fake Synth");
        plugin.take().unwrap_or_else(PluginRt::missing)
    })
}

fn render(h: &mut jdance_engine::EngineProcessor, frames: usize, block: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut l = vec![0.0; block];
    let mut r = vec![0.0; block];
    while out.len() < frames {
        h.render(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out.truncate(frames);
    out
}

#[test]
fn notes_and_steps_turn_on_and_off_on_time() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let long = Note {
        id: 0,
        start: 4 * TICKS_PER_STEP,
        length: 2 * TICKS_PER_STEP,
        key: 72,
        velocity: 0.7,
    };
    let (p, _, pid) = project(&[0], vec![long]);
    let (mut h, mut e) = create_engine(SR);
    h.set_snapshot(compile(&p, pid, Some(synth(&log, 0))))
        .unwrap();
    h.play().unwrap();
    // Odd block size: events fall inside blocks.
    render(&mut e, 10 * STEP as usize, 777);
    assert_eq!(
        *log.lock().unwrap(),
        [
            (0, 60, true),
            (STEP, 60, false),
            (4 * STEP, 72, true),
            (6 * STEP, 72, false)
        ]
    );
}

#[test]
fn loop_wrap_and_stop_turn_sounding_notes_off() {
    let log = Arc::new(Mutex::new(Vec::new()));
    // Ends after the pattern: cut at the repetition end (the wrap).
    let tail = Note {
        id: 0,
        start: 15 * TICKS_PER_STEP,
        length: 4 * TICKS_PER_STEP,
        key: 64,
        velocity: 1.0,
    };
    let (p, _, pid) = project(&[], vec![tail]);
    let (mut h, mut e) = create_engine(SR);
    h.set_snapshot(compile(&p, pid, Some(synth(&log, 0))))
        .unwrap();
    h.play().unwrap();
    render(&mut e, 16 * STEP as usize + 1_000, 512);
    {
        let l = log.lock().unwrap();
        assert_eq!(l[0], (15 * STEP, 64, true));
        assert_eq!(l[1], (16 * STEP, 64, false), "off at the wrap");
    }
    // Next repetition: on again, then Stop turns it off.
    render(&mut e, 15 * STEP as usize, 512);
    h.stop().unwrap();
    render(&mut e, 2_048, 512);
    let l = log.lock().unwrap();
    assert_eq!(l[2].1, 64);
    assert!(l[2].2);
    assert!(!l[3].2, "Stop turns the sounding note off");
    assert_eq!(l.len(), 4);
}

#[test]
fn panic_turns_notes_off_and_playback_continues() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let long = Note {
        id: 0,
        start: 0,
        length: 8 * TICKS_PER_STEP,
        key: 67,
        velocity: 1.0,
    };
    let (p, _, pid) = project(&[10], vec![long]);
    let (mut h, mut e) = create_engine(SR);
    h.set_snapshot(compile(&p, pid, Some(synth(&log, 0))))
        .unwrap();
    h.play().unwrap();
    let before = render(&mut e, 2 * STEP as usize, 500);
    assert!(before[STEP as usize] > 0.0, "note sounds");
    h.panic().unwrap();
    let after = render(&mut e, 9 * STEP as usize, 500);
    assert_eq!(after[100], 0.0, "silenced at the next block");
    assert!(after[8 * STEP as usize + 100] > 0.0, "step 10 still plays");
    let l = log.lock().unwrap();
    assert_eq!((l[1].1, l[1].2), (67, false), "panic sends note-off");
    assert_eq!(l[1].0, 2 * STEP, "at the start of the next block");
    // The cut note gets no second note-off when its gate would end.
    assert_eq!(
        *l,
        [(0, 67, true), (2 * STEP, 67, false), (10 * STEP, 60, true)]
    );
}

#[test]
fn transposition_and_mono_apply_to_instruments() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let a = Note {
        id: 0,
        start: 0,
        length: 4 * TICKS_PER_STEP,
        key: 60,
        velocity: 1.0,
    };
    let b = Note {
        id: 0,
        start: TICKS_PER_STEP,
        key: 64,
        ..a
    };
    let (mut p, ch, pid) = project(&[], vec![a, b]);
    let c = p.channel_mut(ch).unwrap();
    c.pitch = -12;
    c.mono = true;
    let (mut h, mut e) = create_engine(SR);
    h.set_snapshot(compile(&p, pid, Some(synth(&log, 0))))
        .unwrap();
    h.play().unwrap();
    render(&mut e, 6 * STEP as usize, 512);
    let l = log.lock().unwrap();
    assert_eq!(
        l[..3],
        [(0, 48, true), (STEP, 48, false), (STEP, 52, true)],
        "an octave down; the second note cuts the first"
    );
}

#[test]
fn instrument_latency_is_compensated_against_other_channels() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (mut p, _, pid) = project(&[0], Vec::new());
    // A second, silent sampler channel on the same track gets delayed.
    let other = ChannelId(p.ids.next());
    p.channels.push(Channel::new_sampler(other, "Kick"));
    let snap = compile(&p, pid, Some(synth(&log, 100)));
    assert_eq!(snap.latency, 100);
    assert_eq!(snap.channels[0].delay.len(), 0, "the instrument path");
    assert_eq!(snap.channels[1].delay.len(), 100, "the other channel waits");
}

#[test]
fn missing_instrument_is_silent() {
    let (p, _, pid) = project(&[0, 4], Vec::new());
    let (mut h, mut e) = create_engine(SR);
    h.set_snapshot(compile(&p, pid, None)).unwrap();
    h.play().unwrap();
    assert!(
        render(&mut e, 4 * STEP as usize, 512)
            .iter()
            .all(|v| *v == 0.0)
    );
}

/// Counts events without allocating.
struct CountingSynth(Arc<std::sync::atomic::AtomicUsize>);

impl PluginProcessor for CountingSynth {
    fn process(
        &mut self,
        _: &mut [f32],
        _: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &Transport,
    ) {
    }

    fn process_instrument(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        events: &[NoteEvent],
        _: &Transport,
    ) {
        self.0
            .fetch_add(events.len(), std::sync::atomic::Ordering::Relaxed);
        l.fill(0.0);
        r.fill(0.0);
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn has_sidechain_input(&self) -> bool {
        false
    }
}

#[test]
fn instrument_rendering_does_not_allocate() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let all: Vec<u32> = (0..16).collect();
    let notes = (0..64)
        .map(|i| Note {
            id: 0,
            start: i * 60,
            length: 300,
            key: 40 + (i % 40) as u8,
            velocity: 0.5,
        })
        .collect();
    let (p, _, pid) = project(&all, notes);
    let (mut h, mut e) = create_engine(SR);
    let snap = compile(
        &p,
        pid,
        Some(PluginRt::fresh(Box::new(CountingSynth(Arc::clone(&count))))),
    );
    h.set_snapshot(snap).unwrap();
    h.play().unwrap();
    let mut l = vec![0.0; 1024];
    let mut r = vec![0.0; 1024];
    e.render(&mut l, &mut r);
    reset_violations();
    rt_section(|| {
        for i in 0..400 {
            if i == 200 {
                // Stop mid-pattern: all sounding notes off.
                let _ = h.stop();
            }
            e.render(&mut l, &mut r);
        }
    });
    assert_eq!(violations(), 0);
    assert!(count.load(std::sync::atomic::Ordering::Relaxed) > 100);
}

#[test]
fn live_notes_play_while_stopped_until_note_off() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (mut p, ch, pid) = project(&[], Vec::new());
    p.channel_mut(ch).unwrap().pitch = 2;
    let (mut h, mut e) = create_engine(SR);
    h.set_snapshot(compile(&p, pid, Some(synth(&log, 0))))
        .unwrap();
    render(&mut e, 512, 512);
    h.live_note(ch, 60, 0.8).unwrap();
    let held = render(&mut e, 5_000, 500);
    assert!(held.iter().all(|v| *v > 0.0), "sounds while stopped");
    h.live_note_off(ch, 60).unwrap();
    let after = render(&mut e, 1_000, 500);
    assert!(after.iter().all(|v| *v == 0.0));
    assert_eq!(*log.lock().unwrap(), [(512, 62, true), (5_512, 62, false)]);
}
