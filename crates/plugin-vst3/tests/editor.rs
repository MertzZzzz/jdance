// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin editor window against the test plugin (PLG-060..063): attach,
//! plugin-driven resize, the Linux run loop, close and reopen.
//!
//! Runs without the libtest harness so it owns the main thread (AppKit
//! windows can only be created there). On Linux it needs an X server
//! (CI runs it under xvfb-run) and reports NOT RUN otherwise.

use jdance_plugin_api::{EditorScaling, ProcessSetupInfo};
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
    assert!(
        c.open_editor("no gui", EditorScaling::Auto)
            .unwrap()
            .is_none()
    );

    // A connection of our own for the whole test: it also keeps the X
    // server from resetting between the two editors.
    #[cfg(target_os = "linux")]
    let x = x11rb::connect(None).unwrap();
    let (p, c) = instantiate(&module, &class("JD Test Gain"), &SETUP, None).unwrap();
    for round in 0..2 {
        let mut e = c
            .open_editor("JD Test Gain", EditorScaling::Auto)
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
        #[cfg(target_os = "linux")]
        if round == 0 {
            space_reaches_the_host(&mut e, &x);
        }
        // Closing the editor keeps the instance (PLG-063); round 2 reopens.
        drop(e);
    }
    drop(c);
    drop(p);

    // A resizable editor with content scaling that takes its size while
    // attaching without asking the frame: the window follows it, in every
    // scaling mode. 520 × 340 at the screen's scale (1 on CI; Windows
    // gives a plugin that scales itself the screen's scale).
    let (p, c) = instantiate(&module, &class("JD Test Strict"), &SETUP, None).unwrap();
    for scaling in EditorScaling::ALL {
        let mut e = c
            .open_editor("JD Test Strict", scaling)
            .unwrap()
            .expect("the strict class has an editor");
        assert!(e.poll());
        let (w, h) = e.view_size();
        assert!(w >= 520 && w * 340 == h * 520, "{scaling:?}: {w} × {h}");
        assert_eq!(e.size(), (w, h), "window follows the view, {scaling:?}");
        drop(e);
    }
    drop(c);
    drop(p);
    println!("plugin editor test: ok");
}

/// Space typed into the editor (focus in the plugin's own view) starts the
/// transport instead (spec 0.2.3 KEY-004); a held key counts once
/// (KEY-005). XTEST input, like a real keyboard, is subject to the
/// window's key grab.
#[cfg(target_os = "linux")]
fn space_reaches_the_host(
    e: &mut jdance_plugin_vst3::Vst3Editor,
    (conn, screen): &(x11rb::rust_connection::RustConnection, usize),
) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, InputFocus, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::wrapper::ConnectionExt as _;
    let root = conn.setup().roots[*screen].root;
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn
        .get_keyboard_mapping(min, max - min + 1)
        .unwrap()
        .reply()
        .unwrap();
    let per = usize::from(map.keysyms_per_keycode);
    let space = min
        + map
            .keysyms
            .chunks(per)
            .position(|s| s.contains(&0x20))
            .expect("a Space key") as u8;
    // No window manager under Xvfb: the window sits at the origin, and
    // the focus follows the pointer.
    let pointer_root = u32::from(InputFocus::POINTER_ROOT);
    conn.set_input_focus(InputFocus::POINTER_ROOT, pointer_root, x11rb::CURRENT_TIME)
        .unwrap();
    conn.xtest_fake_input(6, 0, x11rb::CURRENT_TIME, root, 40, 40, 0)
        .unwrap(); // MotionNotify
    let key = |kind: u8| {
        conn.xtest_fake_input(kind, space, x11rb::CURRENT_TIME, root, 0, 0, 0)
            .unwrap();
    };
    let _ = jdance_plugin_vst3::take_space_presses();
    key(KEY_PRESS_EVENT);
    key(KEY_RELEASE_EVENT);
    conn.sync().unwrap();
    let start = Instant::now();
    let mut presses = 0;
    while presses == 0 && start.elapsed() < Duration::from_secs(5) {
        assert!(e.poll());
        presses += jdance_plugin_vst3::take_space_presses();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(presses, 1, "Space in the editor reaches the host");
}
