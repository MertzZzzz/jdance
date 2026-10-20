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
fn scan_lists_effect_classes() {
    let d = scan_in_process(bundle()).unwrap();
    let names: Vec<_> = d.iter().map(|d| d.plugin.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "JD Test Gain",
            "JD Test Latency",
            "JD Test Sidechain",
            "JD Test Logger"
        ]
    );
    for x in &d {
        assert_eq!(x.kind, PluginKind::Effect);
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
        ScanOutcome::Ok(d) => assert_eq!(d.len(), 4),
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
