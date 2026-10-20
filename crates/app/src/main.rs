// SPDX-License-Identifier: GPL-3.0-or-later
//! jdance entry point.
//!
//! ```text
//! jdance [PROJECT]                       start the GUI
//! jdance render PROJECT OUT.wav [opts]   headless offline render
//! jdance devices                         list audio backends and devices
//! ```

mod config;
mod host;
mod render;
mod samples;

use jdance_audio_api::AudioBackend;
use jdance_domain::Project;
use jdance_persistence::{WavExportFormat, load_project, save_project};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage:
  jdance [PROJECT] [--autoplay] [--smoke-test-frames N]
  jdance render PROJECT OUT.wav [--loops N] [--sample-rate HZ] [--format pcm16|pcm24|float32]
  jdance devices
  jdance --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("render") => cmd_render(&args[1..]),
        Some("devices") => cmd_devices(),
        Some("--version" | "-V") => {
            println!("jdance {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => cmd_gui(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, String> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .map(|s| Some(s.as_str()))
            .ok_or_else(|| format!("{name} needs a value")),
    }
}

fn positional(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if a == "--autoplay" {
            // Flag without a value.
        } else if a.starts_with("--") {
            skip = true;
        } else {
            out.push(a.as_str());
        }
    }
    out
}

fn parse<T: std::str::FromStr>(v: Option<&str>, name: &str, default: T) -> Result<T, String> {
    v.map_or(Ok(default), |s| {
        s.parse()
            .map_err(|_| format!("invalid value for {name}: {s}"))
    })
}

fn cmd_render(args: &[String]) -> Result<(), String> {
    let pos = positional(args);
    let [project_path, out] = pos.as_slice() else {
        return Err(format!("render needs PROJECT and OUT.wav\n{USAGE}"));
    };
    let loops: u32 = parse(flag_value(args, "--loops")?, "--loops", 1)?;
    let rate: u32 = parse(flag_value(args, "--sample-rate")?, "--sample-rate", 48_000)?;
    if !(8_000..=384_000).contains(&rate) {
        return Err(format!("unsupported sample rate {rate}"));
    }
    let format = match flag_value(args, "--format")?.unwrap_or("pcm24") {
        "pcm16" => WavExportFormat::Pcm16,
        "pcm24" => WavExportFormat::Pcm24,
        "float32" => WavExportFormat::Float32,
        f => return Err(format!("unknown format {f}")),
    };
    let project_path = Path::new(project_path);
    let project = load_project(project_path).map_err(|e| e.to_string())?;
    let outcome = render::render_to_wav(
        &project,
        project_path.parent(),
        Path::new(out),
        rate,
        loops,
        format,
    )?;
    for w in &outcome.warnings {
        eprintln!("warning: missing sample {w}");
    }
    let peak = outcome
        .audio
        .left
        .iter()
        .chain(&outcome.audio.right)
        .fold(0.0f32, |m, s| m.max(s.abs()));
    println!(
        "rendered {} frames at {rate} Hz to {out} (peak {:.1} dBFS)",
        outcome.audio.left.len(),
        jdance_dsp::gain_to_db(peak)
    );
    Ok(())
}

fn cmd_devices() -> Result<(), String> {
    let backends = jdance_audio_cpal::backends();
    if backends.is_empty() {
        println!("no audio backends available");
    }
    for b in &backends {
        println!("{} ({})", b.display_name(), b.id());
        match b.devices() {
            Ok(list) if list.is_empty() => println!("  no output devices"),
            Ok(list) => {
                for d in list {
                    let caps = b
                        .capabilities(&d.id)
                        .map(|c| {
                            format!(
                                "{} ch, rates {:?}, buffer {:?}",
                                c.output_channels, c.sample_rates, c.buffer_size
                            )
                        })
                        .unwrap_or_else(|e| format!("capabilities unavailable: {e}"));
                    let default = if d.is_default { " [default]" } else { "" };
                    println!("  {}{default}\n    id: {}\n    {caps}", d.name, d.id);
                }
            }
            Err(e) => println!("  error: {e}"),
        }
    }
    Ok(())
}

struct FileStore;

impl jdance_ui::ProjectStore for FileStore {
    fn load(&self, path: &Path) -> Result<Project, String> {
        load_project(path).map_err(|e| e.to_string())
    }

    fn save(&self, path: &Path, project: &Project) -> Result<(), String> {
        save_project(path, project).map_err(|e| e.to_string())
    }
}

fn cmd_gui(args: &[String]) -> Result<(), String> {
    let smoke: Option<u32> = flag_value(args, "--smoke-test-frames")?
        .map(|v| v.parse().map_err(|_| format!("invalid frame count {v}")))
        .transpose()?;
    let path = positional(args).first().map(PathBuf::from);
    let mut startup_error = None;
    let project = match &path {
        Some(p) => match load_project(p) {
            Ok(project) => Some(project),
            Err(e) => {
                startup_error = Some(format!("Could not open {}: {e}", p.display()));
                None
            }
        },
        None => None,
    };
    let path = project.as_ref().and(path);
    let project = project.unwrap_or_default();

    let mut audio = host::AudioHost::new(jdance_audio_cpal::backends());
    audio.open_initial();
    if args.iter().any(|a| a == "--autoplay") {
        jdance_ui::Host::play(&mut audio);
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(jdance_ui::APP_NAME)
            .with_inner_size([1100.0, 640.0])
            .with_min_inner_size([640.0, 360.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        jdance_ui::APP_NAME,
        options,
        Box::new(move |_cc| {
            let mut app = jdance_ui::JdanceApp::new(audio, FileStore, project, path);
            if let Some(e) = startup_error {
                app = app.with_startup_notice(jdance_ui::Notice::error(e));
            }
            if let Some(n) = smoke {
                app = app.with_auto_close(n);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_skips_flag_values() {
        let args: Vec<String> = ["a.jdance", "--loops", "3", "out.wav"]
            .map(String::from)
            .to_vec();
        assert_eq!(positional(&args), ["a.jdance", "out.wav"]);
        assert_eq!(flag_value(&args, "--loops").unwrap(), Some("3"));
        assert!(flag_value(&["--loops".to_owned()], "--loops").is_err());
    }
}
