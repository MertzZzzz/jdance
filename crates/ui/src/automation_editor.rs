// SPDX-License-Identifier: GPL-3.0-or-later
//! Automation Editor (spec 0.2.0 §11 "Editor"): the points of one
//! automation source. Double-click adds a point (or deletes the one under
//! the pointer), drag moves a point. A right click on a point deletes it;
//! holding the right button and moving deletes every point passed over.
//! A right click between points sets the shape of that segment (Step,
//! Linear, Curve). Points snap to the toolbar grid; Alt disables
//! snapping. Every gesture is one undo step.

use crate::automation::{AUTOMATION_COLOR, label, value_text};
use crate::{Host, theme};
use egui::{
    Align2, Color32, FontId, PointerButton, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2,
};
use jdance_commands::Edit;
use jdance_domain::{
    AutoPoint, AutomationId, AutomationLane, Curve, MAX_AUTOMATION_TICKS, Project, WHOLE_NOTE,
};

const RULER_H: f32 = 18.0;
const CANVAS_H: f32 = 200.0;
/// Vertical padding of the envelope inside its row.
pub(crate) const PAD: f32 = 6.0;
const POINT_R: f32 = 4.0;

#[derive(Debug, Default)]
pub(crate) struct EditorOutput {
    pub edits: Vec<Edit>,
    /// Edits of a continuous gesture (merged into one undo step).
    pub gesture: Vec<Edit>,
    pub end_gesture: bool,
}

#[derive(Debug)]
pub(crate) struct AutomationEditor {
    pub source: AutomationId,
    /// Pixels per bar.
    zoom: f32,
    /// Point being dragged and where the drag began.
    drag: Option<(usize, Pos2)>,
    /// Point starting the segment under the last right-click.
    context: Option<usize>,
    /// Right-button delete gesture.
    rmb: crate::widgets::RightSweep,
    name: String,
}

/// The edit replacing `lane`'s points with `f` applied.
pub(crate) fn with_points(lane: &AutomationLane, f: impl FnOnce(&mut Vec<AutoPoint>)) -> Edit {
    let mut lane = lane.clone();
    f(&mut lane.points);
    Edit::SetAutomation {
        id: lane.id,
        lane: Some(lane),
    }
}

/// Envelope polyline of `points` in `row`, between `x0` and `x1` on
/// screen; `x_of` maps a source tick to x.
pub(crate) fn envelope_line(
    points: &[AutoPoint],
    row: Rect,
    x_of: &impl Fn(u64) -> f32,
    (x0, x1): (f32, f32),
) -> Vec<Pos2> {
    let pad = PAD.min(row.height() / 4.0);
    let y_of = |v: f32| row.bottom() - pad - v * (row.height() - 2.0 * pad);
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return Vec::new();
    };
    let mut line = vec![pos2(x0.min(x_of(first.tick)), y_of(first.value))];
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (xa, xb) = (x_of(a.tick), x_of(b.tick));
        line.push(pos2(xa, y_of(a.value)));
        if xb < x0 || xa > x1 {
            continue;
        }
        match a.curve {
            Curve::Hold => line.push(pos2(xb, y_of(a.value))),
            Curve::Linear => {}
            Curve::Curve => {
                let n = (((xb - xa) / 3.0).ceil() as usize).clamp(1, 256);
                for k in 1..n {
                    let t = k as f64 / n as f64;
                    let w = jdance_domain::curve_weight(a.curve, a.tension, t) as f32;
                    line.push(pos2(
                        xa + (xb - xa) * t as f32,
                        y_of(a.value + (b.value - a.value) * w),
                    ));
                }
            }
        }
    }
    line.push(pos2(x_of(last.tick), y_of(last.value)));
    line.push(pos2(x1.max(x_of(last.tick)), y_of(last.value)));
    line
}

fn snap(ticks: i64, grid: u64) -> i64 {
    let g = grid.max(1) as i64;
    (ticks as f64 / g as f64).round() as i64 * g
}

impl AutomationEditor {
    pub fn new(source: AutomationId) -> Self {
        Self {
            source,
            zoom: 160.0,
            drag: None,
            context: None,
            rmb: crate::widgets::RightSweep::default(),
            name: String::new(),
        }
    }

    /// Returns `false` when the window is closed or the source is gone.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        grid: u64,
        host: &mut dyn Host,
        out: &mut EditorOutput,
        layout: &mut crate::dock::Layout,
    ) -> bool {
        let Some(lane) = project.automation_lane(self.source) else {
            return false;
        };
        let title = format!("Automation · {}", lane.name);
        let source = self.source.0;
        crate::dock::editor_window(
            ctx,
            layout,
            crate::dock::Panel::AutomationEditor,
            source,
            &title,
            &mut |ui| {
                self.header(ui, project, lane, host, out);
                ui.separator();
                egui::ScrollArea::horizontal()
                    .id_salt(("automation_editor_h", self.source.0))
                    .show(ui, |ui| self.canvas(ui, project, lane, grid, out));
                ui.label(
                    RichText::new(
                        "Double-click: add or delete a point · drag: move · right-click a point: delete (hold to sweep) · right-click between points: Step / Linear / Curve · Alt: no snap",
                    )
                    .size(11.0)
                    .color(theme::MUTED),
                );
            },
        )
    }

    fn header(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        lane: &AutomationLane,
        host: &mut dyn Host,
        out: &mut EditorOutput,
    ) {
        ui.horizontal(|ui| {
            let name_id = egui::Id::unique(("automation_name", self.source.0));
            let editing = ui.memory(|m| m.has_focus(name_id));
            if !editing {
                self.name.clone_from(&lane.name);
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.name)
                    .id(name_id)
                    .desired_width(150.0),
            );
            if r.lost_focus() && self.name.trim() != lane.name && !self.name.trim().is_empty() {
                out.edits.push(Edit::SetAutomation {
                    id: lane.id,
                    lane: Some(AutomationLane {
                        name: self.name.trim().to_owned(),
                        ..lane.clone()
                    }),
                });
            }
            if lane.target.exists(project) {
                let (name, context) = label(project, &lane.target, host);
                ui.label(RichText::new(name).color(AUTOMATION_COLOR));
                ui.label(RichText::new(context).color(theme::MUTED));
            } else {
                ui.label(RichText::new("Target not found").color(theme::WARN))
                    .on_hover_text("The automated object was removed; the source has no effect");
            }
            ui.separator();
            let bar_ticks = u64::from(project.time_signature.bar_ticks());
            let mut bars = lane.length.div_ceil(bar_ticks).max(1);
            let r = ui
                .add(
                    egui::DragValue::new(&mut bars)
                        .range(1..=MAX_AUTOMATION_TICKS / bar_ticks)
                        .suffix(" bars"),
                )
                .on_hover_text("Source length; clips show at most this range");
            if r.changed() {
                out.gesture.push(Edit::SetAutomation {
                    id: lane.id,
                    lane: Some(AutomationLane {
                        length: bars * bar_ticks,
                        ..lane.clone()
                    }),
                });
            }
            out.end_gesture |= r.drag_stopped() || r.lost_focus();
            ui.separator();
            ui.add(
                egui::Slider::new(&mut self.zoom, 40.0..=640.0)
                    .logarithmic(true)
                    .show_value(false)
                    .text("Zoom"),
            );
        });
    }

    #[allow(clippy::too_many_lines)]
    fn canvas(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        lane: &AutomationLane,
        grid: u64,
        out: &mut EditorOutput,
    ) {
        // `zoom` is pixels per whole note (SIG-003).
        let ppt = self.zoom / WHOLE_NOTE as f32;
        let sig = project.time_signature;
        let bar_ticks = u64::from(sig.bar_ticks());
        let bars = lane.length.div_ceil(bar_ticks) + 1;
        let width = (bars as f32 * bar_ticks as f32 * ppt).max(ui.available_width());
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, RULER_H + CANVAS_H), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let origin = rect.min;
        let row = Rect::from_min_max(pos2(rect.left(), origin.y + RULER_H), rect.max);
        let x_of = |t: u64| origin.x + t as f32 * ppt;
        let tick_at = |x: f32| ((x - origin.x) / ppt).max(0.0) as i64;
        let value_at_y =
            |y: f32| ((row.bottom() - PAD - y) / (row.height() - 2.0 * PAD)).clamp(0.0, 1.0);
        let point_pos = |p: &AutoPoint| {
            pos2(
                x_of(p.tick),
                row.bottom() - PAD - p.value * (row.height() - 2.0 * PAD),
            )
        };
        let hit = |p: Pos2| {
            lane.points
                .iter()
                .rposition(|pt| point_pos(pt).distance(p) <= POINT_R + 3.0)
        };
        let alt = ui.input(|i| i.modifiers.alt);
        let snapped =
            |t: i64| if alt { t } else { snap(t, grid) }.clamp(0, lane.length as i64) as u64;

        // Background: bars, the range past the source end, the ruler.
        painter.rect_filled(row, 0.0, theme::BG);
        let end_x = x_of(lane.length);
        painter.rect_filled(
            Rect::from_min_max(pos2(end_x, row.top()), row.max),
            0.0,
            Color32::from_black_alpha(90),
        );
        painter.rect_filled(
            Rect::from_min_size(origin, vec2(width, RULER_H)),
            0.0,
            theme::PANEL,
        );
        let beat_px = sig.beat_ticks() as f32 * ppt;
        let per_bar = u64::from(sig.numerator);
        for b in 0..=bars * per_bar {
            let x = origin.x + b as f32 * beat_px;
            let bar = b % per_bar == 0;
            if !bar && beat_px < 12.0 {
                continue;
            }
            let c = if bar { theme::LINE_STRONG } else { theme::LINE };
            painter.line_segment(
                [pos2(x, row.top()), pos2(x, row.bottom())],
                Stroke::new(1.0, c),
            );
            if bar {
                painter.text(
                    pos2(x + 3.0, origin.y + 2.0),
                    Align2::LEFT_TOP,
                    format!("{}", b / per_bar + 1),
                    theme::mono(11.0),
                    theme::MUTED,
                );
            }
        }
        for v in [0.0, 0.5, 1.0] {
            let y = row.bottom() - PAD - v * (row.height() - 2.0 * PAD);
            painter.line_segment(
                [pos2(rect.left(), y), pos2(rect.right(), y)],
                Stroke::new(1.0, theme::LINE.gamma_multiply(0.6)),
            );
        }

        // Dragged point, kept between its neighbours.
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let moved = |k: usize, from: Pos2| -> Option<AutoPoint> {
            let p = pointer?;
            let pt = *lane.points.get(k)?;
            let t = snapped(pt.tick as i64 + ((p.x - from.x) / ppt) as i64);
            let lo = k.checked_sub(1).map_or(0, |j| lane.points[j].tick);
            let hi = lane.points.get(k + 1).map_or(u64::MAX, |n| n.tick);
            Some(AutoPoint {
                tick: t.clamp(lo, hi),
                value: value_at_y(point_pos(&pt).y + (p.y - from.y)),
                ..pt
            })
        };
        if response.drag_started_by(PointerButton::Primary)
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            self.drag = hit(p).map(|k| (k, p));
        }
        let preview = self.drag.and_then(|(k, from)| Some((k, moved(k, from)?)));
        if response.drag_stopped_by(PointerButton::Primary)
            && let Some((k, _)) = self.drag.take()
            && let Some((_, pt)) = preview
            && lane.points[k] != pt
        {
            out.edits.push(with_points(lane, |pts| pts[k] = pt));
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
            && row.contains(p)
        {
            out.edits.push(match hit(p) {
                Some(k) => with_points(lane, |pts| {
                    pts.remove(k);
                }),
                None => {
                    let tick = snapped(tick_at(p.x));
                    let at = lane.points.partition_point(|q| q.tick <= tick);
                    // New points take the shape of the segment they split.
                    let (curve, tension) = at.checked_sub(1).map_or((Curve::Linear, 0.0), |j| {
                        (lane.points[j].curve, lane.points[j].tension)
                    });
                    let point = AutoPoint {
                        tick,
                        value: value_at_y(p.y),
                        curve,
                        tension,
                    };
                    with_points(lane, |pts| pts.insert(at, point))
                }
            });
        }
        // Right button: on a point it deletes (and keeps deleting while
        // held); between points it opens the segment's shape menu.
        let visible = ui.clip_rect().intersect(row);
        let mut open_menu = false;
        let rmb_pressed = ui.input(|i| i.pointer.button_pressed(PointerButton::Secondary));
        if rmb_pressed
            && self.drag.is_none()
            && let Some(p) = ui.ctx().pointer_latest_pos()
            && visible.contains(p)
            && hit(p).is_none()
        {
            let tick = tick_at(p.x).max(0) as u64;
            self.context = lane.points.iter().rposition(|q| q.tick <= tick);
            open_menu = true;
        }
        let drag_idle = self.drag.is_none();
        let sweep = self
            .rmb
            .update(ui, |p| drag_idle && visible.contains(p) && hit(p).is_some());
        if let Some((path, ended)) = sweep {
            let near = |q: &AutoPoint| {
                path.iter()
                    .any(|p| visible.contains(*p) && point_pos(q).distance(*p) <= POINT_R + 3.0)
            };
            if lane.points.iter().any(near) {
                let mut points = lane.points.clone();
                points.retain(|q| !near(q));
                out.gesture.push(with_points(lane, |pts| *pts = points));
            }
            out.end_gesture |= ended;
        }
        egui::Popup::context_menu(&response)
            .open_memory(open_menu.then_some(egui::SetOpenCommand::Bool(true)))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| point_menu(ui, project, lane, self.context, out));

        // Envelope and points.
        let shown;
        let points = match preview {
            Some((k, pt)) => {
                let mut v = lane.points.clone();
                v[k] = pt;
                shown = v;
                &shown
            }
            None => &lane.points,
        };
        let color = if lane.target.exists(project) {
            AUTOMATION_COLOR
        } else {
            theme::MUTED
        };
        let visible = ui.clip_rect();
        let span = (
            visible.left().max(rect.left()),
            visible.right().min(rect.right()),
        );
        if points.is_empty() {
            painter.text(
                row.center(),
                Align2::CENTER_CENTER,
                "No points: double-click to add one",
                FontId::proportional(12.0),
                theme::MUTED,
            );
        }
        painter.add(egui::Shape::line(
            envelope_line(points, row, &x_of, span),
            Stroke::new(1.5, color),
        ));
        let hovered = pointer.and_then(hit);
        for (k, pt) in points.iter().enumerate() {
            let c = point_pos(pt);
            if c.x < span.0 - POINT_R || c.x > span.1 + POINT_R {
                continue;
            }
            let active =
                preview.is_some_and(|(j, _)| j == k) || (self.drag.is_none() && hovered == Some(k));
            painter.circle(
                c,
                POINT_R,
                if active { theme::TEXT } else { color },
                Stroke::new(1.0, Color32::from_black_alpha(160)),
            );
            if active {
                painter.text(
                    c + vec2(8.0, -8.0),
                    Align2::LEFT_BOTTOM,
                    value_text(project, &lane.target, pt.value),
                    FontId::proportional(11.0),
                    theme::TEXT,
                );
            }
        }
        if hovered.is_some() && self.drag.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
}

/// Context menu of a segment: the shape from point `point` to the next.
fn point_menu(
    ui: &mut egui::Ui,
    project: &Project,
    lane: &AutomationLane,
    point: Option<usize>,
    out: &mut EditorOutput,
) {
    let Some(k) = point else {
        ui.label("Double-click to add a point");
        return;
    };
    if k + 1 >= lane.points.len() {
        ui.label("No next point: double-click to add one");
        return;
    }
    let Some(pt) = lane.points.get(k).copied() else {
        return;
    };
    ui.label(value_text(project, &lane.target, pt.value));
    ui.label("Shape of this segment");
    for (curve, name) in [
        (Curve::Hold, "Step"),
        (Curve::Linear, "Linear"),
        (Curve::Curve, "Curve"),
    ] {
        if ui.radio(pt.curve == curve, name).clicked() && pt.curve != curve {
            // A visible bend to start from.
            let tension = if curve == Curve::Curve && pt.tension == 0.0 {
                0.5
            } else {
                pt.tension
            };
            out.edits.push(with_points(lane, |pts| {
                pts[k].curve = curve;
                pts[k].tension = tension;
            }));
        }
    }
    if pt.curve == Curve::Curve {
        let mut tension = pt.tension;
        let r = ui.add(egui::Slider::new(&mut tension, -1.0..=1.0).text("Tension"));
        if r.changed() {
            out.gesture
                .push(with_points(lane, |pts| pts[k].tension = tension));
        }
        out.end_gesture |= r.drag_stopped() || r.lost_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(tick: u64, value: f32, curve: Curve) -> AutoPoint {
        AutoPoint {
            tick,
            value,
            curve,
            tension: 0.0,
        }
    }

    #[test]
    fn envelope_shapes() {
        let points = vec![pt(100, 0.0, Curve::Hold), pt(200, 1.0, Curve::Linear)];
        // Row of 2·PAD + 100 px: value v sits at y = 106 - 100·v.
        let row = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 2.0 * PAD + 100.0));
        let x_of = |t: u64| t as f32;
        let line = envelope_line(&points, row, &x_of, (0.0, 500.0));
        let y = |v: f32| PAD + 100.0 - 100.0 * v;
        assert_eq!(
            line,
            [
                pos2(0.0, y(0.0)),
                pos2(100.0, y(0.0)),
                pos2(200.0, y(0.0)),
                pos2(200.0, y(1.0)),
                pos2(500.0, y(1.0)),
            ]
        );
        assert!(envelope_line(&[], row, &x_of, (0.0, 500.0)).is_empty());
    }
}
