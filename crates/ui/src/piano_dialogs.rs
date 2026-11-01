// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano roll dialogs (spec "DAW UI clean room", §6): Transpose, Scale
//! helper and Note properties. Each works on the selection (or every note when nothing is selected) and applies as one
//! undo step.

use crate::piano::key_name;
use crate::theme;
use crate::toolbar::bars_text;
use egui::{RichText, vec2};
use jdance_commands::notes::{self, Scale, ScaleMode};
use jdance_domain::{MAX_PATTERN_STEPS, Note, TICKS_PER_STEP};

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

#[derive(Debug, Clone)]
pub(crate) enum Dialog {
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
    /// Slide notes (spec 0.3.5 §6.3).
    pub slide: Option<bool>,
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
            slide: same(notes.iter().map(|n| n.slide)),
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
            if let Some(v) = self.slide {
                n.slide = v;
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

impl Dialog {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Transpose { .. } => "Transpose",
            Self::Scale { .. } => "Scale",
            Self::Properties(_) => "Note properties",
        }
    }

    /// `selected` are indices into `notes`; empty = every note.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        notes: &[Note],
        selected: &[usize],
        sig: jdance_domain::TimeSignature,
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
                // Notes the Fit button moves (UI-RD §10.8).
                let mut outside = 0;
                if let Self::Scale { scale, highlight } = self {
                    ui.set_width(330.0);
                    crate::widgets::caption(ui, "Tonic");
                    egui::Grid::new("scale_tonic")
                        .num_columns(6)
                        .spacing([5.0, 5.0])
                        .show(ui, |ui| {
                            for (i, n) in NAMES.iter().enumerate() {
                                if scale_tile(ui, n, scale.tonic == i as u8, vec2(50.0, 28.0))
                                    .clicked()
                                {
                                    scale.tonic = i as u8;
                                }
                                if i % 6 == 5 {
                                    ui.end_row();
                                }
                            }
                        });
                    ui.add_space(6.0);
                    crate::widgets::caption(ui, "Mode");
                    for m in ScaleMode::ALL {
                        if mode_row(ui, m, scale.mode == m).clicked() {
                            scale.mode = m;
                        }
                    }
                    ui.add_space(6.0);
                    if crate::widgets::switch_row(
                        ui,
                        highlight,
                        theme::SCALE_UI,
                        "Highlight keys of the scale",
                    )
                    .changed()
                    {
                        out.highlight = Some(highlight.then_some(*scale));
                    }
                    if *highlight {
                        out.highlight = Some(Some(*scale));
                    }
                    outside = notes
                        .iter()
                        .enumerate()
                        .filter(|(i, n)| {
                            (selected.is_empty() || selected.contains(i)) && !scale.contains(n.key)
                        })
                        .count();
                    apply = Some(notes::fit_to_scale(notes, selected, *scale));
                }
                egui::Grid::new("piano_dialog_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| match self {
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
                            // UI-RD §5 (board R5 · Scale): tonic tiles, the
                            // modes with their steps, the highlight switch.
                            ui.end_row();
                            let _ = (scale, highlight);
                        }
                        Self::Properties(p) => {
                            props_ui(ui, p, sig);
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
                    let enabled = apply.as_deref().is_some_and(|a| a != notes) && count > 0;
                    if matches!(self, Self::Scale { .. }) {
                        // Fit with the count of notes it moves; Close is
                        // the main button (the dialog stays open).
                        // The count in `OUT_OF_SCALE` red (UI-RD §1.1).
                        let mut text = egui::text::LayoutJob::default();
                        let font = egui::FontId::proportional(12.0);
                        text.append(
                            "Fit notes to scale",
                            0.0,
                            egui::TextFormat::simple(font.clone(), theme::TEXT),
                        );
                        if outside > 0 {
                            text.append(
                                &outside.to_string(),
                                8.0,
                                egui::TextFormat::simple(theme::mono(11.5), theme::ERROR),
                            );
                        }
                        if ui
                            .add_enabled(
                                enabled,
                                egui::Button::new(text).min_size(vec2(200.0, 28.0)),
                            )
                            .on_hover_text("Moves the notes outside the scale to its nearest key")
                            .on_disabled_hover_text("Every note is in the scale")
                            .clicked()
                        {
                            out.notes = apply.take();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::widgets::primary_button(ui, "Close", true).clicked() {
                                out.close = true;
                            }
                        });
                        return;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::widgets::primary_button(ui, "Apply", enabled).clicked() {
                            out.notes = apply.take();
                            out.close = true;
                        }
                        if crate::widgets::outline_button(ui, "Cancel").clicked() {
                            out.close = true;
                        }
                    });
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

fn props_ui(ui: &mut egui::Ui, p: &mut Props, sig: jdance_domain::TimeSignature) {
    opt_field(ui, "Start", &mut p.start, 0, |ui, v| {
        ui.add(
            egui::DragValue::new(v)
                .range(0..=MAX_PATTERN_STEPS * TICKS_PER_STEP)
                .custom_formatter(|t, _| bars_text(t as u64, sig)),
        )
        .on_hover_text("Bar:beat:tick (960 ticks per quarter note); drag or type ticks")
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
    opt_field(ui, "Slide", &mut p.slide, true, |ui, v| {
        ui.checkbox(v, "")
            .on_hover_text("A slide note does not sound: it glides the notes under it to its key")
    });
}

/// A tonic tile: selected = `SCALE_UI` outline.
fn scale_tile(ui: &mut egui::Ui, text: &str, on: bool, size: egui::Vec2) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let (fill, stroke) = if on {
        (theme::SCALE_UI.gamma_multiply(0.15), theme::SCALE_UI)
    } else if resp.hovered() {
        (theme::RAISED, theme::LINE_STRONG)
    } else {
        (theme::FIELD, theme::LINE)
    };
    let p = ui.painter();
    p.rect(
        rect,
        6.0,
        fill,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        theme::mono(11.5),
        if on { theme::TEXT } else { theme::MUTED },
    );
    let name = text.to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &name));
    resp
}

/// A mode row: name and its steps in semitones ("2 1 2 2 1 2 2").
fn mode_row(ui: &mut egui::Ui, m: ScaleMode, on: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 22.0), egui::Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 4.0, theme::SCALE_UI.gamma_multiply(0.14));
        p.rect_filled(
            egui::Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
            1.5,
            theme::SCALE_UI,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, 4.0, theme::RAISED);
    }
    let fg = if on { theme::TEXT } else { theme::MUTED };
    p.text(
        rect.left_center() + vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        m.name(),
        egui::FontId::proportional(12.0),
        fg,
    );
    let iv = m.intervals();
    let steps: Vec<String> = iv
        .iter()
        .zip(iv.iter().skip(1).chain(std::iter::once(&12)))
        .map(|(a, b)| (b - a).to_string())
        .collect();
    p.text(
        rect.right_center() - vec2(8.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        steps.join(" "),
        theme::mono(10.5),
        fg,
    );
    let name = m.name().to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &name));
    resp
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
            muted: false,
            slide: false,
        }
    }

    #[test]
    fn properties_set_and_keep_the_slide_flag() {
        let slide = Note {
            slide: true,
            ..n(0, 72, 0.8)
        };
        let notes = [n(0, 60, 0.8), slide];
        let mut p = Props::of(&notes);
        assert_eq!(p.slide, None, "mixed");
        assert_eq!(p.apply(&notes, &[0, 1]), notes, "kept while mixed");
        p.slide = Some(false);
        assert!(p.apply(&notes, &[0, 1]).iter().all(|n| !n.slide));
        assert_eq!(Props::of(&[slide]).slide, Some(true));
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
