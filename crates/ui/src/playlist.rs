// SPDX-License-Identifier: GPL-3.0-or-later
//! Playlist editor: lanes with pattern and audio clips (spec 0.2.0 §2,
//! §4, §8; UI-VIS §7).
//!
//! Tools (shortcut): Draw (P), Paint (B), Delete (D), Slice (C), Select
//! (E). With Draw, Paint and Select a clip body drags (the selection
//! moves) and either edge resizes; Ctrl-drag selects with a frame in any
//! tool. Draw places one copy of the source, Paint a row of copies, Delete
//! removes what it touches, Slice cuts at the pointer. Every gesture is
//! one undo step. Files dropped from the Browser or the system become
//! audio clips; below the last lane a new lane is made. Double-click opens
//! a clip's editor. Ruler: click seeks, Shift-drag sets the loop. Alt
//! disables snapping while dragging.
//!
//! Automation lanes (ADR-014) sit below the clip lanes on the same
//! timeline: double-click adds a point (or deletes the one under the
//! pointer), drag moves a point, right-click sets its curve.

use crate::automation::{AUTOMATION_COLOR, LaneInfo, value_text};
use crate::browser::BrowserItem;
use crate::icons::{self, Glyph};
use crate::project_browser::ProjectItem;
use crate::toolbar::bars_text;
use crate::{Host, theme};
use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, pos2,
    vec2,
};
use jdance_commands::{Edge, Edit, audio_clip_edge, pattern_clip_edge};
use jdance_domain::{
    AssetId, AudioAsset, AudioClip, AutoPoint, AutomationId, AutomationLane, ClipId, Curve, LaneId,
    PatternClip, PatternId, Project, SampleRef, TICKS_PER_STEP, TrackId, frames_per_tick,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ROW_H: f32 = 32.0;
const RULER_H: f32 = 20.0;
const HEADER_W: f32 = 190.0;
const EDGE_PX: f32 = 6.0;
const BAR_TICKS: u64 = 3_840;
const AUTO_H: f32 = 64.0;
/// Vertical padding of the envelope inside an automation row.
const AUTO_PAD: f32 = 6.0;
const POINT_R: f32 = 4.0;
const NAME_H: f32 = 13.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Draw,
    Paint,
    Delete,
    Slice,
    Select,
}

impl Tool {
    const ALL: [(Self, Glyph, &'static str); 5] = [
        (
            Self::Draw,
            Glyph::Draw,
            "Draw (P): click places the source, drag moves it",
        ),
        (
            Self::Paint,
            Glyph::Paint,
            "Paint (B): drag places a row of copies",
        ),
        (
            Self::Delete,
            Glyph::Delete,
            "Delete (D): click or drag over clips",
        ),
        (
            Self::Slice,
            Glyph::Slice,
            "Slice (C): click a clip to cut it",
        ),
        (
            Self::Select,
            Glyph::Select,
            "Select (E): drag a frame; Shift adds",
        ),
    ];
}

/// What Draw and Paint place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Pattern(PatternId),
    Asset(AssetId),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    Edge(ClipId, Edge),
    Loop(u64),
    Point(AutomationId, usize),
    /// Draw: a copy of the source follows the pointer until release.
    Place,
    /// Paint: copies from `start` on `lane` up to the pointer.
    Paint {
        lane: usize,
        start: u64,
    },
    Erase,
    Frame {
        add: bool,
    },
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
    pub tool: Tool,
    pub source: Option<Source>,
    /// Pattern current at the last frame: choosing another one makes it
    /// the source.
    last_pattern: Option<PatternId>,
    /// Clips touched by a Delete drag.
    erased: BTreeSet<ClipId>,
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
            tool: Tool::Draw,
            source: None,
            last_pattern: None,
            erased: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PlaylistAction {
    /// `count` consecutive copies of `source` from `start` ticks;
    /// `lane: None` = a new lane.
    Place {
        source: Source,
        lane: Option<LaneId>,
        start: u64,
        count: u32,
    },
    Duplicate(Vec<ClipId>),
    MakeUnique(ClipId),
    AddLane,
    RenameLane(LaneId),
    Seek(u64),
    /// Import a file as an audio clip; `lane: None` = a new lane.
    ImportAudio {
        path: PathBuf,
        lane: Option<LaneId>,
        start: u64,
    },
    /// Cut a clip at `tick`.
    Slice {
        clip: ClipId,
        tick: u64,
    },
    /// Double-click: the clip's editor.
    Open(ClipId),
}

#[derive(Debug, Default)]
pub(crate) struct PlaylistOutput {
    pub edits: Vec<Edit>,
    /// Edits of a continuous gesture (merged into one undo step).
    pub gesture: Vec<Edit>,
    pub end_gesture: bool,
    pub actions: Vec<PlaylistAction>,
    /// A short message for the user (nothing to place, …).
    pub hint: Option<String>,
}

fn color(c: [u8; 3]) -> Color32 {
    theme::rgb(c)
}

fn snap(ticks: i64, grid: u64) -> i64 {
    let g = grid.max(1) as i64;
    (ticks as f64 / g as f64).round() as i64 * g
}

/// A clip as drawn: kind, lane row and timeline span in ticks.
#[derive(Debug, Clone, Copy)]
struct Geo {
    item: Item,
    lane: usize,
    start: f64,
    end: f64,
}

#[derive(Debug, Clone, Copy)]
enum Item {
    Pattern(PatternClip),
    Audio(AudioClip),
}

impl Item {
    fn id(&self) -> ClipId {
        match self {
            Item::Pattern(c) => c.id,
            Item::Audio(c) => c.id,
        }
    }
}

fn span(project: &Project, item: &Item) -> Option<(f64, f64)> {
    match item {
        Item::Pattern(c) => {
            let (a, b) = c.interval();
            Some((a as f64, b as f64))
        }
        Item::Audio(c) => project.audio_clip_ticks(c),
    }
}

/// All clips, pattern clips first; the last one is drawn on top.
fn geos(project: &Project) -> Vec<Geo> {
    let pl = &project.playlist;
    let lane_index = |id: LaneId| pl.lanes.iter().position(|l| l.id == id);
    let items = pl
        .clips
        .iter()
        .map(|c| (c.lane, Item::Pattern(*c)))
        .chain(pl.audio_clips.iter().map(|c| (c.lane, Item::Audio(*c))));
    items
        .filter_map(|(lane, item)| {
            let (start, end) = span(project, &item)?;
            Some(Geo {
                item,
                lane: lane_index(lane)?,
                start,
                end,
            })
        })
        .collect()
}

/// Length in ticks of one copy of `source`.
fn source_len(project: &Project, source: Source) -> Option<f64> {
    match source {
        Source::Pattern(id) => project.pattern(id).map(|p| p.length_ticks() as f64),
        Source::Asset(id) => {
            let a = project.asset(id)?;
            let fpt = frames_per_tick(project.tempo_bpm, project.timebase_rate);
            Some(
                a.frames as f64 * f64::from(project.timebase_rate) / f64::from(a.sample_rate) / fpt,
            )
        }
    }
}

fn source_name(project: &Project, source: Source) -> Option<String> {
    match source {
        Source::Pattern(id) => project.pattern(id).map(|p| p.name.clone()),
        Source::Asset(id) => project.asset(id).map(|a| a.name.clone()),
    }
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
        let pl = &project.playlist;
        self.selection
            .retain(|id| pl.clip(*id).is_some() || pl.audio_clip(*id).is_some());
        let valid = match self.source {
            Some(Source::Pattern(id)) => project.pattern(id).is_some(),
            Some(Source::Asset(id)) => project.asset(id).is_some(),
            None => true,
        };
        if !valid {
            self.source = None;
        }
    }

    /// Selects only `ids` (e.g. clips just created).
    pub(crate) fn select_only(&mut self, ids: &[ClipId]) {
        self.selection = ids.iter().copied().collect();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        lanes: &[LaneInfo],
        playhead: Option<u64>,
        snap: u32,
        current_pattern: PatternId,
        host: &mut dyn Host,
        project_dir: Option<&Path>,
    ) -> PlaylistOutput {
        self.grid = u64::from(snap.max(1));
        if self.last_pattern != Some(current_pattern) {
            self.last_pattern = Some(current_pattern);
            self.source = Some(Source::Pattern(current_pattern));
        }
        let mut out = PlaylistOutput::default();
        self.retain_existing(project);
        self.header(ui, project);

        egui::ScrollArea::vertical()
            .id_salt("playlist-v")
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    self.lane_headers(ui, project, lanes, &mut out);
                    egui::ScrollArea::horizontal()
                        .id_salt("playlist-h")
                        .show(ui, |ui| {
                            self.canvas(ui, project, playhead, host, project_dir, &mut out);
                        });
                });
            });
        self.shortcuts(ui, &mut out);
        out
    }

    fn header(&mut self, ui: &mut egui::Ui, project: &Project) {
        ui.horizontal(|ui| {
            for (tool, glyph, tip) in Tool::ALL {
                if icons::tool_button(ui, glyph, self.tool == tool, tip).clicked() {
                    self.tool = tool;
                }
            }
            ui.separator();
            ui.label(RichText::new("Playlist").font(theme::strong(14.0)));
            ui.label(RichText::new("› Arrangement ›").color(theme::MUTED));
            match self.source.and_then(|s| source_name(project, s)) {
                Some(name) => {
                    ui.label(&name)
                        .on_hover_text("What Draw and Paint place (choose a pattern above)");
                }
                None => {
                    ui.label(RichText::new("no source").color(theme::WARN))
                        .on_hover_text("Choose a pattern or a file to place with Draw and Paint");
                }
            }
            ui.separator();
            ui.add(
                egui::Slider::new(&mut self.zoom, 30.0..=480.0)
                    .logarithmic(true)
                    .show_value(false)
                    .text("Zoom"),
            );
            if !self.selection.is_empty() {
                ui.label(
                    RichText::new(format!("{} selected", self.selection.len())).color(theme::MUTED),
                );
            }
        });
    }

    /// Delete, Ctrl+D and tool keys, when no text field has focus.
    fn shortcuts(&mut self, ui: &egui::Ui, out: &mut PlaylistOutput) {
        let ctx = ui.ctx().clone();
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (del, dup) = ctx.input_mut(|i| {
            let del = i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace);
            let dup = i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::D,
            ));
            (del, dup)
        });
        let key = |k| ctx.input(|i| i.key_pressed(k) && i.modifiers.is_none());
        for (k, t) in [
            (egui::Key::P, Tool::Draw),
            (egui::Key::B, Tool::Paint),
            (egui::Key::D, Tool::Delete),
            (egui::Key::C, Tool::Slice),
            (egui::Key::E, Tool::Select),
        ] {
            if key(k) {
                self.tool = t;
            }
        }
        if self.selection.is_empty() {
            return;
        }
        if del {
            out.edits.push(self.delete_edit(&self.selection.clone()));
            self.selection.clear();
        } else if dup {
            out.actions.push(PlaylistAction::Duplicate(
                self.selection.iter().copied().collect(),
            ));
        }
    }

    /// One undo step removing `ids` (pattern or audio clips).
    fn delete_edit(&self, ids: &BTreeSet<ClipId>) -> Edit {
        Edit::Batch(
            ids.iter()
                .map(|id| Edit::Batch(vec![Edit::RemoveClip(*id)]))
                .collect(),
        )
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
            // Rows line up with the canvas: no spacing between them.
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(RULER_H);
            for lane in &project.playlist.lanes {
                let (row, _) = ui.allocate_exact_size(vec2(HEADER_W, ROW_H), Sense::hover());
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(row.shrink2(vec2(2.0, 3.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let ui = &mut child;
                ui.spacing_mut().item_spacing.x = 4.0;
                let (bar, resp) = ui.allocate_exact_size(vec2(4.0, ROW_H - 8.0), Sense::click());
                ui.painter().rect_filled(bar, 2.0, color(lane.color));
                if resp
                    .on_hover_text("Lane color: click for the next one")
                    .clicked()
                {
                    let i = jdance_domain::LANE_COLORS
                        .iter()
                        .position(|c| *c == lane.color)
                        .map_or(0, |i| (i + 1) % jdance_domain::LANE_COLORS.len());
                    out.edits
                        .push(Edit::SetLaneColor(lane.id, jdance_domain::LANE_COLORS[i]));
                }
                let name = ui.add_sized(
                    [74.0, 22.0],
                    egui::Button::new(RichText::new(&lane.name).size(12.0)).truncate(),
                );
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
                let lamp = crate::widgets::lamp(ui, !lane.muted)
                    .on_hover_text("Mute lane (click) · its pattern and audio clips do not play");
                if lamp.clicked() {
                    out.edits.push(Edit::SetLaneMuted(lane.id, !lane.muted));
                }
                let mut s = lane.solo;
                if ui
                    .toggle_value(&mut s, RichText::new("S").size(11.0))
                    .on_hover_text("Solo lane")
                    .changed()
                {
                    out.edits.push(Edit::SetLaneSolo(lane.id, s));
                }
                route_box(ui, project, lane.id, lane.audio_track, out);
            }
            let (row, _) = ui.allocate_exact_size(vec2(HEADER_W, ROW_H), Sense::hover());
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(row.shrink2(vec2(2.0, 4.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            if child
                .button("+ Lane")
                .on_hover_text("Add a lane (dropping a file here also adds one)")
                .clicked()
            {
                out.actions.push(PlaylistAction::AddLane);
            }
            for info in lanes {
                ui.horizontal_top(|ui| {
                    ui.set_height(AUTO_H - ui.spacing().item_spacing.y);
                    let color = if info.missing {
                        theme::WARN
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
                        ui.add(egui::Label::new(RichText::new(&info.name).color(color)).truncate())
                            .on_hover_text(hover);
                        ui.add(
                            egui::Label::new(RichText::new(&info.context).small().weak())
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
        });
    }

    #[allow(clippy::too_many_lines)]
    fn canvas(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        playhead: Option<u64>,
        host: &mut dyn Host,
        project_dir: Option<&Path>,
        out: &mut PlaylistOutput,
    ) {
        let pl = &project.playlist;
        let grid = self.grid;
        let ppt = self.px_per_tick();
        let fpt = frames_per_tick(project.tempo_bpm, project.timebase_rate);
        let end = project.arrangement_end_ticks();
        let bars = (end / BAR_TICKS + 8).max(16);
        let width = bars as f32 * self.zoom;
        let n_lanes = pl.lanes.len();
        // One extra row below the lanes: dropping there adds a lane.
        let lanes_bottom = RULER_H + (n_lanes + 1) as f32 * ROW_H;
        let height = lanes_bottom + project.automation.len() as f32 * AUTO_H;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let origin = rect.min;
        let x_of = |tick: u64| origin.x + tick as f32 * ppt;
        let xf = |tick: f64| origin.x + tick as f32 * ppt;
        let tick_at = |x: f32| (((x - origin.x) / ppt).max(0.0)) as i64;
        // `Some(n_lanes)` = the row that adds a lane.
        let lane_at = |y: f32| -> Option<usize> {
            let i = ((y - origin.y - RULER_H) / ROW_H).floor();
            (i >= 0.0 && (i as usize) <= n_lanes).then_some(i as usize)
        };
        let row_rect = |lane: usize, a: f64, b: f64| {
            let y0 = origin.y + RULER_H + lane as f32 * ROW_H + 2.0;
            Rect::from_min_max(
                pos2(xf(a), y0),
                pos2(xf(b).max(xf(a) + 3.0), y0 + ROW_H - 4.0),
            )
        };
        let auto = &project.automation;
        let auto_row = |i: usize| {
            let y = origin.y + lanes_bottom + i as f32 * AUTO_H;
            Rect::from_min_size(pos2(rect.left(), y), vec2(width, AUTO_H))
        };
        let auto_at = |y: f32| -> Option<usize> {
            let i = ((y - origin.y - lanes_bottom) / AUTO_H).floor();
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
        let point_hit = |i: usize, p: Pos2| -> Option<usize> {
            let row = auto_row(i);
            auto[i]
                .points
                .iter()
                .rposition(|pt| point_pos(row, pt).distance(p) <= POINT_R + 3.0)
        };
        let items = geos(project);
        // Topmost clip under `p` and the edge zone it is in. Narrow clips
        // keep a body zone in the middle third.
        let hit = |p: Pos2| -> Option<(usize, Option<Edge>)> {
            items.iter().enumerate().rev().find_map(|(k, g)| {
                let r = row_rect(g.lane, g.start, g.end);
                if !r.contains(p) {
                    return None;
                }
                let zone = EDGE_PX.min(r.width() / 3.0);
                let edge = if p.x < r.left() + zone {
                    Some(Edge::Left)
                } else if p.x > r.right() - zone {
                    Some(Edge::Right)
                } else {
                    None
                };
                Some((k, edge))
            })
        };
        let in_lanes = |p: Pos2| p.y >= origin.y + RULER_H && p.y < origin.y + lanes_bottom;

        self.background(
            &painter,
            rect,
            bars,
            n_lanes,
            lanes_bottom,
            auto.len(),
            &auto_row,
        );
        if let Some((a, b)) = pl.loop_range {
            let fill = if pl.loop_enabled {
                theme::AMBER.gamma_multiply(0.6)
            } else {
                theme::MUTED.gamma_multiply(0.3)
            };
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x_of(a), origin.y + RULER_H - 4.0),
                    pos2(x_of(b), origin.y + RULER_H),
                ),
                CornerRadius::ZERO,
                fill,
            );
        }

        // Input.
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let (shift, ctrl, alt) =
            ui.input(|i| (i.modifiers.shift, i.modifiers.command, i.modifiers.alt));
        let snapped = |x: f32| -> i64 {
            let t = tick_at(x);
            if alt { t } else { snap(t, grid) }.max(0)
        };
        let press = ui.input(|i| i.pointer.press_origin());

        if response.drag_started()
            && let Some(p) = press.or(response.interact_pointer_pos())
        {
            let kind = if p.y < origin.y + RULER_H {
                shift.then(|| DragKind::Loop(snap(tick_at(p.x), BAR_TICKS).max(0) as u64))
            } else if let Some(i) = auto_at(p.y) {
                point_hit(i, p).map(|k| DragKind::Point(auto[i].id, k))
            } else if ctrl {
                Some(DragKind::Frame { add: shift })
            } else {
                match (hit(p), self.tool) {
                    (_, Tool::Delete) => {
                        self.erased.clear();
                        Some(DragKind::Erase)
                    }
                    (_, Tool::Slice) => None,
                    (Some((k, edge)), _) => {
                        let id = items[k].item.id();
                        if !self.selection.contains(&id) {
                            if !shift {
                                self.selection.clear();
                            }
                            self.selection.insert(id);
                        }
                        Some(match edge {
                            Some(e) => DragKind::Edge(id, e),
                            None => DragKind::Move,
                        })
                    }
                    (None, Tool::Draw) if in_lanes(p) => Some(DragKind::Place),
                    (None, Tool::Paint) if in_lanes(p) => {
                        lane_at(p.y).map(|lane| DragKind::Paint {
                            lane,
                            start: snapped(p.x) as u64,
                        })
                    }
                    (None, _) => Some(DragKind::Frame { add: shift }),
                }
            };
            self.drag = kind.map(|kind| Drag { kind, origin: p });
        }

        // Current drag offsets for preview and commit.
        let (dt, dl) = match (self.drag, pointer) {
            (Some(d), Some(p)) => {
                let raw = ((p.x - d.origin.x) / ppt) as i64;
                let dt = if alt { raw } else { snap(raw, grid) };
                let dl = ((p.y - d.origin.y) / ROW_H).round() as i64;
                (dt, dl)
            }
            _ => (0, 0),
        };
        let drag_kind = self.drag.map(|d| d.kind);
        let selection = self.selection.clone();
        let edge_tick = pointer.map_or(0, |p| snapped(p.x));
        // A clip as the current drag would leave it.
        let moved = |g: &Geo| -> Geo {
            let selected = selection.contains(&g.item.id());
            let item = match (drag_kind, g.item) {
                (Some(DragKind::Move), Item::Pattern(c)) if selected => {
                    Item::Pattern(PatternClip {
                        start: (c.start as i64 + dt).max(0) as u64,
                        ..c
                    })
                }
                (Some(DragKind::Move), Item::Audio(c)) if selected => {
                    let df = (dt as f64 * fpt).round() as i64;
                    Item::Audio(AudioClip {
                        start: (c.start as i64 + df).max(0) as u64,
                        ..c
                    })
                }
                (Some(DragKind::Edge(id, e)), Item::Pattern(c)) if id == c.id => {
                    let len = project.pattern(c.pattern).map_or(1, |p| p.length_ticks());
                    let (start, offset, duration) = pattern_clip_edge(&c, len, e, edge_tick);
                    Item::Pattern(PatternClip {
                        start,
                        offset,
                        duration,
                        ..c
                    })
                }
                (Some(DragKind::Edge(id, e)), Item::Audio(c)) if id == c.id => {
                    match project.asset(c.asset) {
                        Some(a) => {
                            let at = (edge_tick as f64 * fpt).round() as i64;
                            Item::Audio(audio_clip_edge(&c, a, project.timebase_rate, e, at))
                        }
                        None => g.item,
                    }
                }
                (_, item) => item,
            };
            let lane = if matches!(drag_kind, Some(DragKind::Move)) && selected {
                (g.lane as i64 + dl).clamp(0, n_lanes.saturating_sub(1) as i64) as usize
            } else {
                g.lane
            };
            let (start, end) = span(project, &item).unwrap_or((g.start, g.end));
            Geo {
                item,
                lane,
                start,
                end,
            }
        };

        // Ghost of Draw / Paint copies.
        let ghosts: Vec<(usize, f64, f64)> = match (drag_kind, pointer, self.source) {
            (Some(DragKind::Place), Some(p), Some(src)) => {
                match (lane_at(p.y), source_len(project, src)) {
                    (Some(l), Some(len)) if l < n_lanes => {
                        let a = snapped(p.x) as f64;
                        vec![(l, a, a + len)]
                    }
                    _ => Vec::new(),
                }
            }
            (Some(DragKind::Paint { lane, start }), Some(p), Some(src)) if lane < n_lanes => {
                match source_len(project, src) {
                    Some(len) if len > 0.0 => {
                        let count = paint_count(start, tick_at(p.x), len);
                        (0..count)
                            .map(|k| {
                                let a = start as f64 + k as f64 * len;
                                (lane, a, a + len)
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        };

        // Delete sweep.
        if matches!(drag_kind, Some(DragKind::Erase))
            && response.dragged()
            && let Some(p) = pointer
            && let Some((k, _)) = hit(p)
        {
            self.erased.insert(items[k].item.id());
        }
        // Frame selection.
        if let (Some(DragKind::Frame { add }), Some(d), Some(p)) = (drag_kind, self.drag, pointer)
            && response.dragged()
        {
            let frame = Rect::from_two_pos(d.origin, p);
            let mut sel = if add {
                selection.clone()
            } else {
                BTreeSet::new()
            };
            for g in &items {
                if row_rect(g.lane, g.start, g.end).intersects(frame) {
                    sel.insert(g.item.id());
                }
            }
            self.selection = sel;
        }

        // A dragged automation point stays between its neighbours.
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
        let dragged_point = match drag_kind {
            Some(DragKind::Point(id, k)) => project
                .automation_lane(id)
                .and_then(|l| Some((id, k, moved_point(l, k)?))),
            _ => None,
        };

        // Commit on release: one undo step per gesture.
        if response.drag_stopped()
            && let Some(d) = self.drag.take()
        {
            match d.kind {
                DragKind::Move if dt != 0 || dl != 0 => {
                    let edits: Vec<Edit> = items
                        .iter()
                        .filter(|g| selection.contains(&g.item.id()))
                        .map(|g| commit(&moved(g), &pl.lanes))
                        .collect();
                    if !edits.is_empty() {
                        out.edits.push(Edit::Batch(edits));
                    }
                }
                DragKind::Edge(id, _) => {
                    if let Some(g) = items.iter().find(|g| g.item.id() == id) {
                        let after = moved(g);
                        if after.start != g.start || after.end != g.end {
                            out.edits.push(commit(&after, &pl.lanes));
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
                DragKind::Place | DragKind::Paint { .. } => {
                    if let (Some(&(lane, start, _)), Some(source)) = (ghosts.first(), self.source) {
                        out.actions.push(PlaylistAction::Place {
                            source,
                            lane: Some(pl.lanes[lane].id),
                            start: start as u64,
                            count: ghosts.len() as u32,
                        });
                    }
                }
                DragKind::Erase => {
                    let erased = std::mem::take(&mut self.erased);
                    if !erased.is_empty() {
                        self.selection.retain(|id| !erased.contains(id));
                        out.edits.push(delete_edits(project, &erased));
                    }
                }
                DragKind::Move | DragKind::Frame { .. } => {}
            }
        }

        // Clicks.
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            if p.y < origin.y + RULER_H {
                out.actions
                    .push(PlaylistAction::Seek(snap(tick_at(p.x), grid).max(0) as u64));
            } else if in_lanes(p) {
                match (hit(p), self.tool) {
                    (Some((k, _)), Tool::Delete) => {
                        let id = items[k].item.id();
                        self.selection.remove(&id);
                        out.edits.push(delete_edits(project, &BTreeSet::from([id])));
                    }
                    (Some((k, _)), Tool::Slice) => {
                        out.actions.push(PlaylistAction::Slice {
                            clip: items[k].item.id(),
                            tick: snapped(p.x) as u64,
                        });
                    }
                    (Some((k, _)), _) => {
                        let id = items[k].item.id();
                        if shift {
                            if !self.selection.remove(&id) {
                                self.selection.insert(id);
                            }
                        } else {
                            self.selection = BTreeSet::from([id]);
                        }
                    }
                    (None, Tool::Draw | Tool::Paint) => match (lane_at(p.y), self.source) {
                        (Some(l), Some(source)) if l < n_lanes => {
                            out.actions.push(PlaylistAction::Place {
                                source,
                                lane: Some(pl.lanes[l].id),
                                start: snapped(p.x) as u64,
                                count: 1,
                            });
                        }
                        (_, None) => {
                            out.hint = Some("Choose a pattern or a file to place first".into());
                        }
                        _ => {}
                    },
                    (None, _) => self.selection.clear(),
                }
            }
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            if let Some(i) = auto_at(p.y) {
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
                        let (curve, tension) =
                            at.checked_sub(1).map_or((Curve::Linear, 0.0), |j| {
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
            } else if let Some((k, _)) = hit(p) {
                out.actions.push(PlaylistAction::Open(items[k].item.id()));
            }
        }

        // Files from the Browser or the system.
        let drop_at = |p: Pos2| -> (Option<LaneId>, u64) {
            let lane = lane_at(p.y).and_then(|l| pl.lanes.get(l)).map(|l| l.id);
            (lane, snapped(p.x) as u64)
        };
        let mut drop_ghost: Option<(Pos2, String)> = None;
        if let Some(item) = response.dnd_hover_payload::<BrowserItem>()
            && let BrowserItem::Sample(path) = &*item
            && let Some(p) = pointer
            && in_lanes(p)
        {
            drop_ghost = Some((p, display_name(path)));
        }
        if let Some(item) = response.dnd_hover_payload::<ProjectItem>()
            && let Some(p) = pointer
            && in_lanes(p)
        {
            drop_ghost = source_name(project, item.0).map(|n| (p, n));
        }
        if let Some(item) = response.dnd_release_payload::<ProjectItem>()
            && let Some(p) = pointer
            && in_lanes(p)
        {
            let (lane, start) = drop_at(p);
            self.source = Some(item.0);
            out.actions.push(PlaylistAction::Place {
                source: item.0,
                lane,
                start,
                count: 1,
            });
        }
        if let Some(item) = response.dnd_release_payload::<BrowserItem>()
            && let BrowserItem::Sample(path) = &*item
            && let Some(p) = pointer
            && in_lanes(p)
        {
            let (lane, start) = drop_at(p);
            out.actions.push(PlaylistAction::ImportAudio {
                path: path.clone(),
                lane,
                start,
            });
        }
        let hover_pos = ui.ctx().pointer_latest_pos().filter(|p| rect.contains(*p));
        let (dropped, hovered): (Vec<PathBuf>, Vec<PathBuf>) = ui.ctx().input(|i| {
            (
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .collect(),
                i.raw
                    .hovered_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect(),
            )
        });
        if let Some(p) = hover_pos.filter(|p| in_lanes(*p)) {
            if let Some(first) = hovered.first() {
                drop_ghost = Some((p, display_name(first)));
            }
            for (k, path) in dropped.into_iter().enumerate() {
                // Several files go on consecutive lanes.
                let row = lane_at(p.y).map(|l| l + k);
                let lane = row.and_then(|l| pl.lanes.get(l)).map(|l| l.id);
                out.actions.push(PlaylistAction::ImportAudio {
                    path,
                    lane,
                    start: snapped(p.x) as u64,
                });
            }
        }

        if response.secondary_clicked() {
            let p = response.interact_pointer_pos();
            self.context_auto = p.and_then(|p| auto_at(p.y).map(|i| (auto[i].id, point_hit(i, p))));
            self.context_clip = p.and_then(hit).map(|(k, _)| items[k].item.id());
            if let Some(id) = self.context_clip
                && !self.selection.contains(&id)
            {
                self.selection = BTreeSet::from([id]);
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
                self.clip_menu(ui, project, id, out);
            });

        // Clips (with drag preview).
        let visible = ui.clip_rect();
        for g in &items {
            let g = moved(g);
            let r = row_rect(g.lane, g.start, g.end);
            if !r.intersects(visible) {
                continue;
            }
            let lane = &pl.lanes[g.lane];
            let erased = self.erased.contains(&g.item.id());
            let selected = self.selection.contains(&g.item.id());
            let audible = !erased
                && match &g.item {
                    Item::Pattern(c) => pl.admits(c),
                    Item::Audio(c) => pl.admits_audio(c),
                };
            draw_clip(
                ui,
                &painter,
                project,
                &g,
                r,
                color(lane.color),
                audible,
                selected,
                ppt,
                host,
                project_dir,
            );
        }
        for &(lane, a, b) in &ghosts {
            let r = row_rect(lane, a, b);
            painter.rect_filled(r, 4.0, theme::BLUE.gamma_multiply(0.25));
            painter.rect_stroke(r, 4.0, Stroke::new(1.0, theme::BLUE), StrokeKind::Inside);
        }
        if let (Some(DragKind::Frame { .. }), Some(d), Some(p)) = (drag_kind, self.drag, pointer) {
            let frame = Rect::from_two_pos(d.origin, p);
            painter.rect_filled(frame, 0.0, theme::BLUE.gamma_multiply(0.08));
            painter.rect_stroke(
                frame,
                0.0,
                Stroke::new(1.0, theme::BLUE),
                StrokeKind::Inside,
            );
        }
        if let Some((p, name)) = drop_ghost {
            let (lane, start) = drop_at(p);
            let where_ = match lane {
                Some(_) => bars_text(start),
                None => format!("{} · new lane", bars_text(start)),
            };
            let row = lane_at(p.y).unwrap_or(n_lanes);
            let r = Rect::from_min_size(
                pos2(x_of(start), origin.y + RULER_H + row as f32 * ROW_H + 2.0),
                vec2(140.0, ROW_H - 4.0),
            );
            painter.rect_filled(r, 4.0, theme::TEAL.gamma_multiply(0.3));
            painter.rect_stroke(r, 4.0, Stroke::new(1.0, theme::TEAL), StrokeKind::Inside);
            painter.text(
                r.left_center() + vec2(5.0, 0.0),
                Align2::LEFT_CENTER,
                format!("{name} · {where_}"),
                FontId::proportional(11.0),
                theme::TEXT,
            );
        }

        self.automation_rows(
            &painter,
            project,
            dragged_point,
            &auto_row,
            &x_of,
            &point_pos,
            visible,
            rect,
        );
        if let (Some(DragKind::Loop(a)), Some(p)) = (drag_kind, pointer) {
            let b = snap(tick_at(p.x), BAR_TICKS).max(0) as u64;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x_of(a.min(b)), origin.y),
                    pos2(x_of(a.max(b)), origin.y + RULER_H),
                ),
                CornerRadius::ZERO,
                theme::AMBER.gamma_multiply(0.35),
            );
        }
        if let Some(pos) = playhead {
            let x = x_of(pos);
            painter.line_segment(
                [pos2(x, origin.y), pos2(x, rect.bottom())],
                Stroke::new(2.0, Color32::from_rgb(0xF2, 0xF4, 0xF6)),
            );
        }
        // Cursors.
        if self.drag.is_none()
            && let Some(p) = response.hover_pos()
        {
            if let Some(i) = auto_at(p.y)
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
                    theme::TEXT,
                );
            } else if let Some((_, edge)) = hit(p) {
                let icon = match (self.tool, edge) {
                    (Tool::Delete, _) => egui::CursorIcon::NotAllowed,
                    (Tool::Slice, _) => egui::CursorIcon::VerticalText,
                    (_, Some(_)) => egui::CursorIcon::ResizeHorizontal,
                    (_, None) => egui::CursorIcon::Grab,
                };
                ui.ctx().set_cursor_icon(icon);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn background(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        bars: u64,
        n_lanes: usize,
        lanes_bottom: f32,
        n_auto: usize,
        auto_row: &impl Fn(usize) -> Rect,
    ) {
        let origin = rect.min;
        let bottom = origin.y + lanes_bottom;
        painter.rect_filled(rect, CornerRadius::ZERO, theme::BG);
        // Every other bar slightly lighter: bars read without lines.
        for b in (0..bars).filter(|b| b % 2 == 1) {
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(origin.x + b as f32 * self.zoom, origin.y + RULER_H),
                    pos2(origin.x + (b + 1) as f32 * self.zoom, bottom),
                ),
                CornerRadius::ZERO,
                Color32::from_white_alpha(4),
            );
        }
        for i in 1..=n_lanes + 1 {
            let y = origin.y + RULER_H + i as f32 * ROW_H;
            painter.line_segment(
                [pos2(rect.left(), y), pos2(rect.right(), y)],
                Stroke::new(1.0, theme::LINE),
            );
        }
        painter.text(
            pos2(
                rect.left() + 8.0,
                origin.y + RULER_H + n_lanes as f32 * ROW_H + ROW_H / 2.0,
            ),
            Align2::LEFT_CENTER,
            "Drop a file here for a new lane",
            FontId::proportional(11.0),
            theme::MUTED.gamma_multiply(0.6),
        );
        for i in 0..n_auto {
            let row = auto_row(i);
            painter.rect_filled(row, CornerRadius::ZERO, theme::PANEL);
            painter.line_segment(
                [row.left_top(), row.right_top()],
                Stroke::new(1.0, theme::LINE),
            );
        }
        painter.rect_filled(
            Rect::from_min_size(origin, vec2(rect.width(), RULER_H)),
            CornerRadius::ZERO,
            theme::PANEL,
        );
        let beat_px = self.zoom / 4.0;
        for b in 0..=bars * 4 {
            let x = origin.x + b as f32 * beat_px;
            let bar_line = b % 4 == 0;
            if !bar_line && beat_px < 12.0 {
                continue;
            }
            let c = if bar_line {
                theme::LINE_STRONG
            } else {
                theme::LINE
            };
            painter.line_segment(
                [pos2(x, origin.y + RULER_H), pos2(x, bottom - ROW_H)],
                Stroke::new(1.0, c),
            );
            if bar_line {
                painter.line_segment(
                    [pos2(x, origin.y + 12.0), pos2(x, origin.y + RULER_H)],
                    Stroke::new(1.0, theme::LINE_STRONG),
                );
                painter.text(
                    pos2(x + 3.0, origin.y + 2.0),
                    Align2::LEFT_TOP,
                    format!("{}", b / 4 + 1),
                    theme::mono(11.0),
                    theme::MUTED,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn automation_rows(
        &self,
        painter: &egui::Painter,
        project: &Project,
        dragged_point: Option<(AutomationId, usize, AutoPoint)>,
        auto_row: &impl Fn(usize) -> Rect,
        x_of: &impl Fn(u64) -> f32,
        point_pos: &impl Fn(Rect, &AutoPoint) -> Pos2,
        visible: Rect,
        rect: Rect,
    ) {
        let span = (
            visible.left().max(rect.left()),
            visible.right().min(rect.right()),
        );
        for (i, lane) in project.automation.iter().enumerate() {
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
                theme::MUTED
            } else {
                AUTOMATION_COLOR
            };
            if lane.points.is_empty() {
                painter.text(
                    pos2(span.0 + 6.0, row.center().y),
                    Align2::LEFT_CENTER,
                    "No points: double-click to add",
                    FontId::proportional(11.0),
                    theme::MUTED,
                );
                continue;
            }
            painter.add(egui::Shape::line(
                envelope_line(lane, row, x_of, span),
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
        }
    }

    fn clip_menu(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        id: ClipId,
        out: &mut PlaylistOutput,
    ) {
        let pl = &project.playlist;
        let muted = match (pl.clip(id), pl.audio_clip(id)) {
            (Some(c), _) => c.muted,
            (_, Some(c)) => c.muted,
            _ => return,
        };
        if ui.button("Open editor").clicked() {
            out.actions.push(PlaylistAction::Open(id));
            ui.close();
        }
        if let Some(c) = pl.clip(id) {
            if ui
                .button("Make unique")
                .on_hover_text("Copy the pattern for this clip only")
                .clicked()
            {
                out.actions.push(PlaylistAction::MakeUnique(id));
                ui.close();
            }
            let label = if c.looped {
                "Stop looping the pattern"
            } else {
                "Loop the pattern"
            };
            if ui
                .button(label)
                .on_hover_text("A looped clip repeats its pattern for its whole length")
                .clicked()
            {
                let len = project.pattern(c.pattern).map_or(1, |p| p.length_ticks());
                let duration = if c.looped {
                    c.duration.min(len.saturating_sub(c.offset % len).max(1))
                } else {
                    c.duration
                };
                out.edits.push(Edit::SetClipShape {
                    id,
                    start: c.start,
                    offset: if c.looped { c.offset % len } else { c.offset },
                    duration,
                    looped: !c.looped,
                });
                ui.close();
            }
        }
        let label = if muted { "Unmute" } else { "Mute" };
        if ui.button(label).clicked() {
            let edits = self
                .selection
                .iter()
                .filter_map(|c| mute_edit(project, *c, !muted))
                .collect();
            out.edits.push(Edit::Batch(edits));
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            out.actions.push(PlaylistAction::Duplicate(
                self.selection.iter().copied().collect(),
            ));
            ui.close();
        }
        if ui.button("Delete").clicked() {
            out.edits.push(delete_edits(project, &self.selection));
            self.selection.clear();
            ui.close();
        }
    }
}

/// How many copies Paint lays from `start` to the pointer at `to` ticks
/// (at least one; positions the pointer skipped are filled).
fn paint_count(start: u64, to: i64, len: f64) -> usize {
    let n = ((to as f64 - start as f64) / len).floor() as i64 + 1;
    n.clamp(1, 1024) as usize
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// The edit that stores a moved or resized clip.
fn commit(g: &Geo, lanes: &[jdance_domain::Lane]) -> Edit {
    let lane = lanes.get(g.lane).map_or(LaneId(0), |l| l.id);
    match g.item {
        Item::Pattern(c) => Edit::Batch(vec![
            Edit::MoveClip {
                id: c.id,
                start: c.start,
                lane,
            },
            Edit::SetClipShape {
                id: c.id,
                start: c.start,
                offset: c.offset,
                duration: c.duration,
                looped: c.looped,
            },
        ]),
        Item::Audio(c) => Edit::SetAudioClip(AudioClip { lane, ..c }),
    }
}

/// One undo step removing `ids` (pattern or audio clips).
fn delete_edits(project: &Project, ids: &BTreeSet<ClipId>) -> Edit {
    let pl = &project.playlist;
    Edit::Batch(
        ids.iter()
            .filter_map(|id| {
                if pl.clip(*id).is_some() {
                    Some(Edit::RemoveClip(*id))
                } else if pl.audio_clip(*id).is_some() {
                    Some(Edit::RemoveAudioClip(*id))
                } else {
                    None
                }
            })
            .collect(),
    )
}

fn mute_edit(project: &Project, id: ClipId, muted: bool) -> Option<Edit> {
    let pl = &project.playlist;
    if pl.clip(id).is_some() {
        return Some(Edit::SetClipMuted(id, muted));
    }
    pl.audio_clip(id)
        .map(|c| Edit::SetAudioClip(AudioClip { muted, ..*c }))
}

/// Mixer track box of a lane's audio clips (UI-VIS-036).
fn route_box(
    ui: &mut egui::Ui,
    project: &Project,
    lane: LaneId,
    track: TrackId,
    out: &mut PlaylistOutput,
) {
    let number = crate::rack::track_number(project, track);
    let name = project
        .mixer
        .track(track)
        .map_or_else(|| "?".to_owned(), |t| t.name.clone());
    egui::ComboBox::from_id_salt(("lane_route", lane.0))
        .width(34.0)
        .selected_text(RichText::new(format!("{number:<2}")).font(theme::mono(11.0)))
        .show_ui(ui, |ui| {
            for t in project.mixer.all_tracks() {
                let label = format!("{} · {}", crate::rack::track_number(project, t.id), t.name);
                if ui.selectable_label(t.id == track, label).clicked() && t.id != track {
                    out.edits.push(Edit::SetLaneAudioTrack(lane, t.id));
                }
            }
        })
        .response
        .on_hover_text(format!(
            "Audio clips on this lane play through mixer track {number} ({name}). \
             Pattern clips use their channels' own routing."
        ));
}

/// Draws one clip: body, name strip, and its content preview (pattern
/// steps and notes, or the audio waveform).
#[allow(clippy::too_many_arguments)]
fn draw_clip(
    ui: &egui::Ui,
    painter: &egui::Painter,
    project: &Project,
    g: &Geo,
    r: Rect,
    lane_color: Color32,
    audible: bool,
    selected: bool,
    ppt: f32,
    host: &dyn Host,
    project_dir: Option<&Path>,
) {
    let alpha = if audible { 0.75 } else { 0.3 };
    painter.rect_filled(r, 4.0, lane_color.gamma_multiply(alpha * 0.55));
    let strip = Rect::from_min_size(r.min, vec2(r.width(), NAME_H.min(r.height())));
    painter.rect_filled(
        strip,
        CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        },
        lane_color.gamma_multiply(alpha),
    );
    let visible = ui.clip_rect().intersect(r);
    let body = Rect::from_min_max(pos2(r.left(), strip.bottom()), r.max).intersect(visible);
    let ink = Color32::from_black_alpha(if audible { 150 } else { 80 });
    let (name, muted) = match &g.item {
        Item::Pattern(c) => {
            let Some(pat) = project.pattern(c.pattern) else {
                return;
            };
            draw_pattern_preview(painter, project, c, pat, r, body, ppt, ink);
            (pat.name.clone(), c.muted)
        }
        Item::Audio(c) => {
            let Some(asset) = project.asset(c.asset) else {
                return;
            };
            let peaks = host.sample_peaks(
                &SampleRef {
                    path: asset.path.clone(),
                },
                project_dir,
            );
            match peaks {
                Some(peaks) => draw_waveform(painter, c, asset, &peaks, r, body, ink),
                None => {
                    painter.text(
                        body.left_center() + vec2(4.0, 0.0),
                        Align2::LEFT_CENTER,
                        "loading…",
                        FontId::proportional(10.0),
                        ink,
                    );
                }
            }
            (asset.name.clone(), c.muted)
        }
    };
    if muted {
        // Diagonal hatching over muted clips.
        let step = 8.0;
        let mut x = r.left() - r.height();
        while x < r.right() {
            let a = pos2(x.max(r.left()), r.bottom() - (x.max(r.left()) - x));
            let b = pos2(
                (x + r.height()).min(r.right()),
                r.bottom() - ((x + r.height()).min(r.right()) - x),
            );
            painter.line_segment([a, b], Stroke::new(1.0, Color32::from_black_alpha(70)));
            x += step;
        }
    }
    let stroke = if selected {
        Stroke::new(2.0, Color32::from_rgb(0xE9, 0xF3, 0xFF))
    } else {
        Stroke::new(1.0, lane_color.gamma_multiply(0.9))
    };
    painter.rect_stroke(r, 4.0, stroke, StrokeKind::Inside);
    painter
        .with_clip_rect(strip.intersect(ui.clip_rect()))
        .text(
            pos2(r.left().max(visible.left()) + 4.0, strip.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(10.5),
            if audible {
                theme::ON_AMBER
            } else {
                theme::MUTED
            },
        );
}

/// Steps and notes of the pattern repetitions inside the clip, as short
/// marks; repetition starts as faint lines.
#[allow(clippy::too_many_arguments)]
fn draw_pattern_preview(
    painter: &egui::Painter,
    project: &Project,
    c: &PatternClip,
    pat: &jdance_domain::Pattern,
    r: Rect,
    body: Rect,
    ppt: f32,
    ink: Color32,
) {
    if body.height() < 4.0 {
        return;
    }
    let len = pat.length_ticks().max(1);
    let (lo, hi) = c.interval();
    let offset = if c.looped { c.offset % len } else { c.offset };
    let rows: Vec<&jdance_domain::StepRow> = pat
        .rows
        .iter()
        .filter(|row| project.channel(row.channel).is_some())
        .filter(|row| row.steps.iter().any(|s| s.on) || !row.notes.is_empty())
        .collect();
    let row_h = (body.height() / rows.len().max(1) as f32).min(5.0);
    let x_of = |t: i64| r.left() + (t - lo as i64) as f32 * ppt;
    let mut base = lo as i64 - offset as i64;
    let mut reps = 0;
    while base < hi as i64 && reps < 4096 {
        let rep_x = x_of(base);
        if base > lo as i64 && rep_x > body.left() && rep_x < body.right() {
            painter.line_segment(
                [pos2(rep_x, body.top()), pos2(rep_x, body.bottom())],
                Stroke::new(1.0, Color32::from_black_alpha(70)),
            );
        }
        if rep_x > body.right() {
            break;
        }
        let rep_end = (base + len as i64).min(hi as i64);
        if x_of(rep_end) >= body.left() {
            for (k, row) in rows.iter().enumerate() {
                let y = body.top() + 1.0 + k as f32 * row_h;
                for (i, s) in row.steps.iter().enumerate().take(pat.length_steps as usize) {
                    let t = base + (i as i64) * i64::from(TICKS_PER_STEP);
                    if s.on && t >= lo as i64 && t < rep_end {
                        let x = x_of(t);
                        painter.line_segment(
                            [
                                pos2(x, y),
                                pos2(x + (TICKS_PER_STEP as f32 * ppt * 0.6).max(1.0), y),
                            ],
                            Stroke::new(row_h.max(1.0) - 0.5, ink),
                        );
                    }
                }
                for n in &row.notes {
                    let t = base + i64::from(n.start);
                    if t >= lo as i64 && t < rep_end {
                        let e = (t + i64::from(n.length)).min(rep_end);
                        painter.line_segment(
                            [pos2(x_of(t), y), pos2(x_of(e).max(x_of(t) + 1.0), y)],
                            Stroke::new(row_h.max(1.0) - 0.5, ink),
                        );
                    }
                }
            }
        }
        if !c.looped {
            break;
        }
        base += len as i64;
        reps += 1;
    }
}

/// Peak waveform of the clip's source range (peaks cover the whole file).
fn draw_waveform(
    painter: &egui::Painter,
    c: &AudioClip,
    asset: &AudioAsset,
    peaks: &[f32],
    r: Rect,
    body: Rect,
    ink: Color32,
) {
    if peaks.is_empty() || body.width() < 1.0 || body.height() < 3.0 {
        return;
    }
    let frames = asset.frames.max(1) as f64;
    let src = (c.source_out - c.source_in) as f64;
    let mid = body.center().y;
    let half = body.height() / 2.0 - 1.0;
    let mut x = body.left().floor();
    while x < body.right() {
        let t0 = ((x - r.left()) / r.width()) as f64;
        let t1 = ((x + 1.0 - r.left()) / r.width()) as f64;
        let f0 = c.source_in as f64 + t0 * src;
        let f1 = c.source_in as f64 + t1 * src;
        let b0 = ((f0 / frames) * peaks.len() as f64) as usize;
        let b1 = (((f1 / frames) * peaks.len() as f64).ceil() as usize).max(b0 + 1);
        let peak = peaks[b0.min(peaks.len() - 1)..b1.min(peaks.len())]
            .iter()
            .fold(0.0f32, |m, v| m.max(*v));
        let h = (peak.min(1.0) * half).max(0.5);
        painter.line_segment(
            [pos2(x + 0.5, mid - h), pos2(x + 0.5, mid + h)],
            Stroke::new(1.0, ink),
        );
        x += 1.0;
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
