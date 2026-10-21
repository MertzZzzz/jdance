// SPDX-License-Identifier: GPL-3.0-or-later
//! The application binary is its own out-of-process plugin scanner
//! (PLG-012, PLG-022).

use std::process::Command;

#[test]
fn scan_plugin_prints_descriptors_or_fails_cleanly() {
    let bundle = jdance_test_support::test_plugin_bundle(jdance_plugin_vst3::bundle_binary);
    let out = Command::new(env!("CARGO_BIN_EXE_jdance"))
        .arg("scan-plugin")
        .arg(bundle)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let d: Vec<jdance_plugin_api::PluginDescriptor> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(d.len(), 5);
    assert!(d.iter().all(|d| d.path == bundle.display().to_string()));

    let bad = Command::new(env!("CARGO_BIN_EXE_jdance"))
        .args(["scan-plugin", "/nonexistent/X.vst3"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}
