// SPDX-License-Identifier: GPL-3.0-or-later
//! Playlist editor: lanes with pattern clips (spec "Playlist").
//!
//! Interaction model:
//! - double-click empty space: new clip of the current pattern;
//! - click a clip: select (Shift toggles multi-select);
//! - drag a clip body: move the selection in time and across lanes;
//!   drag the right edge: change `repeat_count`; every drag is committed
//!   once, on release, as a single undo step;
//! - Delete removes, Ctrl/Cmd+D duplicates the selection;
//! - ruler: click seeks; Shift+drag sets the loop region;
//! - automation lanes (ADR-014) below the pattern lanes, on the same
//!   timeline: double-click adds a point (or deletes the one under the
//!   pointer), drag moves a point (snapped to steps; Alt: free),
//!   right-click a point for its curve and tension.

use crate::automation::{AUTOMATION_COLOR, LaneInfo, value_text};
use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2,
};
use jdance_commands::Edit;
use jdance_domain::{
    AutoPoint, AutomationId, AutomationLane, ClipId, Curve, LaneId, MAX_REPEAT, PatternClip,
    Project, TICKS_PER_STEP,
};
use std::collections::BTreeSet;

const ROW_H: f32 = 30.0;
const RULER_H: f32 = 20.0;
const HEADER_W: f32 = 170.0;
const EDGE_PX: f32 = 7.0;
const BAR_TICKS: u64 = 3_840;
const AUTO_H: f32 = 64.0;
/// Vertical padding of the envelope inside an automation row.
const AUTO_PAD: f32 = 6.0;
const POINT_R: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    Resize(ClipId),
    Loop(u64),
    Point(AutomationId, usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    kind: DragKind,
    origin: Pos2,
}

/// View state kept by the app between frames.
#[derive(Debug)]
pub(crate) struct PlaylistView {
    /// Pixels per bar.
    zoom: f32,
    selection: BTreeSet<ClipId>,
    drag: Option<Drag>,
    context_clip: Option<ClipId>,
    /// Snap grid in ticks (1 = off), from the toolbar.
    grid: u64,
    /// Automation lane and point under the last right-click.
    context_auto: Option<(AutomationId, Option<usize>)>,
}

impl Default for PlaylistView {
    fn default() -> Self {
        Self {
            zoom: 120.0,
            selection: BTreeSet::new(),
            drag: None,
            context_clip: None,
            grid: u64::from(TICKS_PER_STEP),
            context_auto: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PlaylistAction {
    AddClip { lane: LaneId, start: u64 },
    Duplicate(Vec<ClipId>),
    MakeUnique(ClipId),
    AddLane,
    RenameLane(LaneId),
    Seek(u64),
}

#[derive(Debug, Default)]
pub(crate) struct PlaylistOutput {
    pub edits: Vec<Edit>,
    /// Edits of a continuous gesture (merged into one undo step).
    pub gesture: Vec<Edit>,
    pub end_gesture: bool,
    pub actions: Vec<PlaylistAction>,
}

fn color(c: [u8; 3]) -> Color32 {
    crate::theme::rgb(c)
}

fn snap(ticks: i64, grid: u64) -> i64 {
    let g = grid as i64;
    (ticks as f64 / g as f64).round() as i64 * g
}

fn with_points(lane: &AutomationLane, f: impl FnOnce(&mut Vec<AutoPoint>)) -> Edit {
    let mut lane = lane.clone();
    f(&mut lane.points);
    Edit::SetAutomation {
        id: lane.id,
        lane: Some(lane),
    }
}

/// Envelope polyline of `lane` in `row`, between `x0` and `x1` on screen.
fn envelope_line(
    lane: &AutomationLane,
    row: Rect,
    x_of: &impl Fn(u64) -> f32,
    (x0, x1): (f32, f32),
) -> Vec<Pos2> {
    let y_of = |v: f32| row.bottom() - AUTO_PAD - v * (row.height() - 2.0 * AUTO_PAD);
    let pts = &lane.points;
    let (Some(first), Some(last)) = (pts.first(), pts.last()) else {
        return Vec::new();
    };
    let mut line = vec![pos2(x0.min(x_of(first.tick)), y_of(first.value))];
    for w in pts.windows(2) {
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

impl PlaylistView {
    fn px_per_tick(&self) -> f32 {
        self.zoom / BAR_TICKS as f32
    }

    pub(crate) fn retain_existing(&mut self, project: &Project) {
        self.selection
            .retain(|id| project.playlist.clip(*id).is_some());
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        lanes: &[LaneInfo],
        playhead: Option<u64>,
        snap: u32,
    ) -> PlaylistOutput {
        self.grid = u64::from(snap.max(1));
        let mut out = PlaylistOutput::default();
        self.retain_existing(project);

        ui.horizontal(|ui| {
            ui.strong("Playlist");
            ui.label("Zoom");
            ui.add(egui::Slider::new(&mut self.zoom, 30.0..=480.0).logarithmic(true).show_value(false));
            if !self.selection.is_empty() {
                ui.label(format!("{} selected", self.selection.len()));
            }
            ui.weak("Double-click: new clip · Shift+click: multi-select · Del · Ctrl+D · Shift+drag ruler: loop");
        });

        egui::ScrollArea::vertical()
            .id_salt("playlist-v")
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    self.lane_headers(ui, project, lanes, &mut out);
                    egui::ScrollArea::horizontal()
                        .id_salt("playlist-h")
                        .show(ui, |ui| {
                            self.canvas(ui, project, playhead, &mut out);
                        });
                });
            });

        // Keyboard shortcuts apply when the playlist has a selection and
        // no text field has focus.
        let ctx = ui.ctx().clone();
        if !self.selection.is_empty() && !ctx.egui_wants_keyboard_input() {
            let (del, dup) = ctx.input_mut(|i| {
                let del = i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace);
                let dup = i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::COMMAND,
                    egui::Key::D,
                ));
                (del, dup)
            });
            if del {
                out.edits.push(Edit::Batch(
                    self.selection
                        .iter()
                        .map(|id| Edit::RemoveClip(*id))
                        .collect(),
                ));
                self.selection.clear();
            } else if dup {
                out.actions.push(PlaylistAction::Duplicate(
                    self.selection.iter().copied().collect(),
                ));
            }
        }
        out
    }

    fn lane_headers(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        lanes: &[LaneInfo],
        out: &mut PlaylistOutput,
    ) {
        ui.vertical(|ui| {
            ui.set_width(HEADER_W);
            ui.add_space(RULER_H);
            for lane in &project.playlist.lanes {
                ui.horizontal(|ui| {
                    ui.set_height(ROW_H - ui.spacing().item_spacing.y);
                    let (swatch, resp) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::click());
                    ui.painter()
                        .rect_filled(swatch, CornerRadius::same(2), color(lane.color));
                    if resp.on_hover_text("Click to change color").clicked() {
                        let i = jdance_domain::LANE_COLORS
                            .iter()
                            .position(|c| *c == lane.color)
                            .map_or(0, |i| (i + 1) % jdance_domain::LANE_COLORS.len());
                        out.edits
                            .push(Edit::SetLaneColor(lane.id, jdance_domain::LANE_COLORS[i]));
                    }
                    let name = ui.add_sized([86.0, 20.0], egui::Button::new(&lane.name).truncate());
                    if name.double_clicked() {
                        out.actions.push(PlaylistAction::RenameLane(lane.id));
                    }
                    name.on_hover_text("Double-click to rename")
                        .context_menu(|ui| {
                            if ui.button("Rename…").clicked() {
                                out.actions.push(PlaylistAction::RenameLane(lane.id));
                                ui.close();
                            }
                            if ui.button("Remove lane").clicked() {
                                out.edits.push(Edit::RemoveLane(lane.id));
                                ui.close();
                            }
                        });
                    let mut m = lane.muted;
                    if ui
                        .toggle_value(&mut m, "M")
                        .on_hover_text("Mute lane (its clips' events are not played)")
                        .changed()
                    {
                        out.edits.push(Edit::SetLaneMuted(lane.id, m));
                    }
                    let mut s = lane.solo;
                    if ui
                        .toggle_value(&mut s, "S")
                        .on_hover_text("Solo lane")
                        .changed()
                    {
                        out.edits.push(Edit::SetLaneSolo(lane.id, s));
                    }
                });
            }
            for info in lanes {
                ui.horizontal_top(|ui| {
                    ui.set_height(AUTO_H - ui.spacing().item_spacing.y);
                    let color = if info.missing {
                        ui.visuals().warn_fg_color
                    } else {
                        AUTOMATION_COLOR
                    };
                    let hover = if info.missing {
                        "The automated object was removed; the lane is kept and has no effect"
                            .to_owned()
                    } else {
                        format!(
                            "{} · {}: applied during song playback",
                            info.context, info.name
                        )
                    };
                    ui.vertical(|ui| {
                        ui.set_width(HEADER_W - 30.0);
                        ui.add(
                            egui::Label::new(egui::RichText::new(&info.name).color(color))
                                .truncate(),
                        )
                        .on_hover_text(hover);
                        ui.add(
                            egui::Label::new(egui::RichText::new(&info.context).small().weak())
                                .truncate(),
                        );
                    });
                    if ui
                        .small_button("x")
                        .on_hover_text("Remove automation lane")
                        .clicked()
                    {
                        out.edits.push(Edit::SetAutomation {
                            id: info.id,
                            lane: None,
                        });
                    }
                });
            }
            if ui.small_button("+ Lane").clicked() {
                out.actions.push(PlaylistAction::AddLane);
            }
        });
    }

    #[allow(clippy::too_many_lines)]
    fn canvas(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        playhead: Option<u64>,
        out: &mut PlaylistOutput,
    ) {
        let pl = &project.playlist;
        let grid = self.grid;
        let ppt = self.px_per_tick();
        let end = pl.arrangement_end(&project.patterns);
        let bars = (end / BAR_TICKS + 8).max(16);
        let width = bars as f32 * self.zoom;
        let auto_y0 = RULER_H + pl.lanes.len() as f32 * ROW_H;
        let height = auto_y0 + project.automation.len() as f32 * AUTO_H;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();
        let origin = rect.min;
        let x_of = |tick: u64| origin.x + tick as f32 * ppt;
        let tick_at = |x: f32| (((x - origin.x) / ppt).max(0.0)) as i64;
        let lane_at = |y: f32| -> Option<usize> {
            let i = ((y - origin.y - RULER_H) / ROW_H).floor();
            (i >= 0.0 && (i as usize) < pl.lanes.len()).then_some(i as usize)
        };
        let clip_rect =
            |c: &PatternClip, lane_index: usize, start: u64, repeat: u32| -> Option<Rect> {
                let pat = project.pattern(c.pattern)?;
                let x0 = x_of(start);
                let x1 = x_of(start + pat.length_ticks() * u64::from(repeat));
                let y0 = origin.y + RULER_H + lane_index as f32 * ROW_H + 2.0;
                Some(Rect::from_min_max(pos2(x0, y0), pos2(x1, y0 + ROW_H - 4.0)))
            };
        let lane_index = |id: LaneId| pl.lanes.iter().position(|l| l.id == id);
        let auto = &project.automation;
        let auto_row = |i: usize| {
            let y = origin.y + auto_y0 + i as f32 * AUTO_H;
            Rect::from_min_size(pos2(rect.left(), y), vec2(width, AUTO_H))
        };
        let auto_at = |y: f32| -> Option<usize> {
            let i = ((y - origin.y - auto_y0) / AUTO_H).floor();
            (i >= 0.0 && (i as usize) < auto.len()).then_some(i as usize)
        };
        let value_at_y = |row: Rect, y: f32| {
            ((row.bottom() - AUTO_PAD - y) / (row.height() - 2.0 * AUTO_PAD)).clamp(0.0, 1.0)
        };
        let point_pos = |row: Rect, p: &AutoPoint| {
            pos2(
                x_of(p.tick),
                row.bottom() - AUTO_PAD - p.value * (row.height() - 2.0 * AUTO_PAD),
            )
        };
        // Point of automation row `i` under `p` (the last one wins).
        let point_hit = |i: usize, p: Pos2| -> Option<usize> {
            let row = auto_row(i);
            auto[i]
                .points
                .iter()
                .rposition(|pt| point_pos(row, pt).distance(p) <= POINT_R + 3.0)
        };

        // Background, grid, ruler.
        painter.rect_filled(rect, CornerRadius::ZERO, visuals.extreme_bg_color);
        for (i, _) in pl.lanes.iter().enumerate() {
            if i % 2 == 1 {
                let y = origin.y + RULER_H + i as f32 * ROW_H;
                painter.rect_filled(
                    Rect::from_min_size(pos2(rect.left(), y), vec2(width, ROW_H)),
                    CornerRadius::ZERO,
                    visuals.faint_bg_color,
                );
            }
        }
        for i in 0..auto.len() {
            let row = auto_row(i);
            painter.rect_filled(row, CornerRadius::ZERO, visuals.faint_bg_color);
            painter.line_segment(
                [row.left_top(), row.right_top()],
                visuals.widgets.noninteractive.bg_stroke,
            );
        }
        let beat_px = self.zoom / 4.0;
        for b in 0..=bars * 4 {
            let x = origin.x + b as f32 * beat_px;
            let bar_line = b % 4 == 0;
            if !bar_line && beat_px < 12.0 {
                continue;
            }
            let stroke = if bar_line {
                Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
            } else {
                Stroke::new(
                    1.0,
                    visuals
                        .widgets
                        .noninteractive
                        .bg_stroke
                        .color
                        .gamma_multiply(0.4),
                )
            };
            painter.line_segment(
                [pos2(x, origin.y + RULER_H), pos2(x, rect.bottom())],
                stroke,
            );
            if bar_line {
                painter.text(
                    pos2(x + 3.0, origin.y + 2.0),
                    Align2::LEFT_TOP,
                    format!("{}", b / 4 + 1),
                    FontId::proportional(11.0),
                    visuals.weak_text_color(),
                );
            }
        }
        if let Some((a, b)) = pl.loop_range {
            let fill = if pl.loop_enabled {
                Color32::from_rgba_unmultiplied(90, 170, 235, 70)
            } else {
                Color32::from_rgba_unmultiplied(150, 150, 150, 40)
            };
            painter.rect_filled(
                Rect::from_min_max(pos2(x_of(a), origin.y), pos2(x_of(b), origin.y + RULER_H)),
                CornerRadius::ZERO,
                fill,
            );
        }
        painter.line_segment(
            [
                pos2(rect.left(), origin.y + RULER_H),
                pos2(rect.right(), origin.y + RULER_H),
            ],
            visuals.widgets.noninteractive.bg_stroke,
        );

        // Input.
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let shift = ui.input(|i| i.modifiers.shift);
        let hit = |p: Pos2| -> Option<(ClipId, bool)> {
            // Topmost (last drawn) clip wins.
            pl.clips.iter().rev().find_map(|c| {
                let li = lane_index(c.lane)?;
                let r = clip_rect(c, li, c.start, c.repeat)?;
                r.contains(p).then_some((c.id, p.x > r.right() - EDGE_PX))
            })
        };

        if response.drag_started()
            && let Some(p) = response.interact_pointer_pos()
        {
            if p.y < origin.y + RULER_H {
                if shift {
                    let t = snap(tick_at(p.x), BAR_TICKS).max(0) as u64;
                    self.drag = Some(Drag {
                        kind: DragKind::Loop(t),
                        origin: p,
                    });
                }
            } else if let Some(i) = auto_at(p.y) {
                if let Some(k) = point_hit(i, p) {
                    self.drag = Some(Drag {
                        kind: DragKind::Point(auto[i].id, k),
                        origin: p,
                    });
                }
            } else if let Some((id, edge)) = hit(p) {
                if !self.selection.contains(&id) {
                    if !shift {
                        self.selection.clear();
                    }
                    self.selection.insert(id);
                }
                let kind = if edge {
                    DragKind::Resize(id)
                } else {
                    DragKind::Move
                };
                self.drag = Some(Drag { kind, origin: p });
            }
        }

        // Current drag offsets for preview and commit.
        let (dt, dl, drepeat) = match (self.drag, pointer) {
            (Some(d), Some(p)) => {
                let dt = snap(((p.x - d.origin.x) / ppt) as i64, grid);
                let dl = ((p.y - d.origin.y) / ROW_H).round() as i64;
                let drepeat = match d.kind {
                    DragKind::Resize(id) => pl
                        .clip(id)
                        .and_then(|c| {
                            project
                                .pattern(c.pattern)
                                .map(|pat| (c, pat.length_ticks()))
                        })
                        .map_or(0, |(_, len)| {
                            ((p.x - d.origin.x) / (len as f32 * ppt)).round() as i64
                        }),
                    _ => 0,
                };
                (dt, dl, drepeat)
            }
            _ => (0, 0, 0),
        };
        // Copies, so the preview closure does not borrow `self`.
        let drag_kind = self.drag.map(|d| d.kind);
        let selection = self.selection.clone();
        let moved_clip = |c: &PatternClip| -> (u64, usize, u32) {
            let li = lane_index(c.lane).unwrap_or(0);
            match drag_kind {
                Some(DragKind::Move) if selection.contains(&c.id) => {
                    let start = (c.start as i64 + dt).max(0) as u64;
                    let lane = (li as i64 + dl).clamp(0, pl.lanes.len() as i64 - 1) as usize;
                    (start, lane, c.repeat)
                }
                Some(DragKind::Resize(id)) if id == c.id => {
                    let r = (i64::from(c.repeat) + drepeat).clamp(1, i64::from(MAX_REPEAT)) as u32;
                    (c.start, li, r)
                }
                _ => (c.start, li, c.repeat),
            }
        };

        // A dragged automation point stays between its neighbours.
        let alt = ui.input(|i| i.modifiers.alt);
        let moved_point = |lane: &AutomationLane, k: usize| -> Option<AutoPoint> {
            let (Some(d), Some(p)) = (self.drag, pointer) else {
                return None;
            };
            let pt = *lane.points.get(k)?;
            let row = auto_row(auto.iter().position(|l| l.id == lane.id)?);
            let raw = pt.tick as i64 + ((p.x - d.origin.x) / ppt) as i64;
            let t = if alt { raw } else { snap(raw, grid) };
            let lo = k.checked_sub(1).map_or(0, |j| lane.points[j].tick);
            let hi = lane.points.get(k + 1).map_or(u64::MAX, |n| n.tick);
            Some(AutoPoint {
                tick: (t.max(0) as u64).clamp(lo, hi),
                value: value_at_y(row, point_pos(row, &pt).y + (p.y - d.origin.y)),
                ..pt
            })
        };
        let dragged_point = match self.drag.map(|d| d.kind) {
            Some(DragKind::Point(id, k)) => project
                .automation_lane(id)
                .and_then(|l| Some((id, k, moved_point(l, k)?))),
            _ => None,
        };

        if response.drag_stopped()
            && let Some(d) = self.drag.take()
        {
            match d.kind {
                DragKind::Move if dt != 0 || dl != 0 => {
                    let moves: Vec<Edit> = pl
                        .clips
                        .iter()
                        .filter(|c| selection.contains(&c.id))
                        .map(|c| {
                            let (start, li, _) = moved_clip(c);
                            Edit::MoveClip {
                                id: c.id,
                                start,
                                lane: pl.lanes[li].id,
                            }
                        })
                        .collect();
                    if !moves.is_empty() {
                        out.edits.push(Edit::Batch(moves));
                    }
                }
                DragKind::Resize(id) => {
                    if let Some(c) = pl.clip(id) {
                        let (_, _, r) = moved_clip(c);
                        if r != c.repeat {
                            out.edits.push(Edit::SetClipRepeat(id, r));
                        }
                    }
                }
                DragKind::Loop(a) => {
                    if let Some(p) = pointer {
                        let b = snap(tick_at(p.x), BAR_TICKS).max(0) as u64;
                        let (lo, hi) = (a.min(b), a.max(b));
                        if hi > lo {
                            out.edits.push(Edit::SetLoop {
                                range: Some((lo, hi)),
                                enabled: true,
                            });
                        }
                    }
                }
                DragKind::Point(..) => {
                    if let Some((id, k, moved)) = dragged_point
                        && let Some(lane) = project.automation_lane(id)
                        && lane.points[k] != moved
                    {
                        out.edits.push(with_points(lane, |pts| pts[k] = moved));
                    }
                }
                DragKind::Move => {}
            }
        }

        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            if p.y < origin.y + RULER_H {
                out.actions
                    .push(PlaylistAction::Seek(snap(tick_at(p.x), grid).max(0) as u64));
            } else {
                match hit(p) {
                    Some((id, _)) => {
                        if shift {
                            if !self.selection.remove(&id) {
                                self.selection.insert(id);
                            }
                        } else {
                            self.selection.clear();
                            self.selection.insert(id);
                        }
                    }
                    None => self.selection.clear(),
                }
            }
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
            && let Some(i) = auto_at(p.y)
        {
            let lane = &auto[i];
            out.edits.push(match point_hit(i, p) {
                Some(k) => with_points(lane, |pts| {
                    pts.remove(k);
                }),
                None => {
                    let raw = tick_at(p.x);
                    let tick = if alt { raw } else { snap(raw, grid) }.max(0) as u64;
                    let value = value_at_y(auto_row(i), p.y);
                    // New points take the curve of the segment they split.
                    let at = lane.points.partition_point(|q| q.tick <= tick);
                    let (curve, tension) = at.checked_sub(1).map_or((Curve::Linear, 0.0), |j| {
                        (lane.points[j].curve, lane.points[j].tension)
                    });
                    with_points(lane, |pts| {
                        pts.insert(
                            at,
                            AutoPoint {
                                tick,
                                value,
                                curve,
                                tension,
                            },
                        );
                    })
                }
            });
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
            && hit(p).is_none()
            && let Some(li) = lane_at(p.y)
        {
            let start = snap(tick_at(p.x) - (BAR_TICKS as i64 / 2), BAR_TICKS).max(0) as u64;
            out.actions.push(PlaylistAction::AddClip {
                lane: pl.lanes[li].id,
                start,
            });
        }
        if response.secondary_clicked() {
            let p = response.interact_pointer_pos();
            self.context_auto = p.and_then(|p| auto_at(p.y).map(|i| (auto[i].id, point_hit(i, p))));
            self.context_clip = p.and_then(hit).map(|(id, _)| id);
            if let Some(id) = self.context_clip
                && !self.selection.contains(&id)
            {
                self.selection.clear();
                self.selection.insert(id);
            }
        }
        // Stays open while the curve and tension are edited.
        egui::Popup::context_menu(&response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                if let Some((id, point)) = self.context_auto {
                    if let Some(lane) = project.automation_lane(id) {
                        automation_menu(ui, project, lane, point, out);
                    }
                    return;
                }
                let Some(id) = self.context_clip else {
                    ui.label("No clip here");
                    return;
                };
                let Some(clip) = pl.clip(id) else { return };
                if ui
                    .button("Make unique")
                    .on_hover_text("Copy the pattern for this clip only")
                    .clicked()
                {
                    out.actions.push(PlaylistAction::MakeUnique(id));
                    ui.close();
                }
                let label = if clip.muted { "Unmute" } else { "Mute" };
                if ui.button(label).clicked() {
                    out.edits.push(Edit::Batch(
                        self.selection
                            .iter()
                            .map(|c| Edit::SetClipMuted(*c, !clip.muted))
                            .collect(),
                    ));
                    ui.close();
                }
                if ui.button("Duplicate").clicked() {
                    out.actions.push(PlaylistAction::Duplicate(
                        self.selection.iter().copied().collect(),
                    ));
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    out.edits.push(Edit::Batch(
                        self.selection
                            .iter()
                            .map(|c| Edit::RemoveClip(*c))
                            .collect(),
                    ));
                    self.selection.clear();
                    ui.close();
                }
            });

        // Clips (with drag preview).
        for c in &pl.clips {
            let (start, li, repeat) = moved_clip(c);
            let Some(r) = clip_rect(c, li, start, repeat) else {
                continue;
            };
            let Some(pat) = project.pattern(c.pattern) else {
                continue;
            };
            let lane_color = pl.lanes.get(li).map_or(Color32::GRAY, |l| color(l.color));
            let audible = pl.admits(c);
            let fill = if audible {
                lane_color.gamma_multiply(0.55)
            } else {
                lane_color.gamma_multiply(0.15)
            };
            painter.rect_filled(r, CornerRadius::same(3), fill);
            let len_px = pat.length_ticks() as f32 * ppt;
            for k in 1..repeat {
                let x = r.left() + k as f32 * len_px;
                painter.line_segment(
                    [pos2(x, r.top() + 3.0), pos2(x, r.bottom() - 3.0)],
                    Stroke::new(1.0, Color32::from_black_alpha(120)),
                );
            }
            let selected = self.selection.contains(&c.id);
            let stroke = if selected {
                Stroke::new(2.0, visuals.strong_text_color())
            } else {
                Stroke::new(1.0, lane_color)
            };
            painter.rect_stroke(r, CornerRadius::same(3), stroke, StrokeKind::Inside);
            let mut name = pat.name.clone();
            if c.muted {
                name.push_str(" (muted)");
            }
            painter.text(
                pos2(r.left() + 4.0, r.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(12.0),
                if audible {
                    visuals.strong_text_color()
                } else {
                    visuals.weak_text_color()
                },
            );
        }
        // Automation envelopes (with drag preview).
        let visible = ui.clip_rect();
        let span = (
            visible.left().max(rect.left()),
            visible.right().min(rect.right()),
        );
        for (i, lane) in auto.iter().enumerate() {
            let row = auto_row(i);
            if !visible.intersects(row) {
                continue;
            }
            let preview;
            let lane = match dragged_point {
                Some((id, k, moved)) if id == lane.id => {
                    let mut l = lane.clone();
                    l.points[k] = moved;
                    preview = l;
                    &preview
                }
                _ => lane,
            };
            let missing = !lane.target.exists(project);
            let color = if missing {
                visuals.weak_text_color()
            } else {
                AUTOMATION_COLOR
            };
            if lane.points.is_empty() {
                painter.text(
                    pos2(span.0 + 6.0, row.center().y),
                    Align2::LEFT_CENTER,
                    "No points: double-click to add",
                    FontId::proportional(11.0),
                    visuals.weak_text_color(),
                );
                continue;
            }
            painter.add(egui::Shape::line(
                envelope_line(lane, row, &x_of, span),
                Stroke::new(1.5, color),
            ));
            let first = lane
                .points
                .partition_point(|p| x_of(p.tick) < span.0 - POINT_R);
            for (k, pt) in lane.points.iter().enumerate().skip(first) {
                let c = point_pos(row, pt);
                if c.x > span.1 + POINT_R {
                    break;
                }
                let active = matches!(dragged_point, Some((id, j, _)) if id == lane.id && j == k);
                painter.circle(
                    c,
                    POINT_R,
                    if active {
                        visuals.strong_text_color()
                    } else {
                        color
                    },
                    Stroke::new(1.0, Color32::from_black_alpha(160)),
                );
                if active {
                    painter.text(
                        c + vec2(8.0, -8.0),
                        Align2::LEFT_BOTTOM,
                        value_text(project, &lane.target, pt.value),
                        FontId::proportional(11.0),
                        visuals.strong_text_color(),
                    );
                }
            }
        }
        if let (
            Some(Drag {
                kind: DragKind::Loop(a),
                ..
            }),
            Some(p),
        ) = (self.drag, pointer)
        {
            let b = snap(tick_at(p.x), BAR_TICKS).max(0) as u64;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x_of(a.min(b)), origin.y),
                    pos2(x_of(a.max(b)), origin.y + RULER_H),
                ),
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(90, 170, 235, 110),
            );
        }
        if let Some(pos) = playhead {
            let x = x_of(pos);
            painter.line_segment(
                [pos2(x, origin.y), pos2(x, rect.bottom())],
                Stroke::new(1.5, Color32::WHITE),
            );
        }
        // Hover cursor for the resize edge.
        if let Some(p) = response.hover_pos()
            && let Some((_, true)) = hit(p)
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if self.drag.is_none()
            && let Some(p) = response.hover_pos()
            && let Some(i) = auto_at(p.y)
            && let Some(k) = point_hit(i, p)
        {
            let lane = &auto[i];
            let pt = lane.points[k];
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            painter.text(
                point_pos(auto_row(i), &pt) + vec2(8.0, -8.0),
                Align2::LEFT_BOTTOM,
                value_text(project, &lane.target, pt.value),
                FontId::proportional(11.0),
                visuals.strong_text_color(),
            );
        }
    }
}

/// Context menu of an automation lane, or of one of its points.
fn automation_menu(
    ui: &mut egui::Ui,
    project: &Project,
    lane: &AutomationLane,
    point: Option<usize>,
    out: &mut PlaylistOutput,
) {
    if let Some(k) = point
        && let Some(pt) = lane.points.get(k).copied()
    {
        ui.label(value_text(project, &lane.target, pt.value));
        ui.label("Curve to the next point");
        for (curve, name) in [
            (Curve::Linear, "Linear"),
            (Curve::Curve, "Curve"),
            (Curve::Hold, "Hold"),
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
        if ui.button("Delete point").clicked() {
            out.edits.push(with_points(lane, |pts| {
                pts.remove(k);
            }));
            ui.close();
        }
        ui.separator();
    }
    if ui.button("Remove lane").clicked() {
        out.edits.push(Edit::SetAutomation {
            id: lane.id,
            lane: None,
        });
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{AutomationTarget, TrackId};

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
        let lane = AutomationLane {
            id: AutomationId(1),
            target: AutomationTarget::TrackPan(TrackId(1)),
            points: vec![pt(100, 0.0, Curve::Hold), pt(200, 1.0, Curve::Linear)],
        };
        // Row of 2·AUTO_PAD + 100 px: value v sits at y = 106 - 100·v.
        let row = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 2.0 * AUTO_PAD + 100.0));
        let x_of = |t: u64| t as f32;
        let line = envelope_line(&lane, row, &x_of, (0.0, 500.0));
        let y = |v: f32| AUTO_PAD + 100.0 - 100.0 * v;
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
        let empty = AutomationLane {
            points: Vec::new(),
            ..lane
        };
        assert!(envelope_line(&empty, row, &x_of, (0.0, 500.0)).is_empty());
    }
}
