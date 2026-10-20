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
//! - ruler: click seeks; Shift+drag sets the loop region.

use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2,
};
use jdance_commands::Edit;
use jdance_domain::{ClipId, LaneId, MAX_REPEAT, PatternClip, Project, TICKS_PER_STEP};
use std::collections::BTreeSet;

const ROW_H: f32 = 30.0;
const RULER_H: f32 = 20.0;
const HEADER_W: f32 = 170.0;
const EDGE_PX: f32 = 7.0;
const BAR_TICKS: u64 = 3_840;

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    Resize(ClipId),
    Loop(u64),
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
}

impl Default for PlaylistView {
    fn default() -> Self {
        Self {
            zoom: 120.0,
            selection: BTreeSet::new(),
            drag: None,
            context_clip: None,
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
    pub actions: Vec<PlaylistAction>,
}

fn color(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn snap(ticks: i64, grid: u64) -> i64 {
    let g = grid as i64;
    (ticks as f64 / g as f64).round() as i64 * g
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
        playhead: Option<u64>,
    ) -> PlaylistOutput {
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
                    self.lane_headers(ui, project, &mut out);
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

    fn lane_headers(&mut self, ui: &mut egui::Ui, project: &Project, out: &mut PlaylistOutput) {
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
        let ppt = self.px_per_tick();
        let end = pl.arrangement_end(&project.patterns);
        let bars = (end / BAR_TICKS + 8).max(16);
        let width = bars as f32 * self.zoom;
        let height = RULER_H + pl.lanes.len() as f32 * ROW_H;
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
                let dt = snap(((p.x - d.origin.x) / ppt) as i64, u64::from(TICKS_PER_STEP));
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
                DragKind::Move => {}
            }
        }

        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            if p.y < origin.y + RULER_H {
                out.actions.push(PlaylistAction::Seek(
                    snap(tick_at(p.x), u64::from(TICKS_PER_STEP)).max(0) as u64,
                ));
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
            self.context_clip = response
                .interact_pointer_pos()
                .and_then(hit)
                .map(|(id, _)| id);
            if let Some(id) = self.context_clip
                && !self.selection.contains(&id)
            {
                self.selection.clear();
                self.selection.insert(id);
            }
        }
        response.context_menu(|ui| {
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
    }
}
