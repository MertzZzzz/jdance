// SPDX-License-Identifier: GPL-3.0-or-later
// No console window behind the GUI on Windows; console commands attach to
// the terminal they were started from (`attach_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]
//! jdance entry point.
//!
//! ```text
//! jdance [PROJECT]                       start the GUI
//! jdance render PROJECT OUT.wav [opts]   headless offline render
//! jdance devices                         list audio backends and devices
//! ```

mod browser_fs;
mod config;
mod host;
mod plugins;
mod render;
mod samples;

use jdance_audio_api::AudioBackend;
use jdance_domain::{
    DEFAULT_EXPORT_TAIL_SECONDS, ExportOptions, ExportRange, MAX_EXPORT_TAIL_SECONDS, MAX_REPEAT,
    Project,
};
use jdance_persistence::{WavExportFormat, load_project, save_project};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage:
  jdance [PROJECT] [--autoplay] [--smoke-test-frames N]
  jdance render PROJECT OUT.wav [--pattern NAME] [--loops N] [--tail SECONDS]
                [--sample-rate HZ] [--format pcm16|pcm24|float32]
    Renders the playlist; with --pattern or --loops, one pattern N times
    (an empty playlist renders the first pattern). The tail defaults to 5 s.
  jdance devices
  jdance --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The plugin scanner talks through pipes and needs no console.
    if matches!(
        args.first().map(String::as_str),
        Some("render" | "devices" | "--version" | "-V" | "--help" | "-h")
    ) {
        attach_console();
    }
    let result = match args.first().map(String::as_str) {
        Some("render") => cmd_render(&args[1..]),
        Some("devices") => cmd_devices(),
        // Internal: the out-of-process plugin scanner (PLG-012).
        Some("scan-plugin") => cmd_scan_plugin(&args[1..]),
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

/// A GUI-subsystem program has no console: write to the parent's, if any.
#[cfg(windows)]
#[allow(unsafe_code)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: no preconditions; without a parent console it just fails.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}

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
    let loops = flag_value(args, "--loops")?;
    let pattern_name = flag_value(args, "--pattern")?;
    let loops_n: u32 = parse(loops, "--loops", 1)?;
    if !(1..=MAX_REPEAT).contains(&loops_n) {
        return Err(format!("--loops must be 1..={MAX_REPEAT}"));
    }
    let tail: f32 = parse(
        flag_value(args, "--tail")?,
        "--tail",
        DEFAULT_EXPORT_TAIL_SECONDS,
    )?;
    if !(0.0..=MAX_EXPORT_TAIL_SECONDS).contains(&tail) {
        return Err(format!(
            "--tail must be 0..={MAX_EXPORT_TAIL_SECONDS} seconds"
        ));
    }
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
    let song = pattern_name.is_none() && loops.is_none() && project.arrangement_end_ticks() > 0;
    let range = if song {
        ExportRange::Song
    } else {
        let pattern = match pattern_name {
            Some(name) => project.patterns.iter().find(|p| p.name == name),
            None => project.patterns.first(),
        }
        .ok_or_else(|| format!("no pattern {}", pattern_name.unwrap_or("to render")))?;
        ExportRange::Pattern {
            pattern: pattern.id,
            loops: loops_n,
        }
    };
    let options = ExportOptions {
        range,
        tail_seconds: tail,
        format,
    };
    let outcome = render::render_to_wav(
        &project,
        project_path.parent(),
        Path::new(out),
        rate,
        &options,
        Some(&render::ExportPlugins {
            catalog: &plugins::Catalog::load_cached(),
            modules: &plugins::ModuleCache::default(),
        }),
    )?;
    if let Some(w) = &outcome.tail_warning {
        eprintln!("warning: {w}");
    }
    for w in &outcome.warnings {
        eprintln!("warning: {w}");
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

/// Loads one plugin module and prints its classes as JSON. Run as a
/// child process by the plugin scan so a crash only blocklists the module.
fn cmd_scan_plugin(args: &[String]) -> Result<(), String> {
    let [path] = args else {
        return Err("scan-plugin needs MODULE.vst3".into());
    };
    let d = jdance_plugin_vst3::scan_in_process(Path::new(path))?;
    println!("{}", serde_json::to_string(&d).map_err(|e| e.to_string())?);
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

    fn collect_assets(&self, dir: &Path, project: &mut Project) -> Result<usize, String> {
        jdance_persistence::collect_assets(project, dir).map_err(|e| e.to_string())
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
    // On start only changed modules are scanned (PLG-023).
    jdance_ui::Host::rescan_plugins(&mut audio, false);
    if args.iter().any(|a| a == "--autoplay") {
        // Load the project first so Play does not start on an empty song.
        let dir = path.as_deref().and_then(std::path::Path::parent);
        jdance_ui::Host::set_project(&mut audio, &project, dir);
        jdance_ui::Host::play(&mut audio);
    }

    // Renderer fallback (ADR-002, open): OpenGL first; if the system has
    // no OpenGL 2+ (RDP sessions, VMs, CI runners) retry with wgpu, which
    // uses DX12/Vulkan/Metal and on Windows can fall back to WARP.
    let mut state = Some((audio, project, path, startup_error));
    let mut last_error = String::new();
    // `JDANCE_RENDERER=glow`, `wgpu` or a comma-separated order (diagnostics).
    let renderers: Vec<eframe::Renderer> = std::env::var("JDANCE_RENDERER")
        .ok()
        .map(|v| {
            v.split(',')
                .filter_map(|r| match r.trim() {
                    "glow" => Some(eframe::Renderer::Glow),
                    "wgpu" => Some(eframe::Renderer::Wgpu),
                    _ => None,
                })
                .collect()
        })
        .filter(|v: &Vec<_>| !v.is_empty())
        .unwrap_or_else(|| vec![eframe::Renderer::Glow, eframe::Renderer::Wgpu]);
    for renderer in renderers {
        let Some((audio, project, path, startup_error)) = state.take() else {
            break;
        };
        // The closure owns the app state; if the renderer fails before
        // calling it, the state comes back through `returned`.
        let returned = std::rc::Rc::new(std::cell::RefCell::new(None));
        let back = std::rc::Rc::clone(&returned);
        let started = std::rc::Rc::new(std::cell::Cell::new(false));
        let started_flag = std::rc::Rc::clone(&started);
        let guard = ReturnOnDrop {
            state: Some((audio, project, path, startup_error)),
            back,
        };
        let options = native_options(renderer);
        let result = eframe::run_native(
            jdance_ui::APP_NAME,
            options,
            Box::new(move |_cc| {
                started_flag.set(true);
                let mut guard = guard;
                let (audio, project, path, startup_error) =
                    guard.state.take().expect("app state present");
                let mut app = jdance_ui::JdanceApp::new(audio, FileStore, project, path);
                if let Some(e) = startup_error {
                    app = app.with_startup_notice(jdance_ui::Notice::error(e));
                }
                if let Some(n) = smoke {
                    app = app.with_auto_close(n);
                }
                Ok(Box::new(app))
            }),
        );
        match result {
            Ok(()) => return Ok(()),
            Err(e) if !started.get() => {
                eprintln!("warning: {renderer:?} renderer unavailable: {e}");
                last_error = e.to_string();
                state = returned.borrow_mut().take();
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!("no usable graphics renderer: {last_error}"))
}

type AppState = (host::AudioHost, Project, Option<PathBuf>, Option<String>);

/// Hands the app state back if eframe drops the creator without calling it.
struct ReturnOnDrop {
    state: Option<AppState>,
    back: std::rc::Rc<std::cell::RefCell<Option<AppState>>>,
}

impl Drop for ReturnOnDrop {
    fn drop(&mut self) {
        if let Some(s) = self.state.take() {
            *self.back.borrow_mut() = Some(s);
        }
    }
}

fn native_options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(jdance_ui::APP_NAME)
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([640.0, 360.0])
            .with_drag_and_drop(true),
        renderer,
        ..Default::default()
    }
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
