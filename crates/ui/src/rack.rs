// SPDX-License-Identifier: GPL-3.0-or-later
//! Channel rack with the 16-step sequencer.
//!
//! Drawing collects edits into [`RackOutput`]; the caller applies them after
//! the frame so the document is never borrowed mutably while drawn.

use crate::{Host, SampleStatus};
use egui::{Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, vec2};
use jdance_commands::Edit;
use jdance_domain::{MAX_VOLUME, Project, SampleRef, Step};
use std::path::Path;

const STEP_SIZE: f32 = 22.0;
const NAME_WIDTH: f32 = 110.0;

#[derive(Debug, Default)]
pub(crate) struct RackOutput {
    pub edits: Vec<(Edit, bool)>,
    pub end_gesture: bool,
    /// New channels to create: name and optional sample.
    pub new_channels: Vec<(String, Option<SampleRef>)>,
    pub rename: Option<crate::RenameTarget>,
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    project: &Project,
    host: &dyn Host,
    dir: Option<&Path>,
) -> RackOutput {
    let mut out = RackOutput::default();
    let Some(pattern) = project.active_pattern() else {
        ui.label("No pattern");
        return out;
    };
    let transport = host.transport();
    let current_step = transport
        .loop_pos
        .filter(|_| transport.playing)
        .map(|p| ((p * pattern.length_steps as f32) as u32).min(pattern.length_steps - 1));

    let dropped: Vec<std::path::PathBuf> = ui.ctx().input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect()
    });
    let hovering_files = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let pointer = ui.ctx().input(|i| i.pointer.hover_pos());
    let mut dropped_on_row = false;

    egui::ScrollArea::vertical().show(ui, |ui| {
        for ch in &project.channels {
            let row = ui.horizontal(|ui| {
                let mut muted = ch.muted;
                if ui
                    .toggle_value(&mut muted, "M")
                    .on_hover_text("Mute")
                    .changed()
                {
                    out.edits.push((Edit::SetChannelMuted(ch.id, muted), false));
                }

                let name = ui.add_sized(
                    [NAME_WIDTH, STEP_SIZE],
                    egui::Button::new(egui::RichText::new(&ch.name).strong()).truncate(),
                );
                let sample_status = ch.sample().and_then(|s| host.sample_status(s, dir));
                let hover = match (ch.sample(), &sample_status) {
                    (None, _) => "No sample — drop a WAV file here or right-click".to_owned(),
                    (
                        Some(s),
                        Some(SampleStatus::Ready {
                            frames,
                            source_rate,
                        }),
                    ) => {
                        format!("{}\n{frames} frames, {source_rate} Hz", s.path)
                    }
                    (Some(s), Some(SampleStatus::Failed(e))) => format!("{}\nFailed: {e}", s.path),
                    (Some(s), _) => format!("{}\nLoading…", s.path),
                };
                if name.double_clicked() {
                    out.rename = Some(crate::RenameTarget::Channel(ch.id));
                }
                name.clone().on_hover_text(hover);
                name.context_menu(|ui| {
                    if ui.button("Rename…").clicked() {
                        out.rename = Some(crate::RenameTarget::Channel(ch.id));
                        ui.close();
                    }
                    if ui.button("Load sample…").clicked() {
                        if let Some(p) = pick_wav() {
                            out.edits
                                .push((Edit::SetChannelSample(ch.id, Some(sample_ref(&p))), false));
                        }
                        ui.close();
                    }
                    if ui.button("Clear sample").clicked() {
                        out.edits.push((Edit::SetChannelSample(ch.id, None), false));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Remove channel").clicked() {
                        out.edits.push((Edit::RemoveChannel(ch.id), false));
                        ui.close();
                    }
                });

                // Drawn as a dot: the default fonts lack most symbol glyphs.
                let color = match sample_status {
                    None => ui.visuals().weak_text_color().gamma_multiply(0.4),
                    Some(SampleStatus::Loading) => ui.visuals().weak_text_color(),
                    Some(SampleStatus::Ready { .. }) => Color32::from_rgb(90, 190, 110),
                    Some(SampleStatus::Failed(_)) => ui.visuals().error_fg_color,
                };
                let (dot, _) = ui.allocate_exact_size(vec2(10.0, STEP_SIZE), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, color);

                ui.weak("Vol");
                ui.spacing_mut().slider_width = 70.0;
                let mut vol = ch.volume;
                let r = ui
                    .add(egui::Slider::new(&mut vol, 0.0..=MAX_VOLUME).show_value(false))
                    .on_hover_text(format!("Volume {:.0}%", ch.volume * 100.0));
                if r.changed() {
                    out.edits.push((Edit::SetChannelVolume(ch.id, vol), true));
                }
                out.end_gesture |= r.drag_stopped();

                ui.weak("Pan");
                ui.spacing_mut().slider_width = 50.0;
                let mut pan = ch.pan;
                let r = ui
                    .add(egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false))
                    .on_hover_text(format!(
                        "Pan {:+.0} (double-click to center)",
                        ch.pan * 100.0
                    ));
                if r.changed() {
                    out.edits.push((Edit::SetChannelPan(ch.id, pan), true));
                }
                if r.double_clicked() {
                    out.edits.push((Edit::SetChannelPan(ch.id, 0.0), false));
                }
                out.end_gesture |= r.drag_stopped();

                let track_label = project
                    .mixer
                    .track(ch.track)
                    .map_or("?".to_owned(), |t| t.name.clone());
                egui::ComboBox::from_id_salt(("route", ch.id.0))
                    .width(80.0)
                    .selected_text(track_label)
                    .show_ui(ui, |ui| {
                        for t in project.mixer.all_tracks() {
                            if ui.selectable_label(t.id == ch.track, &t.name).clicked()
                                && t.id != ch.track
                            {
                                out.edits.push((Edit::SetChannelTrack(ch.id, t.id), false));
                            }
                        }
                    })
                    .response
                    .on_hover_text("Mixer track");

                ui.add_space(6.0);
                for i in 0..pattern.length_steps {
                    let step = pattern.step(ch.id, i).unwrap_or_default();
                    if step_button(ui, step, i, current_step == Some(i)).clicked() {
                        out.edits.push((
                            Edit::SetStep {
                                pattern: pattern.id,
                                channel: ch.id,
                                index: i,
                                step: Step {
                                    on: !step.on,
                                    ..step
                                },
                            },
                            false,
                        ));
                    }
                }
            });
            let rect = row.response.rect;
            if hovering_files && pointer.is_some_and(|p| rect.contains(p)) {
                ui.painter().rect_stroke(
                    rect.expand(2.0),
                    CornerRadius::same(3),
                    Stroke::new(2.0, ui.visuals().selection.stroke.color),
                    StrokeKind::Outside,
                );
            }
            if let (Some(p), Some(file)) = (pointer, dropped.first())
                && rect.contains(p)
            {
                out.edits
                    .push((Edit::SetChannelSample(ch.id, Some(sample_ref(file))), false));
                dropped_on_row = true;
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("+ Add channel").clicked() {
                out.new_channels
                    .push((format!("Channel {}", project.channels.len() + 1), None));
            }
            ui.weak(
                "Drop WAV files here to add channels, or onto a channel to replace its sample.",
            );
        });
    });

    if !dropped_on_row {
        for file in dropped {
            let name = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Sample".into());
            out.new_channels.push((name, Some(sample_ref(&file))));
        }
    }
    out
}

fn step_button(ui: &mut egui::Ui, step: Step, index: u32, playhead: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(STEP_SIZE, STEP_SIZE), Sense::click());
    if ui.is_rect_visible(rect) {
        let dark_group = (index / 4) % 2 == 1;
        let visuals = ui.visuals();
        let off = if dark_group {
            visuals.faint_bg_color.gamma_multiply(1.6)
        } else {
            visuals.extreme_bg_color
        };
        let on = Color32::from_rgb(235, 120, 60).gamma_multiply(0.4 + 0.6 * step.velocity);
        let fill = if step.on { on } else { off };
        let inner = Rect::from_center_size(rect.center(), rect.size() - vec2(3.0, 3.0));
        ui.painter().rect_filled(inner, CornerRadius::same(3), fill);
        let stroke = if playhead {
            Stroke::new(2.0, visuals.strong_text_color())
        } else if response.hovered() {
            visuals.widgets.hovered.bg_stroke
        } else {
            visuals.widgets.noninteractive.bg_stroke
        };
        ui.painter()
            .rect_stroke(inner, CornerRadius::same(3), stroke, StrokeKind::Inside);
    }
    response.on_hover_text(format!("Step {}", index + 1))
}

fn sample_ref(path: &Path) -> SampleRef {
    SampleRef {
        path: path.to_string_lossy().into_owned(),
    }
}

fn pick_wav() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("WAV audio", &["wav", "wave"])
        .pick_file()
}
