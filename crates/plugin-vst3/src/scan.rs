// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin discovery and out-of-process scanning (PLG-020..024).

use jdance_plugin_api::PluginDescriptor;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default scanner timeout per module (PLG-022 step 4). Large plugins
/// (sample libraries, license checks) can take well over half a minute
/// the first time.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(90);

/// Standard VST3 folders for this OS (PLG-020).
pub fn default_search_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(windows) {
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            out.push(PathBuf::from(common).join("VST3"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            out.push(h.join("Library/Audio/Plug-Ins/VST3"));
        }
        out.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    } else {
        if let Some(h) = &home {
            out.push(h.join(".vst3"));
        }
        out.push(PathBuf::from("/usr/lib/vst3"));
        out.push(PathBuf::from("/usr/local/lib/vst3"));
    }
    out
}

/// `.vst3` bundles (and Windows single-file modules) under `roots`,
/// sorted. Bundles are not descended into.
pub fn find_modules(roots: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let is_vst3 = p
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("vst3"));
            if is_vst3 && (p.is_dir() || cfg!(windows)) {
                out.push(p);
            } else if p.is_dir() && depth < 8 {
                walk(&p, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    for r in roots {
        walk(r, 0, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// Result of scanning one module.
#[derive(Debug, Clone, PartialEq)]
pub enum ScanOutcome {
    Ok(Vec<PluginDescriptor>),
    /// Scanner crashed, timed out or printed garbage: goes to the
    /// blocklist with this reason (PLG-022 step 5).
    Failed(String),
}

/// Runs `scanner <module>` with a timeout. The scanner prints the
/// module's descriptors as JSON on stdout.
pub fn scan_out_of_process(scanner: &Path, module: &Path, timeout: Duration) -> ScanOutcome {
    let mut cmd = Command::new(scanner);
    cmd.arg(module);
    run_scanner(cmd, timeout)
}

/// Runs a prepared scanner command (see [`scan_out_of_process`]).
pub fn run_scanner(mut cmd: Command, timeout: Duration) -> ScanOutcome {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return ScanOutcome::Failed(format!("could not start scanner: {e}")),
    };
    // Read stdout on a thread so a chatty scanner cannot block on a full
    // pipe while we wait.
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let mut stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return ScanOutcome::Failed(format!(
                    "scanner timed out after {} s",
                    timeout.as_secs_f32()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return ScanOutcome::Failed(format!("scanner wait failed: {e}")),
        }
    };
    let out = reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if !status.success() {
        let detail = err.lines().last().unwrap_or("").trim();
        return ScanOutcome::Failed(if detail.is_empty() {
            format!("scanner exited with {status}")
        } else {
            format!("scanner exited with {status}: {detail}")
        });
    }
    match serde_json::from_str::<Vec<PluginDescriptor>>(&out) {
        Ok(d) => ScanOutcome::Ok(d),
        Err(e) => ScanOutcome::Failed(format!("invalid scanner output: {e}")),
    }
}

/// In-process scan, used by the scanner binary itself.
pub fn scan_in_process(module: &Path) -> Result<Vec<PluginDescriptor>, String> {
    let m = crate::Vst3Module::load(module).map_err(|e| e.to_string())?;
    Ok(m.descriptors())
}
