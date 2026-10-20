// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugins window: plugin folders, rescan and blocklist (PLG-020..024).

use crate::Host;
use std::path::PathBuf;

/// Shows the window while `open`; returns whether it stays open.
pub(crate) fn window(ctx: &egui::Context, host: &mut dyn Host) -> bool {
    let mut open = true;
    egui::Window::new("Plugins")
        .open(&mut open)
        .resizable(true)
        .default_width(520.0)
        .show(ctx, |ui| contents(ui, host));
    open
}

fn contents(ui: &mut egui::Ui, host: &mut dyn Host) {
    let effects = host.plugin_effects();
    ui.label(format!("{} VST3 effect(s) found.", effects.len()));
    ui.horizontal(|ui| {
        let running = host.plugin_scan_running();
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
                .on_hover_text("Scan every plugin again, blocklisted ones included")
                .clicked()
            {
                host.rescan_plugins(true);
            }
        });
        if running {
            ui.spinner();
            ui.label("Scanning…");
        }
    });

    ui.separator();
    ui.strong("Extra folders");
    ui.small("Standard VST3 folders are always scanned.");
    let mut paths = host.plugin_paths();
    let mut changed = false;
    let mut remove = None;
    for (i, p) in paths.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(p.display().to_string());
            if ui.small_button("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        paths.remove(i);
        changed = true;
    }
    if ui.button("Add folder…").clicked()
        && let Some(dir) = rfd::FileDialog::new().pick_folder()
        && !paths.contains(&dir)
    {
        paths.push(dir);
        changed = true;
    }
    if changed {
        host.set_plugin_paths(paths);
    }

    let blocked: Vec<(PathBuf, String)> = host.blocked_plugins();
    if !blocked.is_empty() {
        ui.separator();
        ui.strong(format!("Blocklisted ({})", blocked.len()));
        ui.small(
            "These modules crashed, hung or misbehaved while scanning. Full rescan retries them.",
        );
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                for (path, reason) in blocked {
                    ui.label(path.display().to_string());
                    ui.colored_label(ui.visuals().warn_fg_color, reason);
                }
            });
    }
}
