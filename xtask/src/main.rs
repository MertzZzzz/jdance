// SPDX-License-Identifier: GPL-3.0-or-later
//! `cargo xtask <command>`
//!
//! - `check`: dependency-graph rules (ARCH-001) and SPDX headers (LIC-001);
//! - `gen-demo`: regenerates `assets/demo` (synthesized samples, CR-006).

use jdance_domain::{Channel, ChannelId, ChannelSource, Project, SampleRef};
use jdance_persistence::{WavExportFormat, save_project, write_wav};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// ARCH-001: allowed workspace-internal dependencies per crate.
const ALLOWED: &[(&str, &[&str])] = &[
    ("jdance-domain", &[]),
    ("jdance-commands", &["jdance-domain"]),
    ("jdance-dsp", &[]),
    (
        "jdance-engine",
        &[
            "jdance-domain",
            "jdance-dsp",
            "jdance-audio-api",
            "jdance-plugin-api",
        ],
    ),
    ("jdance-audio-api", &[]),
    ("jdance-audio-cpal", &["jdance-audio-api"]),
    ("jdance-persistence", &["jdance-domain"]),
    ("jdance-plugin-api", &[]),
    (
        "jdance-ui",
        &[
            "jdance-domain",
            "jdance-commands",
            "jdance-audio-api",
            "jdance-plugin-api",
        ],
    ),
    (
        "jdance-app",
        &[
            "jdance-domain",
            "jdance-commands",
            "jdance-dsp",
            "jdance-engine",
            "jdance-audio-api",
            "jdance-audio-cpal",
            "jdance-persistence",
            "jdance-plugin-api",
            "jdance-ui",
        ],
    ),
    ("jdance-test-support", &["jdance-domain", "jdance-dsp"]),
    ("jdance-rt-check", &[]),
    ("xtask", &["jdance-domain", "jdance-persistence"]),
];

/// Third-party crates that must never be reachable from these crates.
const FORBIDDEN_EXTERNAL: &[(&str, &[&str])] = &[
    ("jdance-domain", &["egui", "eframe", "winit", "cpal"]),
    ("jdance-dsp", &["egui", "eframe", "winit", "cpal"]),
    ("jdance-engine", &["egui", "eframe", "winit", "cpal"]),
    ("jdance-commands", &["egui", "eframe", "winit", "cpal"]),
    ("jdance-persistence", &["egui", "eframe", "winit", "cpal"]),
];

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1);
    let result = match cmd.as_deref() {
        Some("check") => check_deps().and_then(|()| check_spdx()),
        Some("gen-demo") => gen_demo(),
        _ => Err("usage: cargo xtask <check|gen-demo>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is in the workspace")
        .to_path_buf()
}

fn metadata() -> Result<Value, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root())
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn check_deps() -> Result<(), String> {
    let meta = metadata()?;
    let packages = meta["packages"].as_array().ok_or("no packages")?;
    let members: BTreeSet<&str> = meta["workspace_members"]
        .as_array()
        .ok_or("no members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let name_of = |id: &str| -> Option<String> {
        packages
            .iter()
            .find(|p| p["id"] == id)
            .and_then(|p| p["name"].as_str())
            .map(str::to_owned)
    };
    let mut errors = Vec::new();
    // Direct, non-dev workspace dependencies.
    for p in packages
        .iter()
        .filter(|p| members.contains(p["id"].as_str().unwrap_or("")))
    {
        let name = p["name"].as_str().unwrap_or_default();
        let Some((_, allowed)) = ALLOWED.iter().find(|(n, _)| *n == name) else {
            errors.push(format!("{name}: not listed in xtask ALLOWED (ARCH-001)"));
            continue;
        };
        for dep in p["dependencies"].as_array().into_iter().flatten() {
            let dep_name = dep["name"].as_str().unwrap_or_default();
            let is_dev = dep["kind"].as_str() == Some("dev");
            if dep["path"].is_string() && !is_dev && !allowed.contains(&dep_name) {
                errors.push(format!("{name} must not depend on {dep_name} (ARCH-001)"));
            }
        }
    }
    // Transitive normal dependencies via the resolve graph.
    let nodes = meta["resolve"]["nodes"]
        .as_array()
        .ok_or("no resolve graph")?;
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in nodes {
        let id = n["id"].as_str().unwrap_or_default().to_owned();
        let deps = n["deps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| {
                d["dep_kinds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|k| k["kind"].is_null())
            })
            .filter_map(|d| d["pkg"].as_str().map(str::to_owned))
            .collect();
        graph.insert(id, deps);
    }
    for (crate_name, forbidden) in FORBIDDEN_EXTERNAL {
        let Some(start) = packages
            .iter()
            .find(|p| p["name"] == *crate_name && members.contains(p["id"].as_str().unwrap_or("")))
            .and_then(|p| p["id"].as_str())
        else {
            continue;
        };
        let mut seen = BTreeSet::new();
        let mut stack = vec![start.to_owned()];
        while let Some(id) = stack.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(n) = name_of(&id)
                && forbidden.contains(&n.as_str())
            {
                errors.push(format!(
                    "{crate_name} transitively depends on {n} (ARCH-001)"
                ));
            }
            stack.extend(graph.get(&id).cloned().unwrap_or_default());
        }
    }
    if errors.is_empty() {
        println!("dependency graph: ok");
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn check_spdx() -> Result<(), String> {
    const HEADER: &str = "// SPDX-License-Identifier: GPL-3.0-or-later";
    let mut missing = Vec::new();
    let mut count = 0;
    for dir in ["crates", "xtask"] {
        walk(&root().join(dir), &mut |p| {
            if p.extension().is_some_and(|e| e == "rs") {
                count += 1;
                let first = std::fs::read_to_string(p)
                    .ok()
                    .and_then(|s| s.lines().next().map(str::to_owned));
                if first.as_deref() != Some(HEADER) {
                    missing.push(p.display().to_string());
                }
            }
        });
    }
    if missing.is_empty() {
        println!("SPDX headers: ok ({count} files)");
        Ok(())
    } else {
        Err(format!(
            "missing `{HEADER}` (LIC-001) in:\n{}",
            missing.join("\n")
        ))
    }
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == "target") {
            continue;
        }
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

/// Small deterministic PRNG for noise (xorshift32).
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}

const DEMO_RATE: u32 = 44_100;

fn kick() -> Vec<f32> {
    let n = (DEMO_RATE as f32 * 0.45) as usize;
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / DEMO_RATE as f32;
            let freq = 45.0 + 110.0 * (-t * 30.0).exp();
            phase += 2.0 * std::f32::consts::PI * freq / DEMO_RATE as f32;
            let click = if i < 60 {
                0.3 * (1.0 - i as f32 / 60.0)
            } else {
                0.0
            };
            (phase.sin() * (-t * 7.0).exp() + click) * 0.9
        })
        .collect()
}

fn snare() -> Vec<f32> {
    let n = (DEMO_RATE as f32 * 0.25) as usize;
    let mut noise = Noise(0x1234_5678);
    (0..n)
        .map(|i| {
            let t = i as f32 / DEMO_RATE as f32;
            let tone = (2.0 * std::f32::consts::PI * 185.0 * t).sin() * (-t * 25.0).exp();
            (noise.next() * (-t * 16.0).exp() * 0.6 + tone * 0.5) * 0.8
        })
        .collect()
}

fn hat() -> Vec<f32> {
    let n = (DEMO_RATE as f32 * 0.07) as usize;
    let mut noise = Noise(0x9e37_79b9);
    let mut prev = 0.0;
    (0..n)
        .map(|i| {
            let t = i as f32 / DEMO_RATE as f32;
            let x = noise.next();
            let hp = x - prev; // crude high-pass
            prev = x;
            hp * (-t * 60.0).exp() * 0.35
        })
        .collect()
}

fn gen_demo() -> Result<(), String> {
    let dir = root().join("assets").join("demo");
    std::fs::create_dir_all(dir.join("samples")).map_err(|e| e.to_string())?;
    let mut project = Project::new();
    project.tempo_bpm = 124.0;
    let parts: [(&str, Vec<f32>, &[u32], f32); 3] = [
        ("Kick", kick(), &[0, 4, 8, 12], 0.0),
        ("Snare", snare(), &[4, 12], 0.05),
        ("Hat", hat(), &[2, 6, 10, 14, 15], -0.2),
    ];
    for (name, data, steps, pan) in parts {
        let file = format!("samples/{}.wav", name.to_lowercase());
        write_wav(
            &dir.join(&file),
            DEMO_RATE,
            &data,
            &data,
            WavExportFormat::Pcm16,
        )
        .map_err(|e| e.to_string())?;
        let id = ChannelId(project.ids.next());
        let mut ch = Channel::new_sampler(id, name);
        ch.source = ChannelSource::Sampler {
            sample: Some(SampleRef { path: file }),
        };
        ch.pan = pan;
        project.channels.push(ch);
        let row = project.patterns[0].row_mut(id);
        for s in steps {
            row.steps[*s as usize].on = true;
        }
    }
    save_project(&dir.join("demo.jdance"), &project).map_err(|e| e.to_string())?;
    println!("wrote {}", dir.display());
    Ok(())
}
