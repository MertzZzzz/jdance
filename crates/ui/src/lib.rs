// SPDX-License-Identifier: GPL-3.0-or-later
//! egui user interface.
//!
//! The UI holds the document and the undo history, edits the document only
//! through `jdance-commands`, and tells the [`Host`] when it changed.

mod automation;
mod automation_editor;
mod bindings;
mod browser;
mod channel_settings;
mod color_picker;
mod dock;
mod drop_name;
mod host;
mod icons;
mod keys;
mod lfo_dialog;
mod library;
mod mixer;
mod name_presets_ui;
mod note_tools_ui;
mod piano;
mod piano_dialogs;
mod playlist;
mod plugin_list;
mod plugins;
mod project_browser;
mod rack;
mod recording;
mod ruler;
mod scripts;
pub use scripts::ScriptJob;
mod sample_editor;
mod settings;
#[cfg(test)]
mod test_ui;
mod theme;
mod toolbar;
mod widgets;

pub use host::{
    COUNT_IN_BARS, DirEntry, FinishedTake, Host, ImportedAudio, IndexStatus, InputDevice,
    LatencyInfo, LibraryRoot, Listing, MeterReadings, MetronomeSettings, Notice, NoticeLevel,
    PluginParamView, PluginPath, RecordingSettings, RecordingState, Recovery, SampleStatus,
    StreamStatus, TransportMode, TransportStatus,
};

use egui::Key;
use jdance_commands::{
    Edit, History, add_insert, add_lane, add_pattern, add_sample_channel, add_send, add_track,
    audio_channels, duplicate_clips, make_unique,
    naming::{self, Named},
};
use jdance_domain::{
    ChannelId, ClipId, DEFAULT_EXPORT_TAIL_SECONDS, Effect, ExportFormat, ExportOptions,
    ExportRange, LaneId, MAX_EXPORT_TAIL_SECONDS, MAX_TEMPO_BPM, MIN_TEMPO_BPM, Mp3Options,
    PatternId, PluginSlot, Project, TrackId,
};
use settings::SettingsWindow;
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "jdance";
const PROJECT_EXT: &str = "jdance";
const NOTICE_SECONDS: f64 = 8.0;

/// File operations the UI needs; implemented by the app with
/// `jdance-persistence` so that the UI does not depend on it.
pub trait ProjectStore {
    fn load(&self, path: &Path) -> Result<Project, String>;
    fn save(&self, path: &Path, project: &Project) -> Result<(), String>;
    /// Copies audio files still outside `dir` into its `assets/` folder
    /// and makes their paths relative; returns how many were copied.
    fn collect_assets(&self, _dir: &Path, _project: &mut Project) -> Result<usize, String> {
        Ok(0)
    }
}

/// The kind of an export range, remembered for the session (EXP-002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeKind {
    Song,
    Pattern,
    Selection,
}

impl RangeKind {
    fn of(range: ExportRange) -> Self {
        match range {
            ExportRange::Song => Self::Song,
            ExportRange::Pattern { .. } => Self::Pattern,
            ExportRange::Selection { .. } => Self::Selection,
        }
    }
}

/// "Bars 5:1 – 9:1 (4 bars, 0:08.000)" (EXP-003).
fn selection_text(start: u64, end: u64, project: &Project) -> String {
    let sig = project.time_signature;
    let at = |t: u64| {
        let (bar, beat, _) = sig.split(t);
        format!("{}:{}", bar + 1, beat + 1)
    };
    let (bars, beats, _) = sig.split(end - start);
    let mut length = format!("{bars} bar{}", if bars == 1 { "" } else { "s" });
    if beats > 0 {
        length += &format!(" {beats} beat{}", if beats == 1 { "" } else { "s" });
    }
    format!(
        "Bars {} – {} ({length}, {})",
        at(start),
        at(end),
        toolbar::time_text(end - start, project.tempo_bpm)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenameTarget {
    Channel(ChannelId),
    Track(TrackId),
    Pattern(PatternId),
    Lane(LaneId),
    Asset(jdance_domain::AssetId),
    Automation(jdance_domain::AutomationId),
}

/// The colour window and what it colours (spec 0.2.5 §6).
#[derive(Debug)]
struct ColorWindow {
    targets: Vec<Named>,
    /// Colours before the window opened, to undo the live view.
    originals: Vec<Option<[u8; 3]>>,
    picker: color_picker::Picker,
    title: String,
}

/// Which editor takes the editor keys (Ctrl+B, Ctrl+D, tools…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyFocus {
    Playlist,
    PianoRoll,
}

/// The plugin picker window: which plugins, and the mixer track an
/// effect goes into (`None`: the selected one).
#[derive(Debug)]
struct PluginPicker {
    list: plugin_list::PluginList,
    scope: plugin_list::Scope,
    track: Option<TrackId>,
}

impl PluginPicker {
    fn new(scope: plugin_list::Scope, track: Option<TrackId>) -> Self {
        Self {
            list: plugin_list::PluginList {
                focus: true,
                click_loads: true,
                ..plugin_list::PluginList::default()
            },
            scope,
            track,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RenameDialog {
    target: RenameTarget,
    text: String,
    /// Explicit color, or `None` for the palette default; absent for
    /// patterns.
    color: Option<Option<[u8; 3]>>,
    /// The color shown when `color` is the default.
    default_color: [u8; 3],
    /// The colour wheel opened from the colour square (spec 0.2.5 §6).
    wheel: Option<color_picker::Picker>,
    /// Pattern names: the preset list (spec 0.2.5 §9).
    presets: Option<name_presets_ui::PresetList>,
    /// The colour was chosen by hand: it wins over a preset's (§9 item 5).
    color_manual: bool,
    /// Colour of the preset under the pointer, shown on the square.
    hover_color: Option<[u8; 3]>,
    /// The pattern was just created at this index: the name and colour
    /// join its creation in one undo step (§9 item 13).
    created: Option<usize>,
    /// First frame: the whole name is selected, so typing replaces it.
    fresh: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    New,
    Open,
    Quit,
}

pub struct JdanceApp<H: Host, S: ProjectStore> {
    host: H,
    store: S,
    project: Project,
    history: History,
    path: Option<PathBuf>,
    settings: Option<SettingsWindow>,
    rename: Option<RenameDialog>,
    /// The colour window (spec 0.2.5 §6).
    color: Option<ColorWindow>,
    /// The name and colour of what is being dragged (§7, §8).
    dragging: Option<drop_name::Dropped>,
    /// Pattern name presets and recent names (§9), user settings.
    presets: jdance_domain::name_presets::NamePresets,
    /// Options › Pattern name presets is open.
    show_presets: bool,
    /// The project revision after a Ctrl+Z undo: the next Ctrl+Z redoes
    /// while the project is still there.
    undo_toggle: Option<u64>,
    /// The editor last clicked gets the editor keys (spec 0.2.7 §1).
    key_focus: KeyFocus,
    /// Options › Keyboard shortcuts is open.
    show_keys: bool,
    /// The plugin picker (F8, spec 0.2.5 §10 item 13), also opened by
    /// the Channel Rack's and mixer's add buttons.
    plugin_picker: Option<PluginPicker>,
    show_mixer: bool,
    show_rack: bool,
    show_playlist: bool,
    show_browser: bool,
    browser_view: browser::BrowserView,
    show_plugins: bool,
    /// Position shown as time instead of bars.
    time_display: bool,
    /// Grid for the Playlist and the Piano Roll (ticks; 0 = off).
    snap: toolbar::Snap,
    tap: Option<toolbar::TapTempo>,
    themed: bool,
    /// Pattern edited in the rack and used for new clips.
    current_pattern: PatternId,
    song_mode: bool,
    /// Song caret in ticks: where Play and Record start and Stop returns
    /// (spec 0.2.3 CUR-001).
    caret: u64,
    /// Pattern caret in ticks from the pattern start (spec 0.2.4 PCR-001),
    /// for `pattern_caret_of`; back to 0 for another pattern (PCR-003).
    pattern_caret: u64,
    pattern_caret_of: Option<PatternId>,
    /// Drag on the position display, in pixels not yet turned into beats.
    caret_drag: f32,
    mode_sent: Option<TransportMode>,
    playlist_view: playlist::PlaylistView,
    sample_editor: Option<sample_editor::SampleEditor>,
    automation_editors: Vec<automation_editor::AutomationEditor>,
    /// Which panels are in their own windows (a user setting).
    layout: dock::Layout,
    show_project_browser: bool,
    lfo_dialog: Option<lfo_dialog::LfoDialog>,
    mixer_view: mixer::MixerView,
    sample_picker: Option<library::SamplePicker>,
    show_library: bool,
    show_recording: bool,
    recording_panel: recording::RecordingPanel,
    /// Window focus last frame (ASIO-025).
    focused: Option<bool>,
    /// Lane (`None` = a new lane) and start tick of the take being
    /// recorded (REC-010/012).
    record_target: Option<(Option<LaneId>, u64)>,
    /// The take was stopped during its count-in: no message for it.
    record_cancelled: bool,
    /// New/Open/Quit asked while recording: runs once the take is in the
    /// project (REC-019).
    after_take: Option<Pending>,
    /// Record was pressed with the loop on: ask first.
    loop_prompt: bool,
    recovery: Option<recording::RecoveryDialog>,
    /// Project path whose folders were checked for unfinished takes.
    recovery_checked: Option<Option<PathBuf>>,
    /// Audio imports in flight: ticket → where the clip goes (`None` =
    /// add the file only).
    pending_imports: std::collections::BTreeMap<u64, Option<(Option<LaneId>, u64)>>,
    /// Audio channels just created by putting samples on the Playlist;
    /// the Channel Rack reveals them (ACH-018).
    new_channels: Vec<ChannelId>,
    project_browser: project_browser::ProjectBrowser,
    /// Pattern or audio file waiting for a delete confirmation.
    confirm_delete: Option<playlist::Source>,
    rack_view: rack::RackView,
    /// Channels waiting for the delete confirmation.
    delete_channels: Option<Vec<ChannelId>>,
    /// Piano roll shown instead of the playlist.
    piano: Option<piano::PianoRoll>,
    /// Open Channel Settings window.
    channel_settings: Option<channel_settings::ChannelSettingsWindow>,
    /// Shortening that needs confirmation: (pattern, steps, dropped).
    pending_length: Option<(PatternId, u32, usize)>,
    notices: Vec<(Notice, f64)>,
    pending: Option<Pending>,
    allow_close: bool,
    sync_needed: bool,
    /// Open export dialog: options and the estimated effect tail.
    export_dialog: Option<(ExportOptions, f32)>,
    /// Range chosen in the last export of the session (EXP-002).
    export_range_kind: Option<RangeKind>,
    export_loops: u32,
    export_tail: f32,
    export_format: ExportFormat,
    /// `Some` = export MP3.
    export_mp3: Option<jdance_domain::Mp3Options>,
    export_title: String,
    export_artist: String,
    auto_close_after: Option<u32>,
    frames: u32,
}

impl<H: Host, S: ProjectStore> std::fmt::Debug for JdanceApp<H, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JdanceApp")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl<H: Host, S: ProjectStore> JdanceApp<H, S> {
    /// If no audio device is open, the audio settings window is shown at
    /// startup so the user can pick an output (Amendment 01 §5).
    pub fn new(host: H, store: S, project: Project, path: Option<PathBuf>) -> Self {
        let current_pattern = project.patterns.first().map_or(PatternId(0), |p| p.id);
        let song_mode = !project.playlist.clips.is_empty();
        let settings = host
            .current_device()
            .is_none()
            .then(|| SettingsWindow::new(&host));
        let layout = dock::Layout::from_text(&host.ui_layout());
        let presets = host.pattern_presets();
        Self {
            host,
            store,
            project,
            history: History::default(),
            path,
            settings,
            rename: None,
            color: None,
            dragging: None,
            presets,
            show_presets: false,
            plugin_picker: None,
            undo_toggle: None,
            key_focus: KeyFocus::Playlist,
            show_keys: false,
            show_mixer: true,
            show_rack: true,
            show_playlist: true,
            show_browser: true,
            browser_view: browser::BrowserView::new(),
            show_plugins: false,
            time_display: false,
            snap: toolbar::Snap::Step,
            tap: None,
            themed: false,
            current_pattern,
            song_mode,
            caret: 0,
            pattern_caret: 0,
            pattern_caret_of: None,
            caret_drag: 0.0,
            mode_sent: None,
            playlist_view: playlist::PlaylistView::default(),
            sample_editor: None,
            automation_editors: Vec::new(),
            layout,
            show_project_browser: true,
            lfo_dialog: None,
            mixer_view: mixer::MixerView::default(),
            sample_picker: None,
            show_library: false,
            show_recording: false,
            recording_panel: recording::RecordingPanel::default(),
            focused: None,
            record_target: None,
            record_cancelled: false,
            after_take: None,
            loop_prompt: false,
            recovery: None,
            recovery_checked: None,
            pending_imports: std::collections::BTreeMap::new(),
            new_channels: Vec::new(),
            project_browser: project_browser::ProjectBrowser::default(),
            confirm_delete: None,
            rack_view: rack::RackView::default(),
            delete_channels: None,
            piano: None,
            channel_settings: None,
            pending_length: None,
            notices: Vec::new(),
            pending: None,
            allow_close: false,
            sync_needed: true,
            export_dialog: None,
            export_range_kind: None,
            export_loops: 4,
            export_tail: DEFAULT_EXPORT_TAIL_SECONDS,
            export_format: ExportFormat::default(),
            export_mp3: None,
            export_title: String::new(),
            export_artist: String::new(),
            auto_close_after: None,
            frames: 0,
        }
    }

    /// Shows `notice` on the first frame (e.g. a project that failed to open).
    pub fn with_startup_notice(mut self, notice: Notice) -> Self {
        self.notices.push((notice, 0.0));
        self
    }

    /// Closes the window after `frames` frames (GUI smoke test, CI-004).
    pub fn with_auto_close(mut self, frames: u32) -> Self {
        self.auto_close_after = Some(frames);
        self
    }

    fn project_dir(&self) -> Option<&Path> {
        self.path.as_deref().and_then(Path::parent)
    }

    fn notify(&mut self, notice: Notice, now: f64) {
        self.notices.push((notice, now));
    }

    fn apply(&mut self, edit: Edit, merging: bool, now: f64) {
        let result = if merging {
            self.history.execute_merging(&mut self.project, edit)
        } else {
            self.history.execute(&mut self.project, edit)
        };
        match result {
            Ok(()) => self.sync_needed = true,
            Err(e) => self.notify(Notice::error(format!("Edit rejected: {e}")), now),
        }
    }

    fn undo(&mut self, now: f64) {
        match self.history.undo(&mut self.project) {
            Ok(true) => self.sync_needed = true,
            Ok(false) => {}
            Err(e) => self.notify(Notice::error(format!("Undo failed: {e}")), now),
        }
    }

    fn redo(&mut self, now: f64) {
        match self.history.redo(&mut self.project) {
            Ok(true) => self.sync_needed = true,
            Ok(false) => {}
            Err(e) => self.notify(Notice::error(format!("Redo failed: {e}")), now),
        }
    }

    fn replace_project(&mut self, project: Project, path: Option<PathBuf>) {
        self.host.stop();
        self.current_pattern = project.patterns.first().map_or(PatternId(0), |p| p.id);
        self.song_mode = !project.playlist.clips.is_empty();
        self.playlist_view = playlist::PlaylistView::default();
        self.project = project;
        self.path = path;
        self.history.reset();
        self.sync_needed = true;
    }

    /// Runs `action` now, or asks first if there are unsaved changes.
    fn guarded(&mut self, action: Pending, ctx: &egui::Context, now: f64) {
        if self.record_target.is_some() {
            // Finish the take first; it then asks about saving (REC-019).
            self.host.stop_recording();
            self.host.stop();
            self.after_take = Some(action);
            return;
        }
        if self.history.is_dirty() {
            self.pending = Some(action);
        } else {
            self.run_pending(action, ctx, now);
        }
    }

    fn run_pending(&mut self, action: Pending, ctx: &egui::Context, now: f64) {
        match action {
            Pending::New => self.replace_project(Project::new(), None),
            Pending::Open => self.open_dialog(now),
            Pending::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn open_dialog(&mut self, now: f64) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("jdance project", &[PROJECT_EXT])
            .pick_file()
        else {
            return;
        };
        match self.store.load(&path) {
            Ok(p) => self.replace_project(p, Some(path)),
            Err(e) => self.notify(
                Notice::error(format!("Could not open {}: {e}", path.display())),
                now,
            ),
        }
    }

    /// Returns `true` if the project was saved.
    fn save(&mut self, save_as: bool, now: f64) -> bool {
        let path = match (&self.path, save_as) {
            (Some(p), false) => p.clone(),
            _ => {
                let Some(mut p) = rfd::FileDialog::new()
                    .add_filter("jdance project", &[PROJECT_EXT])
                    .set_file_name(format!("untitled.{PROJECT_EXT}"))
                    .save_file()
                else {
                    return false;
                };
                if p.extension().is_none() {
                    p.set_extension(PROJECT_EXT);
                }
                p
            }
        };
        self.host.capture_plugin_states(&mut self.project);
        if let Some(dir) = path.parent()
            && let Err(e) = self.store.collect_assets(dir, &mut self.project)
        {
            self.notify(Notice::error(format!("Save failed: {e}")), now);
            return false;
        }
        match self.store.save(&path, &self.project) {
            Ok(()) => {
                self.history.mark_saved();
                let dir_changed = self.project_dir() != path.parent();
                self.path = Some(path);
                // Relative sample paths resolve against the new directory.
                self.sync_needed |= dir_changed;
                true
            }
            Err(e) => {
                self.notify(Notice::error(format!("Save failed: {e}")), now);
                false
            }
        }
    }

    /// The Playlist time selection, when it has a length.
    fn selection(&self) -> Option<(u64, u64)> {
        self.project.playlist.loop_range.filter(|(a, b)| b > a)
    }

    fn export_range(&self, kind: RangeKind) -> ExportRange {
        match (kind, self.selection()) {
            (RangeKind::Selection, Some((start, end))) => ExportRange::Selection { start, end },
            (RangeKind::Pattern, _) => ExportRange::Pattern {
                pattern: self.current_pattern,
                loops: self.export_loops,
            },
            _ => ExportRange::Song,
        }
    }

    /// The export dialog with `kind`, else the last range of the session,
    /// else the current mode's (EXP-002).
    fn open_export_dialog(&mut self, kind: Option<RangeKind>) {
        let mode = if self.song_mode {
            RangeKind::Song
        } else {
            RangeKind::Pattern
        };
        let range = self.export_range(kind.or(self.export_range_kind).unwrap_or(mode));
        let options = ExportOptions {
            range,
            tail_seconds: self.export_tail,
            format: self.export_format,
            mp3: self.export_mp3,
        };
        let estimate = self.host.estimated_tail_seconds(&self.project);
        self.export_dialog = Some((options, estimate));
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        let Some((mut options, estimate)) = self.export_dialog else {
            return;
        };
        let song_empty = self.project.arrangement_end_ticks() == 0;
        let selection = self.selection();
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("export")).show(ctx, |ui| {
            ui.heading("Export audio");
            egui::Grid::new("export_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Range");
                    ui.horizontal(|ui| {
                        let kind = RangeKind::of(options.range);
                        for (k, name) in [
                            (RangeKind::Song, "Song"),
                            (RangeKind::Pattern, "Pattern"),
                            (RangeKind::Selection, "Selection"),
                        ] {
                            let enabled = k != RangeKind::Selection || selection.is_some();
                            if ui
                                .add_enabled(enabled, egui::Button::selectable(kind == k, name))
                                .on_disabled_hover_text(
                                    "Shift+drag on the Playlist ruler to select a range",
                                )
                                .clicked()
                            {
                                options.range = self.export_range(k);
                            }
                        }
                    });
                    ui.end_row();
                    if let ExportRange::Selection { start, end } = options.range {
                        // EXP-003.
                        ui.label("Selection");
                        ui.label(selection_text(start, end, &self.project));
                        ui.end_row();
                    }
                    if let ExportRange::Pattern { pattern, loops } = &mut options.range {
                        let name = self
                            .project
                            .pattern(*pattern)
                            .map_or("?", |p| p.name.as_str());
                        ui.label("Loops");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(loops).range(1..=256));
                            ui.label(format!("of \"{name}\""));
                        });
                        ui.end_row();
                    }
                    ui.label("Tail");
                    ui.add(
                        egui::DragValue::new(&mut options.tail_seconds)
                            .range(0.0..=MAX_EXPORT_TAIL_SECONDS)
                            .speed(0.1)
                            .suffix(" s"),
                    );
                    ui.end_row();
                    ui.label("Format");
                    ui.horizontal(|ui| {
                        if ui.selectable_label(options.mp3.is_none(), "WAV").clicked() {
                            options.mp3 = None;
                        }
                        if ui.selectable_label(options.mp3.is_some(), "MP3").clicked() {
                            options.mp3.get_or_insert_with(Mp3Options::default);
                        }
                    });
                    ui.end_row();
                    match &mut options.mp3 {
                        None => {
                            ui.label("Samples");
                            egui::ComboBox::from_id_salt("export_format")
                                .selected_text(options.format.label())
                                .show_ui(ui, |ui| {
                                    for f in ExportFormat::ALL {
                                        ui.selectable_value(&mut options.format, f, f.label());
                                    }
                                });
                            ui.end_row();
                        }
                        Some(m) => {
                            ui.label("Bitrate");
                            ui.horizontal(|ui| {
                                for k in [128u16, 192, 256, 320] {
                                    ui.selectable_value(&mut m.kbps, k, format!("{k}"));
                                }
                                ui.label("kbit/s CBR, stereo");
                            });
                            ui.end_row();
                            ui.label("Sample rate");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut m.sample_rate, 44_100, "44.1 kHz");
                                ui.selectable_value(&mut m.sample_rate, 48_000, "48 kHz");
                            });
                            ui.end_row();
                            ui.label("Title");
                            ui.text_edit_singleline(&mut self.export_title);
                            ui.end_row();
                            ui.label("Artist");
                            ui.text_edit_singleline(&mut self.export_artist);
                            ui.end_row();
                        }
                    }
                });
            if let Some(w) = options.tail_warning(estimate) {
                ui.colored_label(ui.visuals().warn_fg_color, w);
            }
            let blocked = song_empty && options.range == ExportRange::Song;
            if blocked {
                ui.colored_label(ui.visuals().warn_fg_color, "The playlist is empty.");
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!blocked, egui::Button::new("Export…"))
                    .clicked()
                {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(false);
                }
            });
        });
        if let ExportRange::Pattern { loops, .. } = options.range {
            self.export_loops = loops;
        }
        self.export_range_kind = Some(RangeKind::of(options.range));
        self.export_tail = options.tail_seconds;
        self.export_format = options.format;
        self.export_mp3 = options.mp3;
        self.export_dialog = match choice {
            None => Some((options, estimate)),
            Some(go) => {
                if go {
                    self.export(options);
                }
                None
            }
        };
    }

    fn export(&mut self, options: ExportOptions) {
        let ext = options.extension();
        let filter = if options.mp3.is_some() {
            "MP3 audio"
        } else {
            "WAV audio"
        };
        // "<project> (bars 5-9).wav" for a selection (EXP-008).
        let name = match options.range {
            ExportRange::Selection { start, end } => {
                let sig = self.project.time_signature;
                let stem = self
                    .path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .map_or_else(|| "render".into(), |s| s.to_string_lossy().into_owned());
                format!(
                    "{stem} (bars {}-{}).{ext}",
                    sig.split(start).0 + 1,
                    sig.split(end).0 + 1
                )
            }
            _ => format!("render.{ext}"),
        };
        let Some(mut out) = rfd::FileDialog::new()
            .add_filter(filter, &[ext])
            .set_file_name(&name)
            .save_file()
        else {
            return;
        };
        if out.extension().is_none() {
            out.set_extension(ext);
        }
        let dir = self.project_dir().map(Path::to_path_buf);
        self.host.export_audio(
            &self.project,
            dir.as_deref(),
            out,
            options,
            self.export_title.trim().to_owned(),
            self.export_artist.trim().to_owned(),
        );
    }

    /// Progress and Cancel while an export runs.
    fn export_progress_window(&mut self, ctx: &egui::Context) {
        let Some(f) = self.host.export_progress() else {
            return;
        };
        egui::Window::new("Exporting")
            .id(egui::Id::unique("export_progress"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -40.0])
            .show(ctx, |ui| {
                ui.set_width(320.0);
                let stage = if f < 0.9 {
                    "Rendering"
                } else {
                    "Writing the file"
                };
                ui.add(egui::ProgressBar::new(f).text(format!("{stage} · {:.0}%", f * 100.0)));
                if ui.button("Cancel").clicked() {
                    self.host.cancel_export();
                }
            });
        ctx.request_repaint_after_secs(0.1);
    }

    fn title(&self) -> String {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        let dirty = if self.history.is_dirty() { " *" } else { "" };
        format!("{name}{dirty} — {APP_NAME}")
    }

    fn shortcuts(&mut self, ctx: &egui::Context, now: f64) {
        // This window's keys plus those the undocked windows took last
        // frame (KEY-001).
        let mut k = keys::take(ctx);
        k.merge(std::mem::take(&mut self.layout.keys));
        // Space in a plugin's own editor window (KEY-004); an even count
        // cancels out.
        if self.host.take_editor_space() % 2 == 1 {
            k.space = !k.space;
        }
        if k.link {
            self.route_selected_channels(now);
        }
        // F4: a new pattern and its name window (spec 0.2.5 §9).
        if self.rename.is_none()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F4))
        {
            self.new_pattern(now);
        }
        // F7: the Piano Roll of the selected channel, or closes it.
        if !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| crate::bindings::PIANO_ROLL.take(i))
        {
            if let Some(mut roll) = self.piano.take() {
                roll.release(&mut self.host);
            } else {
                let ch = self
                    .project
                    .channels
                    .iter()
                    .filter(|c| !c.is_audio())
                    .find(|c| self.rack_view.selection.contains(&c.id))
                    .or_else(|| self.project.channels.iter().find(|c| !c.is_audio()));
                if let Some(c) = ch {
                    self.piano = Some(piano::PianoRoll::new(c.id));
                    self.key_focus = KeyFocus::PianoRoll;
                }
            }
        }
        // F8: the plugin picker.
        if !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F8))
        {
            self.plugin_picker = match self.plugin_picker {
                Some(_) => None,
                None => Some(PluginPicker::new(plugin_list::Scope::All, None)),
            };
        }
        if k.redo {
            self.redo(now);
        } else if k.undo_step {
            self.undo(now);
            self.undo_toggle = None;
        } else if k.undo {
            // Ctrl+Z undoes the last action, and again redoes it, as long
            // as nothing changed in between (spec 0.2.7 §1 item 13).
            match self.undo_toggle {
                Some(rev) if rev == self.history.revision() => {
                    self.redo(now);
                    self.undo_toggle = None;
                }
                _ => {
                    self.undo(now);
                    self.undo_toggle = Some(self.history.revision());
                }
            }
        }
        if k.save_as {
            self.save(true, now);
        } else if k.save {
            self.save(false, now);
        }
        if k.open {
            self.guarded(Pending::Open, ctx, now);
        }
        if k.new {
            self.guarded(Pending::New, ctx, now);
        }
        // Metronome and count-in (MET-002, MET-020).
        if k.metronome || k.count_in {
            let mut m = self.host.metronome();
            m.on ^= k.metronome;
            m.count_in ^= k.count_in;
            self.host.set_metronome(m);
        }
        if k.space {
            self.toggle_play();
        }
    }

    /// Ctrl+L (UI-VIS-064, ACH-033): the selected Channel Rack channels,
    /// instruments and audio alike, play into the selected mixer track.
    fn route_selected_channels(&mut self, now: f64) {
        let track = self.mixer_view.selected;
        let ids: Vec<ChannelId> = self
            .project
            .channels
            .iter()
            .map(|c| c.id)
            .filter(|id| self.rack_view.selection.contains(id))
            .collect();
        if ids.is_empty() {
            self.notify(
                Notice::info("Select channels in the Channel Rack, then a mixer track"),
                now,
            );
            return;
        }
        let name = self
            .project
            .mixer
            .track(track)
            .map_or_else(|| "?".to_owned(), |t| t.name.clone());
        let edits = ids
            .iter()
            .map(|id| Edit::SetChannelTrack(*id, track))
            .collect();
        self.apply(Edit::Batch(edits), false, now);
        self.notify(
            Notice::info(format!("{} channel(s) → mixer track {name}", ids.len())),
            now,
        );
    }

    fn toggle_play(&mut self) {
        let t = self.host.transport();
        if t.playing || t.count_in > 0 {
            // Space also ends a recording or its count-in (REC-018).
            if self.record_target.is_some() {
                self.record_cancelled = t.count_in > 0;
                self.host.stop_recording();
            }
            self.host.stop();
        } else {
            // The engine keeps the caret in samples; a tempo change since
            // the last seek moves it, so start from the ticks.
            self.host.seek_ticks(self.mode_caret());
            self.host.play();
        }
    }

    /// Moves the caret (and the playhead in song mode) to `ticks`.
    fn set_caret(&mut self, ticks: u64) {
        self.caret = ticks;
        if self.song_mode {
            self.host.seek_ticks(ticks);
        }
    }

    /// Moves the pattern caret (and the playhead in pattern mode).
    fn set_pattern_caret(&mut self, ticks: u64) {
        self.pattern_caret = ticks;
        if !self.song_mode {
            self.host.seek_ticks(ticks);
        }
    }

    /// The caret of the current mode.
    fn mode_caret(&self) -> u64 {
        if self.song_mode {
            self.caret
        } else {
            self.pattern_caret
        }
    }

    /// Moves the caret of the current mode; while playing the playhead
    /// moves only when `seek` (a released gesture, CUR-012).
    fn move_mode_caret(&mut self, ticks: u64, seek: bool) {
        if self.song_mode {
            self.caret = ticks;
        } else {
            self.pattern_caret = ticks;
        }
        if seek || !self.host.transport().playing {
            self.host.seek_ticks(ticks);
        }
    }

    /// Playhead and caret of `pattern` as pattern positions: the pattern
    /// caret in Pattern mode, the song's mapped through the clip of the
    /// pattern under them in Song mode (spec 0.2.4 PCR-001, PCR-010/011).
    fn pattern_marks(&self, pattern: &jdance_domain::Pattern) -> rack::PatternMarks {
        let t = self.host.transport();
        let len = pattern.length_ticks();
        if self.song_mode {
            let pl = &self.project.playlist;
            let at = |tick: u64| pl.pattern_position(pattern.id, len, tick).map(|(_, p)| p);
            rack::PatternMarks {
                playhead: t.position_ticks.filter(|_| t.playing).and_then(at),
                caret: at(self.caret),
            }
        } else {
            rack::PatternMarks {
                playhead: t
                    .loop_pos
                    .filter(|_| t.playing)
                    .map(|p| (p * len as f32) as u64),
                caret: Some(self.pattern_caret),
            }
        }
    }

    /// A caret gesture on the Piano Roll ruler (PCR-005, PCR-012).
    fn piano_caret(&mut self, request: piano::CaretRequest, now: f64) {
        let (pos, seek) = match request {
            piano::CaretRequest::Preview(p) => (p, false),
            piano::CaretRequest::Seek(p) => (p, true),
        };
        if !self.song_mode {
            self.move_mode_caret(pos, seek);
            return;
        }
        let Some(pattern) = self.project.pattern(self.current_pattern) else {
            return;
        };
        let len = pattern.length_ticks();
        let pl = &self.project.playlist;
        let t = self.host.transport();
        // The clip under the song caret; else, playing, the one under the
        // playhead; else the first one on the timeline.
        let under = |tick: u64| {
            pl.pattern_position(pattern.id, len, tick)
                .map(|(id, _)| (id, tick))
        };
        let row = |c: &jdance_domain::PatternClip| pl.lanes.iter().position(|l| l.id == c.lane);
        let reference = under(self.caret)
            .or_else(|| t.position_ticks.filter(|_| t.playing).and_then(under))
            .map(|(id, tick)| (id, Some(tick)))
            .or_else(|| {
                pl.clips
                    .iter()
                    .filter(|c| c.pattern == pattern.id)
                    .min_by_key(|c| (c.start, row(c)))
                    .map(|c| (c.id, None))
            });
        match reference.and_then(|(id, near)| pl.clip(id).map(|c| (c, near))) {
            Some((clip, near)) => {
                let tick = clip.timeline_tick_of(len, near, pos);
                self.move_mode_caret(tick, seek);
            }
            None if seek => {
                self.notify(Notice::info("This pattern is not in the playlist"), now);
            }
            None => {}
        }
    }

    /// The pattern caret belongs to one pattern and stays inside it
    /// (PCR-003).
    fn keep_pattern_caret(&mut self) {
        let len = self
            .project
            .pattern(self.current_pattern)
            .map_or(0, |p| p.length_ticks());
        if self.pattern_caret_of != Some(self.current_pattern) || self.pattern_caret >= len {
            self.pattern_caret_of = Some(self.current_pattern);
            if self.pattern_caret != 0 {
                self.pattern_caret = 0;
                if !self.song_mode && !self.host.transport().playing {
                    self.host.seek_ticks(0);
                }
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, now: f64) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.style_mut().visuals.button_frame = false;
            ui.label(egui::RichText::new(APP_NAME).font(theme::strong(13.0)));
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    self.guarded(Pending::New, &ctx, now);
                }
                if ui.button("Open…").clicked() {
                    self.guarded(Pending::Open, &ctx, now);
                }
                if ui.button("Save").clicked() {
                    self.save(false, now);
                }
                if ui.button("Save As…").clicked() {
                    self.save(true, now);
                }
                ui.separator();
                if ui.button("Export audio…").clicked() {
                    self.open_export_dialog(None);
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    self.guarded(Pending::Quit, &ctx, now);
                }
            });
            ui.menu_button("Edit", |ui| {
                let undo = self.history.undo_label().map(|l| format!("Undo {l}"));
                if ui
                    .add_enabled(
                        undo.is_some(),
                        egui::Button::new(undo.unwrap_or("Undo".into())),
                    )
                    .clicked()
                {
                    self.undo(now);
                }
                let redo = self.history.redo_label().map(|l| format!("Redo {l}"));
                if ui
                    .add_enabled(
                        redo.is_some(),
                        egui::Button::new(redo.unwrap_or("Redo".into())),
                    )
                    .clicked()
                {
                    self.redo(now);
                }
            });
            ui.menu_button("View", |ui| {
                ui.toggle_value(&mut self.show_browser, "Browser");
                ui.toggle_value(&mut self.show_rack, "Channel Rack");
                ui.toggle_value(&mut self.show_playlist, "Playlist");
                ui.toggle_value(&mut self.show_mixer, "Mixer");
                ui.toggle_value(&mut self.show_project_browser, "Project Browser");
                ui.separator();
                let mut follow = !self.layout.no_follow;
                if ui
                    .checkbox(&mut follow, "Follow playhead")
                    .on_hover_text(
                        "Scroll the Playlist along while playing (always while recording)",
                    )
                    .changed()
                {
                    self.layout.no_follow = !follow;
                    self.layout.changed = true;
                }
                let mut to_mouse = self.layout.zoom_to_mouse;
                if ui
                    .checkbox(&mut to_mouse, "Zoom to mouse")
                    .on_hover_text(
                        "Playlist zoom keeps the point under the mouse in place; off: the caret",
                    )
                    .changed()
                {
                    self.layout.zoom_to_mouse = to_mouse;
                    self.layout.changed = true;
                }
                ui.separator();
                ui.menu_button("Own windows", |ui| {
                    for p in dock::Panel::ALL {
                        let mut on = self.layout.is_undocked(p);
                        if ui
                            .checkbox(&mut on, p.name())
                            .on_hover_text("Show this panel in its own window")
                            .changed()
                        {
                            if on {
                                self.layout.undock(p);
                            } else {
                                self.layout.dock(p);
                            }
                        }
                    }
                    ui.separator();
                    if ui
                        .button("Reset layout")
                        .on_hover_text("Every panel back in the main window")
                        .clicked()
                    {
                        self.layout.reset();
                        ui.close();
                    }
                });
                ui.separator();
                ui.toggle_value(&mut self.show_plugins, "Plugins");
                if ui.button("Tap Tempo").clicked() {
                    self.tap.get_or_insert_with(toolbar::TapTempo::default);
                    ui.close();
                }
            });
            ui.menu_button("Options", |ui| {
                if ui.button("Audio settings…").clicked() {
                    self.settings = Some(SettingsWindow::new(&self.host));
                    ui.close();
                }
                if ui.button("Plugins…").clicked() {
                    self.show_plugins = true;
                    ui.close();
                }
                if ui.button("Library folders…").clicked() {
                    self.show_library = true;
                    ui.close();
                }
                if ui.button("Pattern name presets…").clicked() {
                    self.show_presets = true;
                    ui.close();
                }
                if ui.button("Keyboard shortcuts…").clicked() {
                    self.show_keys = true;
                    ui.close();
                }
                if ui.button("Plugin picker (F8)").clicked() {
                    self.plugin_picker = Some(PluginPicker::new(plugin_list::Scope::All, None));
                    ui.close();
                }
                if ui.button("Recording…").clicked() {
                    self.show_recording = true;
                    ui.close();
                }
                ui.menu_button("Metronome", |ui| {
                    let mut m = self.host.metronome();
                    let before = m;
                    metronome_settings_ui(ui, &mut m);
                    if m != before {
                        self.host.set_metronome(m);
                    }
                });
                // The same choices as the transport field (SIG-010).
                ui.menu_button("Time signature", |ui| {
                    let current = self.project.time_signature;
                    for sig in jdance_domain::COMMON_SIGNATURES {
                        if ui
                            .selectable_label(sig == current, sig.to_string())
                            .clicked()
                            && sig != current
                        {
                            self.apply(Edit::SetTimeSignature(sig), false, now);
                            ui.close();
                        }
                    }
                    ui.weak("Other: the field next to the tempo");
                });
            });
            let name = self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
                || "Untitled".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            let dirty = if self.history.is_dirty() { " •" } else { "" };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("{name}{dirty}")).color(theme::MUTED));
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, now: f64) {
        let transport = self.host.transport();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            // Transport.
            let playing = transport.playing;
            if theme::icon_button(ui, theme::Icon::Play, playing, "Play")
                .on_hover_text("Play / pause (Space)")
                .clicked()
            {
                self.toggle_play();
            }
            if theme::icon_button(ui, theme::Icon::Stop, false, "Stop")
                .on_hover_text("Stop and return to the caret; when already stopped, the caret goes to the start")
                .clicked()
            {
                if self.record_target.is_some() {
                    self.record_cancelled = transport.count_in > 0;
                    self.host.stop_recording();
                }
                if playing || transport.count_in > 0 {
                    self.host.stop();
                } else if self.song_mode {
                    self.set_caret(0);
                } else {
                    self.set_pattern_caret(0);
                }
            }
            let rec_state = self.host.recording_state();
            let counting = transport.count_in > 0;
            let recording = matches!(rec_state, RecordingState::Recording { .. });
            // The button blinks during the count-in (MET-022).
            let blink = counting && (now * 4.0) as i64 % 2 == 0;
            if theme::icon_button(ui, theme::Icon::Record, recording || blink, "Record")
                .on_hover_text("Record the input from the caret")
                .clicked()
            {
                if recording || counting || self.record_target.is_some() {
                    self.record_cancelled = counting;
                    self.host.stop_recording();
                    self.host.stop();
                } else {
                    self.begin_recording(now);
                }
            }
            if counting {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(125));
            }
            self.metronome_buttons(ui);

            if ui
                .button("Panic")
                .on_hover_text("Silence all sounding notes and voices")
                .clicked()
            {
                self.host.panic();
            }
            ui.separator();
            // Mode.
            ui.selectable_value(&mut self.song_mode, false, "Pattern")
                .on_hover_text("Play loops the current pattern");
            ui.selectable_value(&mut self.song_mode, true, "Song")
                .on_hover_text("Play the playlist arrangement");
            ui.separator();
            // Position; stopped in song mode the caret; during a count-in
            // the beats left (MET-022).
            // Also the caret while it is dragged on the ruler (CUR-014).
            let ticks = match transport.position_ticks {
                Some(_) if self.song_mode && self.playlist_view.caret_dragging() => self.caret,
                Some(p) if transport.playing => p,
                _ => self.mode_caret(),
            };
            let text = if counting {
                format!("— {} —", transport.count_in)
            } else if self.time_display {
                toolbar::time_text(ticks, self.project.tempo_bpm)
            } else {
                toolbar::bars_text(ticks, self.project.time_signature)
            };
            let display = ui
                .add(
                    egui::Button::new(egui::RichText::new(text).font(theme::mono(18.0)))
                        .fill(theme::FIELD)
                        .min_size(egui::vec2(130.0, 30.0))
                        .sense(egui::Sense::click_and_drag()),
                )
                .on_hover_text("Click to switch between bars and time; drag to move the caret by beats");
            if display.clicked() {
                self.time_display = !self.time_display;
            }
            if display.dragged() {
                // 8 px per beat (UI-VIS-051 item 3, CUR-003).
                const PX_PER_BEAT: f32 = 8.0;
                self.caret_drag += display.drag_delta().x;
                let beats = (self.caret_drag / PX_PER_BEAT).trunc();
                if beats != 0.0 {
                    self.caret_drag -= beats * PX_PER_BEAT;
                    let beat = i64::from(self.project.time_signature.beat_ticks());
                    let at = ((self.mode_caret() as i64 / beat + beats as i64).max(0) * beat) as u64;
                    if self.song_mode {
                        self.set_caret(at);
                    } else {
                        // Inside the pattern (PCR-005).
                        let len = self
                            .project
                            .pattern(self.current_pattern)
                            .map_or(1, |p| p.length_ticks());
                        let last = (len.saturating_sub(1) / beat as u64) * beat as u64;
                        self.set_pattern_caret(at.min(last));
                    }
                }
            } else {
                self.caret_drag = 0.0;
            }
            // Tempo.
            let mut tempo = self.project.tempo_bpm;
            let r = ui
                .add(
                    egui::DragValue::new(&mut tempo)
                        .range(MIN_TEMPO_BPM..=MAX_TEMPO_BPM)
                        .speed(0.5)
                        .fixed_decimals(3)
                        .suffix(" BPM"),
                )
                .on_hover_text("Drag or type; one drag is one undo step");
            if r.changed() && tempo != self.project.tempo_bpm {
                self.apply(Edit::SetTempo(tempo), true, now);
            }
            if r.drag_stopped() || r.lost_focus() {
                self.history.end_gesture();
            }
            if ui.button("Tap").on_hover_text("Tap Tempo").clicked() {
                self.tap.get_or_insert_with(toolbar::TapTempo::default);
            }
            if let Some(sig) = toolbar::signature_field(ui, self.project.time_signature) {
                self.apply(Edit::SetTimeSignature(sig), false, now);
            }
            ui.separator();
            self.pattern_controls(ui, now);
            ui.separator();
            ui.label(egui::RichText::new("Snap").color(theme::MUTED));
            egui::ComboBox::from_id_salt("snap")
                .width(90.0)
                .selected_text(self.snap.name())
                .show_ui(ui, |ui| {
                    for v in toolbar::SNAPS {
                        ui.selectable_value(&mut self.snap, v, v.name());
                    }
                })
                .response
                .on_hover_text("Grid for the Playlist and the Piano Roll");
            ui.separator();
            ui.toggle_value(&mut self.show_browser, "Browser");
            ui.toggle_value(&mut self.show_rack, "Rack");
            let mut piano = self.piano.is_some();
            if ui
                .toggle_value(&mut piano, "Piano")
                .on_hover_text(
                    "Open the Piano Roll for the first channel; \
                     use a channel's Piano button for another",
                )
                .changed()
            {
                self.piano = match (piano, self.project.channels.first()) {
                    (true, Some(c)) => Some(piano::PianoRoll::new(c.id)),
                    _ => None,
                };
            }
            ui.toggle_value(&mut self.show_playlist, "Playlist");
            ui.toggle_value(&mut self.show_mixer, "Mixer");
            ui.separator();
            // Performance.
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let load = transport.peak_load.clamp(0.0, 1.0);
                let pct = transport.peak_load * 100.0;
                let text = if transport.overloads > 0 {
                    egui::RichText::new(format!("DSP {pct:.0}% · {} late", transport.overloads))
                        .color(theme::RED)
                } else {
                    egui::RichText::new(format!("DSP {pct:.0}%"))
                };
                ui.label(text.font(theme::mono(11.0))).on_hover_text(
                    "Late: buffers that took longer to process than to play; \
                     each one likely caused a dropout",
                );
                let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 5.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, theme::RAISED);
                let mut fill = rect;
                fill.set_width(rect.width() * load);
                let color = if load > 0.85 {
                    theme::RED
                } else if load > 0.6 {
                    theme::AMBER
                } else {
                    theme::GREEN
                };
                ui.painter().rect_filled(fill, 2.0, color);
            })
            .response
            .on_hover_text("Audio processing time per buffer, as a share of the buffer's duration");
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let t = self.host.transport();
        ui.horizontal(|ui| {
            let changes = if self.history.is_dirty() {
                egui::RichText::new("Unsaved changes").color(theme::WARN)
            } else {
                egui::RichText::new("All changes saved").color(theme::OK)
            };
            ui.label(changes.size(11.0));
            ui.label(
                egui::RichText::new(format!("Voices {}", t.voices))
                    .size(11.0)
                    .color(theme::MUTED),
            );
            if t.dropped_commands > 0 {
                ui.colored_label(
                    theme::WARN,
                    format!("Dropped commands {}", t.dropped_commands),
                );
            }
            // Armed input: its level, and the take's length while recording.
            let rec_state = self.host.recording_state();
            if let RecordingState::Recording { seconds } = rec_state {
                ui.label(
                    egui::RichText::new(format!("REC {seconds:.1} s"))
                        .font(theme::mono(11.0))
                        .color(theme::RED),
                );
                ui.ctx().request_repaint_after_secs(0.1);
            }
            if self.playlist_view.armed.is_some()
                && matches!(
                    rec_state,
                    RecordingState::Ready | RecordingState::Recording { .. }
                )
            {
                ui.label(egui::RichText::new("Input").size(11.0).color(theme::MUTED));
                let meter = self.recording_panel.update_meter(&mut self.host);
                recording::input_meter(ui, meter, 80.0);
                ui.ctx().request_repaint_after_secs(0.05);
            }
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match self.host.stream() {
                    Some(s) => {
                        let state = match &s.state {
                            jdance_audio_api::StreamState::Running => String::new(),
                            jdance_audio_api::StreamState::Failed(e) => format!(" · FAILED: {e}"),
                        };
                        let buffer = match (s.stats.min_frames, s.stats.max_frames) {
                            (Some(a), Some(b)) if a == b => format!("{a} frames"),
                            (Some(a), Some(b)) => format!("{a}–{b} frames"),
                            _ => "—".into(),
                        };
                        let ms = s.stats.last_frames.map_or(String::new(), |f| {
                            format!(
                                " · {:.1} ms",
                                f64::from(f) * 1000.0 / f64::from(s.info.sample_rate)
                            )
                        });
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {} · {} Hz · {buffer}{ms}{state}",
                                s.info.backend, s.info.device_name, s.info.sample_rate
                            ))
                            .font(theme::mono(11.0)),
                        );
                    }
                    None => {
                        ui.colored_label(theme::WARN, "No audio device open");
                    }
                },
            );
        });
    }

    fn playlist_action(&mut self, action: playlist::PlaylistAction, now: f64) {
        use playlist::PlaylistAction as A;
        let result = match action {
            A::Duplicate(ids) => {
                duplicate_clips(&mut self.history, &mut self.project, &ids).map(|new| {
                    // The copies are selected and shown (spec 0.2.5 §1).
                    let pl = &self.project.playlist;
                    let first = new
                        .iter()
                        .filter_map(|id| {
                            pl.clip(*id)
                                .map(|c| c.start)
                                .or_else(|| pl.automation_clip(*id).map(|c| c.start))
                                .or_else(|| {
                                    pl.audio_clip(*id)
                                        .and_then(|c| self.project.audio_clip_ticks(c))
                                        .map(|s| s.0 as u64)
                                })
                        })
                        .min();
                    self.playlist_view.select_only(&new);
                    self.playlist_view.reveal = first;
                })
            }
            A::MakeUnique(id) => make_unique(&mut self.history, &mut self.project, id).map(|p| {
                self.current_pattern = p;
            }),
            A::MakeUniqueAudio(ids) => audio_channels::make_audio_unique(&mut self.project, &ids)
                .and_then(|(ch, edit)| {
                    self.history.execute(&mut self.project, edit)?;
                    self.new_channels.push(ch);
                    Ok(())
                }),
            A::SelectChannel(ch) => {
                self.rack_view.selection = std::iter::once(ch).collect();
                self.show_rack = true;
                self.new_channels.push(ch);
                Ok(())
            }
            A::ToSampler(ch) => {
                self.send_to_sampler(ch);
                Ok(())
            }
            A::AddLane => add_lane(&mut self.history, &mut self.project).map(|_| ()),
            A::RenameLane(id) => {
                self.start_rename(RenameTarget::Lane(id));
                Ok(())
            }
            A::Color(id) => {
                self.open_color(vec![Named::Lane(id)]);
                Ok(())
            }
            A::NameFromDrop(id) => {
                self.name_from_drop(Named::Lane(id), now);
                Ok(())
            }
            A::ResetLane(id) => {
                let i = self.project.playlist.lanes.iter().position(|l| l.id == id);
                if let Some(i) = i
                    && let Some(edit) = naming::set_name_color(
                        &self.project,
                        &[Named::Lane(id)],
                        Some(&format!("Lane {}", i + 1)),
                        Some(naming::default_color(&self.project, Named::Lane(id))),
                    )
                {
                    self.apply(edit, false, now);
                }
                Ok(())
            }
            A::Seek(ticks) => {
                self.set_caret(ticks);
                Ok(())
            }
            A::CaretPreview(ticks) => {
                // While playing, the playhead jumps on release (CUR-012).
                self.caret = ticks;
                if self.song_mode && !self.host.transport().playing {
                    self.host.seek_ticks(ticks);
                }
                Ok(())
            }
            A::RenderSelection => {
                self.open_export_dialog(Some(RangeKind::Selection));
                Ok(())
            }
            A::Place {
                source,
                lane,
                start,
                count,
            } => self.place(source, lane, start, count),
            A::ImportAudio { path, lane, start } => {
                let dir = self.project_dir().map(Path::to_path_buf);
                match self.host.import_audio(&path, dir.as_deref()) {
                    Some(t) => {
                        self.pending_imports.insert(t, Some((lane, start)));
                        Ok(())
                    }
                    None => Err(jdance_commands::EditError::Arrangement(
                        "audio import is not available".into(),
                    )),
                }
            }
            A::Slice { clip, tick } => self.slice_clip(clip, tick),
            A::Open(id) => {
                self.open_clip(id);
                Ok(())
            }
            A::AutomateGain(id) => {
                if let Some(c) = self.project.playlist.audio_clip(id) {
                    let request = automation::AutomationRequest::Create {
                        target: jdance_domain::AutomationTarget::AudioClipGain(id),
                        initial: c.gain / jdance_domain::MAX_VOLUME,
                    };
                    self.automation_request(request, now);
                }
                Ok(())
            }
        };
        match result {
            Ok(()) => self.sync_needed = true,
            Err(e) => self.notify(Notice::error(e.to_string()), now),
        }
    }

    /// `count` consecutive copies of `source` on `lane` from `start` ticks,
    /// one undo step; the new clips are selected.
    fn place(
        &mut self,
        source: playlist::Source,
        lane: Option<LaneId>,
        start: u64,
        count: u32,
    ) -> Result<(), jdance_commands::EditError> {
        let p = &mut self.project;
        let mut edits = Vec::new();
        let mut ids = Vec::new();
        let lane = match lane {
            Some(l) => l,
            None => {
                let name = match source {
                    playlist::Source::Pattern(id) => p.pattern(id).map(|x| x.name.clone()),
                    playlist::Source::Asset(id) => p.asset(id).map(|x| x.name.clone()),
                    playlist::Source::Channel(id) => p.channel(id).map(|x| x.name.clone()),
                    playlist::Source::Automation(id) => {
                        p.automation_lane(id).map(|x| x.name.clone())
                    }
                };
                let (id, edit) = new_lane_edit(p, name.unwrap_or_else(|| "Lane".into()));
                edits.push(edit);
                id
            }
        };
        match source {
            playlist::Source::Pattern(id) => {
                let pat = p
                    .pattern(id)
                    .ok_or(jdance_commands::EditError::NoSuchPattern(id))?;
                let len = pat.length_ticks();
                let pat = pat.clone();
                for k in 0..u64::from(count.max(1)) {
                    let cid = ClipId(p.ids.next());
                    ids.push(cid);
                    edits.push(Edit::AddClip(jdance_domain::PatternClip::repeating(
                        cid,
                        lane,
                        &pat,
                        start + k * len,
                        1,
                    )));
                }
            }
            playlist::Source::Asset(_) | playlist::Source::Channel(_) => {
                let gone =
                    || jdance_commands::EditError::Arrangement("the audio file is gone".into());
                // A file gets its channel (found or new, ACH-020); a channel
                // places its own instances (ACH-043).
                let (asset, channel) = match source {
                    playlist::Source::Channel(ch) => {
                        let a = p
                            .channel(ch)
                            .and_then(|c| c.audio_asset())
                            .ok_or_else(gone)?;
                        (p.asset(a).cloned().ok_or_else(gone)?, ch)
                    }
                    playlist::Source::Asset(id) => {
                        let asset = p.asset(id).cloned().ok_or_else(gone)?;
                        let mut plan = audio_channels::ChannelPlan::new();
                        let ch = plan.channel_for(p, &asset, &mut edits);
                        self.new_channels.extend(plan.created());
                        (asset, ch)
                    }
                    _ => unreachable!("matched above"),
                };
                let fpt = jdance_domain::frames_per_tick(p.tempo_bpm, p.timebase_rate);
                let first = (start as f64 * fpt).round() as u64;
                for k in 0..u64::from(count.max(1)) {
                    let cid = ClipId(p.ids.next());
                    ids.push(cid);
                    let mut clip = audio_channels::audio_clip(cid, lane, channel, &asset, 0);
                    clip.start = first + k * clip.length(&asset, p.timebase_rate);
                    edits.push(Edit::AddAudioClip(clip));
                }
            }
            playlist::Source::Automation(id) => {
                let len = p
                    .automation_lane(id)
                    .ok_or_else(|| {
                        jdance_commands::EditError::Arrangement("the automation is gone".into())
                    })?
                    .length;
                for k in 0..u64::from(count.max(1)) {
                    let cid = ClipId(p.ids.next());
                    ids.push(cid);
                    edits.push(Edit::AddAutomationClip(jdance_domain::AutomationClip {
                        id: cid,
                        lane,
                        source: id,
                        start: start + k * len,
                        offset: 0,
                        duration: len,
                        muted: false,
                    }));
                }
            }
        }
        self.history
            .execute(&mut self.project, Edit::Batch(edits))?;
        self.playlist_view.select_only(&ids);
        Ok(())
    }

    /// Imports finished in the background become an asset (reused when the
    /// same content is already in the project) and a clip, as one undo
    /// step; a missing lane is created.
    fn finish_imports(&mut self, now: f64) {
        for (ticket, result) in self.host.take_imports() {
            let Some(target) = self.pending_imports.remove(&ticket) else {
                continue;
            };
            let audio = match result {
                Ok(a) => a,
                Err(e) => {
                    self.notify(Notice::error(format!("Import failed: {e}")), now);
                    continue;
                }
            };
            let p = &mut self.project;
            let mut edits = Vec::new();
            let asset = match p
                .assets
                .iter()
                .find(|a| a.hash == audio.hash && a.size == audio.size)
            {
                Some(a) => a.clone(),
                None => {
                    let a = jdance_domain::AudioAsset {
                        id: jdance_domain::AssetId(p.ids.next()),
                        name: audio.name.clone(),
                        path: audio.path.clone(),
                        hash: audio.hash,
                        size: audio.size,
                        sample_rate: audio.sample_rate,
                        channels: audio.channels,
                        frames: audio.frames,
                    };
                    edits.push(Edit::AddAsset(a.clone()));
                    a
                }
            };
            let Some(target) = target else {
                // Import only: the file joins the project's samples.
                if !edits.is_empty()
                    && let Err(e) = self.history.execute(&mut self.project, Edit::Batch(edits))
                {
                    self.notify(Notice::error(format!("Import failed: {e}")), now);
                }
                continue;
            };
            let (lane, start) = target;
            let lane = match lane {
                Some(l) => l,
                None => {
                    let (id, edit) = new_lane_edit(p, audio.name.clone());
                    edits.push(edit);
                    id
                }
            };
            let fpt = jdance_domain::frames_per_tick(p.tempo_bpm, p.timebase_rate);
            let mut plan = audio_channels::ChannelPlan::new();
            let channel = plan.channel_for(p, &asset, &mut edits);
            let cid = ClipId(p.ids.next());
            let clip = audio_channels::audio_clip(
                cid,
                lane,
                channel,
                &asset,
                (start as f64 * fpt).round() as u64,
            );
            edits.push(Edit::AddAudioClip(clip));
            match self.history.execute(&mut self.project, Edit::Batch(edits)) {
                Ok(()) => {
                    self.sync_needed = true;
                    self.new_channels.extend(plan.created());
                    self.playlist_view.select_only(&[cid]);
                    // A file from the library places a clip; the active
                    // source stays (spec 0.2.7 §2 item 10).
                }
                Err(e) => self.notify(Notice::error(format!("Import failed: {e}")), now),
            }
        }
    }

    /// Cuts a pattern or audio clip at `tick` into two (one undo step).
    fn slice_clip(&mut self, id: ClipId, tick: u64) -> Result<(), jdance_commands::EditError> {
        let p = &mut self.project;
        let new_id = ClipId(p.ids.next());
        let edits = if let Some(c) = p.playlist.clip(id).copied() {
            let len = p.pattern(c.pattern).map_or(1, |x| x.length_ticks());
            let Some((l, r)) = jdance_commands::slice_pattern_clip(&c, len, tick, new_id) else {
                return Ok(());
            };
            vec![
                Edit::SetClipShape {
                    id,
                    start: l.start,
                    offset: l.offset,
                    duration: l.duration,
                    looped: l.looped,
                },
                Edit::AddClip(r),
            ]
        } else if let Some(c) = p.playlist.audio_clip(id).copied() {
            let Some(asset) = p.clip_asset(&c).cloned() else {
                return Ok(());
            };
            let fpt = jdance_domain::frames_per_tick(p.tempo_bpm, p.timebase_rate);
            let at = (tick as f64 * fpt).round() as u64;
            let Some((l, r)) =
                jdance_commands::slice_audio_clip(&c, &asset, p.timebase_rate, at, new_id)
            else {
                return Ok(());
            };
            vec![Edit::SetAudioClip(l), Edit::AddAudioClip(r)]
        } else if let Some(c) = p.playlist.automation_clip(id).copied() {
            let (a, b) = c.interval();
            if tick <= a || tick >= b {
                return Ok(());
            }
            let left = jdance_domain::AutomationClip {
                duration: tick - a,
                ..c
            };
            let right = jdance_domain::AutomationClip {
                id: new_id,
                start: tick,
                offset: c.offset + (tick - a),
                duration: b - tick,
                ..c
            };
            vec![
                Edit::SetAutomationClip(left),
                Edit::AddAutomationClip(right),
            ]
        } else {
            return Ok(());
        };
        self.history.execute(&mut self.project, Edit::Batch(edits))
    }

    /// Double-click on a clip: the Piano Roll of its pattern, or the
    /// Sample Editor of an audio clip (spec 0.2.0 §3).
    fn open_clip(&mut self, id: ClipId) {
        if let Some(c) = self.project.playlist.clip(id) {
            self.current_pattern = c.pattern;
            let channel = self
                .piano
                .as_ref()
                .map(|p| p.channel)
                .or_else(|| {
                    self.rack_view
                        .selection
                        .iter()
                        .copied()
                        .find(|id| self.project.channel(*id).is_some_and(|c| !c.is_audio()))
                })
                .or_else(|| {
                    // Audio channels have no notes.
                    self.project
                        .channels
                        .iter()
                        .find(|c| !c.is_audio())
                        .map(|c| c.id)
                });
            if let Some(ch) = channel {
                self.piano = Some(piano::PianoRoll::new(ch));
            }
        } else if let Some(c) = self.project.playlist.audio_clip(id) {
            self.sample_editor = Some(sample_editor::SampleEditor::for_clip(c));
        } else if let Some(c) = self.project.playlist.automation_clip(id) {
            self.open_automation(c.source);
        }
    }

    /// Tools > LFO: writes the curve into the chosen source (a new one
    /// gets no Playlist clip) and opens it in the Automation Editor.
    fn lfo_window(&mut self, ctx: &egui::Context, now: f64) {
        let Some(mut d) = self.lfo_dialog.take() else {
            return;
        };
        let (project, host) = (&self.project, &mut self.host);
        let r = d.show(ctx, project, &mut |t| {
            automation::source_name(project, t, host)
        });
        if let Some((target, points, range)) = r.apply {
            let p = &mut self.project;
            let (id, lane) = match target {
                lfo_dialog::LfoTarget::Existing(id) => match p.automation_lane(id) {
                    Some(a) => {
                        let mut a = a.clone();
                        a.points =
                            jdance_commands::note_tools::splice_points(&a.points, range, &points);
                        a.length = a.length.max(u64::from(range.end));
                        (id, a)
                    }
                    None => {
                        self.lfo_dialog = None;
                        return;
                    }
                },
                lfo_dialog::LfoTarget::New(target) => {
                    let id = jdance_domain::AutomationId(p.ids.next());
                    let name = automation::source_name(p, &target, &mut self.host);
                    let bar = u64::from(p.time_signature.bar_ticks());
                    (
                        id,
                        jdance_domain::AutomationLane {
                            id,
                            name,
                            target,
                            length: u64::from(range.end).div_ceil(bar).max(1) * bar,
                            points,
                        },
                    )
                }
            };
            self.apply(
                Edit::SetAutomation {
                    id,
                    lane: Some(lane),
                },
                false,
                now,
            );
            self.open_automation(id);
        }
        if !r.close {
            self.lfo_dialog = Some(d);
        }
    }

    /// Opens (or brings back) the Automation Editor of `id`.
    fn open_automation(&mut self, id: jdance_domain::AutomationId) {
        if !self.automation_editors.iter().any(|e| e.source == id) {
            self.automation_editors
                .push(automation_editor::AutomationEditor::new(id));
        }
    }

    fn automation_editor_windows(&mut self, ctx: &egui::Context, now: f64) {
        let mut editors = std::mem::take(&mut self.automation_editors);
        let mut out = automation_editor::EditorOutput::default();
        editors.retain_mut(|e| {
            e.show(
                ctx,
                &self.project,
                u64::from(self.snap.ticks(self.project.time_signature).max(1)),
                &mut self.host,
                &mut out,
                &mut self.layout,
            )
        });
        // A window opened meanwhile (none can be) would be kept too.
        editors.append(&mut self.automation_editors);
        self.automation_editors = editors;
        for edit in out.edits {
            self.apply(edit, false, now);
        }
        for edit in out.gesture {
            self.apply(edit, true, now);
        }
        if out.end_gesture {
            self.history.end_gesture();
        }
    }

    fn project_action(&mut self, action: project_browser::ProjectAction, now: f64) {
        use project_browser::ProjectAction as A;
        let result: Result<(), jdance_commands::EditError> = match action {
            A::Select(src) => {
                if let playlist::Source::Pattern(id) = src {
                    self.current_pattern = id;
                    // The playlist follows the current pattern.
                }
                self.playlist_view.source = Some(src);
                Ok(())
            }
            A::OpenPattern(id) => {
                self.current_pattern = id;
                let channel = self
                    .piano
                    .as_ref()
                    .map(|p| p.channel)
                    .or_else(|| self.rack_view.selection.iter().next().copied())
                    .or_else(|| self.project.channels.first().map(|c| c.id));
                if let Some(ch) = channel {
                    self.piano = Some(piano::PianoRoll::new(ch));
                }
                Ok(())
            }
            A::OpenAsset(id) => {
                self.sample_editor = Some(sample_editor::SampleEditor::new(
                    sample_editor::Target::Asset(id),
                ));
                Ok(())
            }
            A::AddPattern => {
                self.new_pattern(now);
                Ok(())
            }
            A::DuplicatePattern(id) => match self.project.pattern(id).cloned() {
                Some(src) => {
                    let new_id = PatternId(self.project.ids.next());
                    let index = self
                        .project
                        .patterns
                        .iter()
                        .position(|p| p.id == id)
                        .map_or(self.project.patterns.len(), |i| i + 1);
                    self.history
                        .execute(
                            &mut self.project,
                            Edit::AddPattern {
                                pattern: jdance_domain::Pattern {
                                    id: new_id,
                                    name: format!("{} copy", src.name),
                                    ..src
                                },
                                index,
                            },
                        )
                        .map(|()| self.current_pattern = new_id)
                }
                None => Ok(()),
            },
            A::RenamePattern(id) => {
                self.start_rename(RenameTarget::Pattern(id));
                Ok(())
            }
            A::ColorPattern(id) => {
                self.open_color(vec![Named::Pattern(id)]);
                Ok(())
            }
            A::DeletePattern(id) => {
                self.confirm_delete = Some(playlist::Source::Pattern(id));
                Ok(())
            }
            A::ImportFiles => {
                self.sample_picker = Some(library::SamplePicker::new(library::PickFor::Import));
                Ok(())
            }
            A::RenameAsset(id) => {
                self.start_rename(RenameTarget::Asset(id));
                Ok(())
            }
            A::PreviewAsset(id) => {
                if let Some(a) = self.project.asset(id) {
                    let path = match self.project_dir() {
                        Some(d) if Path::new(&a.path).is_relative() => d.join(&a.path),
                        _ => PathBuf::from(&a.path),
                    };
                    self.host.preview_file(Some(&path));
                }
                Ok(())
            }
            A::LoadIntoSampler(id) => {
                if let Some(a) = self.project.asset(id).cloned() {
                    self.load_into_sampler(&a, None, 0.0, 1.0, now);
                }
                Ok(())
            }
            A::DeleteAsset(id) => {
                self.confirm_delete = Some(playlist::Source::Asset(id));
                Ok(())
            }
            A::OpenAutomation(id) => {
                self.open_automation(id);
                Ok(())
            }
            A::RenameAutomation(id) => {
                self.start_rename(RenameTarget::Automation(id));
                Ok(())
            }
            A::DuplicateAutomation(id) => match self.project.automation_lane(id).cloned() {
                Some(src) => {
                    let new_id = jdance_domain::AutomationId(self.project.ids.next());
                    self.history.execute(
                        &mut self.project,
                        Edit::SetAutomation {
                            id: new_id,
                            lane: Some(jdance_domain::AutomationLane {
                                id: new_id,
                                name: format!("{} copy", src.name),
                                ..src
                            }),
                        },
                    )
                }
                None => Ok(()),
            },
            A::DeleteAutomation(id) => {
                self.confirm_delete = Some(playlist::Source::Automation(id));
                Ok(())
            }
        };
        match result {
            Ok(()) => self.sync_needed = true,
            Err(e) => self.notify(Notice::error(e.to_string()), now),
        }
    }

    /// Deleting a pattern or file that clips use needs a confirmation
    /// naming what goes with it (spec 0.2.0 §10). One undo step.
    fn confirm_delete_dialog(&mut self, ctx: &egui::Context, now: f64) {
        let Some(target) = self.confirm_delete else {
            return;
        };
        let p = &self.project;
        let (name, clips, samplers) = match target {
            playlist::Source::Pattern(id) => match p.pattern(id) {
                Some(x) => (
                    x.name.clone(),
                    p.playlist.clips.iter().filter(|c| c.pattern == id).count(),
                    0,
                ),
                None => {
                    self.confirm_delete = None;
                    return;
                }
            },
            playlist::Source::Automation(id) => match p.automation_lane(id) {
                Some(a) => (a.name.clone(), p.automation_usage(id), 0),
                None => {
                    self.confirm_delete = None;
                    return;
                }
            },
            playlist::Source::Channel(id) => match p.channel(id) {
                Some(c) => (c.name.clone(), p.channel_clips(id).count(), 0),
                None => {
                    self.confirm_delete = None;
                    return;
                }
            },
            playlist::Source::Asset(id) => match p.asset(id) {
                Some(a) => (
                    a.name.clone(),
                    p.asset_usage(id),
                    p.channels
                        .iter()
                        .filter(|c| c.sample().is_some_and(|s| s.path == a.path))
                        .count(),
                ),
                None => {
                    self.confirm_delete = None;
                    return;
                }
            },
        };
        if clips == 0 && samplers == 0 {
            self.confirm_delete = None;
            self.delete_source(target, now);
            return;
        }
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("confirm_delete")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(format!("Delete \u{201c}{name}\u{201d}?"));
            if clips > 0 {
                ui.label(format!("{clips} clip(s) in the Playlist use it and are deleted with it."));
            }
            if samplers > 0 {
                ui.label(format!(
                    "{samplers} sampler channel(s) play this file; they keep it (the file itself is not deleted)."
                ));
            }
            ui.label(egui::RichText::new("You can undo this.").color(theme::MUTED));
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    choice = Some(false);
                }
            });
        });
        match choice {
            Some(true) => {
                self.confirm_delete = None;
                self.delete_source(target, now);
            }
            Some(false) => self.confirm_delete = None,
            None => {}
        }
    }

    fn delete_source(&mut self, target: playlist::Source, now: f64) {
        let edit = match target {
            playlist::Source::Pattern(id) => Edit::RemovePattern(id),
            playlist::Source::Automation(id) => {
                if let Err(e) =
                    jdance_commands::remove_automation(&mut self.history, &mut self.project, id)
                {
                    self.notify(Notice::error(e.to_string()), now);
                }
                self.sync_needed = true;
                return;
            }
            playlist::Source::Channel(id) => audio_channels::remove_channel(&self.project, id),
            playlist::Source::Asset(id) => {
                // Its audio channels go with their clips, then the file.
                let p = &self.project;
                let mut edits: Vec<Edit> = p
                    .audio_channels_of(id)
                    .map(|c| audio_channels::remove_channel(p, c.id))
                    .collect();
                edits.push(Edit::RemoveAsset(id));
                Edit::Batch(edits)
            }
        };
        self.apply(edit, false, now);
    }

    fn sample_picker_window(&mut self, ctx: &egui::Context, now: f64) {
        let Some(mut picker) = self.sample_picker.take() else {
            return;
        };
        let dir = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        match picker.show(ctx, &self.project, &mut self.host, dir.as_deref()) {
            None => self.sample_picker = Some(picker),
            Some(None) => {}
            Some(Some(picked)) => {
                let (name, sample) = match &picked {
                    library::Picked::File(p) => (
                        p.file_stem()
                            .map_or_else(|| "Sample".into(), |s| s.to_string_lossy().into_owned()),
                        jdance_domain::SampleRef {
                            path: p.to_string_lossy().into_owned(),
                        },
                    ),
                    library::Picked::Asset(id) => match self.project.asset(*id) {
                        Some(a) => (
                            a.name.clone(),
                            jdance_domain::SampleRef {
                                path: a.path.clone(),
                            },
                        ),
                        None => return,
                    },
                };
                match picker.purpose {
                    library::PickFor::NewChannel => self.add_channel(name, Some(sample), now),
                    library::PickFor::Channel(id) => {
                        self.apply(Edit::SetChannelSample(id, Some(sample)), false, now);
                    }
                    library::PickFor::Import => {
                        if let library::Picked::File(p) = picked {
                            let dir = self.project_dir().map(Path::to_path_buf);
                            if let Some(t) = self.host.import_audio(&p, dir.as_deref()) {
                                self.pending_imports.insert(t, None);
                            }
                        }
                    }
                }
            }
        }
    }

    fn sample_editor_window(&mut self, ctx: &egui::Context, now: f64) {
        let Some(mut ed) = self.sample_editor.take() else {
            return;
        };
        let dir = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let mut out = sample_editor::SampleEditorOutput::default();
        let open = ed.show(
            ctx,
            &self.project,
            &mut self.host,
            dir.as_deref(),
            &mut out,
            &mut self.layout,
        );
        let merging = !out.end_gesture;
        for edit in out.edits {
            self.apply(edit, merging, now);
        }
        if out.end_gesture {
            self.history.end_gesture();
        }
        let at = self.host.transport().position_ticks.unwrap_or(0);
        let first_lane = self.project.playlist.lanes.first().map(|l| l.id);
        if let Some((asset, a, b)) = out.place
            && let Some(lane) = first_lane
            && let Some(asset) = self.project.asset(asset).cloned()
        {
            let p = &mut self.project;
            let fpt = jdance_domain::frames_per_tick(p.tempo_bpm, p.timebase_rate);
            let mut edits = Vec::new();
            // The edited clip's channel; for a file without a clip, its
            // channel or a new one (ACH-017).
            let channel = match ed.clip().and_then(|c| p.playlist.audio_clip(c)) {
                Some(c) => c.channel,
                None => {
                    let mut plan = audio_channels::ChannelPlan::new();
                    let ch = plan.channel_for(p, &asset, &mut edits);
                    self.new_channels.extend(plan.created());
                    ch
                }
            };
            let id = ClipId(p.ids.next());
            let mut clip = audio_channels::audio_clip(
                id,
                lane,
                channel,
                &asset,
                (at as f64 * fpt).round() as u64,
            );
            clip.source_in = a;
            clip.source_out = b.max(a + 1);
            edits.push(Edit::AddAudioClip(clip));
            self.apply(Edit::Batch(edits), false, now);
            self.playlist_view.select_only(&[id]);
        }
        if let Some((asset, channel, (start, end))) = out.load
            && let Some(asset) = self.project.asset(asset).cloned()
        {
            self.load_into_sampler(&asset, channel, start, end, now);
        }
        if let Some(path) = out.import {
            self.playlist_action(
                playlist::PlaylistAction::ImportAudio {
                    path,
                    lane: first_lane,
                    start: at,
                },
                now,
            );
        }
        if open {
            self.sample_editor = Some(ed);
        }
    }

    /// Points a sampler (or a new one) at `asset`, with its start/end
    /// markers on `start..end` (fractions), as one undo step.
    fn load_into_sampler(
        &mut self,
        asset: &jdance_domain::AudioAsset,
        channel: Option<ChannelId>,
        start: f32,
        end: f32,
        now: f64,
    ) {
        let sample = jdance_domain::SampleRef {
            path: asset.path.clone(),
        };
        let id = match channel {
            Some(id) => {
                self.apply(Edit::SetChannelSample(id, Some(sample)), false, now);
                id
            }
            None => match jdance_commands::add_sample_channel(
                &mut self.history,
                &mut self.project,
                asset.name.clone(),
                Some(sample),
            ) {
                Ok(id) => id,
                Err(e) => {
                    self.notify(Notice::error(e.to_string()), now);
                    return;
                }
            },
        };
        if (start, end) != (0.0, 1.0)
            && let Some(ch) = self.project.channel(id)
        {
            let mut s = ch.sampler;
            s.start = start.clamp(0.0, 1.0);
            s.end = end.clamp(s.start, 1.0);
            self.apply(Edit::SetSampler(id, s), false, now);
        }
        self.sync_needed = true;
        self.notify(
            Notice::info(format!(
                "{} loaded into a sampler; edit its notes in the Piano Roll",
                asset.name
            )),
            now,
        );
    }

    /// Pattern selector, new/rename, length and loop toggle.
    fn pattern_controls(&mut self, ui: &mut egui::Ui, now: f64) {
        if self.project.pattern(self.current_pattern).is_none() {
            self.current_pattern = self.project.patterns[0].id;
        }
        let current = self.project.pattern(self.current_pattern).expect("checked");
        let (current_name, current_len) = (current.name.clone(), current.length_steps);
        let mut selected = self.current_pattern;
        egui::ComboBox::from_id_salt("pattern")
            .width(92.0)
            .selected_text(current_name)
            .show_ui(ui, |ui| {
                for p in &self.project.patterns {
                    ui.selectable_value(&mut selected, p.id, &p.name);
                }
            })
            .response
            .on_hover_text("Pattern edited in the channel rack; new clips use it");
        self.current_pattern = selected;
        if ui
            .small_button("+")
            .on_hover_text("New pattern (F4)")
            .clicked()
        {
            self.new_pattern(now);
        }
        if ui.small_button("Rename").clicked() {
            self.start_rename(RenameTarget::Pattern(self.current_pattern));
        }
        let mut steps = current_len;
        let r = ui
            .add(
                egui::DragValue::new(&mut steps)
                    .range(1..=jdance_domain::MAX_PATTERN_STEPS)
                    .suffix(" steps"),
            )
            .on_hover_text("Pattern length; playlist clips keep their length (looped clips repeat the new length)");
        if r.changed() && steps != current_len {
            self.set_pattern_length(self.current_pattern, steps, false, now);
        }
        ui.separator();
        let mut looping = self.project.playlist.loop_enabled;
        if ui
            .toggle_value(&mut looping, "Loop")
            .on_hover_text("Loop the region set with Shift+drag on the playlist ruler (default: whole arrangement)")
            .changed()
        {
            let bar = u64::from(self.project.time_signature.bar_ticks());
            let end = self.project.arrangement_end_ticks().max(bar);
            let range = self.project.playlist.loop_range.or(Some((0, end)));
            self.apply(Edit::SetLoop { range, enabled: looping }, false, now);
        }
    }

    fn set_pattern_length(&mut self, pattern: PatternId, steps: u32, truncate: bool, now: f64) {
        let edit = Edit::SetPatternLength {
            pattern,
            steps,
            truncate,
        };
        match self.history.execute(&mut self.project, edit) {
            Ok(()) => self.sync_needed = true,
            Err(jdance_commands::EditError::WouldDropSteps(n)) => {
                self.pending_length = Some((pattern, steps, n));
            }
            Err(e) => self.notify(Notice::error(e.to_string()), now),
        }
    }

    fn length_dialog(&mut self, ctx: &egui::Context, now: f64) {
        let Some((pattern, steps, dropped)) = self.pending_length else {
            return;
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("shorten")).show(ctx, |ui| {
            ui.heading("Shorten pattern?");
            ui.label(format!(
                "Shortening to {steps} steps removes {dropped} active step(s) beyond the new end. \
                 Clips keep their repeat counts and may now overlap."
            ));
            ui.horizontal(|ui| {
                if ui.button("Truncate").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(false);
                }
            });
        });
        match choice {
            Some(true) => {
                self.pending_length = None;
                self.set_pattern_length(pattern, steps, true, now);
            }
            Some(false) => self.pending_length = None,
            None => {}
        }
    }

    fn start_rename(&mut self, target: RenameTarget) {
        let p = &self.project;
        let found = match target {
            RenameTarget::Channel(id) => p.channel_index(id).map(|i| {
                let c = &p.channels[i];
                (
                    c.name.clone(),
                    Some(c.color),
                    jdance_domain::color_or_default(c.color, i),
                )
            }),
            RenameTarget::Track(id) => p.mixer.track(id).map(|t| {
                let i = p.mixer.tracks.iter().position(|x| x.id == id).unwrap_or(0);
                (
                    t.name.clone(),
                    Some(t.color),
                    jdance_domain::color_or_default(t.color, i),
                )
            }),
            RenameTarget::Pattern(id) => p.pattern(id).map(|x| {
                (
                    x.name.clone(),
                    Some(x.color),
                    jdance_domain::color::DEFAULT_PATTERN_COLOR,
                )
            }),
            RenameTarget::Lane(id) => p
                .playlist
                .lane(id)
                .map(|l| (l.name.clone(), Some(Some(l.color)), l.color)),
            RenameTarget::Asset(id) => p.asset(id).map(|a| (a.name.clone(), None, [0; 3])),
            RenameTarget::Automation(id) => p
                .automation_lane(id)
                .map(|a| (a.name.clone(), None, [0; 3])),
        };
        if let Some((text, color, default_color)) = found {
            self.rename = Some(RenameDialog {
                target,
                text,
                color,
                default_color,
                wheel: None,
                presets: matches!(target, RenameTarget::Pattern(_)).then(|| {
                    name_presets_ui::PresetList {
                        open: true,
                        selected: None,
                    }
                }),
                color_manual: false,
                hover_color: None,
                created: None,
                fresh: true,
            });
        }
    }

    fn browser_action(&mut self, action: browser::BrowserAction, now: f64) {
        use browser::BrowserAction as A;
        match action {
            A::SelectPattern(id) => self.current_pattern = id,
            A::ChannelSettings(id) => {
                self.channel_settings = Some(channel_settings::ChannelSettingsWindow::new(id));
            }
            A::UndoSteps(n) => {
                for _ in 0..n {
                    self.undo(now);
                }
            }
            A::ReplaceSample(path) => {
                self.replace_selected_sample(&path, now);
            }
            A::OpenFile(path) => {
                self.sample_editor = Some(sample_editor::SampleEditor::new(
                    sample_editor::Target::File(path),
                ));
            }
            A::LoadPlugin(d) => self.load_plugin(d, now),
        }
    }

    fn add_channel(&mut self, name: String, sample: Option<jdance_domain::SampleRef>, now: f64) {
        let added = add_sample_channel(&mut self.history, &mut self.project, name, sample);
        match added {
            Ok(_) => self.sync_needed = true,
            Err(e) => self.notify(Notice::error(e.to_string()), now),
        }
    }

    /// The file of audio channel `ch` into a new sampler channel,
    /// selected in the Channel Rack.
    fn send_to_sampler(&mut self, ch: ChannelId) {
        match jdance_commands::sampler::sampler_from_audio(&mut self.history, &mut self.project, ch)
        {
            Ok(id) => {
                self.sync_needed = true;
                self.rack_view.selection = std::iter::once(id).collect();
                self.show_rack = true;
                self.new_channels.push(id);
            }
            Err(e) => self.notify(Notice::error(e.to_string()), 0.0),
        }
    }

    /// An instrument into a new channel, an effect into the selected
    /// mixer track (spec 0.2.5 §10 item 12).
    fn load_plugin(&mut self, d: jdance_plugin_api::PluginDescriptor, now: f64) {
        match d.kind {
            jdance_plugin_api::PluginKind::Instrument => self.add_instrument(d, now),
            jdance_plugin_api::PluginKind::Effect => {
                let track = self.mixer_view.selected;
                self.mixer_action(mixer::MixerAction::AddPlugin(track, d), now);
            }
        }
    }

    fn add_instrument(&mut self, d: jdance_plugin_api::PluginDescriptor, now: f64) {
        let slot = PluginSlot {
            format: "vst3".into(),
            class_id: d.plugin.class_id,
            name: d.plugin.name.clone(),
            vendor: d.plugin.vendor,
            version: d.plugin.version,
            category: d.category,
            component_state: Vec::new(),
            controller_state: Vec::new(),
        };
        match jdance_commands::add_instrument_channel(
            &mut self.history,
            &mut self.project,
            d.plugin.name,
            slot,
        ) {
            Ok(_) => self.sync_needed = true,
            Err(e) => self.notify(Notice::error(e.to_string()), now),
        }
    }

    fn automation_request(&mut self, request: automation::AutomationRequest, now: f64) {
        match request {
            automation::AutomationRequest::Create { target, initial } => {
                let name = automation::source_name(&self.project, &target, &mut self.host);
                match jdance_commands::add_automation(
                    &mut self.history,
                    &mut self.project,
                    target,
                    initial,
                    name,
                ) {
                    Ok(id) => {
                        self.sync_needed = true;
                        self.piano = None;
                        self.show_playlist = true;
                        self.open_automation(id);
                        self.notify(
                            Notice::info("Automation clip added on a new Playlist lane"),
                            now,
                        );
                    }
                    Err(e) => self.notify(Notice::error(e.to_string()), now),
                }
            }
            automation::AutomationRequest::Edit(id) => self.open_automation(id),
            automation::AutomationRequest::Remove(id) => {
                if let Err(e) =
                    jdance_commands::remove_automation(&mut self.history, &mut self.project, id)
                {
                    self.notify(Notice::error(e.to_string()), now);
                }
                self.sync_needed = true;
            }
        }
    }

    /// Record pressed: needs an armed lane and an open input; the loop
    /// is turned off first, after asking (spec 0.2.0 §7).
    fn begin_recording(&mut self, now: f64) {
        if let RecordingState::NoInput(why) = self.host.recording_state() {
            self.show_recording = true;
            self.notify(Notice::warning(why), now);
            return;
        }
        if self.project.playlist.loop_enabled && self.project.playlist.loop_range.is_some() {
            self.loop_prompt = true;
            return;
        }
        self.start_recording(now);
    }

    fn start_recording(&mut self, now: f64) {
        let start = self.caret;
        // REC-012: the armed lane; else the first one free from the caret
        // on; else a new lane, created with the take.
        let lane = playlist::record_lane(&self.project, self.playlist_view.armed, start);
        let n = self
            .project
            .assets
            .iter()
            .filter(|a| a.name.starts_with("Recording"))
            .count()
            + 1;
        let dir = self.project_dir().map(Path::to_path_buf);
        match self
            .host
            .start_recording(&format!("Recording {n}"), dir.as_deref())
        {
            Ok(()) => {
                self.record_target = Some((lane, start));
                self.record_cancelled = false;
                self.playlist_view.live = Some(playlist::LiveTake {
                    lane,
                    start,
                    name: format!("Recording {n}"),
                    peaks: Vec::new(),
                });
                if self.sync_needed {
                    // The engine must have the current project (loop off).
                    self.sync_needed = false;
                    let dir = self.project_dir().map(Path::to_path_buf);
                    self.host.set_project(&self.project, dir.as_deref());
                }
                self.song_mode = true;
                self.host.set_mode(TransportMode::Song);
                self.mode_sent = Some(TransportMode::Song);
                // Recording always starts at the caret: a running playback
                // stops and returns there first (REC-010/011).
                if self.host.transport().playing {
                    self.host.stop();
                }
                self.host.seek_ticks(start);
                let beats = if self.host.metronome().count_in {
                    COUNT_IN_BARS * u32::from(self.project.time_signature.numerator)
                } else {
                    0
                };
                self.host.count_in(beats);
            }
            Err(e) => self.notify(Notice::error(format!("Cannot record: {e}")), now),
        }
    }

    /// Metronome and count-in toggles of the transport panel (MET-002,
    /// MET-020); right-click opens the metronome settings.
    fn metronome_buttons(&mut self, ui: &mut egui::Ui) {
        let mut m = self.host.metronome();
        let before = m;
        let r = ui
            .toggle_value(&mut m.on, "Metro")
            .on_hover_text("Metronome (Ctrl+M) · right-click: level");
        r.context_menu(|ui| metronome_settings_ui(ui, &mut m));
        ui.toggle_value(&mut m.count_in, "1·2·3·4")
            .on_hover_text(format!(
                "Count {COUNT_IN_BARS} bars before recording (Ctrl+P)"
            ));
        if m != before {
            self.host.set_metronome(m);
        }
    }

    /// Loop on at Record: turn it off and record, or cancel.
    fn loop_prompt_dialog(&mut self, ctx: &egui::Context, now: f64) {
        if !self.loop_prompt {
            return;
        }
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("record_loop")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("The loop is on");
            ui.label("Recording does not loop. Turn the loop off and record?");
            ui.horizontal(|ui| {
                if ui.button("Turn loop off and record").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    choice = Some(false);
                }
            });
        });
        if let Some(go) = choice {
            self.loop_prompt = false;
            if go {
                let range = self.project.playlist.loop_range;
                self.apply(
                    Edit::SetLoop {
                        range,
                        enabled: false,
                    },
                    false,
                    now,
                );
                self.sync_needed = true;
                self.start_recording(now);
            }
        }
    }

    /// A finished take becomes an asset, a new audio channel (REC-001…003)
    /// and a clip on the armed lane at the start position plus the offset;
    /// one undo step (undo keeps the file).
    fn finish_recording(&mut self, now: f64) {
        let Some(result) = self.host.take_recording() else {
            return;
        };
        let target = self.record_target.take();
        self.playlist_view.live = None;
        // Releases the transport hold if the take ended on its own.
        self.host.stop_recording();
        let take = match result {
            Ok(t) => t,
            Err(e) => {
                if std::mem::take(&mut self.record_cancelled) {
                    self.notify(Notice::info("Recording cancelled"), now);
                } else {
                    self.notify(Notice::error(format!("Recording failed: {e}")), now);
                }
                return;
            }
        };
        if let Some(problem) = &take.problem {
            self.notify(Notice::error(problem.clone()), now);
        }
        let Some((lane, start)) = target else {
            return;
        };
        let audio = take.audio;
        let settings = self.host.recording_settings();
        let offset_ms = settings.offset_ms;
        let input = settings.channels.label();
        let p = &mut self.project;
        let asset = jdance_domain::AudioAsset {
            id: jdance_domain::AssetId(p.ids.next()),
            name: audio.name.clone(),
            path: audio.path.clone(),
            hash: audio.hash,
            size: audio.size,
            sample_rate: audio.sample_rate,
            channels: audio.channels,
            frames: audio.frames,
        };
        let fpt = jdance_domain::frames_per_tick(p.tempo_bpm, p.timebase_rate);
        // ASIO-042: the driver's input + output latency is subtracted once.
        let latency = f64::from(take.latency_frames) / f64::from(audio.sample_rate.max(1));
        let at = (start as f64 * fpt).round() as i64
            + (f64::from(offset_ms) / 1000.0 * f64::from(p.timebase_rate)).round() as i64
            - (latency * f64::from(p.timebase_rate)).round() as i64;
        let mut edits = vec![Edit::AddAsset(asset.clone())];
        // REC-012: no free lane at the start: a new one, in the same undo
        // step as the take.
        let lane = match lane {
            Some(l) if p.playlist.lane(l).is_some() => l,
            _ => {
                let n = p
                    .playlist
                    .lanes
                    .iter()
                    .filter(|l| l.name.starts_with("Recording"))
                    .count()
                    + 1;
                let (id, add) = jdance_commands::new_lane_edit(p, format!("Recording {n}"));
                edits.push(add);
                id
            }
        };
        let channel = audio_channels::take_channel(p, asset.id, &input, &mut edits);
        let cid = ClipId(p.ids.next());
        let mut clip = audio_channels::audio_clip(cid, lane, channel, &asset, at.max(0) as u64);
        if at < 0 {
            // Earlier than the song start: the first part is trimmed.
            let skip = (-at as f64 * f64::from(asset.sample_rate) / f64::from(p.timebase_rate))
                .round() as u64;
            clip.source_in = skip.min(clip.source_out.saturating_sub(1));
        }
        let seconds = asset.frames as f64 / f64::from(asset.sample_rate.max(1));
        edits.push(Edit::AddAudioClip(clip));
        match self.history.execute(&mut self.project, Edit::Batch(edits)) {
            Ok(()) => {
                self.sync_needed = true;
                self.new_channels.push(channel);
                self.playlist_view.select_only(&[cid]);
                let text = if take.latency_frames > 0 {
                    format!(
                        "Recorded {seconds:.1} s; driver latency {} frames ({:.1} ms) compensated",
                        take.latency_frames,
                        latency * 1000.0
                    )
                } else {
                    format!("Recorded {seconds:.1} s")
                };
                self.notify(Notice::info(text), now);
            }
            Err(e) => self.notify(Notice::error(format!("Recording not added: {e}")), now),
        }
    }

    /// Looks for unfinished takes once per project folder.
    fn check_recovery(&mut self, ctx: &egui::Context, now: f64) {
        if self.recovery_checked.as_ref() != Some(&self.path) {
            self.recovery_checked = Some(self.path.clone());
            let dir = self.project_dir().map(Path::to_path_buf);
            let files = self.host.unfinished_recordings(dir.as_deref());
            if !files.is_empty() {
                self.recovery = Some(recording::RecoveryDialog::new(files));
            }
        }
        let Some(dialog) = &mut self.recovery else {
            return;
        };
        let choices = dialog.show(ctx);
        let done = dialog.files.is_empty();
        let dir = self.project_dir().map(Path::to_path_buf);
        for (path, action) in choices {
            match self.host.resolve_unfinished(&path, action, dir.as_deref()) {
                Ok(Some(audio)) => {
                    let p = &mut self.project;
                    let asset = jdance_domain::AudioAsset {
                        id: jdance_domain::AssetId(p.ids.next()),
                        name: audio.name.clone(),
                        path: audio.path.clone(),
                        hash: audio.hash,
                        size: audio.size,
                        sample_rate: audio.sample_rate,
                        channels: audio.channels,
                        frames: audio.frames,
                    };
                    self.apply(Edit::AddAsset(asset), false, now);
                    self.notify(
                        Notice::info(format!(
                            "{} recovered into the project's samples",
                            audio.name
                        )),
                        now,
                    );
                }
                Ok(None) => {}
                Err(e) => self.notify(Notice::error(format!("Recovery failed: {e}")), now),
            }
        }
        if done {
            self.recovery = None;
        }
    }

    fn mixer_action(&mut self, action: mixer::MixerAction, now: f64) {
        let result = match action {
            mixer::MixerAction::AddTrack => {
                let name = format!("Track {}", self.project.mixer.tracks.len() + 1);
                add_track(&mut self.history, &mut self.project, name).map(|_| ())
            }
            mixer::MixerAction::AddSend { from, to } => {
                add_send(&mut self.history, &mut self.project, from, to).map(|_| ())
            }
            mixer::MixerAction::AddEffect(track, kind) => add_insert(
                &mut self.history,
                &mut self.project,
                track,
                Effect::new(kind),
            )
            .map(|_| ()),
            mixer::MixerAction::AddPlugin(track, d) => add_insert(
                &mut self.history,
                &mut self.project,
                track,
                Effect::plugin(PluginSlot {
                    format: "vst3".into(),
                    class_id: d.plugin.class_id,
                    name: d.plugin.name,
                    vendor: d.plugin.vendor,
                    version: d.plugin.version,
                    category: d.category,
                    component_state: Vec::new(),
                    controller_state: Vec::new(),
                }),
            )
            .map(|_| ()),
            mixer::MixerAction::PickPlugin(id) => {
                self.plugin_picker = Some(PluginPicker::new(plugin_list::Scope::Effects, Some(id)));
                Ok(())
            }
            mixer::MixerAction::Rename(id) => {
                self.start_rename(RenameTarget::Track(id));
                Ok(())
            }
            mixer::MixerAction::Color(id) => {
                self.open_color(vec![Named::Track(id)]);
                Ok(())
            }
            mixer::MixerAction::NameFromDrop(id) => {
                self.name_from_drop(Named::Track(id), now);
                Ok(())
            }
        };
        match result {
            Ok(()) => self.sync_needed = true,
            Err(e) => self.notify(Notice::error(e.to_string()), now),
        }
    }

    /// Middle click in the Browser (spec 0.2.5 §3): the file replaces the
    /// sample of the selected channel, one undo step; no new channels.
    fn replace_selected_sample(&mut self, path: &Path, now: f64) {
        let selected = self
            .project
            .channels
            .iter()
            .map(|c| c.id)
            .find(|id| self.rack_view.selection.contains(id));
        let Some(ch) = selected else {
            self.notify(Notice::info("Select a channel"), now);
            return;
        };
        if !path.is_file() {
            self.notify(
                Notice::error(format!("Cannot open {}", path.display())),
                now,
            );
            return;
        }
        let sample = jdance_domain::SampleRef {
            path: path.to_string_lossy().into_owned(),
        };
        match jdance_commands::sampler::replace_sample(&self.project, ch, sample) {
            Ok(Some(edit)) => self.apply(edit, false, now),
            Ok(None) => {}
            Err(_) => self.notify(
                Notice::info("The selected channel does not take samples"),
                now,
            ),
        }
    }

    /// Names and colours `target` after what was just dropped on it
    /// (spec 0.2.5 §7, §8): one undo step, none when nothing changes.
    fn name_from_drop(&mut self, target: Named, now: f64) {
        let Some(d) = self.dragging.clone() else {
            return;
        };
        if let Some(edit) = naming::set_name_color(&self.project, &[target], Some(&d.name), d.color)
        {
            self.apply(edit, false, now);
        }
    }

    /// Opens the colour window on `targets` (one window: a second call
    /// switches its target).
    fn open_color(&mut self, targets: Vec<Named>) {
        self.close_color();
        let Some(&first) = targets.first() else {
            return;
        };
        let p = &self.project;
        let Some((name, _)) = naming::name_color(p, first) else {
            return;
        };
        let originals = targets
            .iter()
            .map(|t| naming::name_color(p, *t).and_then(|(_, c)| c))
            .collect();
        let picker = color_picker::Picker::new(
            naming::shown_color(p, first),
            naming::default_color(p, first),
            !self.layout.color_snap_off,
            !self.layout.color_safe_off,
        );
        let what = match first {
            Named::Lane(_) => "Lane",
            Named::Track(_) => "Mixer track",
            Named::Channel(_) => "Channel",
            Named::Pattern(_) => "Pattern",
        };
        self.color = Some(ColorWindow {
            targets,
            originals,
            picker,
            title: format!("Color · {what} {name}"),
        });
    }

    /// Closes the colour window, undoing its live view.
    fn close_color(&mut self) {
        if let Some(w) = self.color.take() {
            for (t, c) in w.targets.iter().zip(&w.originals) {
                naming::write_color(&mut self.project, *t, *c);
            }
        }
    }

    fn color_window(&mut self, ctx: &egui::Context, now: f64) {
        let Some(w) = &mut self.color else {
            return;
        };
        let mut open = true;
        let mut event = color_picker::PickerEvent::None;
        let history = self.layout.colors.clone();
        egui::Window::new(&w.title)
            .id(egui::Id::unique("color_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                event = w.picker.ui(ui, &history);
            });
        if self.layout.color_snap_off == w.picker.quantize
            || self.layout.color_safe_off == w.picker.safe
        {
            self.layout.color_snap_off = !w.picker.quantize;
            self.layout.color_safe_off = !w.picker.safe;
            self.layout.changed = true;
        }
        if !open {
            event = color_picker::PickerEvent::Cancel;
        }
        self.color_event(event, now);
    }

    /// Applies what the colour window did: a live view outside the
    /// history, one undo step on Accept, the old colours on Cancel.
    fn color_event(&mut self, event: color_picker::PickerEvent, now: f64) {
        let Some(w) = &self.color else {
            return;
        };
        let color = w.picker.current;
        match event {
            color_picker::PickerEvent::Changed => {
                // Live view, outside the history.
                for t in w.targets.clone() {
                    naming::write_color(&mut self.project, t, Some(color));
                }
            }
            color_picker::PickerEvent::Accept => {
                let targets = w.targets.clone();
                self.close_color();
                if let Some(edit) =
                    naming::set_name_color(&self.project, &targets, None, Some(color))
                {
                    self.apply(edit, false, now);
                }
                jdance_domain::color::push_history(&mut self.layout.colors, color);
                self.layout.changed = true;
            }
            color_picker::PickerEvent::Cancel => self.close_color(),
            color_picker::PickerEvent::None => {}
        }
    }

    fn rename_dialog(&mut self, ctx: &egui::Context, now: f64) {
        let Some(dialog) = &mut self.rename else {
            return;
        };
        let mut commit = false;
        let mut cancel = false;
        let wheel_open = dialog.wheel.is_some();
        egui::Modal::new(egui::Id::unique("rename")).show(ctx, |ui| {
            ui.set_width(320.0);
            let title = if dialog.color.is_some() {
                "Rename and color"
            } else {
                "Rename"
            };
            ui.heading(title);
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Name").color(theme::MUTED));
            let mut picked = None;
            if let Some(list) = &mut dialog.presets {
                picked = name_presets_ui::keys(ui, &self.presets, &dialog.text, list).picked;
            }
            let mut field = egui::TextEdit::singleline(&mut dialog.text)
                .desired_width(f32::INFINITY)
                .show(ui);
            if std::mem::take(&mut dialog.fresh) {
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(dialog.text.chars().count()),
                );
                field.state.cursor.set_char_range(Some(all));
                field.state.store(ui.ctx(), field.response.id);
            }
            let r = field.response;
            if dialog.wheel.is_none() {
                r.request_focus();
            }
            if r.changed()
                && let Some(list) = &mut dialog.presets
            {
                list.open = true;
                list.selected = None;
            }
            // Enter confirms (a highlighted preset took it above). The
            // field asks for focus every frame, so its own focus loss on
            // Enter cannot be relied on.
            if dialog.wheel.is_none() && ui.input(|i| i.key_pressed(Key::Enter)) {
                commit = true;
            }
            dialog.hover_color = None;
            if let Some(list) = &mut dialog.presets {
                let out = name_presets_ui::show(ui, &self.presets, &dialog.text, list);
                dialog.hover_color = out.hover;
                picked = picked.or(out.picked);
            }
            if let Some((name, c)) = picked {
                // A preset sets name and colour and confirms (§9 item 4).
                dialog.text = name;
                if let (Some(slot), Some(c)) = (&mut dialog.color, c) {
                    *slot = Some(c);
                    dialog.color_manual = true;
                }
                commit = true;
            }
            let color_before = dialog.color;
            if let Some(color) = &mut dialog.color {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Color").color(theme::MUTED));
                let current = dialog
                    .hover_color
                    .unwrap_or_else(|| color.unwrap_or(dialog.default_color));
                ui.horizontal_wrapped(|ui| {
                    for c in jdance_domain::PALETTE {
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
                        ui.painter().rect_filled(rect, 5.0, theme::rgb(c));
                        if c == current {
                            ui.painter().rect_stroke(
                                rect.expand(2.0),
                                6.0,
                                egui::Stroke::new(2.0, theme::TEXT),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if resp.clicked() {
                            *color = Some(c);
                        }
                    }
                });
                ui.add_space(4.0);
                // The colour square: click opens the wheel, right click a
                // random safe colour; F2 random, F3 the last used (§6, §9).
                ui.horizontal(|ui| {
                    let (sq, resp) =
                        ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
                    ui.painter().rect_filled(sq, 5.0, theme::rgb(current));
                    let resp = resp.on_hover_text(
                        "Click: the colour wheel · right click: a random colour (F2) · F3: the last used",
                    );
                    if resp.clicked() {
                        dialog.wheel = match dialog.wheel {
                            Some(_) => None,
                            None => Some(color_picker::Picker::new(
                                current,
                                dialog.default_color,
                                !self.layout.color_snap_off,
                                !self.layout.color_safe_off,
                            )),
                        };
                    }
                    let (f2, f3) =
                        ui.input(|i| (i.key_pressed(Key::F2), i.key_pressed(Key::F3)));
                    if resp.secondary_clicked() || f2 {
                        let seed = (ui.input(|i| i.time) * 1000.0) as u64;
                        *color = Some(jdance_domain::color::random_safe(seed));
                    }
                    if f3 && let Some(&last) = self.layout.colors.first() {
                        *color = Some(last);
                    }
                    ui.label(
                        egui::RichText::new(jdance_domain::color::color_name(current))
                            .color(theme::MUTED),
                    );
                });
                if let Some(w) = &mut dialog.wheel {
                    match w.ui(ui, &self.layout.colors) {
                        color_picker::PickerEvent::Changed => *color = Some(w.current),
                        color_picker::PickerEvent::Accept => {
                            *color = Some(w.current);
                            jdance_domain::color::push_history(&mut self.layout.colors, w.current);
                            self.layout.changed = true;
                            dialog.wheel = None;
                        }
                        color_picker::PickerEvent::Cancel => {
                            *color = Some(w.original);
                            dialog.wheel = None;
                        }
                        color_picker::PickerEvent::None => {}
                    }
                }
                let current = color.unwrap_or(dialog.default_color);
                ui.add_space(4.0);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(160.0, 28.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 6.0, theme::RAISED);
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height())),
                    egui::CornerRadius {
                        nw: 6,
                        sw: 6,
                        ne: 0,
                        se: 0,
                    },
                    theme::rgb(current),
                );
                ui.painter().text(
                    rect.left_center() + egui::vec2(12.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    &dialog.text,
                    egui::FontId::proportional(13.0),
                    theme::TEXT,
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                cancel |= ui.button("Cancel").clicked();
                commit |= ui.button("Apply").clicked();
            });
            if dialog.color != color_before && !commit {
                dialog.color_manual = true;
            }
            // Esc with the wheel open closes the wheel only; with the
            // preset list open, the list first (§9 item 3).
            if ui.input(|i| i.key_pressed(Key::Escape)) && !wheel_open {
                match &mut dialog.presets {
                    Some(list) if list.open => list.open = false,
                    _ => cancel = true,
                }
            }
        });
        if commit {
            let name = dialog.text.trim().to_owned();
            let (target, mut color) = (dialog.target, dialog.color.flatten());
            let (manual, created) = (dialog.color_manual, dialog.created);
            self.rename = None;
            if let RenameTarget::Pattern(id) = target {
                // A typed preset name brings its colour unless one was
                // chosen by hand (§9 item 5); typed names are remembered.
                if !manual && let Some(c) = self.presets.color_of(&name) {
                    color = Some(c);
                }
                self.presets.note_recent(&name);
                self.host.set_pattern_presets(self.presets.clone());
                if let Some(index) = created {
                    self.recreate_pattern(id, index, &name, color, now);
                    return;
                }
            }
            let mut edits = Vec::new();
            if !name.is_empty() {
                edits.push(match target {
                    RenameTarget::Channel(id) => Edit::RenameChannel(id, name),
                    RenameTarget::Track(id) => Edit::RenameTrack(id, name),
                    RenameTarget::Pattern(id) => Edit::RenamePattern(id, name),
                    RenameTarget::Lane(id) => Edit::RenameLane(id, name),
                    RenameTarget::Asset(id) => Edit::RenameAsset(id, name),
                    RenameTarget::Automation(id) => match self.project.automation_lane(id) {
                        Some(a) => Edit::SetAutomation {
                            id,
                            lane: Some(jdance_domain::AutomationLane { name, ..a.clone() }),
                        },
                        None => Edit::Batch(Vec::new()),
                    },
                });
            }
            match (target, color) {
                (RenameTarget::Channel(id), Some(_)) => {
                    edits.push(Edit::SetChannelColor(id, color));
                }
                (RenameTarget::Track(id), Some(_)) => edits.push(Edit::SetTrackColor(id, color)),
                (RenameTarget::Lane(id), Some(c)) => edits.push(Edit::SetLaneColor(id, c)),
                (RenameTarget::Pattern(id), Some(_)) => {
                    edits.push(Edit::SetPatternColor(id, color));
                }
                _ => {}
            }
            if !edits.is_empty() {
                self.apply(Edit::Batch(edits), false, now);
            }
        } else if cancel {
            self.rename = None;
        }
    }

    /// Folds a new pattern's name and colour into its creation: one undo
    /// step (spec 0.2.5 §9 item 13).
    fn recreate_pattern(
        &mut self,
        id: PatternId,
        index: usize,
        name: &str,
        color: Option<[u8; 3]>,
        now: f64,
    ) {
        let Some(mut pattern) = self.project.pattern(id).cloned() else {
            return;
        };
        if self.history.undo(&mut self.project).is_err() {
            return;
        }
        if !name.is_empty() {
            pattern.name = jdance_domain::color::clip_name(name);
        }
        pattern.color = color.or(pattern.color);
        self.apply(Edit::AddPattern { pattern, index }, false, now);
        self.current_pattern = id;
    }

    /// Options › Pattern name presets: changes apply at once (§9 item 11).
    fn presets_window(&mut self, ctx: &egui::Context) {
        if !self.show_presets {
            return;
        }
        let mut open = true;
        let mut random = self.layout.random_pattern_colors;
        let mut changed = false;
        egui::Window::new("Pattern name presets")
            .id(egui::Id::unique("presets_window"))
            .open(&mut open)
            .show(ctx, |ui| {
                changed = name_presets_ui::editor(ui, &mut self.presets, &mut random);
            });
        if random != self.layout.random_pattern_colors {
            self.layout.random_pattern_colors = random;
            self.layout.changed = true;
        }
        if changed {
            self.host.set_pattern_presets(self.presets.clone());
        }
        self.show_presets = open;
    }

    /// The plugin picker (F8): instruments load into a new channel,
    /// effects into the selected mixer track (spec 0.2.5 §10).
    fn plugin_picker_window(&mut self, ctx: &egui::Context, now: f64) {
        let Some(mut picker) = self.plugin_picker.take() else {
            return;
        };
        let mut open = true;
        let mut picked = plugin_list::ListOut::default();
        let track = picker.track.unwrap_or(self.mixer_view.selected);
        let track_name = self
            .project
            .mixer
            .track(track)
            .map_or_else(String::new, |t| t.name.clone());
        let (title, hint) = match picker.scope {
            plugin_list::Scope::All => (
                "Plugin picker",
                format!("Click: instruments into a new channel, effects into {track_name}"),
            ),
            plugin_list::Scope::Instruments => (
                "Add instrument",
                "Click: the instrument into a new channel".to_owned(),
            ),
            plugin_list::Scope::Effects => {
                ("Add effect", format!("Click: the effect into {track_name}"))
            }
        };
        egui::Window::new(title)
            .id(egui::Id::unique("plugin_picker"))
            .open(&mut open)
            .default_size([420.0, 480.0])
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(hint).size(11.5).color(theme::MUTED));
                picked = picker
                    .list
                    .show(ui, "picker", &mut self.host, picker.scope, 400.0);
            });
        if let Some(d) = picked.load {
            match d.kind {
                jdance_plugin_api::PluginKind::Instrument => self.add_instrument(d, now),
                jdance_plugin_api::PluginKind::Effect => {
                    self.mixer_action(mixer::MixerAction::AddPlugin(track, d), now);
                }
            }
            // Loaded: done, as a menu would be.
            return;
        }
        if open && !picked.close {
            self.plugin_picker = Some(picker);
        }
    }

    /// A new empty pattern and its name window (spec 0.2.5 §9): "Pattern
    /// N" with the first free N, selected; Esc keeps the pattern as is.
    fn new_pattern(&mut self, now: f64) {
        let name = jdance_domain::name_presets::first_free_pattern_name(
            self.project.patterns.iter().map(|p| p.name.as_str()),
        );
        match add_pattern(&mut self.history, &mut self.project, name) {
            Ok(id) => {
                self.current_pattern = id;
                self.sync_needed = true;
                self.start_rename(RenameTarget::Pattern(id));
                let random = self.layout.random_pattern_colors;
                if let Some(d) = &mut self.rename {
                    d.created = self.project.patterns.iter().position(|p| p.id == id);
                    if random {
                        let seed = (now * 1000.0) as u64 ^ id.0;
                        d.color = Some(Some(jdance_domain::color::random_safe(seed)));
                        d.color_manual = true;
                    }
                }
            }
            Err(e) => self.notify(Notice::error(e.to_string()), now),
        }
    }

    /// Confirms deleting channels and names what goes with them (§5).
    fn delete_channels_dialog(&mut self, ctx: &egui::Context, now: f64) {
        let Some(ids) = &self.delete_channels else {
            return;
        };
        let names: Vec<String> = ids
            .iter()
            .filter_map(|id| self.project.channel(*id).map(|c| c.name.clone()))
            .collect();
        let (patterns, lanes) = ids.iter().fold((0, 0), |(a, b), id| {
            let (p, l) = self.project.channel_usage(*id);
            (a + p, b + l)
        });
        let clips: usize = ids
            .iter()
            .map(|id| self.project.channel_clips(*id).count())
            .sum();
        let mut confirm = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("delete_channels")).show(ctx, |ui| {
            ui.set_width(380.0);
            let title = if names.len() == 1 {
                format!("Delete channel \"{}\"?", names[0])
            } else {
                format!("Delete {} channels?", names.len())
            };
            ui.heading(title);
            if patterns + lanes + clips > 0 {
                ui.label("This also removes:");
                if clips > 0 {
                    ui.label(format!("• {clips} clip(s) in the Playlist"));
                }
                if patterns > 0 {
                    ui.label(format!("• steps and notes in {patterns} pattern(s)"));
                }
                if lanes > 0 {
                    ui.label(format!("• {lanes} automation lane(s) of the instrument"));
                }
            } else {
                ui.label(
                    egui::RichText::new("The channel has no steps, notes or automation.")
                        .color(theme::MUTED),
                );
            }
            ui.label(egui::RichText::new("Undo restores all of it.").color(theme::MUTED));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                cancel |= ui.button("Cancel").clicked();
                confirm |= ui
                    .button(egui::RichText::new("Delete").color(theme::ERROR))
                    .clicked();
            });
            cancel |= ui.input(|i| i.key_pressed(Key::Escape));
        });
        if confirm {
            // Audio channels take their Playlist clips (ACH-033).
            let edits = ids
                .iter()
                .map(|id| audio_channels::remove_channel(&self.project, *id))
                .collect();
            self.delete_channels = None;
            self.apply(Edit::Batch(edits), false, now);
        } else if cancel {
            self.delete_channels = None;
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.record_target.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.host.stop_recording();
                self.host.stop();
                self.after_take = Some(Pending::Quit);
            } else if self.history.is_dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending = Some(Pending::Quit);
            } else {
                self.allow_close = true;
            }
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, now: f64) {
        let Some(action) = self.pending else { return };
        let modal = egui::Modal::new(egui::Id::unique("unsaved")).show(ctx, |ui| {
            ui.heading("Unsaved changes");
            ui.label("The project has unsaved changes.");
            let mut choice = None;
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(true);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(false);
                }
                if ui.button("Cancel").clicked() {
                    self.pending = None;
                }
            });
            choice
        });
        match modal.inner {
            Some(true) => {
                self.pending = None;
                if self.save(false, now) {
                    self.run_pending(action, ctx, now);
                }
            }
            Some(false) => {
                self.pending = None;
                self.run_pending(action, ctx, now);
            }
            None => {
                if modal.should_close() {
                    self.pending = None;
                }
            }
        }
    }

    fn show_notices(&mut self, ctx: &egui::Context, now: f64) {
        self.notices.retain(|(_, t)| now - t < NOTICE_SECONDS);
        if self.notices.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::unique("notices"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -36.0))
            .show(ctx, |ui| {
                for (n, _) in &self.notices {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(420.0);
                        let color = match n.level {
                            NoticeLevel::Info => ui.visuals().text_color(),
                            NoticeLevel::Warning => ui.visuals().warn_fg_color,
                            NoticeLevel::Error => ui.visuals().error_fg_color,
                        };
                        ui.colored_label(color, &n.text);
                    });
                }
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

impl<H: Host, S: ProjectStore> eframe::App for JdanceApp<H, S> {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let focused = ctx.input(|i| i.focused);
        if self.focused != Some(focused) {
            self.focused = Some(focused);
            self.host.focus_changed(focused);
        }
        self.host.poll();
        for n in self.host.take_notices() {
            self.notify(n, now);
        }
        self.handle_close_request(&ctx);
        self.shortcuts(&ctx, now);
        // What a drag would name and colour, for the strips and headers.
        self.dragging = drop_name::dragged(&ctx, &self.project);
        self.mixer_view.drop = self.dragging.clone();
        self.playlist_view.drop = self.dragging.clone();

        if !self.themed {
            theme::install(&ctx);
            self.themed = true;
        }
        let bar = |fill| {
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::symmetric(12, 4))
        };
        egui::Panel::top("menu")
            .frame(bar(theme::SIDEBAR))
            .show(ui, |ui| self.menu_bar(ui, now));
        egui::Panel::top("toolbar")
            .frame(bar(theme::BAR).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| self.toolbar(ui, now));
        egui::Panel::bottom("status")
            .frame(bar(theme::SIDEBAR))
            .show(ui, |ui| self.status_bar(ui));
        if let Some(mut tap) = self.tap.take() {
            let (apply, open) = tap.window(&ctx, now);
            if let Some(bpm) = apply {
                self.apply(Edit::SetTempo((bpm * 1000.0).round() / 1000.0), false, now);
            }
            if open {
                self.tap = Some(tap);
            }
        }
        let mut browser_actions = Vec::new();
        if self.show_browser {
            let labels: Vec<&'static str> = self.history.undo_labels().collect();
            egui::Panel::left("browser")
                .resizable(true)
                .default_size(270.0)
                .min_size(180.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::SIDEBAR)
                        .inner_margin(egui::Margin::same(10)),
                )
                .show(ui, |ui| {
                    let dir = self.path.as_deref().and_then(Path::parent);
                    browser_actions =
                        self.browser_view
                            .show(ui, &self.project, dir, &labels, &mut self.host);
                });
        }
        for action in browser_actions {
            self.browser_action(action, now);
        }
        let mut mix = mixer::MixerOutput::default();
        let mixer_docked = !self.layout.is_undocked(dock::Panel::Mixer);
        if self.show_mixer && !mixer_docked {
            let latency = self.host.latency();
            let meters = self.host.meters();
            let event = self
                .layout
                .window(&ctx, dock::Panel::Mixer, 0, "Mixer", &mut |ui| {
                    egui::ScrollArea::both().show(ui, |ui| {
                        mix = mixer::show(
                            ui,
                            &self.project.mixer,
                            &self.project.automation,
                            &latency,
                            &meters,
                            &mut self.host,
                            &mut self.mixer_view,
                        );
                    });
                });
            if event == dock::WindowEvent::Closed {
                self.show_mixer = false;
            }
        }
        if self.show_mixer && mixer_docked {
            let latency = self.host.latency();
            let meters = self.host.meters();
            egui::Panel::bottom("mixer")
                .resizable(true)
                .default_size(430.0)
                .min_size(120.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        mix = mixer::show(
                            ui,
                            &self.project.mixer,
                            &self.project.automation,
                            &latency,
                            &meters,
                            &mut self.host,
                            &mut self.mixer_view,
                        );
                    });
                });
        }
        let mut out = rack::RackOutput::default();
        if self.show_rack {
            egui::Panel::top("rack")
                .resizable(true)
                .default_size(230.0)
                .min_size(60.0)
                .show(ui, |ui| {
                    let marks = self
                        .project
                        .pattern(self.current_pattern)
                        .map(|p| self.pattern_marks(p))
                        .unwrap_or_default();
                    let dir = self.path.as_deref().and_then(Path::parent);
                    out = self.rack_view.show(
                        ui,
                        &self.project,
                        self.current_pattern,
                        &mut self.host,
                        dir,
                        marks,
                    );
                });
        }
        let mut pl_out = playlist::PlaylistOutput::default();
        let mut project_actions = Vec::new();
        let mut piano_out = piano::PianoOutput::default();
        if let Some(id) = out.open_piano.take() {
            self.piano = Some(piano::PianoRoll::new(id));
        }
        if self
            .piano
            .as_ref()
            .is_some_and(|p| self.project.channel(p.channel).is_none())
        {
            self.piano = None;
        }
        let piano_docked = !self.layout.is_undocked(dock::Panel::PianoRoll);
        // The editor keys go to the editor last clicked; a docked Piano
        // Roll hides the Playlist and has them (spec 0.2.7 §1 item 1).
        if let Some(press) =
            ctx.input(|i| i.pointer.press_origin().filter(|_| i.pointer.any_pressed()))
        {
            if self.piano.as_ref().is_some_and(|p| p.area.contains(press)) {
                self.key_focus = KeyFocus::PianoRoll;
            } else if self.playlist_view.area.contains(press) {
                self.key_focus = KeyFocus::Playlist;
            }
        }
        let piano_keys = self.piano.is_some()
            && (piano_docked || self.key_focus == KeyFocus::PianoRoll || !self.show_playlist);
        if let Some(roll) = &mut self.piano {
            roll.keys_active = piano_keys;
        }
        self.playlist_view.keys_active = !piano_keys;
        let marks = self
            .project
            .pattern(self.current_pattern)
            .map(|p| self.pattern_marks(p))
            .unwrap_or_default();
        let browser_docked = !self.layout.is_undocked(dock::Panel::ProjectBrowser);
        if !piano_docked
            && let (Some(roll), Some(pattern)) =
                (&mut self.piano, self.project.pattern(self.current_pattern))
        {
            let title = format!(
                "Piano Roll · {}",
                self.project
                    .channel(roll.channel)
                    .map_or("?", |c| c.name.as_str())
            );
            let (project, host, snap) = (&self.project, &mut self.host, self.snap);
            let event = self
                .layout
                .window(&ctx, dock::Panel::PianoRoll, 0, &title, &mut |ui| {
                    piano_out = roll.show(ui, project, pattern, host, marks, snap);
                });
            if event == dock::WindowEvent::Closed {
                piano_out.close = true;
            }
        }
        if self.show_project_browser && !browser_docked {
            let (browser, project, source, host) = (
                &mut self.project_browser,
                &self.project,
                self.playlist_view.source,
                &mut self.host,
            );
            let event = self.layout.window(
                &ctx,
                dock::Panel::ProjectBrowser,
                0,
                "Project Browser",
                &mut |ui| {
                    project_actions = browser.show(ui, project, source, host);
                },
            );
            if event == dock::WindowEvent::Closed {
                self.show_project_browser = false;
            }
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let transport = self.host.transport();
            match (&mut self.piano, self.project.pattern(self.current_pattern)) {
                (Some(roll), Some(pattern)) if piano_docked => {
                    piano_out = roll.show(
                        ui,
                        &self.project,
                        pattern,
                        &mut self.host,
                        marks,
                        self.snap,
                    );
                }
                _ if !self.show_playlist => {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("Playlist hidden · View → Playlist or the toolbar's Playlist button")
                                .color(theme::MUTED),
                        );
                    });
                }
                _ => {
                    // Stopped, only the caret is drawn (CUR-005).
                    let playhead = transport
                        .position_ticks
                        .filter(|_| self.song_mode && transport.playing);
                    self.playlist_view.caret = self.song_mode.then_some(self.caret);
                    self.playlist_view.follow = !self.layout.no_follow;
                    self.playlist_view.zoom_to_mouse = self.layout.zoom_to_mouse;
                    let dir = self.path.as_deref().and_then(Path::parent);
                    ui.horizontal_top(|ui| {
                        if self.show_project_browser && browser_docked {
                            ui.vertical(|ui| {
                                project_actions = self.project_browser.show(
                                    ui,
                                    &self.project,
                                    self.playlist_view.source,
                                    &mut self.host,
                                );
                            });
                            ui.separator();
                        }
                        ui.vertical(|ui| {
                            pl_out = self.playlist_view.show(
                                ui,
                                &self.project,
                                playhead,
                                self.snap.ticks(self.project.time_signature),
                                self.current_pattern,
                                &mut self.host,
                                dir,
                            );
                        });
                    });
                }
            }
        });
        if piano_out.close
            && let Some(mut roll) = self.piano.take()
        {
            roll.release(&mut self.host);
        }
        if let Some(id) = piano_out.select_pattern {
            self.current_pattern = id;
        }
        if let Some(range) = piano_out.lfo
            && let Some(ch) = self
                .piano
                .as_ref()
                .and_then(|p| self.project.channel(p.channel))
        {
            self.lfo_dialog = Some(lfo_dialog::LfoDialog::new(range, ch.track));
        }
        self.lfo_window(&ctx, now);
        if let Some(snap) = piano_out.snap {
            self.snap = snap;
        }
        if let Some(request) = piano_out.caret {
            self.piano_caret(request, now);
        }
        for (edit, merging) in piano_out.edits {
            self.apply(edit, merging, now);
        }
        if piano_out.end_gesture {
            self.history.end_gesture();
        }
        for edit in pl_out.edits {
            self.apply(edit, false, now);
        }
        for edit in pl_out.gesture {
            self.apply(edit, true, now);
        }
        if pl_out.end_gesture {
            self.history.end_gesture();
        }
        if let Some(id) = pl_out.select_pattern.take() {
            // The clicked clip's pattern is the current one: the Piano
            // Roll and the pattern selector follow (§2 item 3).
            self.current_pattern = id;
        }
        if let Some(h) = pl_out.hint.take() {
            self.notify(Notice::info(h), now);
        }
        for action in pl_out.actions {
            self.playlist_action(action, now);
        }
        for action in project_actions {
            self.project_action(action, now);
        }
        self.finish_imports(now);
        self.confirm_delete_dialog(&ctx, now);
        self.sample_editor_window(&ctx, now);
        self.automation_editor_windows(&ctx, now);
        if self.show_recording {
            let mut edits = Vec::new();
            self.show_recording =
                self.recording_panel
                    .window(&ctx, &mut self.host, &self.project, &mut edits);
            for edit in edits {
                self.apply(edit, false, now);
            }
        }
        if let Some(t) = &mut self.playlist_view.live {
            t.peaks.extend(self.host.take_recording_peaks());
        }
        self.finish_recording(now);
        if self.record_target.is_none()
            && let Some(action) = self.after_take.take()
        {
            self.guarded(action, &ctx, now);
        }
        self.loop_prompt_dialog(&ctx, now);
        self.check_recovery(&ctx, now);
        if self.show_library {
            self.show_library = library::library_window(&ctx, &mut self.host);
        }
        if let Some(p) = out.pick.take() {
            self.sample_picker = Some(library::SamplePicker::new(p));
        }
        self.sample_picker_window(&ctx, now);
        for (edit, merging) in out.edits.into_iter().chain(mix.edits) {
            self.apply(edit, merging, now);
        }
        if out.end_gesture || mix.end_gesture {
            self.history.end_gesture();
        }
        if let Some(target) = out.rename {
            self.start_rename(target);
        }
        for action in mix.actions {
            self.mixer_action(action, now);
        }
        for request in mix.automation {
            self.automation_request(request, now);
        }
        if let Some(id) = out.toggle_editor {
            let title = self
                .project
                .channel(id)
                .map_or_else(String::new, |c| c.name.clone());
            self.host
                .toggle_plugin_editor(jdance_domain::PluginTarget::Channel(id), &title);
        }
        for id in std::mem::take(&mut out.clone) {
            match jdance_commands::clone_channel(&mut self.history, &mut self.project, id) {
                Ok(_) => self.sync_needed = true,
                Err(e) => self.notify(Notice::error(e.to_string()), now),
            }
        }
        if !out.delete.is_empty() {
            self.delete_channels = Some(std::mem::take(&mut out.delete));
        }
        if let Some(ch) = out.to_sampler.take() {
            self.send_to_sampler(ch);
        }
        if let Some(ch) = out.select_clips.take() {
            let ids: Vec<ClipId> = self.project.channel_clips(ch).map(|c| c.id).collect();
            self.playlist_view.select_only(&ids);
            self.show_playlist = true;
        }
        let first_clip = out
            .open_clip_editor
            .take()
            .and_then(|ch| self.project.channel_clips(ch).next().map(|c| c.id));
        if let Some(id) = first_clip {
            self.open_clip(id);
        }
        if std::mem::take(&mut out.remove_unused)
            && let Some((n, edit)) = audio_channels::remove_unused_audio_channels(&self.project)
        {
            self.apply(edit, false, now);
            self.notify(
                Notice::info(format!("Removed {n} unused audio channel(s)")),
                now,
            );
        }
        for id in std::mem::take(&mut self.new_channels) {
            if !self.rack_view.reveal(&self.project, id, now)
                && let Some(c) = self.project.channel(id)
            {
                self.notify(
                    Notice::info(format!(
                        "Audio channel \"{}\" created (hidden by the Channel Rack filter)",
                        c.name
                    )),
                    now,
                );
            }
        }
        if let Some(id) = out.show_params {
            self.channel_settings = Some(channel_settings::ChannelSettingsWindow::instrument(id));
        }
        if let Some((id, sample)) = out.open_settings {
            self.channel_settings = Some(if sample {
                channel_settings::ChannelSettingsWindow::sample(id)
            } else {
                channel_settings::ChannelSettingsWindow::new(id)
            });
        }
        for d in std::mem::take(&mut out.new_instruments) {
            self.add_instrument(d, now);
        }
        if out.pick_instrument {
            self.plugin_picker = Some(PluginPicker::new(plugin_list::Scope::Instruments, None));
        }
        for (name, sample) in out.new_channels {
            self.add_channel(name, sample, now);
        }

        if let Some(mut w) = self.settings.take()
            && w.show(&ctx, &mut self.host)
        {
            self.settings = Some(w);
        }
        if self.show_plugins {
            self.show_plugins = plugins::window(&ctx, &mut self.host);
        }
        let mut instrument_automation = Vec::new();
        if let Some(mut w) = self.channel_settings.take() {
            let dir = self
                .path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            let (o, open) = w.show(&ctx, &self.project, &mut self.host, dir.as_deref());
            for (edit, merging) in o.edits {
                self.apply(edit, merging, now);
            }
            if o.end_gesture {
                self.history.end_gesture();
            }
            if o.toggle_editor {
                let title = self
                    .project
                    .channel(w.channel)
                    .map_or_else(String::new, |c| c.name.clone());
                self.host
                    .toggle_plugin_editor(jdance_domain::PluginTarget::Channel(w.channel), &title);
            }
            instrument_automation = o.automation;
            if o.select_clips {
                let ids: Vec<ClipId> = self
                    .project
                    .channel_clips(w.channel)
                    .map(|c| c.id)
                    .collect();
                self.playlist_view.select_only(&ids);
                self.show_playlist = true;
            }
            let first = self.project.channel_clips(w.channel).next().map(|c| c.id);
            if o.open_clip_editor
                && let Some(id) = first
            {
                self.open_clip(id);
            }
            if let Some(p) = o.pick {
                self.sample_picker = Some(library::SamplePicker::new(p));
            }
            if open {
                self.channel_settings = Some(w);
            }
        }
        for request in instrument_automation {
            self.automation_request(request, now);
        }
        self.rename_dialog(&ctx, now);
        self.color_window(&ctx, now);
        self.presets_window(&ctx);
        self.plugin_picker_window(&ctx, now);
        if self.show_keys {
            bindings::keys_window(&ctx, &mut self.show_keys);
        }
        self.delete_channels_dialog(&ctx, now);
        self.length_dialog(&ctx, now);
        self.export_dialog(&ctx);
        self.export_progress_window(&ctx);
        self.keep_pattern_caret();
        let mode = if self.song_mode {
            TransportMode::Song
        } else {
            TransportMode::Pattern(self.current_pattern)
        };
        if self.mode_sent != Some(mode) {
            let to_song = mode == TransportMode::Song && self.mode_sent.is_some();
            self.mode_sent = Some(mode);
            self.host.set_mode(mode);
            // Each mode resumes at its caret (PCR-002).
            if to_song {
                self.host.seek_ticks(self.caret);
            } else if self.mode_sent != Some(TransportMode::Song) {
                self.host.seek_ticks(self.pattern_caret);
            }
        }
        self.confirm_dialog(&ctx, now);
        self.show_notices(&ctx, now);
        if self.layout.changed {
            self.layout.changed = false;
            self.host.set_ui_layout(self.layout.to_text());
        }

        if self.sync_needed {
            self.sync_needed = false;
            let dir = self.project_dir().map(Path::to_path_buf);
            self.host.set_project(&self.project, dir.as_deref());
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));
        // Plugin editors need their events and timers run (PLG-062).
        if self.host.transport().playing || self.host.any_plugin_editor_open() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        self.frames += 1;
        if self.auto_close_after.is_some_and(|n| self.frames >= n) {
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// A new lane named `name` at the end of the playlist, as an edit.
fn new_lane_edit(p: &mut Project, name: String) -> (LaneId, Edit) {
    let id = LaneId(p.ids.next());
    let n = p.playlist.lanes.len();
    let edit = Edit::AddLane {
        lane: jdance_domain::Lane {
            id,
            name,
            color: jdance_domain::LANE_COLORS[n % jdance_domain::LANE_COLORS.len()],
            muted: false,
            solo: false,
        },
        index: n,
    };
    (id, edit)
}

/// Metronome settings shared by the transport button's menu and Options.
pub(crate) fn metronome_settings_ui(ui: &mut egui::Ui, m: &mut MetronomeSettings) {
    ui.checkbox(&mut m.on, "Metronome (Ctrl+M)");
    ui.add(
        egui::Slider::new(&mut m.volume_db, -40.0..=0.0)
            .suffix(" dB")
            .text("Level"),
    );
    ui.checkbox(&mut m.only_recording, "Only while recording");
    ui.checkbox(
        &mut m.count_in,
        format!("Count {COUNT_IN_BARS} bars before recording (Ctrl+P)"),
    );
}

#[cfg(test)]
mod pattern_caret_tests {
    use super::*;
    use crate::test_ui::{NoStore, StubHost};
    use jdance_domain::{ClipId, PatternClip};

    /// A one-bar pattern in a two-bar looped clip from bar 5.
    fn app() -> JdanceApp<StubHost, NoStore> {
        let mut p = Project::new();
        let pat = p.patterns[0].id;
        let lane = p.playlist.lanes[0].id;
        let len = p.patterns[0].length_ticks();
        let mut clip = PatternClip::repeating(ClipId(900), lane, &p.patterns[0], 5 * len, 2);
        clip.pattern = pat;
        p.playlist.clips.push(clip);
        JdanceApp::new(StubHost::default(), NoStore, p, None)
    }

    #[test]
    fn song_mode_maps_the_caret_and_playhead_into_the_pattern() {
        let mut a = app();
        a.song_mode = true;
        let pat = a.project.patterns[0].clone();
        let len = pat.length_ticks();
        a.caret = 5 * len + 100;
        assert_eq!(a.pattern_marks(&pat).caret, Some(100));
        a.host.transport.playing = true;
        // Second pass of the clip: back at the pattern start (PCR-010).
        a.host.transport.position_ticks = Some(6 * len + 7);
        assert_eq!(a.pattern_marks(&pat).playhead, Some(7));
        a.host.transport.position_ticks = Some(7 * len);
        assert_eq!(a.pattern_marks(&pat).playhead, None, "past the clip");
        a.caret = 0;
        assert_eq!(a.pattern_marks(&pat).caret, None, "caret outside the clip");
    }

    #[test]
    fn the_piano_ruler_moves_the_song_caret_inside_the_clip() {
        let mut a = app();
        a.song_mode = true;
        let len = a.project.patterns[0].length_ticks();
        // Caret in the second pass: stays in that pass (PCR-012).
        a.caret = 6 * len + 10;
        a.piano_caret(piano::CaretRequest::Seek(240), 0.0);
        assert_eq!(a.caret, 6 * len + 240);
        assert_eq!(a.host.seeks.last(), Some(&(6 * len + 240)));
        // Caret elsewhere: the first clip's first pass.
        a.caret = 0;
        a.piano_caret(piano::CaretRequest::Seek(480), 0.0);
        assert_eq!(a.caret, 5 * len + 480);
        // While playing, a preview does not move the playhead.
        a.host.transport.playing = true;
        let seeks = a.host.seeks.len();
        a.piano_caret(piano::CaretRequest::Preview(720), 0.0);
        assert_eq!(a.caret, 5 * len + 720);
        assert_eq!(a.host.seeks.len(), seeks);
    }

    #[test]
    fn the_pattern_caret_resets_for_another_pattern_and_stop() {
        let mut a = app();
        a.song_mode = false;
        a.keep_pattern_caret();
        a.piano_caret(piano::CaretRequest::Seek(480), 0.0);
        assert_eq!(a.pattern_caret, 480);
        assert_eq!(a.mode_caret(), 480);
        a.keep_pattern_caret();
        assert_eq!(a.pattern_caret, 480, "same pattern: kept");
        // Another pattern: back to 0 (PCR-003).
        let other = PatternId(a.project.ids.next());
        a.project
            .patterns
            .push(jdance_domain::Pattern::new(other, "B"));
        a.current_pattern = other;
        a.keep_pattern_caret();
        assert_eq!(a.pattern_caret, 0);
        assert_eq!(a.host.seeks.last(), Some(&0));
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;
    use crate::test_ui::{NoStore, StubHost};

    #[test]
    fn the_selection_is_described_and_chosen_from_the_ruler_menu() {
        let mut p = Project::new();
        p.tempo_bpm = 120.0;
        let bar = u64::from(p.time_signature.bar_ticks());
        let beat = u64::from(p.time_signature.beat_ticks());
        assert_eq!(
            selection_text(4 * bar, 8 * bar + beat, &p),
            "Bars 5:1 – 9:2 (4 bars 1 beat, 0:08.500)"
        );
        let mut a = JdanceApp::new(StubHost::default(), NoStore, p, None);
        // No selection: Song.
        a.open_export_dialog(Some(RangeKind::Selection));
        assert_eq!(a.export_dialog.map(|d| d.0.range), Some(ExportRange::Song));
        a.project.playlist.loop_range = Some((4 * bar, 8 * bar));
        a.playlist_action(playlist::PlaylistAction::RenderSelection, 0.0);
        assert_eq!(
            a.export_dialog.map(|d| d.0.range),
            Some(ExportRange::Selection {
                start: 4 * bar,
                end: 8 * bar
            })
        );
    }
}

#[cfg(test)]
mod color_tests {
    use super::*;
    use crate::color_picker::PickerEvent;
    use crate::test_ui::{NoStore, StubHost};

    #[test]
    fn the_colour_window_previews_live_and_accepts_in_one_step() {
        let mut p = Project::new();
        p.playlist = jdance_domain::Playlist::with_lanes(2, || p.ids.next());
        let (a, b) = (p.playlist.lanes[0].id, p.playlist.lanes[1].id);
        let mut app = JdanceApp::new(StubHost::default(), NoStore, p, None);
        let before = app.project.playlist.lane(a).unwrap().color;
        let red = [180, 40, 40];
        // Live view, then Esc: back, nothing to undo.
        app.open_color(vec![Named::Lane(a)]);
        app.color.as_mut().unwrap().picker.current = red;
        app.color_event(PickerEvent::Changed, 0.0);
        assert_eq!(app.project.playlist.lane(a).unwrap().color, red);
        app.color_event(PickerEvent::Cancel, 0.0);
        assert_eq!(app.project.playlist.lane(a).unwrap().color, before);
        assert!(app.color.is_none());
        assert_eq!(app.history.undo_label(), None);
        // Two lanes, Accept: both coloured, one undo step, in the history.
        app.open_color(vec![Named::Lane(a), Named::Lane(b)]);
        app.color.as_mut().unwrap().picker.current = red;
        app.color_event(PickerEvent::Changed, 0.0);
        app.color_event(PickerEvent::Accept, 0.0);
        assert_eq!(app.project.playlist.lane(b).unwrap().color, red);
        assert_eq!(app.layout.colors.first(), Some(&red));
        app.history.undo(&mut app.project).unwrap();
        assert_eq!(app.project.playlist.lane(a).unwrap().color, before);
        assert_ne!(app.project.playlist.lane(b).unwrap().color, red);
        assert_eq!(app.history.undo_label(), None, "one step");
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;
    use crate::drop_name::Dropped;
    use crate::test_ui::{NoStore, StubHost};

    #[test]
    fn a_drop_names_and_colours_in_one_step_and_a_repeat_is_no_step() {
        let mut p = Project::new();
        let t = TrackId(p.ids.next());
        p.mixer
            .tracks
            .push(jdance_domain::MixerTrack::new(t, "Insert 1"));
        p.playlist = jdance_domain::Playlist::with_lanes(1, || p.ids.next());
        let lane = p.playlist.lanes[0].id;
        let mut app = JdanceApp::new(StubHost::default(), NoStore, p, None);
        app.dragging = Some(Dropped {
            name: "Kick".into(),
            color: Some([200, 0, 0]),
        });
        app.name_from_drop(Named::Track(t), 0.0);
        let tr = app.project.mixer.track(t).unwrap();
        assert_eq!((tr.name.as_str(), tr.color), ("Kick", Some([200, 0, 0])));
        assert!(app.history.undo_label().is_some());
        app.history.undo(&mut app.project).unwrap();
        let tr = app.project.mixer.track(t).unwrap();
        assert_eq!(
            (tr.name.as_str(), tr.color),
            ("Insert 1", None),
            "one undo: both"
        );
        app.history.redo(&mut app.project).unwrap();
        // The same values again: no new step.
        app.name_from_drop(Named::Track(t), 0.0);
        app.history.undo(&mut app.project).unwrap();
        assert_eq!(app.project.mixer.track(t).unwrap().name, "Insert 1");
        // A file without colour on a lane header: the name only, no clip.
        let colour = app.project.playlist.lane(lane).unwrap().color;
        app.dragging = Some(Dropped {
            name: "clap".into(),
            color: None,
        });
        app.name_from_drop(Named::Lane(lane), 0.0);
        let l = app.project.playlist.lane(lane).unwrap();
        assert_eq!((l.name.as_str(), l.color), ("clap", colour));
        assert!(app.project.playlist.clips.is_empty());
    }
}

#[cfg(test)]
mod preset_tests {
    use super::*;
    use crate::test_ui::{Driver, NoStore, StubHost};
    use egui::Event;

    fn key(k: Key) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Creates a pattern, then sends `events` to its name window, one
    /// list per frame.
    fn create(events: Vec<Vec<Event>>) -> JdanceApp<StubHost, NoStore> {
        let mut app = JdanceApp::new(StubHost::default(), NoStore, Project::new(), None);
        app.new_pattern(0.0);
        assert_eq!(app.rename.as_ref().unwrap().text, "Pattern 2");
        let mut d = Driver::new();
        let mut ui = |ui: &mut egui::Ui| app.rename_dialog(ui.ctx(), 0.0);
        // The modal's sizing pass, then the field takes focus.
        d.frame(vec![], &mut ui);
        d.frame(vec![], &mut ui);
        for e in events {
            d.frame(e, &mut ui);
        }
        d.frame(vec![], &mut ui);
        app
    }

    #[test]
    fn a_preset_names_and_colours_the_new_pattern_in_one_step() {
        let n = Project::new().patterns.len();
        let mut app = create(vec![
            vec![Event::Text("ki".into())],
            vec![key(Key::ArrowDown)],
            vec![key(Key::Enter)],
        ]);
        assert!(app.rename.is_none(), "confirmed");
        let p = app.project.patterns.last().unwrap();
        assert_eq!(p.name, "Kick");
        assert_eq!(p.color, app.presets.color_of("Kick"));
        // One undo removes the pattern with its name and colour.
        app.history.undo(&mut app.project).unwrap();
        assert_eq!(app.project.patterns.len(), n);
    }

    #[test]
    fn a_typed_preset_name_brings_its_colour_and_esc_keeps_the_pattern() {
        let app = create(vec![
            vec![Event::Text("kick".into())],
            vec![key(Key::Enter)],
        ]);
        let p = app.project.patterns.last().unwrap();
        assert_eq!(
            (p.name.as_str(), p.color),
            ("kick", app.presets.color_of("Kick"))
        );
        // Esc twice: the list, then the window; the pattern stays.
        let app = create(vec![vec![key(Key::Escape)], vec![key(Key::Escape)]]);
        assert!(app.rename.is_none());
        assert_eq!(app.project.patterns.last().unwrap().name, "Pattern 2");
    }
}

#[cfg(test)]
mod replace_tests {
    use super::*;
    use crate::test_ui::{NoStore, StubHost};

    #[test]
    fn middle_click_replaces_the_selected_channel_sample_only() {
        let mut p = Project::new();
        let ch = ChannelId(p.ids.next());
        p.channels
            .push(jdance_domain::Channel::new_sampler(ch, "Kick"));
        let mut app = JdanceApp::new(StubHost::default(), NoStore, p, None);
        let file = std::env::temp_dir().join(format!("jdance-mmb-{}.wav", std::process::id()));
        std::fs::write(&file, b"RIFF").unwrap();
        let channels = app.project.channels.len();
        // Nothing selected: nothing changes.
        app.replace_selected_sample(&file, 0.0);
        assert_eq!(app.history.undo_label(), None);
        app.rack_view.selection = std::iter::once(ch).collect();
        app.replace_selected_sample(&file, 0.0);
        let c = app.project.channel(ch).unwrap();
        assert_eq!(c.sample().unwrap().path, file.to_string_lossy());
        assert_eq!(app.project.channels.len(), channels, "no new channel");
        app.history.undo(&mut app.project).unwrap();
        assert!(app.project.channel(ch).unwrap().sample().is_none());
        let _ = std::fs::remove_file(&file);
    }
}

#[cfg(test)]
mod plugin_picker_tests {
    use super::*;
    use crate::test_ui::{Driver, NoStore, StubHost};
    use egui::Event;
    use jdance_plugin_api::{PluginDescriptor, PluginFormat, PluginKind, PluginRef};

    fn plugin(name: &str, kind: PluginKind) -> PluginDescriptor {
        PluginDescriptor {
            plugin: PluginRef {
                format: PluginFormat::Vst3,
                class_id: format!("id-{name}"),
                name: name.into(),
                vendor: "Test".into(),
                version: "1".into(),
            },
            kind,
            category: String::new(),
            path: String::new(),
            sdk_version: None,
        }
    }

    fn key(k: Key) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Opens the picker and sends `events`, one list per frame.
    fn pick(events: Vec<Vec<Event>>) -> JdanceApp<StubHost, NoStore> {
        let host = StubHost {
            plugins: vec![
                plugin("Laser", PluginKind::Instrument),
                plugin("Serum", PluginKind::Instrument),
                plugin("Plate Reverb", PluginKind::Effect),
            ],
            ..StubHost::default()
        };
        let mut app = JdanceApp::new(host, NoStore, Project::new(), None);
        app.plugin_picker = Some(PluginPicker::new(plugin_list::Scope::All, None));
        let mut d = Driver::new();
        let mut ui = |ui: &mut egui::Ui| app.plugin_picker_window(ui.ctx(), 0.0);
        d.frame(vec![], &mut ui);
        d.frame(vec![], &mut ui);
        for e in events {
            d.frame(e, &mut ui);
        }
        d.frame(vec![], &mut ui);
        app
    }

    #[test]
    fn typing_and_enter_load_an_instrument_into_a_new_channel() {
        let n = Project::new().channels.len();
        let app = pick(vec![vec![Event::Text("ser".into())], vec![key(Key::Enter)]]);
        assert_eq!(app.project.channels.len(), n + 1);
        assert_eq!(app.project.channels.last().unwrap().name, "Serum");
        assert_eq!(app.host.plugin_prefs.recent, ["id-Serum"], "remembered");
    }

    #[test]
    fn an_effect_goes_into_the_selected_mixer_track() {
        let app = pick(vec![
            vec![Event::Text("plate".into())],
            vec![key(Key::Enter)],
        ]);
        let track = app.project.mixer.track(app.mixer_view.selected).unwrap();
        assert_eq!(track.inserts.len(), 1);
    }

    fn picker_open(d: &Driver) -> bool {
        d.find("★ Favorites only").is_some()
    }

    /// The picker for `scope`, with the stub's plugins.
    fn picker_app(
        scope: plugin_list::Scope,
        track: Option<TrackId>,
    ) -> JdanceApp<StubHost, NoStore> {
        let host = StubHost {
            plugins: vec![
                plugin("Laser", PluginKind::Instrument),
                plugin("Serum", PluginKind::Instrument),
                plugin("Plate Reverb", PluginKind::Effect),
            ],
            ..StubHost::default()
        };
        let mut app = JdanceApp::new(host, NoStore, Project::new(), None);
        app.plugin_picker = Some(PluginPicker::new(scope, track));
        app
    }

    #[test]
    fn filters_keep_the_window_and_a_click_loads_the_instrument() {
        let mut app = picker_app(plugin_list::Scope::Instruments, None);
        let n = app.project.channels.len();
        let mut d = Driver::new();
        let mut ui = |ui: &mut egui::Ui| app.plugin_picker_window(ui.ctx(), 0.0);
        d.frame(vec![], &mut ui);
        d.frame(vec![], &mut ui);
        for filter in ["VST3", "★ Favorites only", "★ Favorites only", "VST3"] {
            let at = d.find(filter).expect(filter);
            d.click(at, &mut ui);
            assert!(picker_open(&d), "{filter}: the window stays");
        }
        let at = d.find("Serum").expect("listed");
        d.click(at, &mut ui);
        d.frame(vec![], &mut ui);
        assert_eq!(app.project.channels.len(), n + 1);
        assert_eq!(app.project.channels.last().unwrap().name, "Serum");
        assert!(app.plugin_picker.is_none(), "loaded: the window closes");
    }

    #[test]
    fn the_mixer_picker_puts_the_effect_into_its_track() {
        let mut app = picker_app(plugin_list::Scope::All, None);
        let track = jdance_commands::add_track(&mut app.history, &mut app.project, "Bus").unwrap();
        app.mixer_action(mixer::MixerAction::PickPlugin(track), 0.0);
        let mut d = Driver::new();
        let mut ui = |ui: &mut egui::Ui| app.plugin_picker_window(ui.ctx(), 0.0);
        d.frame(vec![], &mut ui);
        d.frame(vec![], &mut ui);
        assert!(d.find("Serum").is_none(), "effects only");
        let at = d.find("Plate Reverb").expect("listed");
        d.click(at, &mut ui);
        assert_ne!(app.mixer_view.selected, track);
        assert_eq!(app.project.mixer.track(track).unwrap().inserts.len(), 1);
    }

    #[test]
    fn esc_clears_the_text_then_closes() {
        let app = pick(vec![vec![Event::Text("x".into())], vec![key(Key::Escape)]]);
        assert!(
            app.plugin_picker
                .as_ref()
                .is_some_and(|p| p.list.text.is_empty())
        );
        let app = pick(vec![vec![key(Key::Escape)]]);
        assert!(app.plugin_picker.is_none());
    }
}
