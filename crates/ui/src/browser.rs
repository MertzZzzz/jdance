// SPDX-License-Identifier: GPL-3.0-or-later
//! Browser (spec "DAW UI clean room", §4): favorites, the project's
//! contents, library folders and plugins, locations; search; a details
//! panel with the waveform and preview.
//!
//! Folder listings and searches run in the background (the host); the
//! preview plays on the engine's preview bus and never changes the
//! project. Files and instruments are dragged onto the Channel Rack.

use crate::{DirEntry, Host, Listing, SampleStatus, theme};
use egui::{RichText, Sense, Stroke, pos2, vec2};
use jdance_domain::{ChannelId, PatternId, Project, SampleRef};
use jdance_plugin_api::PluginDescriptor;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Dragged from the Browser.
#[derive(Debug, Clone)]
pub(crate) enum BrowserItem {
    Sample(PathBuf),
    /// A MIDI file: its notes go into the Piano Roll (spec 0.2.9 §6.5).
    Midi(PathBuf),
    Instrument(PluginDescriptor),
}

/// Requests for the app.
#[derive(Debug, Clone)]
pub(crate) enum BrowserAction {
    SelectPattern(PatternId),
    ChannelSettings(ChannelId),
    /// Undo this many steps.
    UndoSteps(usize),
    /// Double-click: the Sample Editor (nothing is added).
    OpenFile(PathBuf),
    /// Middle click: the file replaces the sample of the selected channel
    /// (spec 0.2.5 §3).
    ReplaceSample(PathBuf),
    /// An instrument into a new channel, an effect into the selected
    /// mixer track (spec 0.2.5 §10 item 12).
    LoadPlugin(PluginDescriptor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Patterns,
    Instruments,
    Assets,
    Automation,
    Effects,
    History,
    Plugins,
}

#[derive(Debug, Default)]
pub(crate) struct BrowserView {
    query: String,
    expanded: BTreeSet<PathBuf>,
    open: BTreeSet<Section>,
    selected: Option<PathBuf>,
    auto_preview: bool,
    /// Files in the order drawn, this frame and the last: the arrows walk
    /// them (spec 0.2.9 §7).
    visible: Vec<PathBuf>,
    shown: Vec<PathBuf>,
    /// Scroll the selected file into view once.
    reveal: bool,
    /// The arrow keys go to the Browser (it was clicked last; the app
    /// sets it).
    pub keys_active: bool,
    /// Where the Browser was drawn, for the app's key focus.
    pub area: Option<egui::Rect>,
    /// A press inside the Browser this frame (spec 0.3.0 §1.3).
    pub pressed: bool,
}

impl BrowserView {
    pub(crate) fn new() -> Self {
        Self {
            auto_preview: true,
            ..Self::default()
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        project_dir: Option<&Path>,
        history: &[&str],
        host: &mut dyn Host,
    ) -> Vec<BrowserAction> {
        let mut out = Vec::new();
        self.area = Some(ui.max_rect());
        self.pressed = crate::widgets::pressed_in(ui, ui.max_rect());
        self.arrows(ui, host);
        self.shown = std::mem::take(&mut self.visible);
        ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("Search samples")
                .desired_width(f32::INFINITY),
        );
        let library = host.library_folders();
        let favorites = host.favorite_folders();
        let details_h = if self.selected.is_some() { 250.0 } else { 0.0 };
        let list_h = (ui.available_height() - details_h).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("browser")
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                crate::widgets::middle_pan(ui);
                if !self.query.trim().is_empty() {
                    let mut roots: Vec<PathBuf> =
                        library.iter().chain(&favorites).cloned().collect();
                    roots.extend(project_dir.map(Path::to_path_buf));
                    roots.sort();
                    roots.dedup();
                    if roots.is_empty() {
                        ui.label(
                            RichText::new("Add a library folder to search.").color(theme::MUTED),
                        );
                    }
                    match host.search_files(&roots, self.query.trim()) {
                        Listing::Loading => {
                            ui.label(RichText::new("Searching…").color(theme::MUTED));
                        }
                        Listing::Failed(e) => {
                            ui.colored_label(theme::ERROR, e);
                        }
                        Listing::Ready(items) => {
                            ui.label(
                                RichText::new(format!("{} result(s)", items.len()))
                                    .size(11.0)
                                    .color(theme::MUTED),
                            );
                            for e in &items {
                                self.file_row(ui, host, e, 0, true, &mut out);
                            }
                        }
                    }
                    return;
                }
                heading(ui, "Favorites");
                if favorites.is_empty() {
                    ui.label(
                        RichText::new("Right-click a folder → Add to favorites")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                }
                for f in &favorites {
                    self.folder(ui, host, f, &display(f), 0, &mut out);
                }
                heading(ui, "Project");
                self.project(ui, project, history, &mut out);
                heading(ui, "Library");
                for r in host.library_roots() {
                    if r.path.is_dir() {
                        self.folder(ui, host, &r.path, &r.display(), 0, &mut out);
                    } else {
                        ui.label(
                            RichText::new(format!("{} · unavailable", r.display()))
                                .size(12.0)
                                .color(theme::WARN),
                        )
                        .on_hover_text(format!(
                            "{} was not found; it stays in the library until you remove it",
                            r.path.display()
                        ));
                    }
                }
                if ui.small_button("+ Add folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    && !library.contains(&dir)
                {
                    let mut l = library.clone();
                    l.push(dir);
                    host.set_library_folders(l);
                }
                self.plugins(ui, host, &mut out);
                heading(ui, "Locations");
                match project_dir {
                    Some(d) => self.folder(ui, host, d, "Project folder", 0, &mut out),
                    None => {
                        ui.label(
                            RichText::new("Save the project to browse its folder")
                                .size(11.0)
                                .color(theme::MUTED),
                        );
                    }
                }
            });
        if let Some(path) = self.selected.clone() {
            ui.separator();
            self.details(ui, host, &path, &favorites);
        }
        out
    }

    /// ↓ and ↑ select the next and previous file drawn and preview it,
    /// as a click does (spec 0.2.9 §7).
    fn arrows(&mut self, ui: &egui::Ui, host: &mut dyn Host) {
        if !self.keys_active || ui.ctx().text_edit_focused() || self.shown.is_empty() {
            return;
        }
        let step = ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                1
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                -1
            } else {
                0
            }
        });
        if step == 0 {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|s| self.shown.iter().position(|p| p == s));
        let next = match at {
            Some(k) => (k as i64 + step).clamp(0, self.shown.len() as i64 - 1) as usize,
            None if step > 0 => 0,
            None => self.shown.len() - 1,
        };
        if at == Some(next) {
            return;
        }
        let path = self.shown[next].clone();
        self.select_file(host, &path);
        self.reveal = true;
    }

    /// Selects `path`; a sample is inspected and, with auto preview,
    /// played.
    fn select_file(&mut self, host: &mut dyn Host, path: &Path) {
        self.selected = Some(path.to_path_buf());
        if is_wav(path) {
            host.inspect_file(path);
            if self.auto_preview {
                host.preview_file(Some(path));
            }
        }
    }

    fn project(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        history: &[&str],
        out: &mut Vec<BrowserAction>,
    ) {
        // Sampler samples and the audio assets of Playlist clips.
        let mut assets: Vec<&str> = p
            .channels
            .iter()
            .filter_map(|c| c.sample().map(|s| s.path.as_str()))
            .chain(p.assets.iter().map(|a| a.path.as_str()))
            .collect();
        assets.sort_unstable();
        assets.dedup();
        let effects: Vec<String> = p
            .mixer
            .all_tracks()
            .flat_map(|t| {
                t.inserts
                    .iter()
                    .map(move |i| format!("{} · {}", t.name, i.effect.name()))
            })
            .collect();
        let rows = [
            (Section::Patterns, "Patterns", p.patterns.len()),
            (Section::Instruments, "Instruments", p.channels.len()),
            (Section::Assets, "Audio assets", assets.len()),
            (Section::Automation, "Automation", p.automation.len()),
            (Section::Effects, "Effects", effects.len()),
            (Section::History, "History", history.len()),
        ];
        for (section, label, count) in rows {
            let open = self.open.contains(&section);
            if disclosure_row(ui, open, label, Some(count), 0).clicked() {
                toggle(&mut self.open, section);
            }
            if !open {
                continue;
            }
            match section {
                Section::Patterns => {
                    for pat in &p.patterns {
                        if item(ui, &pat.name, 1).clicked() {
                            out.push(BrowserAction::SelectPattern(pat.id));
                        }
                    }
                }
                Section::Instruments => {
                    for c in &p.channels {
                        let kind = if c.instrument().is_some() {
                            "VST3"
                        } else {
                            "Sampler"
                        };
                        if item(ui, &format!("{} · {kind}", c.name), 1)
                            .on_hover_text("Open channel settings")
                            .clicked()
                        {
                            out.push(BrowserAction::ChannelSettings(c.id));
                        }
                    }
                }
                Section::Assets => {
                    for a in &assets {
                        if item(ui, a, 1).clicked() {
                            self.selected = Some(PathBuf::from(a));
                        }
                    }
                }
                Section::Automation => {
                    for l in &p.automation {
                        item(
                            ui,
                            &format!("Lane {} · {} point(s)", l.id.0, l.points.len()),
                            1,
                        );
                    }
                }
                Section::Effects => {
                    for e in &effects {
                        item(ui, e, 1);
                    }
                }
                Section::History => {
                    for (k, label) in history.iter().enumerate() {
                        if item(ui, label, 1)
                            .on_hover_text("Click to undo back to before this edit")
                            .clicked()
                        {
                            out.push(BrowserAction::UndoSteps(k + 1));
                        }
                    }
                }
                Section::Plugins => {}
            }
        }
    }

    fn plugins(&mut self, ui: &mut egui::Ui, host: &mut dyn Host, out: &mut Vec<BrowserAction>) {
        let count = host.plugin_instruments().len() + host.plugin_effects().len();
        let open = self.open.contains(&Section::Plugins);
        if disclosure_row(ui, open, "Plugins", Some(count), 0).clicked() {
            toggle(&mut self.open, Section::Plugins);
        }
        if !open {
            return;
        }
        // Search and favorites (spec 0.2.5 §10): double click or Enter
        // loads an instrument into a new channel, an effect into the
        // selected mixer track.
        let picked = crate::plugin_list::PluginList::show_kept(
            ui,
            "browser_plugins",
            host,
            crate::plugin_list::Scope::All,
            320.0,
        );
        if let Some(d) = picked.load {
            out.push(BrowserAction::LoadPlugin(d));
        }
    }

    fn folder(
        &mut self,
        ui: &mut egui::Ui,
        host: &mut dyn Host,
        dir: &Path,
        label: &str,
        depth: usize,
        out: &mut Vec<BrowserAction>,
    ) {
        let open = self.expanded.contains(dir);
        let resp = disclosure_row(ui, open, label, None, depth);
        if resp.clicked() {
            toggle(&mut self.expanded, dir.to_path_buf());
        }
        resp.context_menu(|ui| {
            let favorites = host.favorite_folders();
            if favorites.iter().any(|f| f == dir) {
                if ui.button("Remove from favorites").clicked() {
                    host.set_favorite_folders(favorites.into_iter().filter(|f| f != dir).collect());
                    ui.close();
                }
            } else if ui.button("Add to favorites").clicked() {
                let mut f = favorites;
                f.push(dir.to_path_buf());
                host.set_favorite_folders(f);
                ui.close();
            }
            let library = host.library_folders();
            if library.iter().any(|f| f == dir) && ui.button("Remove library folder").clicked() {
                host.set_library_folders(library.into_iter().filter(|f| f != dir).collect());
                ui.close();
            }
            if ui.button("Refresh").clicked() {
                host.refresh_dir(dir);
                ui.close();
            }
            if ui.button("Show in file manager").clicked() {
                host.reveal_in_file_manager(dir);
                ui.close();
            }
        });
        if !open {
            return;
        }
        match host.list_dir(dir) {
            Listing::Loading => {
                indent_label(ui, depth + 1, RichText::new("Loading…").color(theme::MUTED));
            }
            Listing::Failed(e) => {
                indent_label(ui, depth + 1, RichText::new(e).color(theme::ERROR));
            }
            Listing::Ready(entries) => {
                if entries.is_empty() {
                    indent_label(ui, depth + 1, RichText::new("Empty").color(theme::MUTED));
                }
                for e in &entries {
                    if e.is_dir {
                        self.folder(ui, host, &e.path, &e.name, depth + 1, out);
                    } else {
                        self.file_row(ui, host, e, depth + 1, false, out);
                    }
                }
            }
        }
    }

    fn file_row(
        &mut self,
        ui: &mut egui::Ui,
        host: &mut dyn Host,
        e: &DirEntry,
        depth: usize,
        show_folder: bool,
        out: &mut Vec<BrowserAction>,
    ) {
        let selected = self.selected.as_deref() == Some(e.path.as_path());
        let supported = is_wav(&e.path);
        let midi = is_midi(&e.path);
        self.visible.push(e.path.clone());
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click_and_drag());
        if selected {
            ui.painter().rect_filled(rect, 5.0, theme::BLUE_BG);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 5.0, theme::RAISED);
        }
        let x = rect.left() + 8.0 + depth as f32 * 14.0;
        // Search results come from many folders: name the parent.
        let mut right = rect.right() - 8.0;
        if show_folder && let Some(parent) = e.path.parent().and_then(Path::file_name) {
            right = crate::widgets::fit_text(
                ui.painter(),
                pos2(rect.right() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                parent.to_string_lossy(),
                egui::FontId::proportional(11.0),
                theme::MUTED,
                rect.width() / 3.0,
            )
            .left()
                - 6.0;
        }
        crate::widgets::fit_text(
            ui.painter(),
            pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &e.name,
            egui::FontId::proportional(12.5),
            if supported || midi {
                theme::TEXT
            } else {
                theme::MUTED
            },
            right - x,
        );
        let resp = resp.on_hover_text(if midi {
            format!(
                "{}\nMIDI file: drag onto the Piano Roll to put its notes there (or Ctrl+M in the Piano Roll)",
                e.path.display()
            )
        } else if supported {
            format!(
                "{}\nClick: select and preview · double-click: Sample Editor · middle click: into the selected channel\nDrag onto the Channel Rack or the Playlist (a sampler and its clip)",
                e.path.display()
            )
        } else {
            format!(
                "{}\nThis format is not supported yet (WAV and MP3 only)",
                e.path.display()
            )
        });
        if supported {
            resp.dnd_set_drag_payload(BrowserItem::Sample(e.path.clone()));
        } else if midi {
            resp.dnd_set_drag_payload(BrowserItem::Midi(e.path.clone()));
        }
        if selected && std::mem::take(&mut self.reveal) {
            resp.scroll_to_me(None);
        }
        if resp.clicked() {
            self.select_file(host, &e.path);
        }
        if resp.double_clicked() && supported {
            out.push(BrowserAction::OpenFile(e.path.clone()));
        }
        // Middle click: into the selected channel; no preview (§3).
        if resp.middle_clicked() && supported {
            out.push(BrowserAction::ReplaceSample(e.path.clone()));
        }
    }

    fn details(
        &mut self,
        ui: &mut egui::Ui,
        host: &mut dyn Host,
        path: &Path,
        favorites: &[PathBuf],
    ) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        ui.label(RichText::new(name).font(theme::strong(13.0)));
        let sample = SampleRef {
            path: path.to_string_lossy().into_owned(),
        };
        let supported = is_wav(path);
        let status = if supported {
            host.inspect_file(path);
            host.sample_status(&sample, None)
        } else {
            None
        };
        match (&status, supported) {
            (_, false) => {
                ui.colored_label(theme::WARN, "Unsupported format: WAV and MP3 files only");
            }
            (
                Some(SampleStatus::Ready {
                    frames,
                    source_rate,
                    format,
                }),
                _,
            ) => {
                ui.label(
                    RichText::new(format!(
                        "{:.2} s · {source_rate} Hz · {frames} frames · {}",
                        *frames as f64 / f64::from(*source_rate),
                        format_label(format)
                    ))
                    .font(theme::mono(11.0))
                    .color(theme::MUTED),
                );
            }
            (Some(SampleStatus::Failed(e)), _) => {
                ui.colored_label(theme::ERROR, format!("Damaged or unreadable: {e}"));
            }
            _ => {
                ui.label(RichText::new("Loading…").color(theme::MUTED));
            }
        }
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::hover());
        ui.painter().rect_filled(rect, 6.0, theme::FIELD);
        if let Some(peaks) = host.sample_peaks(&sample, None).filter(|p| !p.is_empty()) {
            let cols = rect.width().max(1.0) as usize;
            for c in 0..cols {
                let a = c * peaks.len() / cols;
                let b = ((c + 1) * peaks.len() / cols).max(a + 1).min(peaks.len());
                let v = peaks[a..b].iter().fold(0.0f32, |m, p| m.max(*p)).min(1.0);
                let h = (v * (rect.height() / 2.0 - 4.0)).max(0.5);
                let x = rect.left() + c as f32 + 0.5;
                ui.painter().line_segment(
                    [pos2(x, rect.center().y - h), pos2(x, rect.center().y + h)],
                    Stroke::new(1.0, theme::BLUE),
                );
            }
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(supported, egui::Button::new("Preview"))
                .clicked()
            {
                host.preview_file(Some(path));
            }
            if ui.button("Stop").clicked() {
                host.preview_file(None);
            }
            ui.checkbox(&mut self.auto_preview, "Auto-preview");
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Show in folder").clicked() {
                host.reveal_in_file_manager(path);
            }
            if let Some(folder) = path.parent() {
                let fav = favorites.iter().any(|f| f == folder);
                if !fav && ui.button("Favorite folder").clicked() {
                    let mut f = favorites.to_vec();
                    f.push(folder.to_path_buf());
                    host.set_favorite_folders(f);
                }
            }
        });
        ui.label(
            RichText::new("Preview does not change the project.")
                .size(11.0)
                .color(theme::MUTED),
        );
    }
}

/// Files the app reads (spec 0.2.5 §5); the content decides the format.
pub(crate) fn is_wav(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "wav" | "wave" | "mp3"))
}

/// Standard MIDI files (spec 0.2.9 §6.6).
pub(crate) fn is_midi(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mid" | "midi"))
}

/// "16-bit" for a WAV, the MP3 description as is.
pub(crate) fn format_label(format: &str) -> String {
    if format.chars().all(|c| c.is_ascii_digit()) && !format.is_empty() {
        format!("{format}-bit")
    } else {
        format.to_owned()
    }
}

fn display(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn toggle<T: Ord>(set: &mut BTreeSet<T>, v: T) {
    if !set.remove(&v) {
        set.insert(v);
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .size(11.0)
            .color(theme::MUTED)
            .extra_letter_spacing(0.6),
    );
}

fn indent_label(ui: &mut egui::Ui, depth: usize, text: RichText) {
    ui.horizontal(|ui| {
        ui.add_space(8.0 + depth as f32 * 14.0);
        ui.label(text.size(11.5));
    });
}

/// A full-width row with a drawn open/closed triangle.
fn disclosure_row(
    ui: &mut egui::Ui,
    open: bool,
    label: &str,
    count: Option<usize>,
    depth: usize,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 5.0, theme::RAISED);
    }
    let x = rect.left() + 8.0 + depth as f32 * 14.0;
    let c = pos2(x + 4.0, rect.center().y);
    let pts = if open {
        vec![
            c + vec2(-4.0, -2.5),
            c + vec2(4.0, -2.5),
            c + vec2(0.0, 3.0),
        ]
    } else {
        vec![
            c + vec2(-2.5, -4.0),
            c + vec2(3.0, 0.0),
            c + vec2(-2.5, 4.0),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(pts, theme::MUTED, Stroke::NONE));
    let mut right = rect.right() - 8.0;
    if let Some(n) = count {
        right = ui
            .painter()
            .text(
                pos2(rect.right() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                n.to_string(),
                theme::mono(11.0),
                theme::MUTED,
            )
            .left()
            - 6.0;
    }
    crate::widgets::fit_text(
        ui.painter(),
        pos2(x + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.5),
        theme::TEXT,
        right - (x + 14.0),
    );
    resp
}

fn item(ui: &mut egui::Ui, label: &str, depth: usize) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click_and_drag());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 5.0, theme::RAISED);
    }
    let x = rect.left() + 8.0 + depth as f32 * 14.0 + 14.0;
    crate::widgets::fit_text(
        ui.painter(),
        pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.0),
        theme::TEXT,
        rect.right() - 8.0 - x,
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ui::{Driver, StubHost};

    #[test]
    fn arrows_walk_the_files_and_preview_them() {
        let mut v = BrowserView::new();
        v.keys_active = true;
        v.shown = ["a.wav", "b.mid", "c.wav"].map(PathBuf::from).to_vec();
        let mut host = StubHost::default();
        let mut d = Driver::new();
        let key = |k| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let mut press = |v: &mut BrowserView, host: &mut StubHost, k| {
            d.frame(vec![key(k)], &mut |ui| v.arrows(ui, host));
        };
        press(&mut v, &mut host, egui::Key::ArrowDown);
        assert_eq!(v.selected.as_deref(), Some(Path::new("a.wav")));
        press(&mut v, &mut host, egui::Key::ArrowDown);
        assert_eq!(v.selected.as_deref(), Some(Path::new("b.mid")));
        press(&mut v, &mut host, egui::Key::ArrowDown);
        press(&mut v, &mut host, egui::Key::ArrowDown);
        assert_eq!(
            v.selected.as_deref(),
            Some(Path::new("c.wav")),
            "stays at the end"
        );
        press(&mut v, &mut host, egui::Key::ArrowUp);
        assert_eq!(v.selected.as_deref(), Some(Path::new("b.mid")));
        // Samples play as they are reached; a MIDI file does not.
        assert_eq!(host.previews, ["a.wav", "c.wav"].map(PathBuf::from));
        assert!(v.reveal, "the list scrolls to it");
        // Without the key focus the arrows are the editors'.
        v.keys_active = false;
        press(&mut v, &mut host, egui::Key::ArrowDown);
        assert_eq!(v.selected.as_deref(), Some(Path::new("b.mid")));
    }
}
