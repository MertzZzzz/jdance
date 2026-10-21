// SPDX-License-Identifier: GPL-3.0-or-later
//! Small custom widgets of the visual system.

use crate::theme;
use egui::{Color32, Response, Sense, Stroke, Ui, pos2, vec2};
use std::ops::RangeInclusive;

/// Pixels of vertical drag for the whole range (Shift: 4× finer).
const KNOB_DRAG_PX: f32 = 160.0;

/// Rotary knob: vertical drag changes the value, double-click resets it to
/// `default`. The arc starts at the minimum, or at the centre for ranges
/// around zero (pan).
pub(crate) fn knob(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    default: f32,
    color: Color32,
    size: f32,
) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) {
            4.0
        } else {
            1.0
        };
        let dy = -resp.drag_delta().y;
        let v = (*value + dy * (hi - lo) / (KNOB_DRAG_PX * fine)).clamp(lo, hi);
        if v != *value {
            *value = v;
            resp.mark_changed();
        }
    }
    if resp.double_clicked() && *value != default {
        *value = default;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = rect.center();
        let r = size / 2.0 - 2.0;
        let hovered = resp.hovered() || resp.dragged();
        painter.circle(
            c,
            r,
            theme::RAISED,
            Stroke::new(
                1.0,
                if hovered {
                    theme::MUTED
                } else {
                    theme::LINE_STRONG
                },
            ),
        );
        let t = ((*value - lo) / (hi - lo)).clamp(0.0, 1.0);
        let angle = |t: f32| (-135.0 + 270.0 * t).to_radians();
        let from = if lo < 0.0 && hi > 0.0 {
            (0.0 - lo) / (hi - lo)
        } else {
            0.0
        };
        let (a0, a1) = if t < from { (t, from) } else { (from, t) };
        let steps = ((a1 - a0) * 24.0).ceil().max(1.0) as usize;
        let arc: Vec<_> = (0..=steps)
            .map(|k| {
                let a = angle(a0 + (a1 - a0) * k as f32 / steps as f32);
                pos2(c.x + (r + 1.0) * a.sin(), c.y - (r + 1.0) * a.cos())
            })
            .collect();
        if a1 > a0 {
            painter.add(egui::Shape::line(arc, Stroke::new(2.0, color)));
        }
        let a = angle(t);
        painter.line_segment(
            [
                c,
                pos2(c.x + (r - 2.0) * a.sin(), c.y - (r - 2.0) * a.cos()),
            ],
            Stroke::new(2.0, theme::TEXT),
        );
    }
    resp
}

/// A small round on/off lamp (channel mute in the rack).
pub(crate) fn lamp(ui: &mut Ui, on: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(20.0, 26.0), Sense::click());
    let fill = if on { theme::GREEN } else { theme::RAISED };
    ui.painter().circle(
        rect.center(),
        5.0,
        fill,
        Stroke::new(1.0, Color32::from_rgb(0x4A, 0x52, 0x5C)),
    );
    resp
}
