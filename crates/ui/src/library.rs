// SPDX-License-Identifier: GPL-3.0-or-later
//! Library folders window and the Sample Selection Dialog (spec 0.2.0
//! §5). Library folders are user settings, not part of a project; removing
//! one deletes nothing on disk, and a missing one shows as unavailable.

use crate::{Host, IndexStatus, Listing, theme};
use egui::RichText;
use jdance_domain::{AssetId, Project};
use std::path::{Path, PathBuf};

/// Library folders: add, remove, display name, recursive, rescan.
/// Returns `false` when closed.
pub(crate) fn library_window(ctx: &egui::Context, host: &mut dyn Host) -> bool {
    let mut open = true;
    egui::Window::new("Library folders")
        .id(egui::Id::unique("library_folders"))
        .default_width(560.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Sample folders shown in the Browser and the sample dialog. They belong to you, not to a project.")
                    .size(11.5)
                    .color(theme::MUTED),
            );
            ui.add_space(4.0);
            let mut roots = host.library_roots();
            let before = roots.clone();
            let mut remove = None;
            egui::Grid::new("library_roots")
                .num_columns(4)
                .spacing([10.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    for (i, r) in roots.iter_mut().enumerate() {
                        let mut name = r.name.clone().unwrap_or_default();
                        let hint = r.display();
                        if ui
                            .add(egui::TextEdit::singleline(&mut name).hint_text(hint).desired_width(130.0))
                            .on_hover_text("Display name (empty: the folder name)")
                            .lost_focus()
                        {
                            r.name = (!name.trim().is_empty()).then(|| name.trim().to_owned());
                        }
                        ui.vertical(|ui| {
                            ui.set_width(250.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(r.path.display().to_string()).font(theme::mono(11.0)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(r.path.display().to_string());
                            if !r.path.is_dir() {
                                ui.label(RichText::new("unavailable: folder not found").size(11.0).color(theme::WARN));
                            }
                        });
                        ui.checkbox(&mut r.recursive, "Subfolders")
                            .on_hover_text("Index subfolders too");
                        if ui
                            .button("Remove")
                            .on_hover_text("Removes the folder from the library; no file is deleted")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
            if let Some(i) = remove {
                roots.remove(i);
            }
            ui.horizontal(|ui| {
                if ui.button("+ Add folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    && !roots.iter().any(|r| r.path == dir)
                {
                    roots.push(crate::LibraryRoot::new(dir));
                }
                ui.separator();
                let IndexStatus {
                    files,
                    running,
                    cancelled,
                } = host.library_index();
                if running {
                    ui.spinner();
                    ui.label(format!("Indexing… {files} files"));
                    if ui.button("Cancel").clicked() {
                        host.cancel_library_scan();
                    }
                    ui.ctx().request_repaint_after_secs(0.2);
                } else {
                    let state = if cancelled {
                        format!("Indexing cancelled at {files} files")
                    } else {
                        format!("{files} audio files indexed")
                    };
                    ui.label(RichText::new(state).color(theme::MUTED));
                    if ui.button("Rescan").clicked() {
                        host.rescan_library();
                    }
                }
            });
            if roots != before {
                host.set_library_roots(roots);
            }
        });
    open
}

/// What a sample is chosen for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickFor {
    /// A new sampler channel.
    NewChannel,
    /// Replace this sampler's sample.
    Channel(jdance_domain::ChannelId),
    /// Add to the project's samples only.
    Import,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Library,
    Project,
    Files,
}

/// The chosen sample.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Picked {
    File(PathBuf),
    Asset(AssetId),
}

#[derive(Debug, Clone)]
pub(crate) struct SamplePicker {
    pub purpose: PickFor,
    tab: Tab,
    query: String,
    selected: Option<Picked>,
}

impl SamplePicker {
    pub fn new(purpose: PickFor) -> Self {
        Self {
            purpose,
            tab: Tab::Library,
            query: String::new(),
            selected: None,
        }
    }

    /// `Some(Some(_))` = chosen, `Some(None)` = cancelled, `None` = open.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        host: &mut dyn Host,
        dir: Option<&Path>,
    ) -> Option<Option<Picked>> {
        let mut result = None;
        let mut open = true;
        egui::Window::new("Choose a sample")
            .id(egui::Id::unique("sample_picker"))
            .default_size([520.0, 420.0])
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (t, label) in [
                        (Tab::Library, "Library"),
                        (Tab::Project, "Project samples"),
                        (Tab::Files, "File system"),
                    ] {
                        if ui.selectable_label(self.tab == t, label).clicked() {
                            self.tab = t;
                        }
                    }
                });
                ui.separator();
                let list_h = 280.0;
                match self.tab {
                    Tab::Library => {
                        let roots = host.library_folders();
                        if roots.is_empty() {
                            ui.label(
                                RichText::new(
                                    "No library folders yet: add one in Options → Library folders.",
                                )
                                .color(theme::MUTED),
                            );
                        } else {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.query)
                                    .hint_text("Search the library"),
                            );
                            let listing = host.search_files(&roots, &self.query);
                            egui::ScrollArea::vertical()
                                .max_height(list_h)
                                .show(ui, |ui| match listing {
                                    Listing::Loading => {
                                        ui.spinner();
                                        ui.ctx().request_repaint_after_secs(0.2);
                                    }
                                    Listing::Failed(e) => {
                                        ui.colored_label(theme::ERROR, e);
                                    }
                                    Listing::Ready(files) => {
                                        for f in files.iter().filter(|f| is_wav(&f.path)) {
                                            let on =
                                                self.selected == Some(Picked::File(f.path.clone()));
                                            let r = ui
                                                .selectable_label(on, &f.name)
                                                .on_hover_text(f.path.display().to_string());
                                            if r.clicked() {
                                                self.selected = Some(Picked::File(f.path.clone()));
                                            }
                                            if r.double_clicked() {
                                                result = Some(Some(Picked::File(f.path.clone())));
                                            }
                                        }
                                    }
                                });
                        }
                    }
                    Tab::Project => {
                        if project.assets.is_empty() {
                            ui.label(
                                RichText::new("The project has no audio files yet.")
                                    .color(theme::MUTED),
                            );
                        }
                        egui::ScrollArea::vertical()
                            .max_height(list_h)
                            .show(ui, |ui| {
                                for a in &project.assets {
                                    let on = self.selected == Some(Picked::Asset(a.id));
                                    let r = ui
                                        .selectable_label(
                                            on,
                                            format!("{} · {:.2} s", a.name, a.seconds()),
                                        )
                                        .on_hover_text(&a.path);
                                    if r.clicked() {
                                        self.selected = Some(Picked::Asset(a.id));
                                    }
                                    if r.double_clicked() {
                                        result = Some(Some(Picked::Asset(a.id)));
                                    }
                                }
                            });
                    }
                    Tab::Files => {
                        ui.label("Any WAV file on this computer.");
                        if ui.button("Browse…").clicked()
                            && let Some(p) = rfd::FileDialog::new()
                                .add_filter("WAV audio", &["wav", "wave"])
                                .pick_file()
                        {
                            self.selected = Some(Picked::File(p));
                        }
                        if let Some(Picked::File(p)) = &self.selected {
                            ui.label(
                                RichText::new(p.display().to_string()).font(theme::mono(11.0)),
                            );
                        }
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let path = self.selected.as_ref().and_then(|s| match s {
                        Picked::File(p) => Some(p.clone()),
                        Picked::Asset(id) => project.asset(*id).map(|a| match dir {
                            Some(d) if Path::new(&a.path).is_relative() => d.join(&a.path),
                            _ => PathBuf::from(&a.path),
                        }),
                    });
                    if ui
                        .add_enabled(path.is_some(), egui::Button::new("Preview"))
                        .clicked()
                        && let Some(p) = &path
                    {
                        host.preview_file(Some(p));
                    }
                    if ui
                        .add_enabled(self.selected.is_some(), egui::Button::new("Select"))
                        .clicked()
                    {
                        result = Some(self.selected.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        result = Some(None);
                    }
                });
            });
        if !open {
            result = Some(None);
        }
        if result.is_some() {
            host.preview_file(None);
        }
        result
    }
}

fn is_wav(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("wave"))
}
