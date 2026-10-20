// SPDX-License-Identifier: GPL-3.0-or-later
//! Wires the UI to the engine, the audio backends and the sample cache.

use crate::config::{self, UserSettings};
use crate::render::render_to_wav;
use crate::samples::SampleCache;
use jdance_audio_api::{
    AudioBackend, AudioError, BackendId, BufferRequest, DeviceCapabilities, DeviceInfo, DeviceKey,
    DeviceSettings, OutputStream, RenderCallback, StreamState,
};
use jdance_audio_cpal::CpalBackend;
use jdance_domain::TrackId;
use jdance_domain::{Project, SampleRef};
use jdance_engine::{
    EngineHandle, EngineProcessor, FeedbackReading, Meters, compile, create_engine,
    param_only_changes,
};
use jdance_persistence::{WavExportFormat, resolve_sample_path};
use jdance_ui::{
    Host, LatencyInfo, MeterReadings, Notice, SampleStatus, StreamStatus, TransportStatus,
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
    project: Project,
    project_dir: Option<PathBuf>,
    needs_compile: bool,
    samples: SampleCache,
    settings: UserSettings,
    settings_path: Option<PathBuf>,
    notices: Vec<Notice>,
    exports: Vec<Receiver<Result<String, String>>>,
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
            project: Project::new(),
            project_dir: None,
            needs_compile: true,
            samples: SampleCache::new(FALLBACK_RATE),
            settings,
            settings_path,
            notices,
            exports: Vec::new(),
        }
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
        let dir = self.project_dir.as_deref();
        let samples = &self.samples;
        let rate = self.engine_rate();
        let snapshot = compile(&self.project, rate, |s| {
            samples.converted(&resolve_sample_path(dir, s))
        });
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
            Ok(()) => self.needs_compile = false,
            // Queue full: retry on the next poll.
            Err(_) => self.needs_compile = true,
        }
    }
}

impl Host for AudioHost {
    fn poll(&mut self) {
        if self.samples.poll() {
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

    fn stop(&mut self) {
        if let Some(a) = &mut self.active {
            let _ = a.engine.stop();
        }
    }

    fn transport(&self) -> TransportStatus {
        let r = &self.reading;
        TransportStatus {
            playing: r.playing,
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
        loops: u32,
    ) {
        let (tx, rx) = channel();
        let project = project.clone();
        let dir = project_dir.map(Path::to_path_buf);
        let rate = self.engine_rate();
        std::thread::spawn(move || {
            let result = render_to_wav(
                &project,
                dir.as_deref(),
                &out,
                rate,
                loops,
                WavExportFormat::Pcm24,
            )
            .map(|o| {
                let mut msg = format!("Exported {} ({} Hz, 24-bit)", out.display(), rate);
                if !o.warnings.is_empty() {
                    msg.push_str(&format!("; {} sample(s) missing", o.warnings.len()));
                }
                msg
            });
            let _ = tx.send(result);
        });
        self.exports.push(rx);
        self.notices.push(Notice::info("Exporting…"));
    }

    fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }
}
