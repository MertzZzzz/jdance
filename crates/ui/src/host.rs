// SPDX-License-Identifier: GPL-3.0-or-later
//! The interface the UI needs from the rest of the application.
//!
//! The UI never touches the engine or audio backends directly (ARCH-001);
//! `jdance-app` implements [`Host`] and does the wiring.

use jdance_audio_api::{
    ActiveStreamInfo, AudioError, BackendId, DeviceCapabilities, DeviceInfo, DeviceKey,
    DeviceSettings, StatsReading, StreamState,
};
use jdance_domain::{ExportOptions, PatternId, PluginTarget, Project, SampleRef, TrackId};
use jdance_plugin_api::{ParamInfo, PluginDescriptor};
use std::path::{Path, PathBuf};

/// One parameter of a live plugin for the generic editor (PLG-064).
#[derive(Debug, Clone, PartialEq)]
pub struct PluginParamView {
    pub info: ParamInfo,
    /// Normalized `0.0..=1.0`.
    pub value: f64,
    /// The plugin's own text for `value`.
    pub text: String,
}

/// What Play plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    /// Loops one pattern.
    Pattern(PatternId),
    /// Plays the playlist arrangement.
    Song,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransportStatus {
    pub playing: bool,
    /// Playhead in ticks (timeline position); `None` if unknown.
    pub position_ticks: Option<u64>,
    /// Playhead inside the pattern loop, `0.0..1.0`; `None` if unknown.
    pub loop_pos: Option<f32>,
    pub peak_load: f32,
    /// Buffers processed slower than real time since the engine started.
    pub overloads: u64,
    pub voices: u32,
    pub dropped_commands: u64,
}

/// Meter levels for display (linear peak, already with UI fall-off).
#[derive(Debug, Clone, PartialEq)]
pub struct MeterReadings {
    pub buses: Vec<(TrackId, f32, f32)>,
    /// Master output after the limiter.
    pub output: (f32, f32),
    /// Current limiter gain (1.0 = no reduction).
    pub limiter_gain: f32,
}

impl Default for MeterReadings {
    fn default() -> Self {
        Self {
            buses: Vec::new(),
            output: (0.0, 0.0),
            limiter_gain: 1.0,
        }
    }
}

impl MeterReadings {
    pub fn bus(&self, id: TrackId) -> (f32, f32) {
        self.buses
            .iter()
            .find(|(t, _, _)| *t == id)
            .map_or((0.0, 0.0), |(_, l, r)| (*l, *r))
    }
}

/// Delay compensation figures from the last compiled snapshot (MIX-034).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LatencyInfo {
    pub sample_rate: u32,
    /// Output latency at the master, in samples.
    pub total: u32,
    pub per_track: Vec<(TrackId, u32)>,
    /// Some edge needed more compensation than the engine supports.
    pub clamped: bool,
}

impl LatencyInfo {
    pub fn track(&self, id: TrackId) -> Option<u32> {
        self.per_track
            .iter()
            .find(|(t, _)| *t == id)
            .map(|(_, l)| *l)
    }

    pub fn format(&self, samples: u32) -> String {
        if self.sample_rate == 0 {
            return format!("{samples} smp");
        }
        let ms = f64::from(samples) * 1000.0 / f64::from(self.sample_rate);
        format!("{samples} smp ({ms:.1} ms)")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamStatus {
    pub info: ActiveStreamInfo,
    pub stats: StatsReading,
    pub state: StreamState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleStatus {
    Loading,
    Ready { frames: usize, source_rate: u32 },
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
}

impl Notice {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Info,
            text: text.into(),
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Warning,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Error,
            text: text.into(),
        }
    }
}

pub trait Host {
    /// Housekeeping, called once per UI frame.
    fn poll(&mut self);
    /// The document changed; recompile what the engine plays.
    fn set_project(&mut self, project: &Project, project_dir: Option<&Path>);
    fn play(&mut self);
    fn stop(&mut self);
    /// Silences every sounding note and voice; the transport keeps going.
    fn panic(&mut self) {}
    fn set_mode(&mut self, mode: TransportMode);
    /// Moves the playhead (song mode).
    fn seek_ticks(&mut self, ticks: u64);
    fn transport(&self) -> TransportStatus;
    fn latency(&self) -> LatencyInfo;
    fn meters(&self) -> MeterReadings;
    fn stream(&self) -> Option<StreamStatus>;
    fn sample_status(&self, sample: &SampleRef, project_dir: Option<&Path>)
    -> Option<SampleStatus>;
    /// Peak level per bucket of a loaded sample (waveform display).
    fn sample_peaks(
        &self,
        _sample: &SampleRef,
        _project_dir: Option<&Path>,
    ) -> Option<std::sync::Arc<Vec<f32>>> {
        None
    }

    fn backends(&self) -> Vec<(BackendId, String)>;
    fn devices(&self, backend: &BackendId) -> Result<Vec<DeviceInfo>, AudioError>;
    fn capabilities(&self, key: &DeviceKey) -> Result<DeviceCapabilities, AudioError>;
    fn saved_settings(&self, key: &DeviceKey) -> Option<DeviceSettings>;
    fn current_device(&self) -> Option<DeviceKey>;
    fn open_device(&mut self, key: DeviceKey, settings: DeviceSettings) -> Result<(), AudioError>;
    fn close_device(&mut self);
    fn open_control_panel(&mut self, key: &DeviceKey) -> Result<(), AudioError>;

    /// Starts an offline render to `out`; the result arrives as a notice.
    fn export_wav(
        &mut self,
        project: &Project,
        project_dir: Option<&Path>,
        out: PathBuf,
        options: ExportOptions,
    );
    /// How long the project's effects are expected to ring after the last
    /// note, for the export tail warning.
    fn estimated_tail_seconds(&mut self, project: &Project) -> f32;
    /// Writes the live plugin states into `project` before it is saved
    /// (PLG-070).
    fn capture_plugin_states(&mut self, _project: &mut Project) {}

    /// Effect plugins from the last scan.
    fn plugin_effects(&self) -> Vec<PluginDescriptor> {
        Vec::new()
    }
    /// Instrument plugins from the last scan.
    fn plugin_instruments(&self) -> Vec<PluginDescriptor> {
        Vec::new()
    }
    /// Parameters of the live instance for `target`; `None` if the plugin
    /// is not loaded.
    fn plugin_params(&mut self, _target: PluginTarget) -> Option<Vec<PluginParamView>> {
        None
    }
    fn set_plugin_param(&mut self, _target: PluginTarget, _id: u32, _value: f64) {}
    /// The plugin's parse of a typed value, normalized.
    fn plugin_param_from_text(
        &mut self,
        _target: PluginTarget,
        _id: u32,
        _text: &str,
    ) -> Option<f64> {
        None
    }
    /// Opens or closes the plugin's own editor window (PLG-060).
    fn toggle_plugin_editor(&mut self, _target: PluginTarget, _title: &str) {}
    fn plugin_editor_open(&self, _target: PluginTarget) -> bool {
        false
    }
    fn any_plugin_editor_open(&self) -> bool {
        false
    }
    /// The plugin in `insert` crashed the application last time and is
    /// not loaded automatically (PLG-025).
    fn plugin_held_back(&self, _target: PluginTarget) -> bool {
        false
    }
    fn retry_plugin(&mut self, _target: PluginTarget) {}
    /// Why the plugin in `insert` is not loaded, if it is not.
    fn plugin_problem(&self, _target: PluginTarget) -> Option<String> {
        None
    }
    /// Blocklisted modules with reasons (PLG-024).
    fn blocked_plugins(&self) -> Vec<(PathBuf, String)> {
        Vec::new()
    }
    fn plugin_scan_running(&self) -> bool {
        false
    }
    /// `full` scans every module again, blocklisted ones included.
    fn rescan_plugins(&mut self, _full: bool) {}
    /// The user's extra plugin folders (stored in user settings).
    fn plugin_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn set_plugin_paths(&mut self, _paths: Vec<PathBuf>) {}
    fn take_notices(&mut self) -> Vec<Notice>;
}
