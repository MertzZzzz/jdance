// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano roll dialogs (spec "DAW UI clean room", §6): Quantize, Humanize,
//! Transpose, Scale helper and Note properties. Each works on the
//! selection (or every note when nothing is selected) and applies as one
//! undo step.

use crate::piano::key_name;
use crate::theme;
use crate::toolbar::{SNAPS, bars_text};
use egui::RichText;
use jdance_commands::notes::{self, Humanize, Quantize, Scale, ScaleMode};
use jdance_domain::{MAX_PATTERN_STEPS, Note, TICKS_PER_STEP};

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

#[derive(Debug, Clone)]
pub(crate) enum Dialog {
    Quantize(Quantize),
    Humanize(Humanize),
    Transpose { semitones: i32, octaves: i32 },
    Scale { scale: Scale, highlight: bool },
    Properties(Props),
}

/// Fields of the Note properties dialog; `None` = keep (mixed values).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Props {
    pub start: Option<u32>,
    pub length: Option<u32>,
    pub key: Option<u8>,
    pub velocity: Option<f32>,
}

fn same<T: PartialEq + Copy>(mut it: impl Iterator<Item = T>) -> Option<T> {
    let first = it.next()?;
    it.all(|x| x == first).then_some(first)
}

impl Props {
    pub fn of(notes: &[Note]) -> Self {
        Self {
            start: notes.iter().map(|n| n.start).min(),
            length: same(notes.iter().map(|n| n.length)),
            key: same(notes.iter().map(|n| n.key)),
            velocity: same(notes.iter().map(|n| n.velocity)),
        }
    }

    /// The selection with the edited fields; the earliest note moves to
    /// `start` and the rest keep their intervals.
    fn apply(&self, notes: &[Note], selected: &[usize]) -> Vec<Note> {
        let mut out = notes.to_vec();
        let first = selected.iter().map(|&i| notes[i].start).min().unwrap_or(0);
        for &i in selected {
            let n = &mut out[i];
            if let Some(s) = self.start {
                n.start = (s + (notes[i].start - first)).min(MAX_PATTERN_STEPS * TICKS_PER_STEP);
            }
            if let Some(l) = self.length {
                n.length = l.max(1);
            }
            if let Some(k) = self.key {
                n.key = k.min(127);
            }
            if let Some(v) = self.velocity {
                n.velocity = v.clamp(0.0, 1.0);
            }
        }
        out
    }
}

/// What the dialog asks the roll to do this frame.
#[derive(Debug, Default)]
pub(crate) struct DialogOutput {
    /// New note list, applied as one undo step.
    pub notes: Option<Vec<Note>>,
    pub close: bool,
    /// Scale highlighting chosen in the Scale dialog (`Some(None)` = off).
    pub highlight: Option<Option<Scale>>,
}

fn grid_combo(ui: &mut egui::Ui, grid: &mut u32) {
    egui::ComboBox::from_id_salt("q_grid")
        .selected_text(crate::toolbar::snap_name(*grid))
        .show_ui(ui, |ui| {
            for (v, name) in SNAPS.into_iter().filter(|(v, _)| *v > 0) {
                ui.selectable_value(grid, v, name);
            }
        });
}

impl Dialog {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Quantize(_) => "Quantize",
            Self::Humanize(_) => "Humanize",
            Self::Transpose { .. } => "Transpose",
            Self::Scale { .. } => "Scale helper",
            Self::Properties(_) => "Note properties",
        }
    }

    /// `selected` are indices into `notes`; empty = every note.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        notes: &[Note],
        selected: &[usize],
    ) -> DialogOutput {
        let mut out = DialogOutput::default();
        let count = if selected.is_empty() {
            notes.len()
        } else {
            selected.len()
        };
        let mut open = true;
        egui::Window::new(self.title())
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(if selected.is_empty() {
                        format!("All {count} note(s) of this channel")
                    } else {
                        format!("{count} selected note(s)")
                    })
                    .color(theme::MUTED),
                );
                ui.add_space(4.0);
                let mut apply = None;
                let mut error = None;
                egui::Grid::new("piano_dialog_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| match self {
                        Self::Quantize(q) => {
                            ui.label("Grid");
                            grid_combo(ui, &mut q.grid);
                            ui.end_row();
                            ui.label("Strength");
                            ui.add(
                                egui::Slider::new(&mut q.strength, 0.0..=1.0)
                                    .custom_formatter(|v, _| format!("{:.0} %", v * 100.0)),
                            );
                            ui.end_row();
                            ui.label("Apply to");
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut q.starts, "Starts");
                                ui.checkbox(&mut q.ends, "Ends (lengths)");
                            });
                            ui.end_row();
                            apply = Some(notes::quantize(notes, selected, *q));
                        }
                        Self::Humanize(h) => {
                            ui.label("Timing ±");
                            ui.add(
                                egui::DragValue::new(&mut h.time)
                                    .range(0..=TICKS_PER_STEP)
                                    .suffix(" ticks"),
                            );
                            ui.end_row();
                            ui.label("Velocity ±");
                            ui.add(
                                egui::Slider::new(&mut h.velocity, 0.0..=0.5)
                                    .custom_formatter(|v, _| format!("{:.0} %", v * 100.0)),
                            );
                            ui.end_row();
                            ui.label("Seed");
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(&mut h.seed));
                                if ui.button("New").clicked() {
                                    h.seed =
                                        h.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                                }
                            });
                            ui.end_row();
                            apply = Some(notes::humanize(notes, selected, *h));
                        }
                        Self::Transpose { semitones, octaves } => {
                            ui.label("Semitones");
                            ui.add(egui::DragValue::new(semitones).range(-24..=24));
                            ui.end_row();
                            ui.label("Octaves");
                            ui.add(egui::DragValue::new(octaves).range(-10..=10));
                            ui.end_row();
                            match notes::transpose(notes, selected, *semitones + 12 * *octaves) {
                                Some(n) => apply = Some(n),
                                None => error = Some("Some notes would leave the key range C0–G10"),
                            }
                        }
                        Self::Scale { scale, highlight } => {
                            ui.label("Tonic");
                            egui::ComboBox::from_id_salt("tonic")
                                .selected_text(NAMES[usize::from(scale.tonic % 12)])
                                .show_ui(ui, |ui| {
                                    for (i, n) in NAMES.iter().enumerate() {
                                        ui.selectable_value(&mut scale.tonic, i as u8, *n);
                                    }
                                });
                            ui.end_row();
                            ui.label("Mode");
                            egui::ComboBox::from_id_salt("mode")
                                .selected_text(scale.mode.name())
                                .show_ui(ui, |ui| {
                                    for m in ScaleMode::ALL {
                                        ui.selectable_value(&mut scale.mode, m, m.name());
                                    }
                                });
                            ui.end_row();
                            ui.label("Rows");
                            if ui
                                .checkbox(highlight, "Highlight keys of the scale")
                                .changed()
                            {
                                out.highlight = Some(highlight.then_some(*scale));
                            }
                            ui.end_row();
                            ui.label("Notes");
                            let outside = notes
                                .iter()
                                .enumerate()
                                .filter(|(i, n)| {
                                    (selected.is_empty() || selected.contains(i))
                                        && !scale.contains(n.key)
                                })
                                .count();
                            ui.label(format!("{outside} outside the scale"));
                            ui.end_row();
                            if *highlight {
                                out.highlight = Some(Some(*scale));
                            }
                            apply = Some(notes::fit_to_scale(notes, selected, *scale));
                        }
                        Self::Properties(p) => {
                            props_ui(ui, p);
                            let idx: Vec<usize> = if selected.is_empty() {
                                (0..notes.len()).collect()
                            } else {
                                selected.to_vec()
                            };
                            apply = Some(p.apply(notes, &idx));
                        }
                    });
                if let Some(e) = error {
                    ui.colored_label(theme::ERROR, e);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let label = match self {
                        Self::Scale { .. } => "Fit notes to scale",
                        _ => "Apply",
                    };
                    let enabled = apply.as_deref().is_some_and(|a| a != notes) && count > 0;
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                        out.notes = apply.take();
                        if !matches!(self, Self::Scale { .. }) {
                            out.close = true;
                        }
                    }
                    let close = if matches!(self, Self::Scale { .. }) {
                        "Close"
                    } else {
                        "Cancel"
                    };
                    if ui.button(close).clicked() {
                        out.close = true;
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    out.close = true;
                }
            });
        out.close |= !open;
        out
    }
}

fn opt_field<T: Copy>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<T>,
    mixed: T,
    add: impl FnOnce(&mut egui::Ui, &mut T) -> egui::Response,
) {
    ui.label(label);
    ui.horizontal(|ui| match value {
        Some(v) => {
            add(ui, v);
        }
        None => {
            if ui.button("mixed · set…").clicked() {
                *value = Some(mixed);
            }
        }
    });
    ui.end_row();
}

fn props_ui(ui: &mut egui::Ui, p: &mut Props) {
    opt_field(ui, "Start", &mut p.start, 0, |ui, v| {
        ui.add(
            egui::DragValue::new(v)
                .range(0..=MAX_PATTERN_STEPS * TICKS_PER_STEP)
                .custom_formatter(|t, _| bars_text(t as u64)),
        )
        .on_hover_text("Bar:beat:tick (960 ticks per beat); drag or type ticks")
    });
    opt_field(ui, "Length", &mut p.length, TICKS_PER_STEP, |ui, v| {
        ui.add(
            egui::DragValue::new(v)
                .range(1..=MAX_PATTERN_STEPS * TICKS_PER_STEP)
                .suffix(" ticks"),
        )
    });
    opt_field(ui, "Key", &mut p.key, 60, |ui, v| {
        ui.add(
            egui::DragValue::new(v)
                .range(0..=127)
                .custom_formatter(|k, _| key_name(k as u8)),
        )
    });
    opt_field(ui, "Velocity", &mut p.velocity, 0.8, |ui, v| {
        ui.add(egui::Slider::new(v, 0.0..=1.0).fixed_decimals(2))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(start: u32, key: u8, velocity: f32) -> Note {
        Note {
            id: 0,
            start,
            length: 240,
            key,
            velocity,
        }
    }

    #[test]
    fn properties_keep_intervals_and_mixed_values() {
        let notes = [n(480, 60, 0.5), n(960, 64, 0.5), n(0, 70, 0.9)];
        let mut p = Props::of(&[notes[0], notes[1]]);
        assert_eq!(p.start, Some(480));
        assert_eq!(p.key, None, "mixed keys");
        assert_eq!(p.velocity, Some(0.5));
        p.start = Some(0);
        p.velocity = Some(1.0);
        let out = p.apply(&notes, &[0, 1]);
        assert_eq!((out[0].start, out[1].start), (0, 480), "intervals kept");
        assert_eq!((out[0].key, out[1].key), (60, 64), "mixed keys unchanged");
        assert_eq!(out[1].velocity, 1.0);
        assert_eq!(out[2], notes[2]);
    }
}
