// SPDX-License-Identifier: GPL-3.0-or-later
//! The preset list under a pattern's name field (spec 0.2.5 §9) and the
//! preset editor (Options › Pattern name presets).

use crate::theme;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Key, RichText, Sense, Ui, vec2};
use jdance_domain::color::{Rgb, parse_hex, to_hex};
use jdance_domain::name_presets::{NamePresets, PresetItem};

/// The list's state while the name window is open.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PresetList {
    /// Shown (Esc hides it first, §9 item 3).
    pub open: bool,
    /// Highlighted row: index into the rows of the current filter.
    pub selected: Option<usize>,
}

/// What the list did this frame.
#[derive(Debug, Default)]
pub(crate) struct ListOut {
    /// A preset or recent name chosen: name and colour.
    pub picked: Option<(String, Option<Rgb>)>,
    /// The colour of the row under the pointer, shown on the square.
    pub hover: Option<Rgb>,
}

/// One row of the list.
#[derive(Debug, Clone)]
enum Row {
    Heading(String),
    Preset {
        name: String,
        color: Option<Rgb>,
        range: Option<(usize, usize)>,
    },
    Recent(String),
}

fn rows(presets: &NamePresets, text: &str) -> Vec<Row> {
    let q = text.trim().to_lowercase();
    let mut out: Vec<Row> = presets
        .filter(text)
        .into_iter()
        .map(|m| match &presets.items[m.index] {
            PresetItem::Category { name } => Row::Heading(name.clone()),
            p @ PresetItem::Preset { name, .. } => Row::Preset {
                name: name.clone(),
                color: p.color(),
                range: m.range,
            },
        })
        .collect();
    let recent: Vec<Row> = presets
        .recent
        .iter()
        .filter(|r| q.is_empty() || r.to_lowercase().contains(&q))
        .map(|r| Row::Recent(r.clone()))
        .collect();
    if !recent.is_empty() {
        out.push(Row::Heading("Recent".into()));
        out.extend(recent);
    }
    out
}

/// Keys of the list: Up and Down move, Enter picks. Call before the name
/// field so Enter picks a highlighted row instead of confirming the text.
pub(crate) fn keys(ui: &Ui, presets: &NamePresets, text: &str, list: &mut PresetList) -> ListOut {
    let mut out = ListOut::default();
    if !list.open {
        return out;
    }
    let rows = rows(presets, text);
    let selectable: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| !matches!(r, Row::Heading(_)))
        .map(|(i, _)| i)
        .collect();
    let (up, down) = ui.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
        )
    });
    let pos = list
        .selected
        .and_then(|s| selectable.iter().position(|&i| i == s));
    if down {
        list.selected = match pos {
            Some(p) => selectable.get(p + 1).or(selectable.last()).copied(),
            None => selectable.first().copied(),
        };
    }
    if up {
        list.selected = pos.and_then(|p| p.checked_sub(1)).map(|p| selectable[p]);
    }
    if let Some(s) = list.selected.filter(|s| *s < rows.len())
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter))
    {
        out.picked = pick(&rows[s]);
    }
    out
}

fn pick(r: &Row) -> Option<(String, Option<Rgb>)> {
    match r {
        Row::Preset { name, color, .. } => Some((name.clone(), *color)),
        Row::Recent(name) => Some((name.clone(), None)),
        Row::Heading(_) => None,
    }
}

/// Draws the list under the name field.
pub(crate) fn show(
    ui: &mut Ui,
    presets: &NamePresets,
    text: &str,
    list: &mut PresetList,
) -> ListOut {
    let mut out = ListOut::default();
    if !list.open {
        return out;
    }
    let rows = rows(presets, text);
    if rows.is_empty() {
        return out;
    }
    egui::ScrollArea::vertical()
        .id_salt("name_presets_list")
        .max_height(180.0)
        .show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                match row {
                    Row::Heading(name) => {
                        ui.label(RichText::new(name).strong().color(theme::MUTED).size(11.0));
                    }
                    Row::Preset { name, color, range } => {
                        let resp = row_ui(ui, name, *color, *range, list.selected == Some(i));
                        if resp.hovered() {
                            out.hover = *color;
                        }
                        if resp.clicked() {
                            out.picked = pick(row);
                        }
                    }
                    Row::Recent(name) => {
                        let resp = row_ui(ui, name, None, None, list.selected == Some(i));
                        if resp.clicked() {
                            out.picked = pick(row);
                        }
                    }
                }
            }
        });
    out
}

fn row_ui(
    ui: &mut Ui,
    name: &str,
    color: Option<Rgb>,
    range: Option<(usize, usize)>,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    if selected || resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme::BLUE_BG);
    }
    if let Some(c) = color {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min + vec2(4.0, 4.0), vec2(12.0, 12.0)),
            2.0,
            theme::rgb(c),
        );
    }
    // The matched part highlighted (§9 item 3).
    let mut job = LayoutJob::default();
    let font = FontId::proportional(12.5);
    let plain = TextFormat::simple(font.clone(), theme::TEXT);
    match range.filter(|(a, b)| {
        a < b && *b <= name.len() && name.is_char_boundary(*a) && name.is_char_boundary(*b)
    }) {
        Some((a, b)) => {
            job.append(&name[..a], 0.0, plain.clone());
            job.append(
                &name[a..b],
                0.0,
                TextFormat {
                    background: theme::AMBER.gamma_multiply(0.35),
                    ..plain.clone()
                },
            );
            job.append(&name[b..], 0.0, plain);
        }
        None => job.append(name, 0.0, plain),
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter()
        .galley(rect.min + vec2(22.0, 3.0), galley, Color32::WHITE);
    resp
}

/// The preset editor: rename, recolour, reorder, delete, add presets and
/// categories, restore the factory set (§9 item 11). Returns true when
/// the presets changed.
pub(crate) fn editor(ui: &mut Ui, presets: &mut NamePresets, random_colors: &mut bool) -> bool {
    let mut changed = false;
    changed |= ui
        .checkbox(random_colors, "New patterns get a random safe colour")
        .changed();
    ui.separator();
    let mut remove = None;
    let mut swap = None;
    let n = presets.items.len();
    egui::ScrollArea::vertical()
        .id_salt("name_presets_editor")
        .max_height(360.0)
        .show(ui, |ui| {
            for (i, item) in presets.items.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(i > 0, egui::Button::new("↑").small())
                        .clicked()
                    {
                        swap = Some((i - 1, i));
                    }
                    if ui
                        .add_enabled(i + 1 < n, egui::Button::new("↓").small())
                        .clicked()
                    {
                        swap = Some((i, i + 1));
                    }
                    match item {
                        PresetItem::Category { name } => {
                            changed |= ui
                                .add(
                                    egui::TextEdit::singleline(name)
                                        .desired_width(160.0)
                                        .font(egui::TextStyle::Heading),
                                )
                                .changed();
                            ui.label(RichText::new("category").color(theme::MUTED));
                        }
                        PresetItem::Preset { name, color } => {
                            let c = parse_hex(color).unwrap_or([128, 128, 128]);
                            let (r, resp) =
                                ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                            ui.painter().rect_filled(r, 3.0, theme::rgb(c));
                            if resp.on_hover_text("Click: a random safe colour").clicked() {
                                let seed = (ui.input(|i| i.time) * 1e3) as u64 ^ i as u64;
                                *color = to_hex(jdance_domain::color::random_safe(seed));
                                changed = true;
                            }
                            changed |= ui
                                .add(egui::TextEdit::singleline(name).desired_width(140.0))
                                .changed();
                            let mut hex = color.clone();
                            let bad = parse_hex(&hex).is_none();
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut hex)
                                        .desired_width(70.0)
                                        .text_color_opt(bad.then_some(theme::ERROR)),
                                )
                                .changed()
                            {
                                *color = hex;
                                changed = true;
                            }
                        }
                    }
                    if ui.small_button("×").on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
        });
    if let Some((a, b)) = swap {
        presets.items.swap(a, b);
        changed = true;
    }
    if let Some(i) = remove {
        presets.items.remove(i);
        changed = true;
    }
    ui.horizontal(|ui| {
        if ui.button("Add preset").clicked() {
            presets.items.push(PresetItem::Preset {
                name: "New".into(),
                color: to_hex(jdance_domain::color::DEFAULT_PATTERN_COLOR),
            });
            changed = true;
        }
        if ui.button("Add category").clicked() {
            presets.items.push(PresetItem::Category {
                name: "Category".into(),
            });
            changed = true;
        }
        if ui.button("Restore defaults").clicked() {
            let recent = std::mem::take(&mut presets.recent);
            *presets = NamePresets::factory_patterns();
            presets.recent = recent;
            changed = true;
        }
    });
    changed
}
