// SPDX-License-Identifier: GPL-3.0-or-later
//! Tools > LFO (spec 0.2.0 §18.21): writes an LFO curve into an
//! automation source. Opened from the Piano Roll it offers the existing
//! sources or a new one for a supported target; a new source gets no
//! Playlist clip. Points inside the range are replaced after the preview;
//! points outside stay.

use crate::theme;
use egui::RichText;
use jdance_commands::note_tools::{self as nt, Lfo, LfoShape, Range};
use jdance_domain::{AutomationId, AutomationTarget, Project, TrackId};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LfoTarget {
    Existing(AutomationId),
    New(AutomationTarget),
}

#[derive(Debug, Clone)]
pub(crate) struct LfoDialog {
    pub target: Option<LfoTarget>,
    pub lfo: Lfo,
    pub range: Range,
    /// The channel the Piano Roll shows (its mixer track is offered).
    track: TrackId,
}

#[derive(Debug, Default)]
pub(crate) struct LfoOutput {
    /// Target, points to write and their range.
    pub apply: Option<(LfoTarget, Vec<jdance_domain::AutoPoint>, Range)>,
    pub close: bool,
}

impl LfoDialog {
    pub fn new(range: Range, track: TrackId) -> Self {
        Self {
            target: None,
            lfo: Lfo::default(),
            range,
            track,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        label: &mut dyn FnMut(&AutomationTarget) -> String,
    ) -> LfoOutput {
        let mut out = LfoOutput::default();
        let mut open = true;
        egui::Window::new("LFO")
            .id(egui::Id::unique("lfo_dialog"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Writes curve points into an automation source, not into notes.")
                        .size(11.0)
                        .color(theme::MUTED),
                );
                let name =
                    |t: &Option<LfoTarget>, label: &mut dyn FnMut(&AutomationTarget) -> String| {
                        match t {
                            None => "Choose…".to_owned(),
                            Some(LfoTarget::Existing(id)) => project
                                .automation_lane(*id)
                                .map_or_else(|| "?".into(), |a| a.name.clone()),
                            Some(LfoTarget::New(t)) => format!("New: {}", label(t)),
                        }
                    };
                let current = name(&self.target, label);
                let news = [
                    AutomationTarget::TrackVolume(self.track),
                    AutomationTarget::TrackPan(self.track),
                ];
                egui::Grid::new("lfo_grid")
                    .num_columns(2)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Automation");
                        egui::ComboBox::from_id_salt("lfo_target")
                            .width(240.0)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for a in &project.automation {
                                    let t = Some(LfoTarget::Existing(a.id));
                                    ui.selectable_value(&mut self.target, t, &a.name);
                                }
                                ui.separator();
                                for t in news.iter().filter(|t| project.lane_for(t).is_none()) {
                                    let v = Some(LfoTarget::New(t.clone()));
                                    let text = format!("New: {}", label(t));
                                    ui.selectable_value(&mut self.target, v, text);
                                }
                            });
                        ui.end_row();
                        ui.label("Shape");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.lfo.shape, LfoShape::Sine, "Sine");
                            ui.selectable_value(
                                &mut self.lfo.shape,
                                LfoShape::Triangle,
                                "Triangle",
                            );
                            ui.selectable_value(&mut self.lfo.shape, LfoShape::Square, "Square");
                        });
                        ui.end_row();
                        ui.label("Range");
                        ui.horizontal(|ui| {
                            let mut len = self.range.len();
                            ui.add(
                                egui::DragValue::new(&mut self.range.start)
                                    .range(0..=1 << 24)
                                    .suffix(" from"),
                            );
                            ui.add(
                                egui::DragValue::new(&mut len)
                                    .range(1..=1 << 24)
                                    .suffix(" ticks"),
                            );
                            self.range.end = self.range.start + len;
                        });
                        ui.end_row();
                        ui.label("Phase");
                        ui.add(egui::Slider::new(&mut self.lfo.phase, 0.0..=1.0));
                        ui.end_row();
                        ui.label("Centre start / end");
                        ui.horizontal(|ui| {
                            ui.add(egui::Slider::new(&mut self.lfo.center.0, 0.0..=1.0));
                            ui.add(egui::Slider::new(&mut self.lfo.center.1, 0.0..=1.0));
                        });
                        ui.end_row();
                        ui.label("Amplitude start / end");
                        ui.horizontal(|ui| {
                            ui.add(egui::Slider::new(&mut self.lfo.amplitude.0, 0.0..=0.5));
                            ui.add(egui::Slider::new(&mut self.lfo.amplitude.1, 0.0..=0.5));
                        });
                        ui.end_row();
                        ui.label("Period start / end");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.lfo.period.0)
                                    .range(1..=1 << 20)
                                    .suffix(" ticks"),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.lfo.period.1)
                                    .range(1..=1 << 20)
                                    .suffix(" ticks"),
                            );
                        });
                        ui.end_row();
                        if self.lfo.shape != LfoShape::Square {
                            ui.label("Resolution");
                            ui.add(
                                egui::DragValue::new(&mut self.lfo.resolution)
                                    .range(1..=3840)
                                    .suffix(" ticks"),
                            );
                            ui.end_row();
                        }
                    });
                let points = nt::lfo_points(&self.lfo, self.range);
                match &points {
                    Ok(p) => {
                        let replaced = match &self.target {
                            Some(LfoTarget::Existing(id)) => {
                                project.automation_lane(*id).map_or(0, |a| {
                                    a.points
                                        .iter()
                                        .filter(|p| {
                                            self.range
                                                .contains(p.tick.min(u64::from(u32::MAX)) as u32)
                                        })
                                        .count()
                                })
                            }
                            _ => 0,
                        };
                        ui.label(format!(
                            "{} point(s); {replaced} existing point(s) in the range are replaced",
                            p.len()
                        ));
                    }
                    Err(e) => {
                        ui.colored_label(theme::ERROR, e);
                    }
                }
                ui.horizontal(|ui| {
                    let ok = points.is_ok() && self.target.is_some();
                    if ui.add_enabled(ok, egui::Button::new("Apply")).clicked()
                        && let (Ok(p), Some(t)) = (points, self.target.clone())
                    {
                        out.apply = Some((t, p, self.range));
                        out.close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        out.close = true;
                    }
                    if ui.button("Reset").clicked() {
                        self.lfo = Lfo::default();
                    }
                });
            });
        if !open {
            out.close = true;
        }
        out
    }
}
