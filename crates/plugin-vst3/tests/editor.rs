// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin editor window against the test plugin (PLG-060..063): attach,
//! plugin-driven resize, the Linux run loop, close and reopen.
//!
//! Runs without the libtest harness so it owns the main thread (AppKit
//! windows can only be created there). On Linux it needs an X server
//! (CI runs it under xvfb-run) and reports NOT RUN otherwise.

use jdance_plugin_api::ProcessSetupInfo;
use jdance_plugin_vst3::{Vst3Module, bundle_binary, instantiate};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SETUP: ProcessSetupInfo = ProcessSetupInfo {
    sample_rate: 48_000.0,
    max_block: 256,
    offline: false,
};

fn main() {
    if cfg!(target_os = "linux") && std::env::var_os("DISPLAY").is_none() {
        eprintln!("plugin editor test: NOT RUN (no DISPLAY; run under xvfb-run)");
        return;
    }
    let bundle = jdance_test_support::test_plugin_bundle(bundle_binary);
    let module = Arc::new(Vst3Module::load(bundle).unwrap());
    let class = |name: &str| {
        module
            .descriptors()
            .into_iter()
            .find(|d| d.plugin.name == name)
            .unwrap()
            .plugin
            .class_id
    };

    // A plugin without an editor: generic editor only.
    let (_p, c) = instantiate(&module, &class("JD Test Latency"), &SETUP, None).unwrap();
    assert!(c.open_editor("no gui").unwrap().is_none());

    let (p, c) = instantiate(&module, &class("JD Test Gain"), &SETUP, None).unwrap();
    for round in 0..2 {
        let mut e = c
            .open_editor("JD Test Gain")
            .unwrap()
            .expect("the gain class has an editor");
        // The view asked for a new size while attaching.
        assert_eq!(e.view_size(), (480, 320), "round {round}");
        // On Linux a run-loop timer asks for another size.
        let expected = if cfg!(target_os = "linux") {
            (500, 330)
        } else {
            (480, 320)
        };
        let start = Instant::now();
        while (e.size() != expected || e.view_size() != expected)
            && start.elapsed() < Duration::from_secs(5)
        {
            assert!(e.poll(), "window closed unexpectedly");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(e.view_size(), expected, "plugin size, round {round}");
        assert_eq!(e.size(), expected, "window size, round {round}");
        // Closing the editor keeps the instance (PLG-063); round 2 reopens.
        drop(e);
    }
    drop(c);
    drop(p);
    println!("plugin editor test: ok");
}
