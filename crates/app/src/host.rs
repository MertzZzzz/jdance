// SPDX-License-Identifier: GPL-3.0-or-later
//! Wires the UI to the engine, the audio backends and the sample cache.

use crate::config::{self, UserSettings};
use crate::plugins::{Catalog, PluginHost};
use crate::render::{estimated_tail_seconds, render_to_wav};
use crate::samples::SampleCache;
use jdance_audio_api::{
    AudioBackend, AudioError, BackendId, BufferRequest, DeviceCapabilities, DeviceInfo, DeviceKey,
    DeviceSettings, OutputStream, RenderCallback, StreamState,
};
use jdance_audio_cpal::CpalBackend;
use jdance_domain::InsertId;
use jdance_domain::TrackId;
use jdance_domain::{ExportOptions, Project, SampleRef};
use jdance_engine::{
    EngineHandle, EngineProcessor, FeedbackReading, Meters, PlayMode, compile_full, create_engine,
    param_only_changes, samples_per_tick,
};
use jdance_persistence::resolve_sample_path;
use jdance_plugin_api::{PluginController, PluginDescriptor, PluginKind};
use jdance_ui::{
    Host, LatencyInfo, MeterReadings, Notice, PluginParamView, SampleStatus, StreamStatus,
    TransportMode, TransportStatus,
};
use std::path::{Path, PathBuf};
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

/// Adapts the engine processor to the audio-api callback.
struct EngineCallback(EngineProcessor);

impl RenderCallback for EngineCallback {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.0.render(left, right);
    }
}

struct Active {
    key: DeviceKey,
    stream: Box<dyn OutputStream>,
    engine: EngineHandle,
    failure_reported: bool,
}

pub struct AudioHost {
    backends: Vec<CpalBackend>,
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
    plugins: PluginHost,
    /// Incremented for every new engine; instances sent to an older one
    /// are gone.
    engine_generation: u64,
    scan: Option<Receiver<Catalog>>,
}

impl std::fmt::Debug for AudioHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioHost")
            .field("device", &self.active.as_ref().map(|a| &a.key))
            .finish_non_exhaustive()
    }
}

impl AudioHost {
    pub fn new(backends: Vec<CpalBackend>) -> Self {
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
            plugins: PluginHost::new(Catalog::load_cached()),
            engine_generation: 0,
            scan: None,
        }
    }

    /// Plugin folders: the standard ones plus the user's (PLG-020).
    fn plugin_roots(&self) -> Vec<PathBuf> {
        let mut roots = jdance_plugin_vst3::default_search_paths();
        roots.extend(self.settings.plugin_paths.iter().cloned());
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
        std::thread::spawn(move || {
            let _ = tx.send(catalog.rescan(&roots, &scanner, full));
        });
        self.scan = Some(rx);
    }

    fn backend(&self, id: &BackendId) -> Option<&CpalBackend> {
        self.backends.iter().find(|b| &b.id() == id)
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
        let Some(backend) = self.backends.first() else {
            self.notices.push(Notice::error(
                "No audio backend is available on this system.",
            ));
            return;
        };
        let default = backend
            .devices()
            .ok()
            .and_then(|list| list.into_iter().find(|d| d.is_default));
        let Some(device) = default else {
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
        if self.plugins.poll() {
            self.needs_compile = true;
        }
        self.plugins.poll_editors();
        if let Some(rx) = &self.scan
            && let Ok(catalog) = rx.try_recv()
        {
            self.scan = None;
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
                self.notices.push(match result {
                    Ok(msg) => Notice::info(msg),
                    Err(e) => Notice::error(format!("Export failed: {e}")),
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
        let paths: Vec<PathBuf> = project
            .channels
            .iter()
            .filter_map(|c| c.sample())
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

    fn sample_status(
        &self,
        sample: &SampleRef,
        project_dir: Option<&Path>,
    ) -> Option<SampleStatus> {
        self.samples
            .status(&resolve_sample_path(project_dir, sample))
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
        let backend = self
            .backend(&key.backend)
            .ok_or_else(|| AudioError::Unsupported(format!("backend {}", key.backend)))?;
        let (engine, processor) = create_engine(settings.sample_rate);
        let request = jdance_audio_api::StreamRequest {
            sample_rate: settings.sample_rate,
            buffer: settings.buffer,
            first_output_channel: settings.first_output_channel,
        };
        let stream =
            backend.open_output(&key.device, &request, Box::new(EngineCallback(processor)))?;
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
        Ok(())
    }

    fn close_device(&mut self) {
        // Dropping the stream stops the callback; the processor and its
        // snapshot are freed on this (non-realtime) thread.
        self.active = None;
    }

    fn open_control_panel(&mut self, key: &DeviceKey) -> Result<(), AudioError> {
        self.backend(&key.backend)
            .ok_or_else(|| AudioError::Unsupported(format!("backend {}", key.backend)))?
            .open_control_panel(&key.device)
    }

    fn export_wav(
        &mut self,
        project: &Project,
        project_dir: Option<&Path>,
        out: PathBuf,
        options: ExportOptions,
    ) {
        let (tx, rx) = channel();
        let mut project = project.clone();
        // The export uses its own instances, restored from the current
        // plugin states (spec "Export").
        self.plugins.capture_states(&mut project);
        let catalog = self.plugins.catalog.clone();
        let modules = std::sync::Arc::clone(&self.plugins.modules);
        let dir = project_dir.map(Path::to_path_buf);
        let rate = self.engine_rate();
        std::thread::spawn(move || {
            let plugins = crate::render::ExportPlugins {
                catalog: &catalog,
                modules: &modules,
            };
            let result = render_to_wav(
                &project,
                dir.as_deref(),
                &out,
                rate,
                &options,
                Some(&plugins),
            )
            .map(|o| {
                let mut msg = format!(
                    "Exported {} ({} Hz, {})",
                    out.display(),
                    rate,
                    options.format.label()
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
        self.notices.push(Notice::info("Exporting…"));
    }

    fn capture_plugin_states(&mut self, project: &mut Project) {
        self.plugins.capture_states(project);
    }

    fn plugin_effects(&self) -> Vec<PluginDescriptor> {
        let mut v: Vec<_> = self
            .plugins
            .catalog
            .plugins()
            .filter(|d| d.kind == PluginKind::Effect)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            (&a.plugin.vendor, &a.plugin.name).cmp(&(&b.plugin.vendor, &b.plugin.name))
        });
        v
    }

    fn plugin_params(&mut self, insert: InsertId) -> Option<Vec<PluginParamView>> {
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

    fn set_plugin_param(&mut self, insert: InsertId, id: u32, value: f64) {
        if let Some(c) = self.plugins.controller(insert) {
            c.set_param_normalized(id, value);
        }
    }

    fn toggle_plugin_editor(&mut self, insert: InsertId, title: &str) {
        if let Err(e) = self.plugins.toggle_editor(insert, title) {
            self.notices
                .push(Notice::warning(format!("Cannot open the editor: {e}")));
        }
    }

    fn plugin_editor_open(&self, insert: InsertId) -> bool {
        self.plugins.editor_open(insert)
    }

    fn any_plugin_editor_open(&self) -> bool {
        self.plugins.any_editor_open()
    }

    fn plugin_problem(&self, insert: InsertId) -> Option<String> {
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

    fn plugin_paths(&self) -> Vec<PathBuf> {
        self.settings.plugin_paths.clone()
    }

    fn set_plugin_paths(&mut self, paths: Vec<PathBuf>) {
        self.settings.plugin_paths = paths;
        self.save_settings();
        self.start_plugin_scan(false);
    }

    fn estimated_tail_seconds(&mut self, project: &Project) -> f32 {
        estimated_tail_seconds(project, self.engine_rate())
    }

    fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }
}
