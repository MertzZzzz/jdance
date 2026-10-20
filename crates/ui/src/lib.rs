// SPDX-License-Identifier: GPL-3.0-or-later
//! egui user interface.
//!
//! The UI holds the document and the undo history, edits the document only
//! through `jdance-commands`, and tells the [`Host`] when it changed.

mod host;
mod mixer;
mod rack;
mod settings;

pub use host::{
    Host, LatencyInfo, MeterReadings, Notice, NoticeLevel, SampleStatus, StreamStatus,
    TransportStatus,
};

use egui::{Key, KeyboardShortcut, Modifiers};
use jdance_commands::{Edit, History, add_insert, add_sampler_channel, add_send, add_track};
use jdance_domain::{
    ChannelId, Effect, MAX_TEMPO_BPM, MAX_VOLUME, MIN_TEMPO_BPM, Project, TrackId,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenameTarget {
    Channel(ChannelId),
    Track(TrackId),
}

#[derive(Debug, Clone, PartialEq)]
struct RenameDialog {
    target: RenameTarget,
    text: String,
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
    show_mixer: bool,
    notices: Vec<(Notice, f64)>,
    pending: Option<Pending>,
    allow_close: bool,
    sync_needed: bool,
    export_loops: u32,
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
        let settings = host
            .current_device()
            .is_none()
            .then(|| SettingsWindow::new(&host));
        Self {
            host,
            store,
            project,
            history: History::default(),
            path,
            settings,
            rename: None,
            show_mixer: true,
            notices: Vec::new(),
            pending: None,
            allow_close: false,
            sync_needed: true,
            export_loops: 4,
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
        self.project = project;
        self.path = path;
        self.history.reset();
        self.sync_needed = true;
    }

    /// Runs `action` now, or asks first if there are unsaved changes.
    fn guarded(&mut self, action: Pending, ctx: &egui::Context, now: f64) {
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

    fn export(&mut self) {
        let Some(mut out) = rfd::FileDialog::new()
            .add_filter("WAV audio", &["wav"])
            .set_file_name("render.wav")
            .save_file()
        else {
            return;
        };
        if out.extension().is_none() {
            out.set_extension("wav");
        }
        let dir = self.project_dir().map(Path::to_path_buf);
        self.host
            .export_wav(&self.project, dir.as_deref(), out, self.export_loops);
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
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        // egui matches shortcuts "logically" (extra Shift is ignored), so the
        // Shift variants must be consumed before the plain ones.
        let (redo, undo, save_as, save, open, new) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&cmd_shift(Key::Z)) || i.consume_shortcut(&cmd(Key::Y)),
                i.consume_shortcut(&cmd(Key::Z)),
                i.consume_shortcut(&cmd_shift(Key::S)),
                i.consume_shortcut(&cmd(Key::S)),
                i.consume_shortcut(&cmd(Key::O)),
                i.consume_shortcut(&cmd(Key::N)),
            )
        });
        if redo {
            self.redo(now);
        } else if undo {
            self.undo(now);
        }
        if save_as {
            self.save(true, now);
        } else if save {
            self.save(false, now);
        }
        if open {
            self.guarded(Pending::Open, ctx, now);
        }
        if new {
            self.guarded(Pending::New, ctx, now);
        }
        let space = !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Space));
        if space {
            self.toggle_play();
        }
    }

    fn toggle_play(&mut self) {
        if self.host.transport().playing {
            self.host.stop();
        } else {
            self.host.play();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, now: f64) {
        let ctx = ui.ctx().clone();
        ui.horizontal_wrapped(|ui| {
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
                ui.horizontal(|ui| {
                    ui.label("Export loops:");
                    ui.add(egui::DragValue::new(&mut self.export_loops).range(1..=256));
                });
                if ui.button("Export WAV…").clicked() {
                    self.export();
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
            ui.toggle_value(&mut self.show_mixer, "Mixer");
            if ui.button("Audio settings").clicked() {
                self.settings = Some(SettingsWindow::new(&self.host));
            }
            ui.separator();

            let playing = self.host.transport().playing;
            if ui
                .selectable_label(playing, if playing { "Stop" } else { "Play" })
                .on_hover_text("Space")
                .clicked()
            {
                self.toggle_play();
            }

            ui.label("Tempo");
            let mut tempo = self.project.tempo_bpm;
            let r = ui.add(
                egui::DragValue::new(&mut tempo)
                    .range(MIN_TEMPO_BPM..=MAX_TEMPO_BPM)
                    .speed(0.5)
                    .fixed_decimals(1)
                    .suffix(" BPM"),
            );
            if r.changed() && tempo != self.project.tempo_bpm {
                self.apply(Edit::SetTempo(tempo), true, now);
            }
            if r.drag_stopped() || r.lost_focus() {
                self.history.end_gesture();
            }

            ui.label("Master");
            let mut master = self.project.mixer.master.volume;
            let r = ui.add(egui::Slider::new(&mut master, 0.0..=MAX_VOLUME).show_value(false));
            if r.changed() {
                self.apply(Edit::SetMasterVolume(master), true, now);
            }
            if r.drag_stopped() {
                self.history.end_gesture();
            }
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let t = self.host.transport();
        ui.horizontal_wrapped(|ui| {
            match self.host.stream() {
                Some(s) => {
                    let state = match &s.state {
                        jdance_audio_api::StreamState::Running => "running".to_owned(),
                        jdance_audio_api::StreamState::Failed(e) => format!("FAILED: {e}"),
                    };
                    let buffer = match (s.stats.min_frames, s.stats.max_frames) {
                        (Some(a), Some(b)) if a == b => format!("{a} frames"),
                        (Some(a), Some(b)) => format!("{a}–{b} frames"),
                        _ => "—".into(),
                    };
                    ui.label(format!(
                        "{} · {} · {} Hz · buffer {buffer} · {state}",
                        s.info.backend, s.info.device_name, s.info.sample_rate
                    ));
                }
                None => {
                    ui.colored_label(ui.visuals().warn_fg_color, "No audio device open");
                }
            }
            ui.separator();
            ui.label(format!("CPU {:.0}%", t.peak_load * 100.0));
            ui.label(format!("Voices {}", t.voices));
            if t.dropped_commands > 0 {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Dropped commands {}", t.dropped_commands),
                );
            }
        });
    }

    fn start_rename(&mut self, target: RenameTarget) {
        let text = match target {
            RenameTarget::Channel(id) => self.project.channel(id).map(|c| c.name.clone()),
            RenameTarget::Track(id) => self.project.mixer.track(id).map(|t| t.name.clone()),
        };
        if let Some(text) = text {
            self.rename = Some(RenameDialog { target, text });
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
            mixer::MixerAction::Rename(id) => {
                self.start_rename(RenameTarget::Track(id));
                Ok(())
            }
        };
        match result {
            Ok(()) => self.sync_needed = true,
            Err(e) => self.notify(Notice::error(e.to_string()), now),
        }
    }

    fn rename_dialog(&mut self, ctx: &egui::Context, now: f64) {
        let Some(dialog) = &mut self.rename else {
            return;
        };
        let mut commit = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("rename")).show(ctx, |ui| {
            ui.heading("Rename");
            let r = ui.text_edit_singleline(&mut dialog.text);
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                commit = true;
            }
            ui.horizontal(|ui| {
                commit |= ui.button("OK").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
            cancel |= ui.input(|i| i.key_pressed(Key::Escape));
        });
        if commit {
            let name = dialog.text.trim().to_owned();
            let target = dialog.target;
            self.rename = None;
            if !name.is_empty() {
                let edit = match target {
                    RenameTarget::Channel(id) => Edit::RenameChannel(id, name),
                    RenameTarget::Track(id) => Edit::RenameTrack(id, name),
                };
                self.apply(edit, false, now);
            }
        } else if cancel {
            self.rename = None;
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.history.is_dirty() {
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
        self.host.poll();
        for n in self.host.take_notices() {
            self.notify(n, now);
        }
        self.handle_close_request(&ctx);
        self.shortcuts(&ctx, now);

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, now));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        let mut mix = mixer::MixerOutput::default();
        if self.show_mixer {
            let latency = self.host.latency();
            let meters = self.host.meters();
            egui::Panel::bottom("mixer")
                .resizable(true)
                .default_size(330.0)
                .min_size(120.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        mix = mixer::show(ui, &self.project.mixer, &latency, &meters);
                    });
                });
        }
        let mut out = rack::RackOutput::default();
        egui::CentralPanel::default().show(ui, |ui| {
            out = rack::show(ui, &self.project, &self.host, self.project_dir());
        });
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
        for (name, sample) in out.new_channels {
            match add_sampler_channel(&mut self.history, &mut self.project, name) {
                Ok(id) => {
                    if let Some(s) = sample {
                        self.apply(Edit::SetChannelSample(id, Some(s)), false, now);
                    }
                    self.sync_needed = true;
                }
                Err(e) => self.notify(Notice::error(e.to_string()), now),
            }
        }

        if let Some(mut w) = self.settings.take()
            && w.show(&ctx, &mut self.host)
        {
            self.settings = Some(w);
        }
        self.rename_dialog(&ctx, now);
        self.confirm_dialog(&ctx, now);
        self.show_notices(&ctx, now);

        if self.sync_needed {
            self.sync_needed = false;
            let dir = self.project_dir().map(Path::to_path_buf);
            self.host.set_project(&self.project, dir.as_deref());
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));
        if self.host.transport().playing {
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
