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
    egui::Window::new("Plugins")
        .id(egui::Id::unique("plugins_window"))
        .open(&mut open)
        .resizable(true)
        .default_width(640.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(ctx.content_rect().height() * 0.8)
                .show(ui, |ui| contents(ui, host));
        });
    open
}

fn contents(ui: &mut egui::Ui, host: &mut dyn Host) {
    search_paths(ui, host);
    ui.separator();
    scan(ui, host);
    ui.separator();
    found(ui, host);
    let blocked: Vec<(PathBuf, String)> = host.blocked_plugins();
    if !blocked.is_empty() {
        ui.separator();
        ui.strong(format!("Scan errors ({})", blocked.len()));
        ui.label(
            RichText::new(
                "These modules crashed, hung or misbehaved while scanning and are not loaded. \
                 A full rescan tries them again.",
            )
            .size(11.0)
            .color(theme::MUTED),
        );
        egui::Grid::new("plugin_errors")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for (path, reason) in blocked {
                    ui.add(
                        egui::Label::new(RichText::new(file_name(&path)).font(theme::mono(11.0)))
                            .truncate(),
                    )
                    .on_hover_text(path.display().to_string());
                    ui.colored_label(theme::WARN, reason);
                    ui.end_row();
                }
            });
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn search_paths(ui: &mut egui::Ui, host: &mut dyn Host) {
    ui.strong("Search paths");
    ui.label(
        RichText::new("VST3 only: other module files are never loaded as plugins.")
            .size(11.0)
            .color(theme::MUTED),
    );
    let paths = host.plugin_search_paths();
    let mut user = host.plugin_paths();
    let mut remove = None;
    let mut toggle = None;
    egui::Grid::new("plugin_paths")
        .num_columns(4)
        .spacing([10.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for p in &paths {
                let mut on = p.enabled;
                if ui
                    .checkbox(&mut on, "")
                    .on_hover_text("Scan this folder")
                    .changed()
                {
                    toggle = Some((p.path.clone(), on));
                }
                ui.vertical(|ui| {
                    ui.set_width(380.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(p.path.display().to_string()).font(theme::mono(11.0)),
                        )
                        .truncate(),
                    )
                    .on_hover_text(p.path.display().to_string());
                    if !p.exists {
                        ui.label(
                            RichText::new("folder not found")
                                .size(11.0)
                                .color(theme::MUTED),
                        );
                    }
                });
                ui.label(
                    RichText::new(if p.system { "standard" } else { "yours" })
                        .size(11.0)
                        .color(theme::MUTED),
                );
                if p.system {
                    ui.label("");
                } else if ui.small_button("Remove").clicked() {
                    remove = user.iter().position(|u| *u == p.path);
                }
                ui.end_row();
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
    if ui.button("+ Add folder…").clicked()
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

fn scan(ui: &mut egui::Ui, host: &mut dyn Host) {
    let running = host.plugin_scan_running();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!running, |ui| {
            if ui
                .button("Rescan")
                .on_hover_text("Scan new and changed plugins")
                .clicked()
            {
                host.rescan_plugins(false);
            }
            if ui
                .button("Full rescan")
                .on_hover_text("Scan every plugin again, the failed ones included")
                .clicked()
            {
                host.rescan_plugins(true);
            }
        });
        if running && ui.button("Cancel").clicked() {
            host.cancel_plugin_scan();
        }
    });
    if running {
        let (done, total, module) = host.plugin_scan_progress().unwrap_or_default();
        let f = if total == 0 {
            0.0
        } else {
            done as f32 / total as f32
        };
        ui.add(
            egui::ProgressBar::new(f).text(format!("{done} of {total} · {}", file_name(&module))),
        );
        ui.ctx().request_repaint_after_secs(0.2);
    }
}

fn found(ui: &mut egui::Ui, host: &mut dyn Host) {
    let mut all: Vec<PluginDescriptor> = host.plugin_effects();
    all.extend(host.plugin_instruments());
    let effects = all.iter().filter(|d| d.kind == PluginKind::Effect).count();
    egui::CollapsingHeader::new(format!(
        "{} plugin(s): {effects} effect(s), {} instrument(s)",
        all.len(),
        all.len() - effects
    ))
    .id_salt("plugins_found")
    .show(ui, |ui| {
        egui::Grid::new("plugins_found_grid")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                for d in &all {
                    ui.label(&d.plugin.name);
                    ui.label(RichText::new(&d.plugin.vendor).color(theme::MUTED));
                    ui.label(
                        RichText::new(match d.kind {
                            PluginKind::Effect => "effect",
                            PluginKind::Instrument => "instrument",
                        })
                        .size(11.0),
                    );
                    ui.add(
                        egui::Label::new(RichText::new(&d.category).size(11.0).color(theme::MUTED))
                            .truncate(),
                    )
                    .on_hover_text(&d.path);
                    ui.end_row();
                }
            });
    });
}
