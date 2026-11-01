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
    if resp.dragged() || resp.hovered() {
        // The status line shows the value while it moves (UI-RD §1.5).
        let text = if lo < 0.0 && hi > 0.0 {
            format!("{:+.0}%", *value / hi.max(-lo) * 100.0)
        } else {
            format!("{:.0}%", (*value - lo) / (hi - lo) * 100.0)
        };
        set_hint(
            ui.ctx(),
            format!("{text} — drag, Shift for fine, double-click to reset"),
        );
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = rect.center();
        let r = size / 2.0 - 2.0;
        let hovered = resp.hovered() || resp.dragged();
        // UI-RD §1.5: a FIELD disc, a LINE_STRONG ring, the value arc.
        let t = ((*value - lo) / (hi - lo)).clamp(0.0, 1.0);
        let angle = |t: f32| (-135.0 + 270.0 * t).to_radians();
        let at = |a: f32, rr: f32| pos2(c.x + rr * a.sin(), c.y - rr * a.cos());
        let track: Vec<_> = (0..=32).map(|k| at(angle(k as f32 / 32.0), r)).collect();
        let arc_w = if size >= 28.0 { 3.0 } else { 2.4 };
        painter.add(egui::Shape::line(
            track,
            Stroke::new(
                arc_w,
                if hovered {
                    theme::LINE_STRONG
                } else {
                    theme::LINE
                },
            ),
        ));
        painter.circle(
            c,
            r - arc_w - 0.5,
            theme::FIELD,
            Stroke::new(1.0, theme::LINE_STRONG),
        );
        let from = if lo < 0.0 && hi > 0.0 {
            (0.0 - lo) / (hi - lo)
        } else {
            0.0
        };
        let (a0, a1) = if t < from { (t, from) } else { (from, t) };
        let steps = ((a1 - a0) * 32.0).ceil().max(1.0) as usize;
        let arc: Vec<_> = (0..=steps)
            .map(|k| at(angle(a0 + (a1 - a0) * k as f32 / steps as f32), r))
            .collect();
        if a1 > a0 {
            painter.add(egui::Shape::line(arc, Stroke::new(arc_w, color)));
        }
        let a = angle(t);
        painter.line_segment(
            [at(a, (r - arc_w) * 0.25), at(a, r - arc_w - 1.5)],
            Stroke::new(1.6, theme::TEXT),
        );
    }
    resp
}

/// A small round on/off lamp (channel mute in the rack).
pub(crate) fn lamp(ui: &mut Ui, on: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(20.0, 26.0), Sense::click());
    led(ui.painter(), rect.center(), 4.5, on);
    resp
}

/// A lamp dot (UI-RD §1.5): `LED_ON` with a halo, or `LED_OFF`.
pub(crate) fn led(painter: &egui::Painter, c: Pos2, r: f32, on: bool) {
    if on {
        painter.circle_filled(c, r + 3.0, theme::LED_ON.gamma_multiply(0.12));
        painter.circle_filled(c, r + 1.5, theme::LED_ON.gamma_multiply(0.22));
        painter.circle_filled(c, r, theme::LED_ON);
    } else {
        painter.circle(c, r, theme::LED_OFF, Stroke::new(1.0, theme::LINE_STRONG));
    }
}

/// The accent ring of a soloed lamp (spec 0.3.0 §7.3).
pub(crate) fn solo_ring(ui: &Ui, lamp: &Response) {
    ui.painter()
        .circle_stroke(lamp.rect.center(), 7.0, Stroke::new(1.5, theme::AMBER));
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
            offset = o.state.clamped_offset();
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

// ---------------------------------------------------------------------
// UI-RD §1.5: shared components of the 2026 redesign.
// ---------------------------------------------------------------------

use crate::icons::{self, Glyph};
use egui::{Align2, CornerRadius, FontId, Rect, StrokeKind};

const HINT_ID: &str = "jdance-status-hint";

/// Puts `text` into the status line's hint (UI-RD §1.5) for this frame.
pub(crate) fn set_hint(ctx: &egui::Context, text: impl Into<String>) {
    let text = text.into();
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique(HINT_ID), text));
}

/// The hint while `resp` is hovered.
pub(crate) fn hint_on_hover(resp: &Response, text: &str) {
    if resp.hovered() && !text.is_empty() {
        set_hint(&resp.ctx, text);
    }
}

/// Takes the hint set since the last call (the status line reads it once
/// per frame, after the panels).
pub(crate) fn take_hint(ctx: &egui::Context) -> Option<String> {
    ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::unique(HINT_ID)))
}

/// `Response::on_hover_text` that also fills the status hint.
pub(crate) trait Hint {
    fn hint(self, text: &str) -> Self;
}

impl Hint for Response {
    fn hint(self, text: &str) -> Self {
        hint_on_hover(&self, text);
        self.on_hover_text(text)
    }
}

/// A section caption: Sans 9.5, upper case, 1 px spacing, `DIM`.
pub(crate) fn caption(ui: &mut Ui, text: &str) -> Response {
    ui.label(caption_text(text))
}

pub(crate) fn caption_text(text: &str) -> egui::RichText {
    egui::RichText::new(spaced(&text.to_uppercase()))
        .size(9.5)
        .color(theme::DIM)
}

/// Letter spacing for captions: a thin space between letters is too wide
/// in Plex, so a hair space is used.
fn spaced(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 4);
    for (i, ch) in text.chars().enumerate() {
        if i > 0 && ch != ' ' {
            out.push('\u{200A}');
        }
        out.push(ch);
    }
    out
}

/// A segmented group: `FIELD` ground, `LINE` border, radius 8.
pub(crate) fn segmented<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::FIELD)
        .stroke(Stroke::new(1.0, theme::LINE))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(add).inner
        })
        .inner
}

/// Style of a button inside a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    /// Selected segment: `RAISED`.
    Raised,
    /// Selected toggle: `BLUE_BG` with a `BLUE` border.
    Blue,
    /// The accent fill (Play, the main action).
    Amber,
}

/// A button with an optional glyph and text; `on` selects it with `look`.
/// Its accessible name is `text`, or `tip` for a glyph alone.
pub(crate) fn button(
    ui: &mut Ui,
    glyph: Option<Glyph>,
    text: &str,
    on: bool,
    look: Look,
    tip: &str,
) -> Response {
    let font = FontId::proportional(11.5);
    let galley = (!text.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT)
    });
    let icon = 14.0;
    let pad = if text.is_empty() { 7.0 } else { 9.0 };
    let gap = if glyph.is_some() && galley.is_some() {
        6.0
    } else {
        0.0
    };
    let w =
        pad * 2.0 + glyph.map_or(0.0, |_| icon) + gap + galley.as_ref().map_or(0.0, |g| g.size().x);
    let h = ui.spacing().interact_size.y.max(26.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w.max(h), h), Sense::click());
    let enabled = ui.is_enabled();
    let (fill, stroke, fg) = if on {
        match look {
            Look::Raised => (theme::RAISED, Stroke::NONE, theme::TEXT),
            Look::Blue => (theme::BLUE_BG, Stroke::new(1.0, theme::BLUE), theme::TEXT),
            Look::Amber => (theme::AMBER, Stroke::NONE, theme::ON_AMBER),
        }
    } else if resp.hovered() && enabled {
        (theme::RAISED, Stroke::NONE, theme::TEXT)
    } else {
        (Color32::TRANSPARENT, Stroke::NONE, theme::MUTED)
    };
    let fg = if enabled {
        fg
    } else {
        theme::DIM.gamma_multiply(0.6)
    };
    let p = ui.painter();
    p.rect(rect, 6.0, fill, stroke, StrokeKind::Inside);
    let content_w = w - pad * 2.0;
    let mut x = rect.center().x - content_w / 2.0;
    if let Some(g) = glyph {
        icons::paint_with(
            p,
            g,
            Rect::from_center_size(pos2(x + icon / 2.0, rect.center().y), vec2(icon, icon)),
            fg,
        );
        x += icon + gap;
    }
    if let Some(galley) = galley {
        let pos = pos2(x, rect.center().y - galley.size().y / 2.0);
        p.galley_with_override_text_color(pos, galley, fg);
    }
    let name = if text.is_empty() { tip } else { text };
    let name = name.to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, on, &name));
    if tip.is_empty() { resp } else { resp.hint(tip) }
}

/// A transport button, 36×30, inside the transport group: `on` fills it
/// with `accent` (Play amber, Record red).
pub(crate) fn transport_button(
    ui: &mut Ui,
    glyph: Glyph,
    on: bool,
    accent: Color32,
    name: &str,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 30.0), Sense::click());
    // Transport buttons never keep the keyboard focus (spec 0.3.0 §4).
    if resp.clicked() || resp.has_focus() {
        resp.surrender_focus();
    }
    let p = ui.painter();
    let fill = if on {
        accent
    } else if resp.hovered() {
        theme::RAISED
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, 6.0, fill);
    let color = if on {
        theme::ON_AMBER
    } else if glyph == Glyph::Record {
        theme::RED
    } else {
        theme::TEXT
    };
    let size = if glyph == Glyph::Play { 13.0 } else { 11.0 };
    icons::paint_with(
        p,
        glyph,
        Rect::from_center_size(rect.center(), vec2(size, size)),
        color,
    );
    let name = name.to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &name));
    resp
}

/// A window toggle of the transport bar: glyph over its key.
pub(crate) fn window_toggle(ui: &mut Ui, glyph: Glyph, key: &str, on: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(38.0, 32.0), Sense::click());
    let (fill, stroke) = if on {
        (theme::BLUE_BG, Stroke::new(1.0, theme::BLUE))
    } else if resp.hovered() {
        (theme::RAISED, Stroke::NONE)
    } else {
        (Color32::TRANSPARENT, Stroke::NONE)
    };
    let p = ui.painter();
    p.rect(rect, 6.0, fill, stroke, StrokeKind::Inside);
    let color = if on {
        Color32::from_rgb(0xBB, 0xD9, 0xFF)
    } else if resp.hovered() {
        theme::TEXT
    } else {
        theme::DIM
    };
    icons::paint_with(
        p,
        glyph,
        Rect::from_center_size(pos2(rect.center().x, rect.top() + 11.0), vec2(14.0, 14.0)),
        color,
    );
    p.text(
        pos2(rect.center().x, rect.bottom() - 2.0),
        Align2::CENTER_BOTTOM,
        key,
        FontId::proportional(8.5),
        color,
    );
    let name = tip.split(" (").next().unwrap_or(tip).to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &name));
    resp.hint(tip)
}

/// A raised field-like button: glyph, text and a ▾ — opens a list.
pub(crate) fn drop_button(
    ui: &mut Ui,
    glyph: Option<(Glyph, Color32)>,
    text: &str,
    tip: &str,
) -> Response {
    let font = FontId::proportional(11.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, theme::TEXT);
    let w = 8.0 + glyph.map_or(0.0, |_| 20.0) + galley.size().x + 22.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
    let p = ui.painter();
    let border = if resp.hovered() {
        theme::MUTED
    } else {
        theme::LINE_STRONG
    };
    p.rect(
        rect,
        6.0,
        theme::RAISED,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 8.0;
    if let Some((g, c)) = glyph {
        icons::paint_with(
            p,
            g,
            Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(14.0, 14.0)),
            c,
        );
        x += 20.0;
    }
    p.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT,
    );
    icons::paint_with(
        p,
        Glyph::ChevronDown,
        Rect::from_center_size(pos2(rect.right() - 11.0, rect.center().y), vec2(10.0, 10.0)),
        theme::MUTED,
    );
    let name = text.to_owned();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    resp.hint(tip)
}

/// A chip: height 22, fully rounded; on: `BLUE_BG` + `BLUE` border.
pub(crate) fn chip(ui: &mut Ui, glyph: Option<Glyph>, text: &str, on: bool) -> Response {
    let font = FontId::proportional(11.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, theme::TEXT);
    let w = 20.0 + glyph.map_or(0.0, |_| 17.0) + galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
    let (fill, stroke, fg) = if on {
        (theme::BLUE_BG, Stroke::new(1.0, theme::BLUE), theme::TEXT)
    } else if resp.hovered() {
        (
            theme::RAISED,
            Stroke::new(1.0, theme::LINE_STRONG),
            theme::TEXT,
        )
    } else {
        (theme::FIELD, Stroke::new(1.0, theme::LINE), theme::MUTED)
    };
    let p = ui.painter();
    p.rect(rect, 11.0, fill, stroke, StrokeKind::Inside);
    let mut x = rect.left() + 10.0;
    if let Some(g) = glyph {
        icons::paint_with(
            p,
            g,
            Rect::from_center_size(pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0)),
            fg,
        );
        x += 17.0;
    }
    p.galley_with_override_text_color(pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    let name = text.to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &name));
    resp
}

/// A 28×16 switch; `color` when on.
pub(crate) fn switch(ui: &mut Ui, on: &mut bool, color: Color32, label: &str) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(28.0, 16.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let p = ui.painter();
    let fill = if *on { color } else { theme::RAISED };
    p.rect(
        rect,
        8.0,
        fill,
        Stroke::new(1.0, if *on { color } else { theme::LINE_STRONG }),
        StrokeKind::Inside,
    );
    let x = egui::lerp(rect.left() + 8.0..=rect.right() - 8.0, t);
    p.circle_filled(
        pos2(x, rect.center().y),
        5.0,
        if *on { theme::ON_AMBER } else { theme::MUTED },
    );
    let (name, value) = (label.to_owned(), *on);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, value, &name));
    resp
}

/// A switch with its label to the right.
pub(crate) fn switch_row(ui: &mut Ui, on: &mut bool, color: Color32, label: &str) -> Response {
    ui.horizontal(|ui| {
        let r = switch(ui, on, color, label);
        let l = ui.label(egui::RichText::new(label).color(if *on {
            theme::TEXT
        } else {
            theme::MUTED
        }));
        if l.interact(Sense::click()).clicked() {
            *on = !*on;
            let mut r = r;
            r.mark_changed();
            return r;
        }
        r
    })
    .inner
}

/// A key badge: Mono 9.5 on #171A1E, border #2F353D, radius 4.
pub(crate) fn kbd(ui: &mut Ui, keys: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(keys.to_owned(), theme::mono(9.5), theme::MUTED);
    let size = galley.size() + vec2(10.0, 5.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    paint_kbd(ui.painter(), rect, galley);
    resp
}

fn paint_kbd(p: &egui::Painter, rect: Rect, galley: std::sync::Arc<egui::Galley>) {
    p.rect(
        rect,
        4.0,
        theme::KBD_BG,
        Stroke::new(1.0, theme::KBD_LINE),
        StrokeKind::Inside,
    );
    p.galley(rect.center() - galley.size() / 2.0, galley, theme::MUTED);
}

/// A menu entry with a glyph on the left and a key badge on the right.
pub(crate) fn menu_item(ui: &mut Ui, glyph: Option<Glyph>, text: &str, keys: &str) -> Response {
    menu_item_on(ui, glyph, text, keys, true)
}

pub(crate) fn menu_item_on(
    ui: &mut Ui,
    glyph: Option<Glyph>,
    text: &str,
    keys: &str,
    enabled: bool,
) -> Response {
    let font = FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, theme::TEXT);
    let kg = (!keys.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(keys.to_owned(), theme::mono(9.5), theme::MUTED)
    });
    let kw = kg.as_ref().map_or(0.0, |g| g.size().x + 10.0);
    let min_w = 28.0 + galley.size().x + 18.0 + kw + 8.0;
    let w = ui.available_width().max(min_w).max(200.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), sense);
    let p = ui.painter();
    if resp.hovered() && enabled {
        p.rect_filled(rect, 5.0, theme::RAISED);
    }
    let fg = if enabled { theme::TEXT } else { theme::DIM };
    if let Some(g) = glyph {
        icons::paint_with(
            p,
            g,
            Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(14.0, 14.0)),
            if enabled { theme::MUTED } else { theme::DIM },
        );
    }
    p.galley_with_override_text_color(
        pos2(rect.left() + 28.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    if let Some(kg) = kg {
        let size = kg.size() + vec2(10.0, 5.0);
        let r = Rect::from_min_size(
            pos2(rect.right() - 6.0 - size.x, rect.center().y - size.y / 2.0),
            size,
        );
        paint_kbd(p, r, kg);
    }
    let name = text.to_owned();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MenuItem, enabled, &name));
    if resp.clicked() {
        ui.close();
    }
    resp
}

/// A section card (UI-RD §1.3): `CARD` ground, `CARD_LINE` border, radius
/// 9, padding 10; the title is a 12 px glyph and a caption.
pub(crate) fn card<R>(
    ui: &mut Ui,
    glyph: Option<Glyph>,
    title: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::CARD_LINE))
        .corner_radius(9)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            if !title.is_empty() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if let Some(g) = glyph {
                        let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                        icons::paint(ui, g, r, theme::DIM);
                    }
                    caption(ui, title);
                });
                ui.add_space(2.0);
            }
            add(ui)
        })
        .inner
}

/// What the buttons of a panel header asked for.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeaderOut {
    pub undock: bool,
    pub close: bool,
}

/// Which window buttons a panel header shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum WindowButtons {
    #[default]
    None,
    Close,
    /// Undock (or Dock when `true`) and Close.
    UndockClose(bool),
}

/// A panel header, 32 px (UI-RD §1.5): glyph, title, " › " context, the
/// panel's own controls (`extras`), and on the right Undock and Close.
pub(crate) fn panel_header(
    ui: &mut Ui,
    glyph: Glyph,
    title: &str,
    context: &[&str],
    buttons: WindowButtons,
    extras: impl FnOnce(&mut Ui),
) -> HeaderOut {
    let mut out = HeaderOut::default();
    let w = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(w, 32.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(vec2(w, 32.0));
            ui.spacing_mut().item_spacing.x = 6.0;
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            icons::paint(ui, glyph, r, theme::MUTED);
            ui.label(egui::RichText::new(title).font(theme::strong(12.5)));
            for c in context {
                ui.label(egui::RichText::new("›").color(theme::DIM));
                ui.label(egui::RichText::new(*c).color(theme::MUTED));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let small = vec2(24.0, 24.0);
                match buttons {
                    WindowButtons::None => {}
                    WindowButtons::Close => {
                        out.close =
                            icons::icon_button_sized(ui, Glyph::Close, false, "Close", small, 13.0)
                                .clicked();
                    }
                    WindowButtons::UndockClose(docked_back) => {
                        out.close =
                            icons::icon_button_sized(ui, Glyph::Close, false, "Close", small, 13.0)
                                .clicked();
                        let (g, tip) = if docked_back {
                            (Glyph::Dock, "Dock back into the main window")
                        } else {
                            (Glyph::Undock, "Undock into its own window")
                        };
                        out.undock =
                            icons::icon_button_sized(ui, g, false, tip, small, 13.0).clicked();
                    }
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    extras(ui);
                });
            });
        },
    );
    let y = ui.min_rect().top() + 32.0;
    let r = ui.max_rect();
    ui.painter()
        .hline(r.left()..=r.right(), y, Stroke::new(1.0, theme::LINE));
    out
}

/// The caret (UI-RD §1.5): an amber triangle on the ruler and a dashed
/// amber line through the grid.
pub(crate) fn caret(painter: &egui::Painter, x: f32, ruler: Rect, grid: Rect) {
    let x = x.round() + 0.5;
    let y = ruler.bottom();
    let tri = vec![
        pos2(x - 5.0, y - 9.0),
        pos2(x + 5.0, y - 9.0),
        pos2(x, y - 1.0),
    ];
    painter.add(egui::Shape::convex_polygon(tri, theme::AMBER, Stroke::NONE));
    let mut t = grid.top();
    while t < grid.bottom() {
        let b = (t + 4.0).min(grid.bottom());
        painter.line_segment([pos2(x, t), pos2(x, b)], Stroke::new(1.0, theme::AMBER));
        t += 7.0;
    }
}

/// The playhead: a solid 2 px line.
pub(crate) fn playhead(painter: &egui::Painter, x: f32, top: f32, bottom: f32) {
    painter.line_segment(
        [pos2(x, top), pos2(x, bottom)],
        Stroke::new(2.0, theme::PLAYHEAD),
    );
}

/// A label cell of the position display: caption above a mono value.
pub(crate) fn lcd_cell(
    painter: &egui::Painter,
    rect: Rect,
    caption: &str,
    value: &str,
    color: Color32,
) {
    painter.text(
        pos2(rect.left() + 12.0, rect.top() + 5.0),
        Align2::LEFT_TOP,
        spaced(caption),
        FontId::proportional(9.0),
        theme::DIM,
    );
    painter.text(
        pos2(rect.left() + 12.0, rect.bottom() - 4.0),
        Align2::LEFT_BOTTOM,
        value,
        theme::mono(18.0),
        color,
    );
}

/// A thin vertical divider for toolbars.
pub(crate) fn divider(ui: &mut Ui, height: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(1.0, height), Sense::hover());
    ui.painter()
        .vline(r.center().x, r.y_range(), Stroke::new(1.0, theme::LINE));
}

/// The frame of a dialog body; dialogs keep egui's title bar and use
/// cards inside (UI-RD §7).
pub(crate) fn dialog_buttons(
    ui: &mut Ui,
    primary: &str,
    primary_enabled: bool,
    cancel: Option<&str>,
) -> (bool, bool) {
    let mut ok = false;
    let mut cancelled = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ok = primary_button(ui, primary, primary_enabled).clicked();
        if let Some(c) = cancel {
            cancelled = outline_button(ui, c).clicked();
        }
    });
    (ok, cancelled)
}

/// The main action of a dialog: amber, bottom right.
pub(crate) fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(
            egui::RichText::new(text)
                .color(theme::ON_AMBER)
                .font(theme::strong(12.0)),
        )
        .fill(theme::AMBER)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(6))
        .min_size(vec2(84.0, 28.0)),
    )
}

/// Cancel and other secondary actions: outlined.
pub(crate) fn outline_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(text)
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, theme::LINE_STRONG))
            .corner_radius(CornerRadius::same(6))
            .min_size(vec2(72.0, 28.0)),
    )
}

/// A destructive action: `ERROR`.
pub(crate) fn danger_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(theme::ERROR))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, theme::ERROR.gamma_multiply(0.6)))
            .corner_radius(CornerRadius::same(6))
            .min_size(vec2(72.0, 28.0)),
    )
}

/// A dialog title row: glyph and title (inside `egui::Window` bodies whose
/// own title bar is hidden).
pub(crate) fn dialog_title(ui: &mut Ui, glyph: Glyph, title: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        icons::paint(ui, glyph, r, theme::AMBER);
        ui.label(egui::RichText::new(title).font(theme::strong(13.5)));
    });
}

#[cfg(test)]
mod kit_tests {
    use super::*;
    use crate::test_ui::Driver;

    #[test]
    fn buttons_keep_their_names_and_hints_reach_the_status_line() {
        let mut d = Driver::new();
        let mut clicked = 0;
        let mut ui = |ui: &mut Ui| {
            if button(ui, Some(Glyph::Loop), "", false, Look::Blue, "Loop").clicked() {
                clicked += 1;
            }
        };
        d.frame(vec![], &mut ui);
        let p = d.find("Loop").expect("named by its tip");
        d.click(p, &mut ui);
        assert_eq!(clicked, 1);
        assert_eq!(take_hint(&d.ctx).as_deref(), Some("Loop"));
        assert_eq!(take_hint(&d.ctx), None, "read once");
    }

    #[test]
    fn a_switch_toggles() {
        let mut d = Driver::new();
        let mut on = false;
        let mut ui = |ui: &mut Ui| {
            switch(ui, &mut on, theme::LED_ON, "Thing");
        };
        d.frame(vec![], &mut ui);
        let p = d.find("Thing").unwrap();
        d.click(p, &mut ui);
        assert!(on);
    }
}
