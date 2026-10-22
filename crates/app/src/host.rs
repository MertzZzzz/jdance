// SPDX-License-Identifier: GPL-3.0-or-later
//! Wires the UI to the engine, the audio backends and the sample cache.

use crate::browser_fs::BrowserFs;
use crate::config::{self, UserSettings};
use crate::plugins::{Catalog, PluginHost};
use crate::recording::{self, MonitorControl, MonitorMix, Session, Shared};
use crate::render::{ExportTags, estimated_tail_seconds, render_to_file};
use crate::samples::SampleCache;
use jdance_audio_api::{
    AudioBackend, AudioError, BackendId, BufferRequest, DeviceCapabilities, DeviceId, DeviceInfo,
    DeviceKey, DeviceSettings, InputStream, OutputStream, RenderCallback, StreamState,
};
use jdance_domain::PluginTarget;
use jdance_domain::TrackId;
use jdance_domain::{ExportOptions, Project, SampleRef};
use jdance_engine::{
    EngineHandle, EngineProcessor, FeedbackReading, Meters, PlayMode, compile_full, create_engine,
    param_only_changes, samples_per_tick,
};
use jdance_persistence::resolve_sample_path;
use jdance_plugin_api::{PluginController, PluginDescriptor, PluginKind};
use jdance_ui::{
    FinishedTake, Host, InputDevice, LatencyInfo, Listing, MeterReadings, Notice, PluginParamView,
    RecordingSettings, RecordingState, Recovery, SampleStatus, StreamStatus, TransportMode,
    TransportStatus,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, channel};

/// Rate used for samples and export when no device is open.
const FALLBACK_RATE: u32 = 48_000;

/// Meter display state: peaks read from the engine with a visual fall-off
/// (the engine reports raw peaks since the previous read).
#[derive(Debug, Default)]
struct MeterState {
    readings: MeterReadings,
}

impl MeterState {
    /// Per UI frame: ≈ 20 dB/s fall-off at 60 frames per second.
    const FALL: f32 = 0.93;

    fn update(&mut self, m: &Meters) {
        let fall = |old: f32, new: f32| new.max(old * Self::FALL);
        let mut buses = Vec::with_capacity(self.readings.buses.len());
        for slot in m.buses.iter() {
            let (id, l, r) = slot.take();
            if id == u64::MAX {
                continue;
            }
            let id = TrackId(id);
            let (ol, or) = self.readings.bus(id);
            buses.push((id, fall(ol, l), fall(or, r)));
        }
        let (_, l, r) = m.output.take();
        let (ol, or) = self.readings.output;
        let gain = m.take_limiter_gain();
        self.readings = MeterReadings {
            buses,
            output: (fall(ol, l), fall(or, r)),
            // Show reduction recovering visually like a meter.
            limiter_gain: gain.min(1.0 - (1.0 - self.readings.limiter_gain) * Self::FALL),
        };
    }
}

/// Adapts the engine processor to the audio-api callback; the input
/// monitor is added after the engine.
struct EngineCallback(EngineProcessor, MonitorMix);

impl RenderCallback for EngineCallback {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.0.render(left, right);
        self.1.add(left, right);
    }
}

/// Recording input and the take being written (spec 0.2.0 §7).
struct Recorder {
    shared: Arc<Shared>,
    input: Option<Box<dyn InputStream>>,
    /// Recording ring reader while no take holds it.
    ring: Option<rtrb::Consumer<f32>>,
    session: Option<Session>,
    finished: Option<Result<FinishedTake, String>>,
    /// Monitor feed of the open output.
    monitor: Option<MonitorControl>,
    /// Why no input is open.
    problem: Option<String>,
    /// Input plus output latency the driver reports for the open input,
    /// in frames; taken off takes once (spec 0.2.2 ASIO-042).
    round_trip: u32,
    monitor_on: bool,
    project_dir: Option<PathBuf>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared::default()),
            input: None,
            ring: None,
            session: None,
            finished: None,
            monitor: None,
            problem: None,
            round_trip: 0,
            monitor_on: false,
            project_dir: None,
        }
    }
}

struct Active {
    key: DeviceKey,
    stream: Box<dyn OutputStream>,
    engine: EngineHandle,
    failure_reported: bool,
}

pub struct AudioHost {
    backends: Vec<Box<dyn AudioBackend>>,
    active: Option<Active>,
    reading: FeedbackReading,
    latency: LatencyInfo,
    meters: MeterState,
    mode: TransportMode,
    project: Project,
    project_dir: Option<PathBuf>,
    needs_compile: bool,
    samples: SampleCache,
    settings: UserSettings,
    settings_path: Option<PathBuf>,
    notices: Vec<Notice>,
    exports: Vec<Receiver<Result<String, String>>>,
    /// Progress (‰) and cancel flag of the running export.
    export_state: Option<(
        Arc<std::sync::atomic::AtomicU32>,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
    plugins: PluginHost,
    /// Incremented for every new engine; instances sent to an older one
    /// are gone.
    engine_generation: u64,
    scan: Option<Receiver<Catalog>>,
    /// Progress (done, total, module) and cancel flag of the scan.
    scan_state: Option<ScanState>,
    browser: BrowserFs,
    /// File waiting to load before its preview starts.
    /// File to preview once loaded, and the part of it (fractions).
    pending_preview: Option<(PathBuf, (f64, f64))>,
    /// Background imports: results and the next ticket.
    imports: (
        std::sync::mpsc::Sender<ImportResult>,
        std::sync::mpsc::Receiver<ImportResult>,
    ),
    next_import: u64,
    recorder: Recorder,
    scripts: ScriptHost,
    /// ASIO-025: the device closed while the window is inactive.
    suspended: Option<(DeviceKey, DeviceSettings)>,
}

/// Script tools: the loaded scripts and the run in flight.
#[derive(Default)]
struct ScriptHost {
    list: Vec<jdance_scripts::Script>,
    loaded_at: Option<std::time::Instant>,
    next: u64,
    running: Option<(
        u64,
        Arc<std::sync::atomic::AtomicBool>,
        Receiver<ScriptResult>,
    )>,
    done: Vec<(u64, ScriptResult)>,
}

type ScriptResult = Result<jdance_commands::note_tools::ScriptOutput, String>;

impl ScriptHost {
    fn folder() -> Option<PathBuf> {
        config::config_dir().map(|d| d.join("scripts"))
    }

    /// Built-ins plus the user folder, read again every few seconds.
    fn refresh(&mut self, notices: &mut Vec<Notice>) {
        if self
            .loaded_at
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3))
        {
            return;
        }
        let first = self.loaded_at.is_none();
        self.loaded_at = Some(std::time::Instant::now());
        let mut list = jdance_scripts::builtins();
        if let Some(dir) = Self::folder() {
            let (user, errors) = jdance_scripts::load_folder(&dir);
            list.extend(user);
            if first {
                for e in errors {
                    notices.push(Notice::warning(format!("Script not loaded: {e}")));
                }
            }
        }
        self.list = list;
    }
}

impl std::fmt::Debug for AudioHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioHost")
            .field("device", &self.active.as_ref().map(|a| &a.key))
            .finish_non_exhaustive()
    }
}

impl AudioHost {
    pub fn new(backends: Vec<Box<dyn AudioBackend>>) -> Self {
        let settings_path = config::settings_path();
        let mut notices = Vec::new();
        let settings = match &settings_path {
            Some(p) => {
                let (s, warning) = config::load(p);
                notices.extend(warning.map(Notice::warning));
                s
            }
            None => UserSettings::default(),
        };
        let mut browser = BrowserFs::new();
        browser.index(
            settings
                .library_folders
                .iter()
                .map(config::LibraryFolder::root)
                .collect(),
        );
        let (plugins, crashed) = PluginHost::new(Catalog::load_cached());
        if let Some(name) = crashed {
            notices.push(Notice::error(format!(
                "The plugin {name} crashed jdance while loading. It will not be loaded \
                 automatically; use \"Load again\" in its mixer slot to retry."
            )));
        }
        Self {
            backends,
            active: None,
            reading: FeedbackReading::default(),
            latency: LatencyInfo::default(),
            meters: MeterState::default(),
            mode: TransportMode::Song,
            project: Project::new(),
            project_dir: None,
            needs_compile: true,
            samples: SampleCache::new(FALLBACK_RATE),
            settings,
            settings_path,
            notices,
            exports: Vec::new(),
            export_state: None,
            plugins,
            engine_generation: 0,
            scan: None,
            scan_state: None,
            browser,
            pending_preview: None,
            imports: std::sync::mpsc::channel(),
            next_import: 0,
            recorder: Recorder::default(),
            scripts: ScriptHost::default(),
            suspended: None,
        }
    }

    /// Opens the configured input at the output's rate (or closes it).
    fn open_input(&mut self) {
        let rec = &mut self.recorder;
        if rec.session.is_some() {
            return;
        }
        rec.input = None;
        rec.ring = None;
        rec.problem = None;
        let cfg = self.settings.recording.clone();
        let Some(device) = cfg.device else {
            rec.problem = Some("Choose an input in Options → Recording.".into());
            return;
        };
        let Some(active) = &self.active else {
            rec.problem = Some("Open an audio output first (recording uses its rate).".into());
            return;
        };
        let Some(backend) = self.backends.iter().find(|b| b.id() == active.key.backend) else {
            return;
        };
        let rate = active.engine.sample_rate();
        let (capture, ring, mon) = recording::capture(Arc::clone(&rec.shared), rate);
        let request = jdance_audio_api::InputRequest {
            sample_rate: rate,
            channels: cfg.channels,
        };
        match backend.open_input(&DeviceId(device), &request, Box::new(capture)) {
            Ok(stream) => {
                rec.round_trip = stream.info().round_trip.unwrap_or(0);
                rec.input = Some(stream);
                rec.ring = Some(ring);
                if let Some(m) = &mut rec.monitor {
                    m.send(mon);
                }
            }
            Err(e) => {
                rec.problem = Some(format!(
                    "The input could not be opened: {e}. If the system asks for microphone \
                     access, allow it and choose the input again."
                ));
            }
        }
    }

    /// Where takes are written: the project's `assets/`, or the settings
    /// folder for an unsaved project.
    fn recording_dir(project_dir: Option<&Path>) -> Option<PathBuf> {
        match project_dir {
            Some(d) => Some(d.join(jdance_persistence::ASSETS_DIR)),
            None => config::config_dir().map(|d| d.join("recordings")),
        }
    }

    fn poll_recording(&mut self) {
        let rec = &mut self.recorder;
        if let Some(m) = &mut rec.monitor {
            m.collect();
        }
        if let Some(input) = &rec.input
            && let StreamState::Failed(e) = input.state()
        {
            rec.shared.recording.store(false, Relaxed);
            rec.problem = Some(format!("The input stopped: {e}"));
            rec.input = None;
            self.notices
                .push(Notice::error(format!("Recording input failed: {e}")));
        }
        let rec = &mut self.recorder;
        if rec.session.as_ref().is_some_and(Session::is_finished) {
            let (ring, result) = rec.session.take().expect("checked").join();
            if rec.input.is_some() {
                rec.ring = ring;
            }
            let dir = rec.project_dir.clone();
            let latency_frames = rec.round_trip;
            rec.finished = Some(result.and_then(|take| {
                if take.frames == 0 {
                    let _ = std::fs::remove_file(&take.path);
                    return Err(take
                        .problem
                        .unwrap_or_else(|| "nothing was recorded".into()));
                }
                let audio = import_file(&take.path, dir.as_deref())?;
                Ok(FinishedTake {
                    audio,
                    problem: take.problem,
                    latency_frames,
                })
            }));
        }
    }

    fn plugins_of_kind(&self, kind: PluginKind) -> Vec<PluginDescriptor> {
        let mut v: Vec<_> = self
            .plugins
            .catalog
            .plugins()
            .filter(|d| d.kind == kind)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            (&a.plugin.vendor, &a.plugin.name).cmp(&(&b.plugin.vendor, &b.plugin.name))
        });
        v
    }

    /// Plugin folders scanned: the standard ones plus the user's
    /// (PLG-020), without the disabled ones (spec 0.2.0 §6).
    fn plugin_roots(&self) -> Vec<PathBuf> {
        let mut roots = jdance_plugin_vst3::default_search_paths();
        roots.extend(self.settings.plugin_paths.iter().cloned());
        roots.retain(|r| !self.settings.disabled_plugin_paths.contains(r));
        roots
    }

    /// Starts a background scan. Unchanged modules keep their cached
    /// result unless `full` (PLG-023).
    fn start_plugin_scan(&mut self, full: bool) {
        if self.scan.is_some() {
            return;
        }
        let Ok(scanner) = std::env::current_exe() else {
            self.notices.push(Notice::warning(
                "Cannot scan plugins: executable path unknown.",
            ));
            return;
        };
        let (tx, rx) = channel();
        let catalog = self.plugins.catalog.clone();
        let roots = self.plugin_roots();
        let progress = Arc::new(std::sync::Mutex::new((0usize, 0usize, PathBuf::new())));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.scan_state = Some((Arc::clone(&progress), Arc::clone(&cancel)));
        std::thread::spawn(move || {
            let report = |done: usize, total: usize, module: &Path| {
                if let Ok(mut p) = progress.lock() {
                    *p = (done, total, module.to_path_buf());
                }
            };
            let _ = tx.send(catalog.rescan(&roots, &scanner, full, &report, &cancel));
        });
        self.scan = Some(rx);
    }

    fn backend(&self, id: &BackendId) -> Option<&dyn AudioBackend> {
        self.backends
            .iter()
            .find(|b| &b.id() == id)
            .map(|b| b.as_ref())
    }

    /// Opens the last used device, or the system default on first start.
    /// A saved device that is missing is reported, never silently replaced
    /// (Amendment 01 §5).
    pub fn open_initial(&mut self) {
        if let Some(key) = self.settings.audio.last.clone() {
            let present = self
                .backend(&key.backend)
                .and_then(|b| b.devices().ok())
                .is_some_and(|list| list.iter().any(|d| d.id == key.device));
            if !present {
                self.notices.push(Notice::warning(format!(
                    "The audio device used last ({} on {}) is not available. \
                     Choose an output in Audio settings.",
                    key.device, key.backend
                )));
                return;
            }
            let settings = self.settings.audio.get(&key).unwrap_or(DeviceSettings {
                sample_rate: FALLBACK_RATE,
                buffer: BufferRequest::Default,
                first_output_channel: 0,
            });
            if let Err(e) = self.open_device(key.clone(), settings) {
                self.notices.push(Notice::error(format!(
                    "Could not open {} on {}: {e}. Choose an output in Audio settings.",
                    key.device, key.backend
                )));
            }
            return;
        }
        if self.backends.is_empty() {
            self.notices.push(Notice::error(
                "No audio backend is available on this system.",
            ));
            return;
        }
        // The first backend with a system default output (ASIO, listed
        // first on Windows, has none: its drivers are chosen explicitly).
        let default = self.backends.iter().find_map(|b| {
            b.devices()
                .ok()
                .and_then(|list| list.into_iter().find(|d| d.is_default))
                .map(|d| (b.as_ref(), d))
        });
        let Some((backend, device)) = default else {
            self.notices.push(Notice::warning(
                "No default audio output was found. Choose one in Audio settings.",
            ));
            return;
        };
        let key = DeviceKey {
            backend: backend.id(),
            device: device.id,
        };
        let rate = backend
            .capabilities(&key.device)
            .ok()
            .and_then(|c| c.default_sample_rate)
            .unwrap_or(FALLBACK_RATE);
        let settings = DeviceSettings {
            sample_rate: rate,
            buffer: BufferRequest::Default,
            first_output_channel: 0,
        };
        if let Err(e) = self.open_device(key, settings) {
            self.notices.push(Notice::error(format!(
                "Could not open the default output: {e}. Choose one in Audio settings."
            )));
        }
    }

    fn save_settings(&mut self) {
        if let Some(p) = &self.settings_path
            && let Err(e) = config::save(p, &self.settings)
        {
            self.notices
                .push(Notice::warning(format!("Could not save settings: {e}")));
        }
    }

    fn engine_rate(&self) -> u32 {
        self.active
            .as_ref()
            .map_or(FALLBACK_RATE, |a| a.engine.sample_rate())
    }

    fn resolve(&self, s: &SampleRef) -> PathBuf {
        resolve_sample_path(self.project_dir.as_deref(), s)
    }

    fn recompile(&mut self) {
        let rate = self.engine_rate();
        let generation = self.engine_generation;
        // Instances exist without a device too (the generic editor works);
        // their processors wait for an engine.
        for m in self.plugins.sync(&self.project, rate, generation) {
            self.notices.push(Notice::warning(m));
        }
        let dir = self.project_dir.as_deref();
        let samples = &self.samples;
        let mode = match self.mode {
            TransportMode::Song => PlayMode::Song,
            TransportMode::Pattern(id) => PlayMode::Pattern(id),
        };
        let plugins = &mut self.plugins;
        let has_engine = self.active.is_some();
        let snapshot = compile_full(
            &self.project,
            rate,
            mode,
            |s| samples.converted(&resolve_sample_path(dir, s)),
            &mut |id, _| {
                if has_engine {
                    plugins.supply(id, generation)
                } else {
                    // No engine: only the latency display uses this.
                    plugins.latency_only(id)
                }
            },
        );
        self.latency = LatencyInfo {
            sample_rate: rate,
            total: snapshot.latency,
            per_track: snapshot.track_latencies(),
            clamped: snapshot.pdc_clamped,
        };
        let Some(active) = &mut self.active else {
            // Nothing plays; the compile above still feeds the latency display.
            self.needs_compile = false;
            return;
        };
        match active.engine.set_snapshot(snapshot) {
            Ok(()) => {
                self.plugins.delivered();
                self.needs_compile = false;
            }
            // Queue full: retry on the next poll.
            Err(_) => {
                self.plugins.undelivered();
                self.needs_compile = true;
            }
        }
    }
}

impl Host for AudioHost {
    fn poll(&mut self) {
        if self.samples.poll() {
            self.needs_compile = true;
        }
        self.browser.poll();
        if let Some((path, range)) = self.pending_preview.clone() {
            match self.samples.status(&path) {
                Some(SampleStatus::Failed(e)) => {
                    self.pending_preview = None;
                    self.notices.push(Notice::warning(format!(
                        "Cannot preview {}: {e}",
                        path.display()
                    )));
                }
                Some(SampleStatus::Ready { .. }) => {
                    if let (Some(buf), Some(a)) = (self.samples.converted(&path), &mut self.active)
                        && buf.sample_rate == a.engine.sample_rate()
                    {
                        self.pending_preview = None;
                        let buf = if range == (0.0, 1.0) {
                            buf
                        } else {
                            let n = buf.len() as f64;
                            let lo = (range.0.clamp(0.0, 1.0) * n) as usize;
                            let hi = ((range.1.clamp(0.0, 1.0) * n) as usize).max(lo);
                            std::sync::Arc::new(jdance_dsp::SampleBuffer {
                                sample_rate: buf.sample_rate,
                                left: buf.left[lo..hi].to_vec(),
                                right: buf.right[lo..hi].to_vec(),
                            })
                        };
                        let _ = a.engine.preview(Some(buf));
                    }
                }
                _ => {}
            }
        }
        if self.plugins.poll() {
            self.needs_compile = true;
        }
        self.plugins.poll_editors();
        if let Some(rx) = &self.scan
            && let Ok(catalog) = rx.try_recv()
        {
            self.scan = None;
            self.scan_state = None;
            if let Err(e) = catalog.save_cached() {
                self.notices.push(Notice::warning(format!(
                    "Could not save the plugin list: {e}"
                )));
            }
            let blocked = catalog.blocked().count();
            if blocked > self.plugins.catalog.blocked().count() {
                self.notices.push(Notice::warning(format!(
                    "{blocked} plugin module(s) failed to scan and were blocklisted."
                )));
            }
            self.plugins.catalog = catalog;
            // Missing plugins may be installed now.
            self.needs_compile = true;
        }
        if self.needs_compile {
            self.recompile();
        }
        self.poll_recording();
        if let Some(a) = &mut self.active {
            a.engine.collect_garbage();
            self.reading = a.engine.feedback().read();
            self.meters.update(&a.engine.feedback().meters);
            if let StreamState::Failed(e) = a.stream.state()
                && !a.failure_reported
            {
                a.failure_reported = true;
                self.notices.push(Notice::error(format!(
                    "Audio stream failed: {e}. Reopen the device in Audio settings."
                )));
            }
        } else {
            self.reading = FeedbackReading::default();
        }
        let mut finished = Vec::new();
        for (i, rx) in self.exports.iter().enumerate() {
            if let Ok(result) = rx.try_recv() {
                finished.push(i);
                self.export_state = None;
                self.notices.push(match result {
                    Ok(msg) => Notice::info(msg),
                    Err(e) if e == crate::render::CANCELLED => {
                        Notice::info("Export cancelled; no file was written.")
                    }
                    Err(e) => Notice::error(format!("Export failed: {e}; no file was written.")),
                });
            }
        }
        for i in finished.into_iter().rev() {
            self.exports.remove(i);
        }
    }

    fn set_project(&mut self, project: &Project, project_dir: Option<&Path>) {
        let same_dir = self.project_dir.as_deref() == project_dir;
        // Fast path: only effect parameters changed → smoothed parameter
        // commands instead of rebuilding effect buffers.
        if same_dir
            && !self.needs_compile
            && let Some(changes) = param_only_changes(&self.project, project)
            && let Some(a) = &mut self.active
        {
            let sent = changes
                .iter()
                .all(|c| a.engine.set_param(c.insert, c.index, c.value).is_ok());
            self.project = project.clone();
            if !sent {
                self.needs_compile = true;
                self.recompile();
            }
            return;
        }
        self.project = project.clone();
        self.project_dir = project_dir.map(Path::to_path_buf);
        let assets: Vec<SampleRef> = project
            .assets
            .iter()
            .map(|a| SampleRef {
                path: a.path.clone(),
            })
            .collect();
        let paths: Vec<PathBuf> = project
            .channels
            .iter()
            .filter_map(|c| c.sample())
            .chain(&assets)
            .map(|s| self.resolve(s))
            .collect();
        for p in paths {
            self.samples.request(&p);
        }
        self.needs_compile = true;
        self.recompile();
    }

    fn play(&mut self) {
        match &mut self.active {
            Some(a) => {
                if a.engine.play().is_err() {
                    self.notices
                        .push(Notice::warning("Engine is busy; try again."));
                }
            }
            None => self.notices.push(Notice::warning(
                "No audio device is open. Choose an output in Audio settings.",
            )),
        }
    }

    fn set_mode(&mut self, mode: TransportMode) {
        if self.mode != mode {
            self.mode = mode;
            self.needs_compile = true;
            self.recompile();
        }
    }

    fn seek_ticks(&mut self, ticks: u64) {
        let spt = samples_per_tick(self.project.tempo_bpm, self.engine_rate());
        if let Some(a) = &mut self.active {
            let _ = a.engine.seek((ticks as f64 * spt).round() as u64);
        }
    }

    fn stop(&mut self) {
        if let Some(a) = &mut self.active {
            let _ = a.engine.stop();
        }
    }

    fn panic(&mut self) {
        if let Some(a) = &mut self.active {
            let _ = a.engine.panic();
        }
    }

    fn live_note(&mut self, channel: jdance_domain::ChannelId, key: u8, velocity: Option<f32>) {
        if let Some(a) = &mut self.active {
            let _ = match velocity {
                Some(v) => a.engine.live_note(channel, key, v),
                None => a.engine.live_note_off(channel, key),
            };
        }
    }

    fn transport(&self) -> TransportStatus {
        let r = &self.reading;
        let spt = samples_per_tick(self.project.tempo_bpm, self.engine_rate());
        TransportStatus {
            playing: r.playing,
            position_ticks: self
                .active
                .is_some()
                .then(|| (r.position as f64 / spt).round() as u64),
            loop_pos: (r.loop_len > 0).then(|| r.position as f32 / r.loop_len as f32),
            peak_load: r.peak_load,
            overloads: r.overloads,
            voices: r.active_voices,
            dropped_commands: r.dropped_commands,
        }
    }

    fn latency(&self) -> LatencyInfo {
        self.latency.clone()
    }

    fn meters(&self) -> MeterReadings {
        if self.active.is_none() {
            return MeterReadings::default();
        }
        self.meters.readings.clone()
    }

    fn stream(&self) -> Option<StreamStatus> {
        self.active.as_ref().map(|a| StreamStatus {
            info: a.stream.info().clone(),
            stats: a.stream.stats().read(),
            state: a.stream.state(),
        })
    }

    fn import_audio(&mut self, path: &Path, project_dir: Option<&Path>) -> Option<u64> {
        self.next_import += 1;
        let ticket = self.next_import;
        let tx = self.imports.0.clone();
        let (path, dir) = (path.to_path_buf(), project_dir.map(Path::to_path_buf));
        std::thread::Builder::new()
            .name("jdance-import".into())
            .spawn(move || {
                let _ = tx.send((ticket, import_file(&path, dir.as_deref())));
            })
            .ok()?;
        Some(ticket)
    }

    fn input_devices(&mut self) -> Vec<InputDevice> {
        let Some(backend) = self
            .active
            .as_ref()
            .and_then(|a| self.backends.iter().find(|b| b.id() == a.key.backend))
            .or_else(|| self.backends.first())
        else {
            return Vec::new();
        };
        backend
            .input_devices()
            .unwrap_or_default()
            .into_iter()
            .map(|d| InputDevice {
                channels: backend.input_channels(&d.id).unwrap_or(0),
                names: backend.channel_names(&d.id).unwrap_or_default(),
                id: d.id.0,
                name: d.name,
                is_default: d.is_default,
            })
            .collect()
    }

    fn recording_settings(&self) -> RecordingSettings {
        let c = &self.settings.recording;
        RecordingSettings {
            device: c.device.clone(),
            channels: c.channels,
            monitor: self.recorder.monitor_on,
            offset_ms: c.offset_ms,
        }
    }

    fn set_recording_settings(&mut self, s: RecordingSettings) -> Result<(), String> {
        let reopen = s.device != self.settings.recording.device
            || s.channels != self.settings.recording.channels
            || self.recorder.input.is_none();
        self.settings.recording = config::RecordingConfig {
            device: s.device,
            channels: s.channels,
            offset_ms: s.offset_ms.clamp(-1000.0, 1000.0),
        };
        self.save_settings();
        self.recorder.monitor_on = s.monitor;
        self.recorder.shared.monitor.store(s.monitor, Relaxed);
        if reopen {
            self.open_input();
        }
        match &self.recorder.problem {
            Some(p) if self.settings.recording.device.is_some() => Err(p.clone()),
            _ => Ok(()),
        }
    }

    fn input_meter(&mut self) -> (f32, f32) {
        self.recorder.shared.take_peaks()
    }

    fn recording_state(&self) -> RecordingState {
        let rec = &self.recorder;
        if rec.session.is_some() {
            if rec.shared.recording.load(Relaxed) {
                let rate = f64::from(self.engine_rate().max(1));
                return RecordingState::Recording {
                    seconds: rec.shared.captured.load(Relaxed) as f64 / rate,
                };
            }
            return RecordingState::Finishing;
        }
        if rec.input.is_some() {
            return RecordingState::Ready;
        }
        RecordingState::NoInput(
            rec.problem
                .clone()
                .unwrap_or_else(|| "Choose an input in Options → Recording.".into()),
        )
    }

    fn start_recording(&mut self, name: &str, project_dir: Option<&Path>) -> Result<(), String> {
        if self.recorder.session.is_some() {
            return Err("a recording is already running".into());
        }
        let channels = self.settings.recording.channels.count();
        let rate = self.engine_rate();
        let rec = &mut self.recorder;
        let Some(ring) = rec.ring.take() else {
            return Err(rec
                .problem
                .clone()
                .unwrap_or_else(|| "no recording input is open".into()));
        };
        let dir = Self::recording_dir(project_dir)
            .ok_or_else(|| "no folder for recordings".to_owned())?;
        if let Err(e) = std::fs::create_dir_all(&dir) {
            rec.ring = Some(ring);
            return Err(format!("cannot create {}: {e}", dir.display()));
        }
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || " -_".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let part = (1..)
            .map(|n| {
                let stem = if n == 1 {
                    safe.clone()
                } else {
                    format!("{safe}-{n}")
                };
                dir.join(format!("{stem}{}", recording::PART_SUFFIX))
            })
            .find(|p| !p.exists())
            .expect("unbounded");
        rec.shared.overflow.store(false, Relaxed);
        rec.shared.captured.store(0, Relaxed);
        match Session::start(part, channels, rate, ring, Arc::clone(&rec.shared)) {
            Ok(session) => {
                rec.session = Some(session);
                rec.project_dir = project_dir.map(Path::to_path_buf);
                rec.shared.recording.store(true, Relaxed);
                Ok(())
            }
            Err((ring, e)) => {
                rec.ring = Some(ring);
                Err(e)
            }
        }
    }

    fn stop_recording(&mut self) {
        self.recorder.shared.recording.store(false, Relaxed);
    }

    fn take_recording(&mut self) -> Option<Result<FinishedTake, String>> {
        self.recorder.finished.take()
    }

    fn unfinished_recordings(&mut self, project_dir: Option<&Path>) -> Vec<PathBuf> {
        let dirs: Vec<PathBuf> = [Self::recording_dir(project_dir), Self::recording_dir(None)]
            .into_iter()
            .flatten()
            .collect();
        let current = self.recorder.session.as_ref().map(|s| s.part.clone());
        recording::unfinished(&dirs)
            .into_iter()
            .filter(|p| Some(p) != current.as_ref())
            .collect()
    }

    fn resolve_unfinished(
        &mut self,
        path: &Path,
        action: Recovery,
        project_dir: Option<&Path>,
    ) -> Result<Option<jdance_ui::ImportedAudio>, String> {
        if action == Recovery::Delete {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
            return Ok(None);
        }
        recording::repair(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let done = recording::final_name(path);
        std::fs::rename(path, &done).map_err(|e| e.to_string())?;
        match action {
            Recovery::Import => import_file(&done, project_dir).map(Some),
            _ => {
                self.notices.push(Notice::info(format!(
                    "Recording kept as {}",
                    done.display()
                )));
                Ok(None)
            }
        }
    }

    fn take_imports(&mut self) -> Vec<(u64, Result<jdance_ui::ImportedAudio, String>)> {
        self.imports.1.try_iter().collect()
    }

    fn sample_status(
        &self,
        sample: &SampleRef,
        project_dir: Option<&Path>,
    ) -> Option<SampleStatus> {
        self.samples
            .status(&resolve_sample_path(project_dir, sample))
    }

    fn sample_peaks(
        &self,
        sample: &SampleRef,
        project_dir: Option<&Path>,
    ) -> Option<std::sync::Arc<Vec<f32>>> {
        self.samples
            .peaks(&resolve_sample_path(project_dir, sample))
    }

    fn backends(&self) -> Vec<(BackendId, String)> {
        self.backends
            .iter()
            .map(|b| (b.id(), b.display_name()))
            .collect()
    }

    fn devices(&self, backend: &BackendId) -> Result<Vec<DeviceInfo>, AudioError> {
        self.backend(backend)
            .ok_or_else(|| AudioError::Unsupported(format!("backend {backend}")))?
            .devices()
    }

    fn capabilities(&self, key: &DeviceKey) -> Result<DeviceCapabilities, AudioError> {
        self.backend(&key.backend)
            .ok_or_else(|| AudioError::Unsupported(format!("backend {}", key.backend)))?
            .capabilities(&key.device)
    }

    fn saved_settings(&self, key: &DeviceKey) -> Option<DeviceSettings> {
        self.settings.audio.get(key)
    }

    fn current_device(&self) -> Option<DeviceKey> {
        self.active.as_ref().map(|a| a.key.clone())
    }

    fn open_device(&mut self, key: DeviceKey, settings: DeviceSettings) -> Result<(), AudioError> {
        // AUD-014: stop and close the old stream before opening a new one.
        self.close_device();
        self.suspended = None;
        let backend = self
            .backend(&key.backend)
            .ok_or_else(|| AudioError::Unsupported(format!("backend {}", key.backend)))?;
        let (engine, processor) = create_engine(settings.sample_rate);
        let (mix, control) = recording::monitor_mix();
        let request = jdance_audio_api::StreamRequest {
            sample_rate: settings.sample_rate,
            buffer: settings.buffer,
            first_output_channel: settings.first_output_channel,
        };
        let stream = backend.open_output(
            &key.device,
            &request,
            Box::new(EngineCallback(processor, mix)),
        )?;
        self.engine_generation += 1;
        self.active = Some(Active {
            key: key.clone(),
            stream,
            engine,
            failure_reported: false,
        });
        self.samples.set_rate(settings.sample_rate);
        self.settings.audio.set(key.clone(), settings);
        self.settings.audio.last = Some(key);
        self.save_settings();
        self.needs_compile = true;
        self.recompile();
        // The input follows the output's rate and feeds its monitor.
        self.recorder.monitor = Some(control);
        self.open_input();
        Ok(())
    }

    fn close_device(&mut self) {
        // Dropping the stream stops the callback; the processor and its
        // snapshot are freed on this (non-realtime) thread.
        self.recorder.shared.recording.store(false, Relaxed);
        self.active = None;
        self.recorder.monitor = None;
    }

    fn open_control_panel(&mut self, key: &DeviceKey) -> Result<(), AudioError> {
        self.backend(&key.backend)
            .ok_or_else(|| AudioError::Unsupported(format!("backend {}", key.backend)))?
            .open_control_panel(&key.device)
    }

    fn release_on_blur(&self) -> bool {
        self.settings.release_asio_on_blur
    }

    fn set_release_on_blur(&mut self, on: bool) {
        self.settings.release_asio_on_blur = on;
        self.save_settings();
    }

    fn focus_changed(&mut self, focused: bool) {
        if focused {
            if let Some((key, settings)) = self.suspended.take()
                && self.active.is_none()
                && let Err(e) = self.open_device(key, settings)
            {
                self.notices.push(Notice::error(format!(
                    "The ASIO driver did not reopen: {e}"
                )));
            }
            return;
        }
        // Never during playback or recording.
        let Some(a) = &self.active else { return };
        if !self.settings.release_asio_on_blur
            || a.key.backend.0 != "asio"
            || self.transport().playing
            || self.recorder.shared.recording.load(Relaxed)
        {
            return;
        }
        if let Some(settings) = self.settings.audio.get(&a.key) {
            self.suspended = Some((a.key.clone(), settings));
            self.close_device();
        }
    }

    fn channel_names(&self, key: &DeviceKey) -> jdance_audio_api::ChannelNames {
        self.backend(&key.backend)
            .and_then(|b| b.channel_names(&key.device).ok())
            .unwrap_or_default()
    }

    fn export_audio(
        &mut self,
        project: &Project,
        project_dir: Option<&Path>,
        out: PathBuf,
        options: ExportOptions,
        title: String,
        artist: String,
    ) {
        if self.export_state.is_some() {
            self.notices
                .push(Notice::warning("An export is already running."));
            return;
        }
        let (tx, rx) = channel();
        let mut project = project.clone();
        // The export uses its own instances, restored from the current
        // plugin states (spec "Export").
        self.plugins.capture_states(&mut project);
        let catalog = self.plugins.catalog.clone();
        let modules = std::sync::Arc::clone(&self.plugins.modules);
        let dir = project_dir.map(Path::to_path_buf);
        let rate = self.engine_rate();
        let progress = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.export_state = Some((Arc::clone(&progress), Arc::clone(&cancel)));
        std::thread::spawn(move || {
            let plugins = crate::render::ExportPlugins {
                catalog: &catalog,
                modules: &modules,
            };
            let tags = ExportTags { title, artist };
            let result = render_to_file(
                &project,
                dir.as_deref(),
                &out,
                rate,
                &options,
                &tags,
                Some(&plugins),
                &mut |f| {
                    progress.store((f.clamp(0.0, 1.0) * 1000.0) as u32, Relaxed);
                    !cancel.load(Relaxed)
                },
            )
            .map(|o| {
                let rate = o.audio.sample_rate;
                let mut msg = format!(
                    "Exported {} ({} Hz, {})",
                    out.display(),
                    rate,
                    options.describe()
                );
                if let Some(w) = o.warnings.first() {
                    let more = o.warnings.len() - 1;
                    msg.push_str(&format!("; warning: {w}"));
                    if more > 0 {
                        msg.push_str(&format!(" (+{more} more)"));
                    }
                }
                if let Some(w) = o.tail_warning {
                    msg.push_str(&format!(". {w}"));
                }
                msg
            });
            let _ = tx.send(result);
        });
        self.exports.push(rx);
    }

    fn export_progress(&self) -> Option<f32> {
        self.export_state
            .as_ref()
            .map(|(p, _)| p.load(Relaxed) as f32 / 1000.0)
    }

    fn cancel_export(&mut self) {
        if let Some((_, c)) = &self.export_state {
            c.store(true, Relaxed);
        }
    }

    fn capture_plugin_states(&mut self, project: &mut Project) {
        self.plugins.capture_states(project);
    }

    fn plugin_effects(&self) -> Vec<PluginDescriptor> {
        self.plugins_of_kind(PluginKind::Effect)
    }

    fn plugin_instruments(&self) -> Vec<PluginDescriptor> {
        self.plugins_of_kind(PluginKind::Instrument)
    }

    fn plugin_params(&mut self, insert: PluginTarget) -> Option<Vec<PluginParamView>> {
        let c = self.plugins.controller(insert)?;
        Some(
            c.params()
                .iter()
                .map(|info| {
                    let value = c.param_normalized(info.id);
                    PluginParamView {
                        text: c.param_text(info.id, value),
                        info: info.clone(),
                        value,
                    }
                })
                .collect(),
        )
    }

    fn set_plugin_param(&mut self, insert: PluginTarget, id: u32, value: f64) {
        if let Some(c) = self.plugins.controller(insert) {
            c.set_param_normalized(id, value);
        }
    }

    fn plugin_param_from_text(&mut self, insert: PluginTarget, id: u32, text: &str) -> Option<f64> {
        self.plugins.controller(insert)?.param_from_text(id, text)
    }

    fn toggle_plugin_editor(&mut self, insert: PluginTarget, title: &str) {
        if let Err(e) = self.plugins.toggle_editor(insert, title) {
            self.notices
                .push(Notice::warning(format!("Cannot open the editor: {e}")));
        }
    }

    fn plugin_editor_open(&self, insert: PluginTarget) -> bool {
        self.plugins.editor_open(insert)
    }

    fn any_plugin_editor_open(&self) -> bool {
        self.plugins.any_editor_open()
    }

    fn plugin_held_back(&self, insert: PluginTarget) -> bool {
        self.plugins.held_back(insert)
    }

    fn retry_plugin(&mut self, insert: PluginTarget) {
        self.plugins.retry(insert);
        self.needs_compile = true;
    }

    fn plugin_problem(&self, insert: PluginTarget) -> Option<String> {
        self.plugins.problem(insert)
    }

    fn blocked_plugins(&self) -> Vec<(PathBuf, String)> {
        self.plugins
            .catalog
            .blocked()
            .map(|(p, r)| (p.to_path_buf(), r.to_owned()))
            .collect()
    }

    fn plugin_scan_running(&self) -> bool {
        self.scan.is_some()
    }

    fn rescan_plugins(&mut self, full: bool) {
        self.start_plugin_scan(full);
    }

    fn library_roots(&self) -> Vec<jdance_ui::LibraryRoot> {
        self.settings
            .library_folders
            .iter()
            .map(crate::config::LibraryFolder::root)
            .collect()
    }

    fn set_library_roots(&mut self, roots: Vec<jdance_ui::LibraryRoot>) {
        self.settings.library_folders = roots
            .iter()
            .map(crate::config::LibraryFolder::from_root)
            .collect();
        self.save_settings();
        self.browser.index(roots);
    }

    fn rescan_library(&mut self) {
        let roots = self.library_roots();
        for r in &roots {
            self.browser.refresh(&r.path);
        }
        self.browser.index(roots);
    }

    fn cancel_library_scan(&mut self) {
        self.browser.cancel_index();
    }

    fn library_index(&self) -> jdance_ui::IndexStatus {
        self.browser.index_status()
    }

    fn favorite_folders(&self) -> Vec<PathBuf> {
        self.settings.favorite_folders.clone()
    }

    fn set_favorite_folders(&mut self, folders: Vec<PathBuf>) {
        self.settings.favorite_folders = folders;
        self.save_settings();
    }

    fn list_dir(&mut self, dir: &Path) -> Listing {
        self.browser.list(dir)
    }

    fn refresh_dir(&mut self, dir: &Path) {
        self.browser.refresh(dir);
    }

    fn search_files(&mut self, roots: &[PathBuf], query: &str) -> Listing {
        self.browser.search(roots, query)
    }

    fn inspect_file(&mut self, path: &Path) {
        self.samples.request(path);
    }

    fn preview_file(&mut self, path: Option<&Path>) {
        match path {
            Some(p) => {
                if self.active.is_none() {
                    self.notices.push(Notice::warning(
                        "No audio device open: nothing to preview on.",
                    ));
                    return;
                }
                self.samples.request(p);
                self.pending_preview = Some((p.to_path_buf(), (0.0, 1.0)));
            }
            None => {
                self.pending_preview = None;
                if let Some(a) = &mut self.active {
                    let _ = a.engine.preview(None);
                }
            }
        }
    }

    fn preview_range(&mut self, path: &Path, range: (f64, f64)) {
        self.preview_file(Some(path));
        if let Some(p) = &mut self.pending_preview {
            p.1 = range;
        }
    }

    fn reveal_in_file_manager(&mut self, path: &Path) {
        if let Err(e) = crate::browser_fs::reveal(path) {
            self.notices.push(Notice::warning(format!(
                "Could not open the file manager: {e}"
            )));
        }
    }

    fn plugin_paths(&self) -> Vec<PathBuf> {
        self.settings.plugin_paths.clone()
    }

    fn set_plugin_paths(&mut self, paths: Vec<PathBuf>) {
        self.settings.plugin_paths = paths;
        self.save_settings();
        self.start_plugin_scan(false);
    }

    fn scripts(&mut self) -> Vec<jdance_commands::note_tools::ScriptInfo> {
        self.scripts.refresh(&mut self.notices);
        self.scripts.list.iter().map(|s| s.info.clone()).collect()
    }

    fn script_folder(&mut self) -> Option<PathBuf> {
        let dir = ScriptHost::folder()?;
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.notices.push(Notice::warning(format!(
                "Cannot create {}: {e}",
                dir.display()
            )));
            return None;
        }
        Some(dir)
    }

    fn start_script(
        &mut self,
        run: jdance_commands::note_tools::ScriptRun,
        job: jdance_ui::ScriptJob,
    ) -> Option<u64> {
        self.cancel_script();
        let script = self
            .scripts
            .list
            .iter()
            .find(|s| s.info.id == run.id)?
            .clone();
        self.scripts.next += 1;
        let ticket = self.scripts.next;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("jdance-script".into())
            .spawn(move || {
                let input = jdance_commands::note_tools::Input {
                    notes: &job.notes,
                    selected: &job.selected,
                    pattern_len: job.pattern_len,
                    grid: job.grid,
                    range: job.range,
                    signature: job.signature,
                };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    script.run(&run, &input, flag)
                }))
                .unwrap_or_else(|_| Err("the script runtime failed".into()));
                let _ = tx.send(r);
            })
            .ok()?;
        self.scripts.running = Some((ticket, cancel, rx));
        Some(ticket)
    }

    fn script_result(&mut self, ticket: u64) -> Option<ScriptResult> {
        if let Some((t, _, rx)) = &self.scripts.running
            && let Ok(r) = rx.try_recv()
        {
            self.scripts.done.push((*t, r));
            self.scripts.running = None;
        }
        let i = self.scripts.done.iter().position(|(t, _)| *t == ticket)?;
        Some(self.scripts.done.remove(i).1)
    }

    fn cancel_script(&mut self) {
        if let Some((_, c, _)) = self.scripts.running.take() {
            c.store(true, Relaxed);
        }
        self.scripts.done.clear();
    }

    fn ui_layout(&self) -> String {
        self.settings.ui_layout.clone()
    }

    fn set_ui_layout(&mut self, layout: String) {
        if self.settings.ui_layout != layout {
            self.settings.ui_layout = layout;
            self.save_settings();
        }
    }

    fn plugin_search_paths(&self) -> Vec<jdance_ui::PluginPath> {
        let disabled = &self.settings.disabled_plugin_paths;
        let entry = |path: PathBuf, system: bool| jdance_ui::PluginPath {
            enabled: !disabled.contains(&path),
            exists: path.is_dir(),
            path,
            system,
        };
        jdance_plugin_vst3::default_search_paths()
            .into_iter()
            .map(|p| entry(p, true))
            .chain(
                self.settings
                    .plugin_paths
                    .iter()
                    .cloned()
                    .map(|p| entry(p, false)),
            )
            .collect()
    }

    fn set_plugin_path_enabled(&mut self, path: &Path, enabled: bool) {
        let d = &mut self.settings.disabled_plugin_paths;
        d.retain(|p| p != path);
        if !enabled {
            d.push(path.to_path_buf());
        }
        self.save_settings();
        self.start_plugin_scan(false);
    }

    fn plugin_scan_progress(&self) -> Option<(usize, usize, PathBuf)> {
        let (p, _) = self.scan_state.as_ref()?;
        p.lock().ok().map(|p| p.clone())
    }

    fn cancel_plugin_scan(&mut self) {
        if let Some((_, c)) = &self.scan_state {
            c.store(true, Relaxed);
        }
    }

    fn estimated_tail_seconds(&mut self, project: &Project) -> f32 {
        estimated_tail_seconds(project, self.engine_rate())
    }

    fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }
}

/// Progress (done, total, module) and cancel flag of a plugin scan.
type ScanState = (
    Arc<std::sync::Mutex<(usize, usize, PathBuf)>>,
    Arc<std::sync::atomic::AtomicBool>,
);

/// A finished background import: ticket and file or error.
type ImportResult = (u64, Result<jdance_ui::ImportedAudio, String>);

/// Reads (and copies into the project's `assets/` folder) one audio file
/// for import; runs on a worker thread.
fn import_file(path: &Path, dir: Option<&Path>) -> Result<jdance_ui::ImportedAudio, String> {
    let info = jdance_persistence::inspect_audio(path).map_err(|e| e.to_string())?;
    let stored = match dir {
        Some(d) => {
            jdance_persistence::copy_into_assets(path, d, info.hash).map_err(|e| e.to_string())?
        }
        None => path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned(),
    };
    Ok(jdance_ui::ImportedAudio {
        source: path.to_path_buf(),
        path: stored,
        name: path
            .file_stem()
            .map_or_else(|| "audio".into(), |s| s.to_string_lossy().into_owned()),
        hash: info.hash,
        size: info.size,
        sample_rate: info.sample_rate,
        channels: info.channels,
        frames: info.frames,
    })
}
