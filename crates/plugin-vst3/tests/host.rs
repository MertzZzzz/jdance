// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless host tests against `test-plugins/jdance-test-fx` (PLG-081).

use jdance_plugin_api::{
    PluginController, PluginDescriptor, PluginEvent, PluginKind, PluginProcessor, PluginState,
    ProcessSetupInfo, Transport,
};
use jdance_plugin_vst3::{
    ScanOutcome, Vst3Controller, Vst3Module, Vst3Processor, bundle_binary, find_modules,
    instantiate, run_scanner, scan_in_process, scan_out_of_process,
};
use jdance_rt_check::{GuardedAllocator, reset_violations, rt_section, violations};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

#[global_allocator]
static ALLOC: GuardedAllocator = GuardedAllocator;

const LATENCY: usize = 64;

fn bundle() -> &'static Path {
    jdance_test_support::test_plugin_bundle(bundle_binary)
}

fn module() -> Arc<Vst3Module> {
    Arc::new(Vst3Module::load(bundle()).unwrap())
}

fn descriptor(name: &str) -> PluginDescriptor {
    module()
        .descriptors()
        .into_iter()
        .find(|d| d.plugin.name == name)
        .unwrap_or_else(|| panic!("no class {name}"))
}

const SETUP: ProcessSetupInfo = ProcessSetupInfo {
    sample_rate: 48_000.0,
    max_block: 256,
    offline: false,
};

fn open(name: &str, state: Option<&PluginState>) -> (Vst3Processor, Vst3Controller) {
    let d = descriptor(name);
    instantiate(&module(), &d.plugin.class_id, &SETUP, state).unwrap()
}

fn ramp(n: usize) -> Vec<f32> {
    (0..n).map(|i| ((i % 100) as f32 - 50.0) / 100.0).collect()
}

fn run(p: &mut Vst3Processor, input: &[f32], sidechain: Option<&[f32]>) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (input.to_vec(), input.to_vec());
    let t = Transport {
        playing: true,
        tempo_bpm: 120.0,
        ..Transport::default()
    };
    p.process(&mut l, &mut r, sidechain.map(|s| (s, s)), &t);
    (l, r)
}

#[test]
fn scan_lists_classes_and_kinds() {
    let d = scan_in_process(bundle()).unwrap();
    let names: Vec<_> = d.iter().map(|d| d.plugin.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "JD Test Gain",
            "JD Test Latency",
            "JD Test Sidechain",
            "JD Test Logger",
            "JD Test Synth",
            "JD Test Drums",
            "JD Test Mono",
            "JD Test Strict"
        ]
    );
    for x in &d {
        let kind = if ["JD Test Synth", "JD Test Drums"].contains(&x.plugin.name.as_str()) {
            PluginKind::Instrument
        } else {
            PluginKind::Effect
        };
        assert_eq!(x.kind, kind);
        assert_eq!(x.plugin.vendor, "jdance tests");
        assert_eq!(x.plugin.version, "1.0.0");
        assert_eq!(x.plugin.class_id.len(), 32);
    }
    assert_eq!(d[0].plugin.class_id, "4A445446584500000000000000000000");
    assert_eq!(
        find_modules(&[bundle().parent().unwrap().to_path_buf()]),
        [bundle()]
    );
}

#[test]
fn out_of_process_scan_and_blocklist_reasons() {
    let scanner = Path::new(env!("CARGO_BIN_EXE_jdance-plugin-scanner"));
    match scan_out_of_process(scanner, bundle(), Duration::from_secs(30)) {
        ScanOutcome::Ok(d) => assert_eq!(d.len(), 8),
        f => panic!("{f:?}"),
    }
    let missing = bundle().with_file_name("Missing.vst3");
    assert!(matches!(
        scan_out_of_process(scanner, &missing, Duration::from_secs(30)),
        ScanOutcome::Failed(_)
    ));
}

#[test]
fn scanner_crash_and_hang_are_contained() {
    let scanner = Path::new(env!("CARGO_BIN_EXE_jdance-plugin-scanner"));
    // The fault mode is read by the plugin inside the scanner process.
    let cmd = |fault: &str| {
        let mut c = Command::new(scanner);
        c.arg(bundle()).env("JDANCE_TEST_FX_FAULT", fault);
        c
    };
    match run_scanner(cmd("crash"), Duration::from_secs(30)) {
        ScanOutcome::Failed(reason) => assert!(reason.contains("exited"), "{reason}"),
        ok => panic!("{ok:?}"),
    }
    match run_scanner(cmd("hang"), Duration::from_millis(500)) {
        ScanOutcome::Failed(reason) => assert!(reason.contains("timed out"), "{reason}"),
        ok => panic!("{ok:?}"),
    }
}

#[test]
fn plugin_output_on_stdout_does_not_break_the_scan() {
    let scanner = Path::new(env!("CARGO_BIN_EXE_jdance-plugin-scanner"));
    let mut cmd = Command::new(scanner);
    cmd.arg(bundle()).env("JDANCE_TEST_FX_FAULT", "chatty");
    match run_scanner(cmd, Duration::from_secs(30)) {
        ScanOutcome::Ok(d) => assert_eq!(d.len(), 8),
        f => panic!("{f:?}"),
    }
}

#[test]
fn gain_follows_parameter_changes() {
    let (mut p, mut c) = open("JD Test Gain", None);
    let x = ramp(1000);
    let (l, r) = run(&mut p, &x, None);
    assert_eq!(l, x, "default gain is unity");
    assert_eq!(r, x);
    c.set_param_normalized(0, 0.25);
    assert_eq!(c.param_normalized(0), 0.25);
    let (l, _) = run(&mut p, &x, None);
    for (a, b) in l.iter().zip(&x) {
        assert_eq!(*a, b * 0.5);
    }
    assert_eq!(c.param_text(0, 0.25), "0.500");
    assert_eq!(c.param_from_text(0, "1.5"), Some(0.75));
    let bypass = c.params().iter().find(|p| p.flags.is_bypass).unwrap();
    assert_eq!(bypass.step_count, 1);
}

#[test]
fn latency_is_reported_and_exact() {
    let (mut p, _c) = open("JD Test Latency", None);
    assert_eq!(p.latency_samples() as usize, LATENCY);
    let mut x = vec![0.0; 1000];
    x[10] = 1.0;
    let (l, _) = run(&mut p, &x, None);
    assert_eq!(l.iter().position(|v| *v != 0.0), Some(10 + LATENCY));
}

#[test]
fn sidechain_bus_receives_the_sidechain_signal() {
    let (mut p, _c) = open("JD Test Sidechain", None);
    assert!(p.has_sidechain_input());
    let main = vec![0.25; 600];
    let side = ramp(600);
    let (l, _) = run(&mut p, &main, Some(&side));
    assert_eq!(l, side);
    let (l, _) = run(&mut p, &main, None);
    assert!(l.iter().all(|v| *v == 0.0), "no source: silence");
    let (gain, _) = open("JD Test Gain", None);
    assert!(!gain.has_sidechain_input());
}

#[test]
fn parameter_changes_reach_processor_and_output_changes_return() {
    let (mut p, mut c) = open("JD Test Logger", None);
    let fixed = c.params().iter().find(|p| p.id == 1).unwrap();
    assert!(!fixed.flags.automatable);
    assert!(
        c.params()
            .iter()
            .find(|p| p.id == 2)
            .unwrap()
            .flags
            .read_only
    );
    c.set_param_normalized(0, 0.75);
    run(&mut p, &ramp(300), None);
    let events = c.poll();
    assert!(
        events.contains(&PluginEvent::ParamChanged { id: 2, value: 0.75 }),
        "{events:?}"
    );
    // The logger keeps every point it received in its state.
    let state = c.state().unwrap();
    let log = &state.component[17..];
    assert_eq!(
        u32::from_le_bytes(state.component[13..17].try_into().unwrap()),
        1
    );
    assert_eq!(
        u32::from_le_bytes(log[0..4].try_into().unwrap()),
        0,
        "param id"
    );
    assert_eq!(
        i32::from_le_bytes(log[4..8].try_into().unwrap()),
        0,
        "sample offset"
    );
    assert_eq!(f64::from_le_bytes(log[8..16].try_into().unwrap()), 0.75);
}

#[test]
fn state_round_trips() {
    let (mut p, mut c) = open("JD Test Gain", None);
    c.set_param_normalized(0, 0.3);
    run(&mut p, &ramp(64), None);
    let state = c.state().unwrap();
    assert!(!state.controller.is_empty());
    let (mut p2, mut c2) = open("JD Test Gain", Some(&state));
    assert_eq!(
        c2.param_normalized(0),
        0.3,
        "controller gets the component state"
    );
    let x = ramp(500);
    assert_eq!(run(&mut p2, &x, None), run(&mut p, &x, None));
    assert_eq!(
        c2.state().unwrap(),
        state,
        "save → load → save is identical"
    );
}

#[test]
fn long_blocks_are_split_at_max_block() {
    let d = descriptor("JD Test Latency");
    let setup = ProcessSetupInfo {
        max_block: 32,
        ..SETUP
    };
    let (mut p, _c) = instantiate(&module(), &d.plugin.class_id, &setup, None).unwrap();
    let mut x = vec![0.0; 1000];
    x[500] = 1.0;
    let (l, _) = run(&mut p, &x, None);
    assert_eq!(l.iter().position(|v| *v != 0.0), Some(500 + LATENCY));
}

#[test]
fn unknown_class_is_an_error() {
    let m = module();
    assert!(instantiate(&m, "00000000000000000000000000000000", &SETUP, None).is_err());
    assert!(instantiate(&m, "zz", &SETUP, None).is_err());
}

#[test]
fn load_process_unload_repeatedly() {
    let id = descriptor("JD Test Gain").plugin.class_id;
    for _ in 0..1000 {
        let m = module();
        let (mut p, c) = instantiate(&m, &id, &SETUP, None).unwrap();
        drop(m);
        run(&mut p, &[0.5; 64], None);
        drop(c);
        drop(p);
    }
}

#[test]
fn host_side_processing_does_not_allocate() {
    let (mut p, mut c) = open("JD Test Logger", None);
    let x = ramp(2048);
    let (mut l, mut r) = (x.clone(), x.clone());
    let t = Transport::default();
    p.process(&mut l, &mut r, None, &t);
    reset_violations();
    c.set_param_normalized(0, 0.6);
    rt_section(|| {
        for _ in 0..20 {
            p.process(&mut l, &mut r, Some((&x, &x)), &t);
        }
    });
    assert_eq!(violations(), 0);
}

/// Synth log entries (id, sample offset, key) from the component state.
fn synth_log(c: &mut Vst3Controller) -> Vec<(u32, i32, u8)> {
    let s = c.state().unwrap().component;
    let n = u32::from_le_bytes(s[13..17].try_into().unwrap()) as usize;
    (0..n)
        .map(|i| {
            let e = &s[17 + i * 16..17 + (i + 1) * 16];
            (
                u32::from_le_bytes(e[0..4].try_into().unwrap()),
                i32::from_le_bytes(e[4..8].try_into().unwrap()),
                f64::from_le_bytes(e[8..16].try_into().unwrap()) as u8,
            )
        })
        .collect()
}

#[test]
fn instrument_receives_sample_accurate_notes() {
    use jdance_plugin_api::{NoteEvent, NoteKind};
    let d = descriptor("JD Test Synth");
    // A small max block splits the 1000-sample call into chunks; offsets
    // must be rebased per chunk.
    let setup = ProcessSetupInfo {
        max_block: 300,
        ..SETUP
    };
    let (mut p, mut c) = instantiate(&module(), &d.plugin.class_id, &setup, None).unwrap();
    let ev = |offset, key, kind| NoteEvent {
        offset,
        note_id: i32::from(key),
        key,
        kind,
    };
    let events = [
        ev(10, 69, NoteKind::On { velocity: 1.0 }),
        ev(310, 72, NoteKind::On { velocity: 0.5 }),
        ev(650, 69, NoteKind::Off),
    ];
    let (mut l, mut r) = (vec![1.0; 1000], vec![1.0; 1000]);
    p.process_instrument(&mut l, &mut r, &events, &Transport::default());
    assert!(
        l[..10].iter().all(|v| *v == 0.0),
        "silence before the first note"
    );
    assert!(l[11..300].iter().any(|v| v.abs() > 0.1), "A4 sounds");
    assert_eq!(l, r);
    // Chunk-relative offsets: 10 in chunk 0, 10 in chunk 1, 50 in chunk 2.
    assert_eq!(
        synth_log(&mut c),
        [(0x1000, 10, 69), (0x1000, 10, 72), (0x1001, 50, 69)]
    );
}

#[test]
fn a_multi_output_instrument_that_refuses_stereo_everywhere_opens() {
    use jdance_plugin_api::{NoteEvent, NoteKind};
    // Stereo main out plus separate stereo and mono outputs; it refuses
    // stereo on all of them, as multi-output drum instruments do.
    let (mut p, _c) = open("JD Test Drums", None);
    let events = [NoteEvent {
        offset: 0,
        note_id: 60,
        key: 60,
        kind: NoteKind::On { velocity: 1.0 },
    }];
    let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
    p.process_instrument(&mut l, &mut r, &events, &Transport::default());
    assert!(l.iter().any(|v| v.abs() > 0.1), "the main output sounds");
    assert_eq!(l, r);
}

#[test]
fn a_mono_effect_takes_the_mix_and_feeds_both_sides() {
    let (mut p, _c) = open("JD Test Mono", None);
    let left = ramp(600);
    let mut right: Vec<f32> = left.iter().map(|v| -v * 0.5).collect();
    let mut l = left.clone();
    let t = Transport::default();
    p.process(&mut l, &mut right, None, &t);
    // Default gain 0.5 → ×1: the output is the mono mix on both sides.
    for (i, (a, b)) in l.iter().zip(&right).enumerate() {
        let mix = 0.5 * (left[i] - left[i] * 0.5);
        assert!((a - mix).abs() < 1e-6, "{i}: {a} vs {mix}");
        assert_eq!(a, b);
    }
}

/// A plugin that asks a lot of its host (as JUCE plugins and others do)
/// opens, processes and keeps its instance when its latency changes.
#[test]
fn a_picky_plugin_gets_what_it_needs_from_the_host() {
    let (mut p, mut c) = open("JD Test Strict", None);
    let report = String::from_utf8(c.state().unwrap().controller).unwrap();
    assert_eq!(
        report, "factory=1 host=1 iface=1 handler2=1 handler_first=1 message=1",
        "host context for factory and instance, IPlugInterfaceSupport, \
         IComponentHandler2, the handler before the state, a message sent \
         from another thread"
    );
    // It refuses to process without an output event list: the gain shows
    // that it ran.
    c.set_param_normalized(0, 0.25);
    let input = ramp(256);
    let (l, _) = run(&mut p, &input, None);
    let half: Vec<f32> = input.iter().map(|x| x * 0.5).collect();
    assert_eq!(l, half);
    // Its latency follows the gain; the change is reported and read from
    // the same instance.
    assert!(c.poll().contains(&PluginEvent::LatencyChanged));
    assert_eq!(c.latency(), 32);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        let dest = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// A bundle folder renamed by the user (the binary inside keeps its own
/// name) still loads.
#[test]
fn a_renamed_bundle_loads() {
    let dir = std::env::temp_dir().join(format!("jdance-renamed-{}", std::process::id()));
    let renamed = dir.join("Renamed Effect.vst3");
    if bundle().is_dir() {
        copy_dir(bundle(), &renamed);
    } else {
        // Windows single-file module: nothing to rename inside.
        return;
    }
    let m = Vst3Module::load(&renamed).unwrap();
    assert_eq!(m.descriptors().len(), 8);
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
}
