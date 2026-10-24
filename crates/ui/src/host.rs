// SPDX-License-Identifier: GPL-3.0-or-later
//! The interface the UI needs from the rest of the application.
//!
//! The UI never touches the engine or audio backends directly (ARCH-001);
//! `jdance-app` implements [`Host`] and does the wiring.

use jdance_audio_api::{
    ActiveStreamInfo, AudioError, BackendId, DeviceCapabilities, DeviceInfo, DeviceKey,
    DeviceSettings, InputChannels, StatsReading, StreamState,
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

/// A folder or audio file in a Browser listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// Bytes; 0 for folders.
    pub size: u64,
}

/// A folder listing or search result, filled in the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Loading,
    Ready(Vec<DirEntry>),
    Failed(String),
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
    /// Count-in beats still to come (MET-022); 0 = none.
    pub count_in: u32,
}

/// Metronome and count-in settings (spec 0.2.3 MET-002/003/020); user
/// settings, not the project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetronomeSettings {
    pub on: bool,
    /// Level, −40…0 dB.
    pub volume_db: f32,
    /// Click only while recording.
    pub only_recording: bool,
    /// Count four bars before a recording.
    pub count_in: bool,
}

/// Bars of the recording count-in (MET-020: fixed).
pub const COUNT_IN_BARS: u32 = 4;

impl Default for MetronomeSettings {
    fn default() -> Self {
        Self {
            on: false,
            volume_db: -6.0,
            only_recording: false,
            count_in: false,
        }
    }
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

/// A sample library folder (spec 0.2.0 §5). Removing one never deletes
/// files; a missing folder shows as unavailable and stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRoot {
    pub path: PathBuf,
    /// Display name instead of the folder name.
    pub name: Option<String>,
    /// Index subfolders too.
    pub recursive: bool,
}

impl LibraryRoot {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            name: None,
            recursive: true,
        }
    }

    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            self.path.file_name().map_or_else(
                || self.path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        })
    }
}

/// Progress of the library index (search runs over it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexStatus {
    pub files: usize,
    pub running: bool,
    pub cancelled: bool,
}

/// A plugin search folder (spec 0.2.0 §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPath {
    pub path: PathBuf,
    /// A standard folder of the OS (cannot be removed, only disabled).
    pub system: bool,
    pub enabled: bool,
    pub exists: bool,
}

/// A device that can record, on the current audio backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub channels: u16,
    pub is_default: bool,
    /// Driver channel names (ASIO); empty when the backend has none.
    pub names: jdance_audio_api::ChannelNames,
}

/// Recording input (spec 0.2.0 §7); a user setting, not part of projects.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingSettings {
    /// `None` = no input open.
    pub device: Option<String>,
    pub channels: InputChannels,
    /// Input heard through the output; off by default.
    pub monitor: bool,
    /// Manual alignment: positive moves recordings later.
    pub offset_ms: f32,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            device: None,
            channels: InputChannels::Mono(0),
            monitor: false,
            offset_ms: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecordingState {
    /// No input is open (the reason, for display).
    NoInput(String),
    Ready,
    Recording {
        seconds: f64,
    },
    /// The writer is finishing the file.
    Finishing,
}

/// What to do with a take left unfinished by a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Repair it and add it to the project's samples.
    Import,
    /// Repair it and leave it as a file.
    KeepFile,
    Delete,
}

/// A finished take: the file as imported, and why it ended early.
#[derive(Debug, Clone, PartialEq)]
pub struct FinishedTake {
    pub audio: ImportedAudio,
    pub problem: Option<String>,
    /// Driver-reported input + output latency at the take's rate, taken
    /// off the take's position once (ASIO-042); 0 when not reported.
    pub latency_frames: u32,
}

/// An audio file read for import (spec 0.2.0 §2): copied into the
/// project's `assets/` folder when the project has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAudio {
    /// The file that was dropped or chosen.
    pub source: PathBuf,
    /// Stored path: relative to the project folder, or absolute while the
    /// project is unsaved.
    pub path: String,
    pub name: String,
    pub hash: u64,
    pub size: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleStatus {
    Loading,
    Ready {
        frames: usize,
        source_rate: u32,
        /// The file's encoding for display: "16", "MP3 192 kbit/s stereo".
        format: String,
    },
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
    /// Plays (`Some(velocity)`) or releases a held note on `channel`
    /// outside the sequencer: piano keys, MIDI input.
    fn live_note(&mut self, _channel: jdance_domain::ChannelId, _key: u8, _velocity: Option<f32>) {}
    /// The channel MIDI input plays (spec 0.3.0 §11.3, §12).
    fn set_input_channel(&mut self, _channel: Option<jdance_domain::ChannelId>) {}
    fn set_mode(&mut self, mode: TransportMode);
    /// Clicks `beats` beats while stopped, then plays (MET-021); Stop
    /// cancels the count-in.
    fn count_in(&mut self, beats: u32) {
        let _ = beats;
        self.play();
    }
    /// Space presses in plugin editor windows since the last call
    /// (KEY-004).
    fn take_editor_space(&mut self) -> u32 {
        0
    }
    fn metronome(&self) -> MetronomeSettings {
        MetronomeSettings::default()
    }
    fn set_metronome(&mut self, _settings: MetronomeSettings) {}
    /// Moves the playhead (song mode).
    fn seek_ticks(&mut self, ticks: u64);
    fn transport(&self) -> TransportStatus;
    fn latency(&self) -> LatencyInfo;
    fn meters(&self) -> MeterReadings;
    fn stream(&self) -> Option<StreamStatus>;
    fn sample_status(&self, sample: &SampleRef, project_dir: Option<&Path>)
    -> Option<SampleStatus>;
    /// Library folders shown in the Browser (user settings).
    fn library_folders(&self) -> Vec<PathBuf> {
        self.library_roots().into_iter().map(|r| r.path).collect()
    }
    /// Replaces the folders (new ones index recursively).
    fn set_library_folders(&mut self, folders: Vec<PathBuf>) {
        let old = self.library_roots();
        let roots = folders
            .into_iter()
            .map(|p| {
                old.iter()
                    .find(|r| r.path == p)
                    .cloned()
                    .unwrap_or_else(|| LibraryRoot::new(p))
            })
            .collect();
        self.set_library_roots(roots);
    }
    fn library_roots(&self) -> Vec<LibraryRoot> {
        Vec::new()
    }
    fn set_library_roots(&mut self, _roots: Vec<LibraryRoot>) {}
    /// Rebuilds the library index in the background.
    fn rescan_library(&mut self) {}
    fn cancel_library_scan(&mut self) {}
    fn library_index(&self) -> IndexStatus {
        IndexStatus::default()
    }
    /// Favorite folders (user settings).
    fn favorite_folders(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn set_favorite_folders(&mut self, _folders: Vec<PathBuf>) {}
    /// Folders and audio files in `dir`; `Loading` until the background
    /// listing finishes.
    fn list_dir(&mut self, _dir: &Path) -> Listing {
        Listing::Failed("not available".into())
    }
    fn refresh_dir(&mut self, _dir: &Path) {}
    /// Audio files under `roots` whose names contain `query`.
    fn search_files(&mut self, _roots: &[PathBuf], _query: &str) -> Listing {
        Listing::Failed("not available".into())
    }
    /// Loads an audio file for inspection (status, waveform) in the
    /// background.
    fn inspect_file(&mut self, _path: &Path) {}
    /// Plays a file on the preview bus; `None` stops. The project does not
    /// change.
    fn preview_file(&mut self, _path: Option<&Path>) {}
    /// Plays the part `[from, to)` of a file (fractions of its length) on
    /// the preview bus.
    fn preview_range(&mut self, path: &Path, _range: (f64, f64)) {
        self.preview_file(Some(path));
    }
    fn reveal_in_file_manager(&mut self, _path: &Path) {}
    /// Starts reading `path` for import in the background; the result
    /// comes from [`Host::take_imports`] with the returned ticket.
    fn import_audio(&mut self, _path: &Path, _project_dir: Option<&Path>) -> Option<u64> {
        None
    }
    fn take_imports(&mut self) -> Vec<(u64, Result<ImportedAudio, String>)> {
        Vec::new()
    }
    /// The notes of a MIDI file (spec 0.2.9 §6), from tick 0. MIDI files
    /// are small: read at once.
    fn read_midi(&mut self, _path: &Path) -> Result<MidiNotes, String> {
        Err("MIDI files are not available here".into())
    }

    /// Recording devices of the current backend.
    fn input_devices(&mut self) -> Vec<InputDevice> {
        Vec::new()
    }
    fn recording_settings(&self) -> RecordingSettings {
        RecordingSettings::default()
    }
    /// Stores the settings and (re)opens the input at the output's rate.
    fn set_recording_settings(&mut self, _settings: RecordingSettings) -> Result<(), String> {
        Err("recording is not available".into())
    }
    /// Input peaks since the last call.
    fn input_meter(&mut self) -> (f32, f32) {
        (0.0, 0.0)
    }
    fn recording_state(&self) -> RecordingState {
        RecordingState::NoInput("recording is not available".into())
    }
    /// Starts writing a take named `name`; the caller starts playback
    /// right after.
    fn start_recording(&mut self, _name: &str, _project_dir: Option<&Path>) -> Result<(), String> {
        Err("recording is not available".into())
    }
    fn stop_recording(&mut self) {}
    /// Channels of the open input that mixer tracks can use, with driver
    /// names where known (spec 0.2.3 INP-001); `None` when no input is
    /// open.
    fn live_inputs(&self) -> Option<(u16, Vec<String>)> {
        None
    }
    /// Waveform of the running take since the last call: the largest
    /// absolute sample per few hundred frames (REC-014).
    fn take_recording_peaks(&mut self) -> Vec<f32> {
        Vec::new()
    }
    /// The finished take, once its file is complete.
    fn take_recording(&mut self) -> Option<Result<FinishedTake, String>> {
        None
    }
    /// Takes left unfinished by a crash.
    fn unfinished_recordings(&mut self, _project_dir: Option<&Path>) -> Vec<PathBuf> {
        Vec::new()
    }
    fn resolve_unfinished(
        &mut self,
        _path: &Path,
        _action: Recovery,
        _project_dir: Option<&Path>,
    ) -> Result<Option<ImportedAudio>, String> {
        Err("recording is not available".into())
    }

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
    /// ASIO-025: whether the ASIO driver is released while the window is
    /// inactive.
    fn release_on_blur(&self) -> bool {
        false
    }
    fn set_release_on_blur(&mut self, _on: bool) {}
    /// The window gained or lost focus; with the option on, an ASIO
    /// driver is closed while inactive and reopened on return.
    fn focus_changed(&mut self, _focused: bool) {}
    /// Driver channel names (ASIO-023); empty when the backend has none.
    fn channel_names(&self, _key: &DeviceKey) -> jdance_audio_api::ChannelNames {
        jdance_audio_api::ChannelNames::default()
    }

    /// Starts an offline render to `out` (WAV or MP3); the result arrives
    /// as a notice. `title`/`artist` go into MP3 tags.
    fn export_audio(
        &mut self,
        project: &Project,
        project_dir: Option<&Path>,
        out: PathBuf,
        options: ExportOptions,
        title: String,
        artist: String,
    );
    /// Progress `0..=1` of the running export.
    fn export_progress(&self) -> Option<f32> {
        None
    }
    fn cancel_export(&mut self) {}
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
    /// Class id of the loaded plugin in `target`.
    fn plugin_class_id(&self, _target: PluginTarget) -> Option<String> {
        None
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
    /// Every folder scanned for VST3: the standard ones and the user's.
    fn plugin_search_paths(&self) -> Vec<PluginPath> {
        Vec::new()
    }
    /// Leaves a folder out of scans (or takes it back); rescans.
    fn set_plugin_path_enabled(&mut self, _path: &Path, _enabled: bool) {}
    /// (modules done, modules found, current module) while scanning.
    fn plugin_scan_progress(&self) -> Option<(usize, usize, PathBuf)> {
        None
    }
    /// Stops the scan; modules not reached keep their earlier result.
    fn cancel_plugin_scan(&mut self) {}
    /// Script tools: the built-in ones and the user's folder (cached).
    fn scripts(&mut self) -> Vec<jdance_commands::note_tools::ScriptInfo> {
        Vec::new()
    }
    /// The user's script folder (created if missing).
    fn script_folder(&mut self) -> Option<PathBuf> {
        None
    }
    /// Starts a script run on a worker; the result comes from
    /// [`Host::script_result`] with the ticket. A new run replaces the
    /// running one.
    fn start_script(
        &mut self,
        _run: jdance_commands::note_tools::ScriptRun,
        _job: crate::ScriptJob,
    ) -> Option<u64> {
        None
    }
    fn script_result(
        &mut self,
        _ticket: u64,
    ) -> Option<Result<jdance_commands::note_tools::ScriptOutput, String>> {
        None
    }
    fn cancel_script(&mut self) {}
    /// Saved window layout (dock/undock), as written by the UI.
    fn ui_layout(&self) -> String {
        String::new()
    }
    fn set_ui_layout(&mut self, _layout: String) {}
    /// Name presets for patterns (spec 0.2.5 §9), from the user settings;
    /// the factory set until the user changes it.
    fn pattern_presets(&self) -> jdance_domain::name_presets::NamePresets {
        jdance_domain::name_presets::NamePresets::factory_patterns()
    }
    fn set_pattern_presets(&mut self, _presets: jdance_domain::name_presets::NamePresets) {}
    /// Plugin favorites, recent plugins and the plugin list's filters
    /// (spec 0.2.5 §10), from the user settings.
    fn plugin_prefs(&self) -> jdance_plugin_api::search::PluginPrefs {
        jdance_plugin_api::search::PluginPrefs::default()
    }
    fn set_plugin_prefs(&mut self, _prefs: jdance_plugin_api::search::PluginPrefs) {}
    fn take_notices(&mut self) -> Vec<Notice>;
}

/// The notes of a MIDI file for the Piano Roll (spec 0.2.9 §6).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MidiNotes {
    /// From tick 0 (the earliest note), in the document's ticks.
    pub notes: Vec<jdance_domain::Note>,
    /// Notes too late for the longest pattern, left out.
    pub dropped: usize,
    /// The file's first tempo, not applied (§6.8).
    pub tempo_bpm: Option<f64>,
    /// Tracks and events read, for a file without notes (spec 0.3.0 §3.5).
    pub tracks: u16,
    pub events: usize,
}
