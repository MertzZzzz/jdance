// SPDX-License-Identifier: GPL-3.0-or-later
//! Small custom widgets of the visual system.

use crate::theme;
use egui::{Color32, PointerButton, Pos2, Response, Sense, Stroke, Ui, pos2, vec2};
use std::ops::RangeInclusive;

/// `text` on one line no wider than `max_width`, cut with "…" when it
/// does not fit.
pub(crate) fn fit_galley(
    painter: &egui::Painter,
    text: impl Into<String>,
    font: egui::FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    painter.layout_job(job)
}

/// [`egui::Painter::text`] that stays within `max_width`: a long text is
/// cut with "…" instead of running over its neighbours.
pub(crate) fn fit_text(
    painter: &egui::Painter,
    pos: Pos2,
    anchor: egui::Align2,
    text: impl Into<String>,
    font: egui::FontId,
    color: Color32,
    max_width: f32,
) -> egui::Rect {
    let galley = fit_galley(painter, text, font, color, max_width);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

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

/// The accent ring of a soloed lamp (spec 0.3.0 §7.3).
pub(crate) fn solo_ring(ui: &Ui, lamp: &Response) {
    ui.painter()
        .circle_stroke(lamp.rect.center(), 7.5, Stroke::new(1.5, theme::AMBER));
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

/// Whether a pointer press started inside `area` in the window `ui` is
/// drawn in (spec 0.3.0 §1.3: each window reads its own input).
pub(crate) fn pressed_in(ui: &egui::Ui, area: egui::Rect) -> bool {
    ui.input(|i| {
        i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| area.contains(p))
    })
}

/// Middle button held down inside the visible part of a scroll area:
/// the view follows the mouse both ways (spec 0.3.0 §6.3). Call it inside
/// the scroll area's contents; nested areas each take their own axis.
pub(crate) fn middle_pan(ui: &Ui) {
    let clip = ui.clip_rect();
    let delta = ui.input(|i| {
        let inside = i.pointer.press_origin().is_some_and(|p| clip.contains(p));
        (i.pointer.middle_down() && inside).then(|| i.pointer.delta())
    });
    if let Some(d) = delta.filter(|d| *d != egui::Vec2::ZERO) {
        ui.scroll_with_delta_animation(d, egui::style::ScrollAnimation::none());
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
}

/// While something is dragged near the edge of the visible part of a
/// scroll area, the view moves after the mouse, faster the further out
/// it goes (spec 0.3.0 §6.4). Call it inside the scroll area's contents.
pub(crate) fn edge_scroll(ui: &Ui, dragging: bool) {
    const MARGIN: f32 = 28.0;
    if !dragging {
        return;
    }
    let clip = ui.clip_rect();
    let Some(p) = ui.input(|i| {
        i.pointer
            .primary_down()
            .then(|| i.pointer.latest_pos())
            .flatten()
    }) else {
        return;
    };
    let step = |pos: f32, lo: f32, hi: f32| -> f32 {
        if pos < lo + MARGIN {
            (lo + MARGIN - pos).min(4.0 * MARGIN) * 0.35
        } else if pos > hi - MARGIN {
            -(pos - (hi - MARGIN)).min(4.0 * MARGIN) * 0.35
        } else {
            0.0
        }
    };
    let d = egui::vec2(
        step(p.x, clip.left(), clip.right()),
        step(p.y, clip.top(), clip.bottom()),
    );
    if d != egui::Vec2::ZERO {
        ui.scroll_with_delta_animation(d, egui::style::ScrollAnimation::none());
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod pan_tests {
    use crate::test_ui::Driver;
    use egui::{Event, PointerButton, pos2, vec2};

    fn run(d: &mut Driver, events: Vec<Event>, drag: bool) -> egui::Vec2 {
        let mut offset = egui::Vec2::ZERO;
        d.frame(events, &mut |ui| {
            let o = egui::ScrollArea::both()
                .id_salt("pan")
                .max_width(400.0)
                .max_height(300.0)
                .show(ui, |ui| {
                    super::middle_pan(ui);
                    super::edge_scroll(ui, drag);
                    ui.allocate_space(vec2(3000.0, 3000.0));
                });
            offset = o.state.offset();
        });
        offset
    }

    #[test]
    fn the_middle_button_drags_the_view_and_edges_follow_a_drag() {
        let mut d = Driver::new();
        let middle = |p, pressed| Event::PointerButton {
            pos: p,
            button: PointerButton::Middle,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        run(&mut d, vec![Event::PointerMoved(pos2(200.0, 150.0))], false);
        run(&mut d, vec![middle(pos2(200.0, 150.0), true)], false);
        run(&mut d, vec![Event::PointerMoved(pos2(170.0, 130.0))], false);
        let o = run(&mut d, vec![], false);
        assert_eq!(o, vec2(30.0, 20.0), "the content follows the mouse");
        run(&mut d, vec![middle(pos2(170.0, 130.0), false)], false);
        // A primary drag at the right edge scrolls right.
        let before = run(&mut d, vec![Event::PointerMoved(pos2(200.0, 150.0))], false);
        let press = d.button(pos2(200.0, 150.0), true);
        run(&mut d, vec![press], true);
        run(&mut d, vec![Event::PointerMoved(pos2(398.0, 150.0))], true);
        let after = run(&mut d, vec![], true);
        assert!(after.x > before.x, "{before:?} → {after:?}");
        assert_eq!(after.y, before.y);
    }
}
