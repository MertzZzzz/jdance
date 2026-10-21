// SPDX-License-Identifier: GPL-3.0-or-later
//! Our own tool glyphs (UI-VIS-041): 1.5 px lines on a 16×16 grid inside
//! a 24×24 button, drawn as shapes (the interface fonts have no such
//! symbols).

use crate::theme;
use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Glyph {
    Draw,
    Paint,
    Delete,
    Slice,
    Select,
}

/// A 24×24 tool button; `on` = the active tool.
pub(crate) fn tool_button(ui: &mut Ui, glyph: Glyph, on: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    let (fill, stroke) = if on {
        (theme::BLUE_BG, Stroke::new(1.0, theme::BLUE))
    } else if resp.hovered() {
        (theme::RAISED, Stroke::new(1.0, theme::LINE_STRONG))
    } else {
        (Color32::TRANSPARENT, Stroke::NONE)
    };
    let p = ui.painter();
    p.rect_filled(rect, 5.0, fill);
    p.rect_stroke(rect, 5.0, stroke, StrokeKind::Inside);
    let color = if on || resp.hovered() {
        theme::TEXT
    } else {
        theme::MUTED
    };
    paint(
        ui,
        glyph,
        Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
        color,
    );
    resp.on_hover_text(tip)
}

/// Draws `glyph` into the 16×16 `r`.
pub(crate) fn paint(ui: &Ui, glyph: Glyph, r: Rect, color: Color32) {
    let p = ui.painter();
    let s = Stroke::new(1.5, color);
    let at = |x: f32, y: f32| -> Pos2 { pos2(r.left() + x, r.top() + y) };
    let line = |a: Pos2, b: Pos2| p.line_segment([a, b], s);
    match glyph {
        Glyph::Draw => {
            // Pencil at 45°: body, tip, eraser cap.
            line(at(4.0, 12.0), at(12.0, 4.0));
            line(at(6.0, 14.0), at(14.0, 6.0));
            line(at(12.0, 4.0), at(14.0, 6.0));
            line(at(4.0, 12.0), at(2.0, 14.0));
            line(at(6.0, 14.0), at(2.0, 14.0));
        }
        Glyph::Paint => {
            // Paint roller: drum and handle.
            p.rect_stroke(
                Rect::from_min_max(at(2.0, 2.0), at(12.0, 6.0)),
                1.0,
                s,
                StrokeKind::Middle,
            );
            line(at(12.0, 4.0), at(14.0, 4.0));
            line(at(14.0, 4.0), at(14.0, 9.0));
            line(at(14.0, 9.0), at(8.0, 9.0));
            line(at(8.0, 9.0), at(8.0, 14.0));
        }
        Glyph::Delete => {
            // Eraser: a slanted block over a base line.
            let pts = vec![at(3.0, 10.0), at(9.0, 4.0), at(13.0, 8.0), at(7.0, 14.0)];
            p.add(egui::Shape::closed_line(pts, s));
            line(at(5.0, 8.0), at(11.0, 14.0));
            line(at(7.0, 14.0), at(15.0, 14.0));
        }
        Glyph::Slice => {
            // Scissors: two rings and crossing blades.
            p.circle_stroke(at(4.5, 11.5), 2.5, s);
            p.circle_stroke(at(11.5, 11.5), 2.5, s);
            line(at(6.0, 9.5), at(12.0, 2.0));
            line(at(10.0, 9.5), at(4.0, 2.0));
        }
        Glyph::Select => {
            // Dashed rectangle.
            let rr = Rect::from_min_max(at(2.0, 3.0), at(14.0, 13.0));
            for (a, b) in [
                (rr.left_top(), rr.right_top()),
                (rr.right_top(), rr.right_bottom()),
                (rr.right_bottom(), rr.left_bottom()),
                (rr.left_bottom(), rr.left_top()),
            ] {
                let n = 4;
                for k in 0..n {
                    let t0 = k as f32 / n as f32;
                    let t1 = t0 + 0.5 / n as f32;
                    line(a + (b - a) * t0, a + (b - a) * t1);
                }
            }
        }
    }
}
