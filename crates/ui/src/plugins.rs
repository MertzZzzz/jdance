// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugins window (PLG-020..024, spec 0.2.0 §6): VST3 search paths
//! (standard and the user's, each can be disabled), scan with progress
//! and cancel, the plugins found and the modules that failed.
//!
//! Scanning runs in a separate process per module with a timeout; a
//! module that crashes or hangs is blocklisted and only a full rescan
//! tries it again.

use crate::{Host, theme};
use egui::RichText;
use jdance_plugin_api::{PluginDescriptor, PluginKind};
use std::path::PathBuf;

/// Shows the window while `open`; returns whether it stays open.
pub(crate) fn window(ctx: &egui::Context, host: &mut dyn Host) -> bool {
    let mut open = true;
    egui::Window::new(RichText::new("Plugins · VST3").font(theme::strong(13.0)))
        .id(egui::Id::unique("plugins_window"))
        .open(&mut open)
        .resizable(true)
        .default_width(700.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(ctx.content_rect().height() * 0.8)
                .show(ui, |ui| contents(ui, host));
        });
    open
}

/// Which list the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    Found,
    Failed,
}

/// UI-RD §7 (board R5 · Plugins): search paths, the scan, the lists,
/// Full rescan and Rescan at the bottom right.
fn contents(ui: &mut egui::Ui, host: &mut dyn Host) {
    search_paths(ui, host);
    ui.add_space(8.0);
    scan(ui, host);
    lists(ui, host);
    ui.add_space(8.0);
    let running = host.plugin_scan_running();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("VST3 only: other module files are never loaded as plugins.")
                .size(11.0)
                .color(theme::DIM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled_ui(!running, |ui| {
                if crate::widgets::primary_button(ui, "Rescan", true)
                    .on_hover_text("Scan new and changed plugins")
                    .clicked()
                {
                    host.rescan_plugins(false);
                }
                if crate::widgets::outline_button(ui, "Full rescan")
                    .on_hover_text("Scan every plugin again, the failed ones included")
                    .clicked()
                {
                    host.rescan_plugins(true);
                }
            });
        });
    });
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn search_paths(ui: &mut egui::Ui, host: &mut dyn Host) {
    use crate::icons::Glyph as G;
    let paths = host.plugin_search_paths();
    let mut user = host.plugin_paths();
    let mut remove = None;
    let mut toggle = None;
    let mut add = false;
    crate::widgets::card(ui, None, "", |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            crate::widgets::caption(ui, "Search paths");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                add = crate::widgets::button(
                    ui,
                    Some(G::New),
                    "Folder…",
                    false,
                    crate::widgets::Look::Raised,
                    "Add a folder to scan",
                )
                .clicked();
            });
        });
        for p in &paths {
            egui::Frame::new()
                .fill(theme::FIELD)
                .corner_radius(7)
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let mut on = p.enabled;
                        let label = p.path.display().to_string();
                        if crate::widgets::switch(ui, &mut on, theme::LED_ON, &label)
                            .on_hover_text("Scan this folder")
                            .changed()
                        {
                            toggle = Some((p.path.clone(), on));
                        }
                        let (r, _) =
                            ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                        crate::icons::paint(ui, G::Folder, r, theme::MUTED);
                        let mut text = RichText::new(&label).font(theme::mono(11.0));
                        if !p.enabled {
                            text = text.strikethrough().color(theme::DIM);
                        }
                        let room = (ui.available_width() - 120.0).max(80.0);
                        ui.allocate_ui(egui::vec2(room, 20.0), |ui| {
                            ui.add(egui::Label::new(text).truncate())
                                .on_hover_text(&label);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if !p.system
                                && crate::icons::icon_button_sized(
                                    ui,
                                    G::Close,
                                    false,
                                    "Remove this folder",
                                    egui::vec2(22.0, 20.0),
                                    11.0,
                                )
                                .clicked()
                            {
                                remove = user.iter().position(|u| *u == p.path);
                            }
                            let note = match (p.system, p.exists) {
                                (_, false) => "not found",
                                (true, true) => "standard",
                                (false, true) => "user",
                            };
                            ui.label(RichText::new(note).size(10.5).color(theme::DIM));
                        });
                    });
                });
        }
    });
    if let Some((path, on)) = toggle {
        host.set_plugin_path_enabled(&path, on);
    }
    let mut changed = false;
    if let Some(i) = remove {
        user.remove(i);
        changed = true;
    }
    if add
        && let Some(dir) = rfd::FileDialog::new().pick_folder()
        && !paths.iter().any(|p| p.path == dir)
    {
        user.push(dir);
        changed = true;
    }
    if changed {
        host.set_plugin_paths(user);
    }
}

/// The running scan: an amber banner with the module, progress and
/// Cancel.
fn scan(ui: &mut egui::Ui, host: &mut dyn Host) {
    if !host.plugin_scan_running() {
        return;
    }
    let (done, total, module) = host.plugin_scan_progress().unwrap_or_default();
    let f = if total == 0 {
        0.0
    } else {
        done as f32 / total as f32
    };
    egui::Frame::new()
        .fill(theme::AMBER.gamma_multiply(0.08))
        .stroke(egui::Stroke::new(1.0, theme::AMBER.gamma_multiply(0.45)))
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 90.0).max(100.0));
                    ui.horizontal(|ui| {
                        ui.label("Scanning");
                        ui.label(RichText::new(file_name(&module)).color(theme::AMBER));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{done} / {total}")).font(theme::mono(10.5)),
                            );
                        });
                    });
                    let (bar, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 6.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(bar, 3.0, theme::FIELD);
                    let mut part = bar;
                    part.set_width(bar.width() * f);
                    ui.painter().rect_filled(part, 3.0, theme::AMBER);
                });
                if crate::widgets::outline_button(ui, "Cancel").clicked() {
                    host.cancel_plugin_scan();
                }
            });
        });
    ui.ctx().request_repaint_after_secs(0.2);
    ui.add_space(8.0);
}

fn lists(ui: &mut egui::Ui, host: &mut dyn Host) {
    let mut all: Vec<PluginDescriptor> = host.plugin_instruments();
    all.extend(host.plugin_effects());
    let blocked: Vec<(PathBuf, String)> = host.blocked_plugins();
    let id = egui::Id::unique("plugins_list_tab");
    let mut tab = ui.data(|d| d.get_temp::<List>(id)).unwrap_or(List::Found);
    if blocked.is_empty() {
        tab = List::Found;
    }
    crate::widgets::card(ui, None, "", |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            crate::widgets::segmented(ui, |ui| {
                if crate::widgets::button(
                    ui,
                    None,
                    &format!("Found {}", all.len()),
                    tab == List::Found,
                    crate::widgets::Look::Raised,
                    "",
                )
                .clicked()
                {
                    tab = List::Found;
                }
                if !blocked.is_empty()
                    && crate::widgets::button(
                        ui,
                        Some(crate::icons::Glyph::Warning),
                        &format!("Failed {}", blocked.len()),
                        tab == List::Failed,
                        crate::widgets::Look::Raised,
                        "",
                    )
                    .clicked()
                {
                    tab = List::Failed;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("failed modules are blocklisted · Full rescan tries them again")
                        .size(10.5)
                        .color(theme::DIM),
                );
            });
        });
        ui.separator();
        match tab {
            List::Found => {
                if all.is_empty() {
                    ui.label(RichText::new("No plugins found yet: Rescan.").color(theme::MUTED));
                }
                egui::Grid::new("plugins_found_grid")
                    .num_columns(5)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        for d in &all {
                            let (glyph, color, kind) = match d.kind {
                                PluginKind::Instrument => {
                                    (crate::icons::Glyph::Instrument, theme::AMBER, "Instrument")
                                }
                                PluginKind::Effect => {
                                    (crate::icons::Glyph::Eq, theme::TEAL, "Effect")
                                }
                            };
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(
                                    egui::vec2(14.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                crate::icons::paint(ui, glyph, r, color);
                                ui.label(RichText::new(&d.plugin.name).strong());
                            });
                            ui.label(RichText::new(&d.plugin.vendor).color(theme::MUTED));
                            let tag = if d.category.is_empty() {
                                kind.to_owned()
                            } else {
                                format!("{kind} · {}", d.category)
                            };
                            egui::Frame::new()
                                .fill(theme::RAISED)
                                .corner_radius(4)
                                .inner_margin(egui::Margin::symmetric(6, 1))
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(RichText::new(tag).size(10.5)).truncate(),
                                    );
                                });
                            ui.label(
                                RichText::new(&d.plugin.version)
                                    .font(theme::mono(10.5))
                                    .color(theme::DIM),
                            )
                            .on_hover_text(&d.path);
                            ui.end_row();
                        }
                    });
            }
            List::Failed => {
                egui::Grid::new("plugin_errors")
                    .num_columns(2)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        for (path, reason) in &blocked {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(file_name(path)).font(theme::mono(11.0)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(path.display().to_string());
                            ui.colored_label(theme::WARN, reason);
                            ui.end_row();
                        }
                    });
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, tab));
}
