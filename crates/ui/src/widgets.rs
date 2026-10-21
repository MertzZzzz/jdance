// SPDX-License-Identifier: GPL-3.0-or-later
//! Small custom widgets of the visual system.

use crate::theme;
use egui::{Color32, PointerButton, Pos2, Response, Sense, Stroke, Ui, pos2, vec2};
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

/// A right-button delete gesture: a press starts it, and while the button
/// is held every point the pointer passes over is reported, including the
/// path between frames (a fast sweep must not skip small objects).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RightSweep {
    last: Option<Pos2>,
}

impl RightSweep {
    pub fn active(&self) -> bool {
        self.last.is_some()
    }

    /// Starts a gesture when the right button went down at a point
    /// `start` accepts. While one runs, returns the points passed over
    /// since the last frame and whether the button was released.
    pub fn update(
        &mut self,
        ui: &Ui,
        start: impl FnOnce(Pos2) -> bool,
    ) -> Option<(Vec<Pos2>, bool)> {
        let (pressed, down, latest, origin) = ui.input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Secondary),
                i.pointer.button_down(PointerButton::Secondary),
                i.pointer.latest_pos(),
                i.pointer.press_origin(),
            )
        });
        if pressed
            && self.last.is_none()
            && let Some(p) = origin.or(latest)
            && start(p)
        {
            self.last = Some(p);
        }
        let from = self.last?;
        let to = latest.unwrap_or(from);
        let steps = (from.distance(to) / 2.0).ceil().max(1.0) as usize;
        let points = (0..=steps)
            .map(|k| from.lerp(to, k as f32 / steps as f32))
            .collect();
        self.last = down.then_some(to);
        Some((points, !down))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, RawInput};

    fn frame(ctx: &egui::Context, events: Vec<Event>, f: &mut impl FnMut(&Ui)) {
        let input = RawInput {
            events,
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| f(ui));
        out.textures_delta.clear();
    }

    fn button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn right_sweep_reports_the_whole_path_until_release() {
        let ctx = egui::Context::default();
        let mut sweep = RightSweep::default();
        let mut seen: Vec<(Vec<Pos2>, bool)> = Vec::new();
        let mut step = |events: Vec<Event>, seen: &mut Vec<_>| {
            frame(&ctx, events, &mut |ui| {
                if let Some(r) = sweep.update(ui, |p| p.x >= 0.0) {
                    seen.push(r);
                }
            });
        };
        let a = pos2(10.0, 10.0);
        let b = pos2(50.0, 10.0);
        step(vec![Event::PointerMoved(a), button(a, true)], &mut seen);
        step(vec![Event::PointerMoved(b)], &mut seen);
        step(vec![button(b, false)], &mut seen);
        step(vec![Event::PointerMoved(a)], &mut seen);
        assert_eq!(seen.len(), 3, "press, move, release; nothing after");
        assert!(!seen[0].1 && !seen[1].1 && seen[2].1);
        // The move frame covers every 2 px from a to b: a 3 px object in
        // between is not skipped.
        let path = &seen[1].0;
        assert_eq!(path.first(), Some(&a));
        assert_eq!(path.last(), Some(&b));
        assert!(path.windows(2).all(|w| w[0].distance(w[1]) <= 2.0));
    }

    #[test]
    fn right_sweep_needs_an_accepted_start() {
        let ctx = egui::Context::default();
        let mut sweep = RightSweep::default();
        let p = pos2(-5.0, 0.0);
        let mut started = false;
        for events in [
            vec![Event::PointerMoved(p), button(p, true)],
            vec![button(p, false)],
        ] {
            frame(&ctx, events, &mut |ui| {
                started |= sweep.update(ui, |p| p.x >= 0.0).is_some();
            });
        }
        assert!(!started);
        assert!(!sweep.active());
    }
}
