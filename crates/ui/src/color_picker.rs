// SPDX-License-Identifier: GPL-3.0-or-later
//! The colour wheel (spec 0.2.5 §6): a wheel of hue sectors and
//! saturation rings with a grey ring and centre, a lightness slider, ten
//! recent colours, random / hex / default buttons, the colour's name, HSL
//! and RGB fields, Reset and Accept. Cells, palette and names are this
//! project's own (`jdance_domain::color`).

use crate::theme;
use egui::{Color32, Key, Pos2, RichText, Sense, Shape, Stroke, Ui, vec2};
use jdance_domain::color::{
    self, Hsl, Rgb, WHEEL_GREYS, WHEEL_RINGS, WHEEL_SECTORS, from_hsl, to_hsl,
};
use std::f32::consts::TAU;

const WHEEL: f32 = 200.0;
/// Radius fractions: grey centre, coloured rings, grey ring.
const CENTRE: f32 = 0.16;
const RINGS_END: f32 = 0.82;

/// Picker state for one target (or one set of targets).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Picker {
    pub current: Rgb,
    /// The colour when the picker opened (Reset).
    pub original: Rgb,
    /// The target's default colour (the D button).
    pub default: Rgb,
    /// Magnet: a click picks a whole cell.
    pub quantize: bool,
    /// Lock: saturation and lightness kept in the safe range.
    pub safe: bool,
    /// Lightness of the wheel cells, 0–100.
    lightness: f32,
    /// The # field while open, and whether its text is invalid.
    hex: Option<(String, bool)>,
    /// Counter for the dice.
    rolls: u64,
}

/// What a frame of the picker did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerEvent {
    None,
    /// The colour changed: show it live.
    Changed,
    Accept,
    Cancel,
}

impl Picker {
    pub(crate) fn new(current: Rgb, default: Rgb, quantize: bool, safe: bool) -> Self {
        Self {
            current,
            original: current,
            default,
            quantize,
            safe,
            lightness: to_hsl(current).l,
            hex: None,
            rolls: 0,
        }
    }

    fn set(&mut self, c: Rgb) -> PickerEvent {
        let c = if self.safe { color::safe(c) } else { c };
        if c == self.current {
            return PickerEvent::None;
        }
        self.current = c;
        PickerEvent::Changed
    }

    /// The colour under wheel point `p` (relative to the centre, in
    /// radii), or `None` outside the wheel.
    fn color_at(&self, p: egui::Vec2) -> Option<Rgb> {
        let r = p.length();
        if r > 1.0 {
            return None;
        }
        let angle = p.y.atan2(p.x).rem_euclid(TAU);
        if r < CENTRE {
            return Some(from_hsl(Hsl {
                h: 0.0,
                s: 0.0,
                l: self.lightness,
            }));
        }
        if r >= RINGS_END {
            let k = ((angle / TAU * WHEEL_GREYS as f32) as usize).min(WHEEL_GREYS - 1);
            return Some(grey(k));
        }
        let rings = WHEEL_RINGS.len();
        let t = (r - CENTRE) / (RINGS_END - CENTRE);
        if self.quantize {
            let sector = ((angle / TAU * WHEEL_SECTORS as f32).round() as usize) % WHEEL_SECTORS;
            let ring = ((t * rings as f32) as usize).min(rings - 1);
            Some(color::wheel_cell(sector, ring, self.lightness))
        } else {
            Some(from_hsl(Hsl {
                h: angle.to_degrees(),
                s: (t * 100.0).clamp(0.0, 100.0),
                l: self.lightness,
            }))
        }
    }

    /// Where the current colour sits on the wheel, in radii.
    fn marker(&self) -> egui::Vec2 {
        let h = to_hsl(self.current);
        if h.s < 1.0 {
            return egui::Vec2::ZERO;
        }
        let r = CENTRE + (RINGS_END - CENTRE) * (h.s / 100.0).clamp(0.0, 1.0) * 0.999;
        let a = h.h.to_radians();
        vec2(a.cos(), a.sin()) * r
    }

    /// Draws the picker; `history` is newest first.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn ui(&mut self, ui: &mut Ui, history: &[Rgb]) -> PickerEvent {
        let mut ev = PickerEvent::None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Circle").color(theme::MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.safe, "Safe")
                    .on_hover_text("Safe colours: calm saturation and lightness. Off: any colour");
                ui.toggle_value(&mut self.quantize, "Snap").on_hover_text(
                    "A click picks a whole cell. Off: hue and saturation follow the pointer",
                );
            });
        });
        ui.horizontal(|ui| {
            // The wheel.
            let (rect, resp) = ui.allocate_exact_size(vec2(WHEEL, WHEEL), Sense::click_and_drag());
            let c = rect.center();
            let radius = WHEEL / 2.0 - 2.0;
            self.paint_wheel(ui, c, radius);
            if (resp.clicked() || resp.dragged())
                && let Some(p) = resp.interact_pointer_pos()
                && let Some(col) = self.color_at((p - c) / radius)
            {
                merge(&mut ev, self.set(col));
            }
            let m = c + self.marker() * radius;
            ui.painter()
                .circle_stroke(m, 6.0, Stroke::new(2.0, Color32::WHITE));
            ui.painter()
                .circle_stroke(m, 7.5, Stroke::new(1.0, Color32::BLACK));
            if resp.hovered() || resp.has_focus() {
                // Arrows: hue by a sector; up and down: lightness.
                let (l, r, u, d) = ui.input(|i| {
                    (
                        i.key_pressed(Key::ArrowLeft),
                        i.key_pressed(Key::ArrowRight),
                        i.key_pressed(Key::ArrowUp),
                        i.key_pressed(Key::ArrowDown),
                    )
                });
                let mut h = to_hsl(self.current);
                let step = 360.0 / WHEEL_SECTORS as f32;
                if l || r {
                    h.h += if r { step } else { -step };
                    merge(&mut ev, self.set(from_hsl(h)));
                }
                if u || d {
                    self.lightness =
                        (self.lightness + if u { 5.0 } else { -5.0 }).clamp(0.0, 100.0);
                    merge(
                        &mut ev,
                        self.set(from_hsl(Hsl {
                            l: self.lightness,
                            ..h
                        })),
                    );
                }
            }
            // Lightness.
            let mut l = self.lightness;
            if ui
                .add(
                    egui::Slider::new(&mut l, 0.0..=100.0)
                        .vertical()
                        .show_value(false),
                )
                .on_hover_text("Lightness")
                .changed()
            {
                self.lightness = l;
                let h = to_hsl(self.current);
                merge(&mut ev, self.set(from_hsl(Hsl { l, ..h })));
            }
        });
        // History and buttons.
        ui.horizontal(|ui| {
            for k in 0..10 {
                let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                match history.get(k) {
                    Some(&h) => {
                        ui.painter().rect_filled(r, 3.0, theme::rgb(h));
                        if resp
                            .on_hover_text(format!("{k}: {}", color::to_hex(h)))
                            .clicked()
                        {
                            merge(&mut ev, self.set(h));
                        }
                    }
                    None => {
                        ui.painter().rect_stroke(
                            r,
                            3.0,
                            Stroke::new(1.0, theme::LINE),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    k.to_string(),
                    egui::FontId::monospace(9.0),
                    Color32::from_white_alpha(140),
                );
            }
            ui.separator();
            if ui
                .button("🎲")
                .on_hover_text("A random safe colour")
                .clicked()
            {
                self.rolls += 1;
                let seed = self.rolls
                    ^ u64::from(u32::from_be_bytes([
                        0,
                        self.current[0],
                        self.current[1],
                        self.current[2],
                    ]));
                merge(&mut ev, self.set(color::random_safe(seed)));
            }
            if ui
                .button("#")
                .on_hover_text("Type a colour: #RGB or #RRGGBB")
                .clicked()
            {
                self.hex = match self.hex {
                    Some(_) => None,
                    None => Some((color::to_hex(self.current), false)),
                };
            }
            if ui
                .button("D")
                .on_hover_text("The default colour of this item")
                .clicked()
            {
                merge(&mut ev, self.set(self.default));
            }
        });
        if let Some((text, bad)) = &mut self.hex {
            let r = ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(100.0)
                    .text_color_opt(bad.then_some(theme::ERROR)),
            );
            if r.changed() {
                match color::parse_hex(text) {
                    Some(c) => {
                        *bad = false;
                        // Typed colours are taken as they are.
                        let safe = std::mem::replace(&mut self.safe, false);
                        merge(&mut ev, self.set(c));
                        self.safe = safe;
                        self.lightness = to_hsl(c).l;
                    }
                    None => *bad = true,
                }
            }
        }
        // Name, hex, and the colour beside its safe version.
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(64.0, 36.0), Sense::hover());
            let (a, b) = r.split_left_right_at_fraction(0.5);
            ui.painter().rect_filled(a, 0.0, theme::rgb(self.current));
            ui.painter()
                .rect_filled(b, 0.0, theme::rgb(color::safe(self.current)));
            ui.vertical(|ui| {
                ui.label(RichText::new(color::color_name(self.current)).strong());
                ui.label(RichText::new(color::to_hex(self.current)).font(theme::mono(12.0)));
            });
        });
        // Numeric fields: applied at once.
        let hsl = to_hsl(self.current);
        let (mut h, mut s, mut l) = (hsl.h.round(), hsl.s.round(), hsl.l.round());
        let mut rgb = self.current.map(i32::from);
        let mut hsl_changed = false;
        let mut rgb_changed = false;
        egui::Grid::new("color_fields")
            .num_columns(6)
            .show(ui, |ui| {
                ui.label("Hue");
                hsl_changed |= ui
                    .add(egui::DragValue::new(&mut h).range(0.0..=360.0))
                    .changed();
                ui.label("Sat");
                hsl_changed |= ui
                    .add(egui::DragValue::new(&mut s).range(0.0..=100.0))
                    .changed();
                ui.label("Lum");
                hsl_changed |= ui
                    .add(egui::DragValue::new(&mut l).range(0.0..=100.0))
                    .changed();
                ui.end_row();
                for (name, v) in ["Red", "Green", "Blue"].iter().zip(rgb.iter_mut()) {
                    ui.label(*name);
                    rgb_changed |= ui.add(egui::DragValue::new(v).range(0..=255)).changed();
                }
                ui.end_row();
            });
        if hsl_changed {
            self.lightness = l;
            merge(&mut ev, self.set(from_hsl(Hsl { h, s, l })));
        } else if rgb_changed {
            let c = rgb.map(|v| v.clamp(0, 255) as u8);
            self.lightness = to_hsl(c).l;
            merge(&mut ev, self.set(c));
        }
        ui.horizontal(|ui| {
            if ui
                .button("Reset")
                .on_hover_text("Back to the colour it had")
                .clicked()
            {
                let safe = std::mem::replace(&mut self.safe, false);
                merge(&mut ev, self.set(self.original));
                self.safe = safe;
            }
            if ui.button("Accept").clicked() {
                ev = PickerEvent::Accept;
            }
        });
        let (enter, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if enter && self.hex.is_none() {
            ev = PickerEvent::Accept;
        }
        if esc {
            ev = PickerEvent::Cancel;
        }
        ev
    }

    fn paint_wheel(&self, ui: &Ui, c: Pos2, radius: f32) {
        let painter = ui.painter();
        let rings = WHEEL_RINGS.len();
        let ring_w = (RINGS_END - CENTRE) / rings as f32;
        const SLICES: usize = 6;
        let at = |a: f32, r: f32| c + vec2(a.cos(), a.sin()) * r * radius;
        for sector in 0..WHEEL_SECTORS {
            let step = TAU / WHEEL_SECTORS as f32;
            let a0 = (sector as f32 - 0.5) * step;
            for ring in 0..rings {
                let (r0, r1) = (
                    CENTRE + ring as f32 * ring_w,
                    CENTRE + (ring + 1) as f32 * ring_w,
                );
                let col = theme::rgb(if self.quantize {
                    color::wheel_cell(sector, ring, self.lightness)
                } else {
                    from_hsl(Hsl {
                        h: sector as f32 * 360.0 / WHEEL_SECTORS as f32,
                        s: (ring as f32 + 0.5) / rings as f32 * 100.0,
                        l: self.lightness,
                    })
                });
                for k in 0..SLICES {
                    let (b0, b1) = (
                        a0 + step * k as f32 / SLICES as f32,
                        a0 + step * (k + 1) as f32 / SLICES as f32,
                    );
                    painter.add(Shape::convex_polygon(
                        vec![at(b0, r0), at(b1, r0), at(b1, r1), at(b0, r1)],
                        col,
                        Stroke::NONE,
                    ));
                }
            }
        }
        // Grey ring and centre.
        for k in 0..WHEEL_GREYS {
            let step = TAU / WHEEL_GREYS as f32;
            let col = theme::rgb(grey(k));
            for s in 0..SLICES {
                let (b0, b1) = (
                    k as f32 * step + step * s as f32 / SLICES as f32,
                    k as f32 * step + step * (s + 1) as f32 / SLICES as f32,
                );
                painter.add(Shape::convex_polygon(
                    vec![
                        at(b0, RINGS_END),
                        at(b1, RINGS_END),
                        at(b1, 1.0),
                        at(b0, 1.0),
                    ],
                    col,
                    Stroke::NONE,
                ));
            }
        }
        painter.circle_filled(
            c,
            CENTRE * radius,
            theme::rgb(from_hsl(Hsl {
                h: 0.0,
                s: 0.0,
                l: self.lightness,
            })),
        );
    }
}

/// Keeps the last event that did something.
fn merge(ev: &mut PickerEvent, e: PickerEvent) {
    if e != PickerEvent::None {
        *ev = e;
    }
}

/// Grey `k` of the outer ring, dark to light.
fn grey(k: usize) -> Rgb {
    from_hsl(Hsl {
        h: 0.0,
        s: 0.0,
        l: 6.0 + k as f32 * 88.0 / (WHEEL_GREYS - 1) as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wheel_maps_points_to_cells_and_greys() {
        let p = Picker::new([128, 128, 128], [1, 2, 3], true, false);
        // Centre: grey at the slider lightness.
        let c = p.color_at(vec2(0.0, 0.0)).unwrap();
        assert_eq!(c[0], c[1]);
        // Right edge of the coloured rings, angle 0: the red sector, the
        // most saturated ring.
        let red = p.color_at(vec2(0.8, 0.0)).unwrap();
        assert_eq!(red, color::wheel_cell(0, 3, p.lightness));
        // Outside: nothing.
        assert!(p.color_at(vec2(1.0, 1.0)).is_none());
        // Outer ring: a grey.
        let g = p.color_at(vec2(0.0, 0.9)).unwrap();
        assert!(g[0] == g[1] && g[1] == g[2]);
    }

    #[test]
    fn the_lock_keeps_colours_safe_and_reset_restores() {
        let mut p = Picker::new([10, 20, 30], [1, 2, 3], false, true);
        assert_eq!(p.set([255, 0, 0]), PickerEvent::Changed);
        let h = to_hsl(p.current);
        assert!(h.s <= color::SAFE_SATURATION.1 + 1.0);
        p.safe = false;
        p.set([255, 0, 0]);
        assert_eq!(p.current, [255, 0, 0], "unlocked: pure colours");
        assert_eq!(p.original, [10, 20, 30]);
    }
}
