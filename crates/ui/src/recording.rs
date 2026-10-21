// SPDX-License-Identifier: GPL-3.0-or-later
//! Recording UI (spec 0.2.0 §7): the input settings window with its
//! meter, the input meter widget and the recovery dialog for takes left
//! unfinished by a crash.

use crate::{Host, RecordingState, Recovery, theme};
use egui::{Rect, RichText, Sense, vec2};
use jdance_audio_api::InputChannels;
use std::path::PathBuf;

/// Meter decay per frame (visual only).
const FALL: f32 = 0.85;

#[derive(Debug, Default)]
pub(crate) struct RecordingPanel {
    meter: (f32, f32),
    /// Monitoring needs a confirmation (feedback warning).
    confirm_monitor: bool,
    problem: Option<String>,
}

impl RecordingPanel {
    /// Reads the input peaks with a visual fall-off.
    pub fn update_meter(&mut self, host: &mut dyn Host) -> (f32, f32) {
        let (l, r) = host.input_meter();
        self.meter = (l.max(self.meter.0 * FALL), r.max(self.meter.1 * FALL));
        self.meter
    }

    /// Returns `false` when closed.
    pub fn window(&mut self, ctx: &egui::Context, host: &mut dyn Host) -> bool {
        let mut open = true;
        let meter = self.update_meter(host);
        egui::Window::new("Recording")
            .id(egui::Id::unique("recording_settings"))
            .default_width(420.0)
            .open(&mut open)
            .show(ctx, |ui| {
                let mut s = host.recording_settings();
                let before = s.clone();
                let devices = host.input_devices();
                egui::Grid::new("recording_grid")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Input device");
                        let current = s
                            .device
                            .as_ref()
                            .and_then(|id| devices.iter().find(|d| &d.id == id))
                            .map_or_else(
                                || {
                                    if s.device.is_some() {
                                        "(not available)".to_owned()
                                    } else {
                                        "None".to_owned()
                                    }
                                },
                                |d| d.name.clone(),
                            );
                        egui::ComboBox::from_id_salt("rec_device")
                            .width(260.0)
                            .truncate()
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut s.device, None, "None");
                                for d in &devices {
                                    let label = if d.is_default {
                                        format!("{} (default)", d.name)
                                    } else {
                                        d.name.clone()
                                    };
                                    ui.selectable_value(&mut s.device, Some(d.id.clone()), label);
                                }
                            });
                        ui.end_row();
                        ui.label("Channels");
                        let count = s
                            .device
                            .as_ref()
                            .and_then(|id| devices.iter().find(|d| &d.id == id))
                            .map_or(0, |d| d.channels);
                        egui::ComboBox::from_id_salt("rec_channels")
                            .width(260.0)
                            .selected_text(channel_label(s.channels))
                            .show_ui(ui, |ui| {
                                for c in 0..count {
                                    let m = InputChannels::Mono(c);
                                    ui.selectable_value(&mut s.channels, m, channel_label(m));
                                }
                                for c in (0..count.saturating_sub(1)).step_by(2) {
                                    let st = InputChannels::Stereo(c);
                                    ui.selectable_value(&mut s.channels, st, channel_label(st));
                                }
                            });
                        ui.end_row();
                        ui.label("Level");
                        input_meter(ui, meter, 260.0);
                        ui.end_row();
                        ui.label("Offset");
                        ui.add(
                            egui::DragValue::new(&mut s.offset_ms)
                                .range(-1000.0..=1000.0)
                                .speed(0.1)
                                .suffix(" ms"),
                        )
                        .on_hover_text(
                            "Moves recorded clips later (positive) or earlier (negative) to line \
                             them up with playback; measure it once with a loopback cable",
                        );
                        ui.end_row();
                        ui.label("Monitoring");
                        let mut monitor = s.monitor;
                        if ui
                            .checkbox(&mut monitor, "Hear the input through the output")
                            .changed()
                        {
                            if monitor {
                                self.confirm_monitor = true;
                            } else {
                                s.monitor = false;
                            }
                        }
                        ui.end_row();
                    });
                if self.confirm_monitor {
                    ui.colored_label(
                        theme::WARN,
                        "With speakers and an open microphone this can feed back loudly. \
                         Use headphones.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Turn monitoring on").clicked() {
                            s.monitor = true;
                            self.confirm_monitor = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_monitor = false;
                        }
                    });
                }
                if s.device.is_some() && s.channels.needed() > 0 {
                    let count = s
                        .device
                        .as_ref()
                        .and_then(|id| devices.iter().find(|d| &d.id == id))
                        .map_or(0, |d| d.channels);
                    if count > 0 && s.channels.needed() > count {
                        s.channels = InputChannels::Mono(0);
                    }
                }
                if s != before {
                    self.problem = host.set_recording_settings(s).err();
                }
                ui.separator();
                match host.recording_state() {
                    RecordingState::NoInput(why) => {
                        ui.colored_label(theme::MUTED, why);
                    }
                    RecordingState::Ready => {
                        ui.label(
                            RichText::new(
                                "Ready: arm a Playlist lane (the ring next to its name), place \
                                 the playhead and press Record.",
                            )
                            .color(theme::MUTED),
                        );
                    }
                    RecordingState::Recording { seconds } => {
                        ui.colored_label(theme::RED, format!("Recording · {seconds:.1} s"));
                    }
                    RecordingState::Finishing => {
                        ui.label("Finishing the file…");
                    }
                }
                if let Some(p) = &self.problem {
                    ui.colored_label(theme::ERROR, p);
                }
                ui.label(
                    RichText::new(
                        "The input runs at the output's sample rate. A separate input device \
                         has its own clock: long takes may drift. Alignment is set by the \
                         offset above, not measured.",
                    )
                    .size(11.0)
                    .color(theme::MUTED),
                );
                ui.ctx().request_repaint_after_secs(0.05);
            });
        open
    }
}

fn channel_label(c: InputChannels) -> String {
    match c {
        InputChannels::Mono(_) => format!("{} (mono)", c.label()),
        InputChannels::Stereo(_) => format!("{} (stereo)", c.label()),
    }
}

/// Two horizontal peak bars; red above -1 dBFS.
pub(crate) fn input_meter(ui: &mut egui::Ui, (l, r): (f32, f32), width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 12.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, theme::FIELD);
    for (k, v) in [l, r].into_iter().enumerate() {
        let db = 20.0 * v.max(1e-6).log10();
        let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let y = rect.top() + 1.0 + k as f32 * 5.5;
        let bar = Rect::from_min_size(
            egui::pos2(rect.left() + 1.0, y),
            vec2((rect.width() - 2.0) * frac, 4.5),
        );
        let color = if db > -1.0 { theme::RED } else { theme::TEAL };
        p.rect_filled(bar, 1.0, color);
    }
}

/// Takes left by a crash: repair and import, keep the file, or delete it.
#[derive(Debug)]
pub(crate) struct RecoveryDialog {
    pub files: Vec<PathBuf>,
    confirm_delete: Option<PathBuf>,
}

impl RecoveryDialog {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            confirm_delete: None,
        }
    }

    /// The choices made this frame.
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<(PathBuf, Recovery)> {
        let mut out = Vec::new();
        egui::Modal::new(egui::Id::unique("recovery_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Unfinished recordings");
            ui.label(
                "jdance stopped while these takes were being written. They can be repaired: \
                 everything that reached the disk is kept.",
            );
            ui.add_space(6.0);
            for f in &self.files {
                let name = f
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                ui.label(RichText::new(name).strong());
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            f.parent()
                                .map_or_else(String::new, |d| d.display().to_string()),
                        )
                        .font(theme::mono(11.0))
                        .color(theme::MUTED),
                    )
                    .truncate(),
                )
                .on_hover_text(f.display().to_string());
                ui.horizontal(|ui| {
                    if ui
                        .button("Recover into project")
                        .on_hover_text("Repair and add to the project's samples")
                        .clicked()
                    {
                        out.push((f.clone(), Recovery::Import));
                    }
                    if ui
                        .button("Keep as file")
                        .on_hover_text("Repair and leave it where it is")
                        .clicked()
                    {
                        out.push((f.clone(), Recovery::KeepFile));
                    }
                    if self.confirm_delete.as_ref() == Some(f) {
                        ui.colored_label(theme::WARN, "Delete for good?");
                        if ui.button("Delete").clicked() {
                            out.push((f.clone(), Recovery::Delete));
                            self.confirm_delete = None;
                        }
                        if ui.button("No").clicked() {
                            self.confirm_delete = None;
                        }
                    } else if ui.button("Delete…").clicked() {
                        self.confirm_delete = Some(f.clone());
                    }
                });
                ui.separator();
            }
            if ui.button("Decide later").clicked() {
                self.files.clear();
            }
        });
        self.files.retain(|f| !out.iter().any(|(p, _)| p == f));
        out
    }
}
