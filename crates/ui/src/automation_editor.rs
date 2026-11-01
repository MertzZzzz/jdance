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
const CANVAS_H: f32 = 240.0;
/// The value scale left of the canvas (UI-RD §10.10).
const SCALE_W: f32 = 52.0;
/// Vertical padding of the envelope inside its row.
pub(crate) const PAD: f32 = 6.0;
const POINT_R: f32 = 4.0;

#[derive(Debug, Default)]
pub(crate) struct EditorOutput {
    pub edits: Vec<Edit>,
    /// Edits of a continuous gesture (merged into one undo step).
    pub gesture: Vec<Edit>,
    pub end_gesture: bool,
    /// LFO… asks for the LFO window on this source (UI-RD §10.10).
    pub lfo: Option<AutomationId>,
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
                self.header(ui, project, lane, grid, host, out);
                ui.add_space(4.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // The value scale in the parameter's units.
                    let (scale, _) =
                        ui.allocate_exact_size(vec2(SCALE_W, RULER_H + CANVAS_H), Sense::hover());
                    value_scale(ui, scale, project, lane);
                    egui::ScrollArea::horizontal()
                        .id_salt(("automation_editor_h", self.source.0))
                        .show(ui, |ui| self.canvas(ui, project, lane, grid, out));
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for (keys, text) in [
                        ("2×click", "add / delete point"),
                        ("drag", "move"),
                        ("RMB", "delete · hold to sweep"),
                        ("RMB between", "shape"),
                        ("Alt", "no snap"),
                    ] {
                        crate::widgets::kbd(ui, keys);
                        ui.label(RichText::new(text).size(11.0).color(theme::MUTED));
                        ui.add_space(8.0);
                    }
                });
            },
        )
    }

    fn header(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        lane: &AutomationLane,
        grid: u64,
        host: &mut dyn Host,
        out: &mut EditorOutput,
    ) {
        use crate::icons::Glyph as G;
        // UI-RD §6 (board R4 · Automation): name, the target chip, the
        // length, Snap, LFO… and the zoom.
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            crate::icons::paint(ui, G::Rename, r, theme::MUTED);
            let name_id = egui::Id::unique(("automation_name", self.source.0));
            let editing = ui.memory(|m| m.has_focus(name_id));
            if !editing {
                self.name.clone_from(&lane.name);
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.name)
                    .id(name_id)
                    .desired_width(140.0),
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
                egui::Frame::new()
                    .fill(AUTOMATION_COLOR.gamma_multiply(0.12))
                    .stroke(Stroke::new(1.0, AUTOMATION_COLOR.gamma_multiply(0.6)))
                    .corner_radius(7)
                    .inner_margin(egui::Margin::symmetric(9, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                            crate::icons::paint(ui, G::Automation, r, AUTOMATION_COLOR);
                            ui.label(RichText::new(name).color(AUTOMATION_COLOR).strong());
                            ui.label(RichText::new(context).size(11.0).color(theme::MUTED));
                        });
                    });
            } else {
                ui.label(RichText::new("Target not found").color(theme::WARN))
                    .on_hover_text("The automated object was removed; the source has no effect");
            }
            crate::widgets::divider(ui, 20.0);
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
            let snap = if grid <= 1 {
                "Off"
            } else {
                crate::toolbar::snap_name(grid as u32, project.time_signature)
            };
            crate::widgets::button(
                ui,
                Some(G::Snap),
                snap,
                false,
                crate::widgets::Look::Raised,
                "Points snap to the main Snap (Alt: no snap)",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().slider_width = 110.0;
                ui.add(
                    egui::Slider::new(&mut self.zoom, 40.0..=640.0)
                        .logarithmic(true)
                        .show_value(false),
                )
                .on_hover_text("Zoom");
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                crate::icons::paint(ui, G::Zoom, r, theme::DIM);
                if crate::widgets::button(
                    ui,
                    Some(G::Lfo),
                    "LFO…",
                    false,
                    crate::widgets::Look::Raised,
                    "Write an LFO curve into this source",
                )
                .clicked()
                {
                    out.lfo = Some(lane.id);
                }
            });
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
        let line = envelope_line(points, row, &x_of, span);
        // The area under the envelope, fading down (UI-RD §6).
        if line.len() > 1 {
            let mut fill = egui::Mesh::default();
            let base = row.bottom();
            for w in line.windows(2) {
                let (a, b) = (w[0], w[1]);
                if b.x <= a.x {
                    continue;
                }
                let i = fill.vertices.len() as u32;
                fill.colored_vertex(a, color.gamma_multiply(0.28));
                fill.colored_vertex(b, color.gamma_multiply(0.28));
                fill.colored_vertex(pos2(b.x, base), color.gamma_multiply(0.03));
                fill.colored_vertex(pos2(a.x, base), color.gamma_multiply(0.03));
                fill.add_triangle(i, i + 1, i + 2);
                fill.add_triangle(i, i + 2, i + 3);
            }
            painter.add(fill);
        }
        painter.add(egui::Shape::line(line, Stroke::new(2.0, color)));
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
                if active { POINT_R + 1.5 } else { POINT_R },
                if active { color } else { theme::BG },
                Stroke::new(1.5, color),
            );
            if active {
                // The point's value pops up beside it (UI-RD §10.10).
                let text = format!(
                    "{} · {}",
                    crate::toolbar::bars_text(pt.tick, sig),
                    value_text(project, &lane.target, pt.value)
                );
                let galley = painter.layout_no_wrap(text, theme::mono(10.5), theme::TEXT);
                let size = galley.size() + vec2(14.0, 8.0);
                let mut at = c + vec2(12.0, -size.y - 4.0);
                if at.x + size.x > span.1 {
                    at.x = c.x - 12.0 - size.x;
                }
                at.y = at.y.max(row.top() + 2.0);
                let bubble = Rect::from_min_size(at, size);
                painter.rect(
                    bubble,
                    6.0,
                    theme::LCD,
                    Stroke::new(1.0, color),
                    egui::StrokeKind::Inside,
                );
                painter.galley(bubble.center() - galley.size() / 2.0, galley, theme::TEXT);
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
    let _ = value_text(project, &lane.target, pt.value);
    crate::widgets::caption(ui, "Segment shape");
    ui.horizontal(|ui| {
        for (curve, name) in [
            (Curve::Hold, "Step"),
            (Curve::Linear, "Linear"),
            (Curve::Curve, "Curve"),
        ] {
            if shape_card(ui, curve, name, pt.curve == curve).clicked() && pt.curve != curve {
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
    });
    if pt.curve == Curve::Curve {
        let mut tension = pt.tension;
        let r = ui
            .horizontal(|ui| {
                ui.label("Tension");
                ui.add(egui::Slider::new(&mut tension, -1.0..=1.0).fixed_decimals(2))
            })
            .inner;
        if r.changed() {
            out.gesture
                .push(with_points(lane, |pts| pts[k].tension = tension));
        }
        out.end_gesture |= r.drag_stopped() || r.lost_focus();
    }
}

/// A shape button of the segment menu: the shape drawn over its name.
fn shape_card(ui: &mut egui::Ui, curve: Curve, name: &str, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(66.0, 46.0), Sense::click());
    let (fill, stroke, fg) = if on {
        (
            AUTOMATION_COLOR.gamma_multiply(0.15),
            AUTOMATION_COLOR,
            AUTOMATION_COLOR,
        )
    } else if resp.hovered() {
        (theme::RAISED, theme::LINE_STRONG, theme::TEXT)
    } else {
        (theme::FIELD, theme::LINE, theme::MUTED)
    };
    let p = ui.painter();
    p.rect(
        rect,
        7.0,
        fill,
        Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    let g = Rect::from_center_size(rect.center() - vec2(0.0, 7.0), vec2(22.0, 12.0));
    let pts: Vec<Pos2> = match curve {
        Curve::Hold => vec![
            g.left_bottom(),
            pos2(g.center().x, g.bottom()),
            pos2(g.center().x, g.top()),
            g.right_top(),
        ],
        Curve::Linear => vec![g.left_bottom(), g.right_top()],
        Curve::Curve => (0..=12)
            .map(|k| {
                let t = k as f32 / 12.0;
                let w = (1.0 - (t * std::f32::consts::PI).cos()) / 2.0;
                pos2(g.left() + t * g.width(), g.bottom() - w * g.height())
            })
            .collect(),
    };
    p.add(egui::Shape::line(pts, Stroke::new(1.5, fg)));
    p.text(
        pos2(rect.center().x, rect.bottom() - 10.0),
        Align2::CENTER_CENTER,
        name,
        FontId::proportional(11.0),
        fg,
    );
    let name = name.to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &name));
    resp
}

/// Values of the source at 0, ¼, ½, ¾ and 1 of its range, in the
/// parameter's units, beside the canvas rows (UI-RD §10.10).
fn value_scale(ui: &egui::Ui, rect: Rect, project: &Project, lane: &AutomationLane) {
    let p = ui.painter_at(rect);
    let row = Rect::from_min_max(pos2(rect.left(), rect.top() + RULER_H), rect.max);
    for v in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let y = row.bottom() - PAD - v * (row.height() - 2.0 * PAD);
        let align = if v == 1.0 {
            Align2::RIGHT_TOP
        } else if v == 0.0 {
            Align2::RIGHT_BOTTOM
        } else {
            Align2::RIGHT_CENTER
        };
        p.text(
            pos2(rect.right() - 6.0, y),
            align,
            value_text(project, &lane.target, v),
            theme::mono(9.0),
            theme::DIM,
        );
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
