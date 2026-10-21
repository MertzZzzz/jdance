// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano roll for one channel of one pattern.
//!
//! Double-click adds a note, drag moves it (or resizes it at its right
//! edge), right-click or Delete removes, Shift-click adds to the
//! selection. The velocity lane below sets velocities by dragging. Every
//! drag is one undo step. Only visible notes are drawn, so large rolls
//! stay cheap.

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use jdance_commands::Edit;
use jdance_domain::{ChannelId, MAX_NOTES_PER_ROW, Note, Pattern, Project, TICKS_PER_STEP};

const KEYS: u8 = 128;
const KEYBOARD_W: f32 = 44.0;
const VELOCITY_H: f32 = 64.0;
const EDGE_GRAB: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    Resize,
    Velocity,
}

#[derive(Debug, Clone)]
struct Drag {
    kind: DragKind,
    /// Notes when the drag started and which of them are dragged.
    original: Vec<Note>,
    dragged: Vec<usize>,
    start: Pos2,
}

#[derive(Debug, Clone)]
pub(crate) struct PianoRoll {
    pub channel: ChannelId,
    /// Pixels per step.
    zoom: f32,
    key_h: f32,
    /// Snap in ticks (0 = off).
    snap: u32,
    /// Length of new notes in ticks (the last one placed or resized).
    new_len: u32,
    selection: Vec<Note>,
    drag: Option<Drag>,
    scrolled: bool,
}

#[derive(Debug, Default)]
pub(crate) struct PianoOutput {
    pub edits: Vec<(Edit, bool)>,
    pub end_gesture: bool,
    pub close: bool,
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// `C5` for key 60 (the sampler's root key).
pub(crate) fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[usize::from(key % 12)], key / 12)
}

impl PianoRoll {
    pub fn new(channel: ChannelId) -> Self {
        Self {
            channel,
            zoom: 28.0,
            key_h: 12.0,
            snap: TICKS_PER_STEP,
            new_len: TICKS_PER_STEP,
            selection: Vec::new(),
            drag: None,
            scrolled: false,
        }
    }

    fn snap_tick(&self, t: f32) -> u32 {
        let t = t.max(0.0);
        if self.snap == 0 {
            t.round() as u32
        } else {
            ((t / self.snap as f32).floor() as u32) * self.snap
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        pattern: &Pattern,
        playhead_tick: Option<f32>,
        snap: u32,
    ) -> PianoOutput {
        self.snap = snap;
        let mut out = PianoOutput::default();
        let channel_name = project
            .channel(self.channel)
            .map_or("?", |c| c.name.as_str());
        let notes: Vec<Note> = pattern
            .row(self.channel)
            .map(|r| r.notes.clone())
            .unwrap_or_default();
        self.selection.retain(|s| notes.contains(s));

        ui.horizontal(|ui| {
            ui.strong(format!("Piano roll · {channel_name} · {}", pattern.name));
            ui.separator();
            ui.label("Zoom");
            ui.add(egui::Slider::new(&mut self.zoom, 8.0..=96.0).show_value(false));
            ui.weak(format!("{} note(s)", notes.len()));
            ui.weak("Double-click: add · drag: move/resize · right-click/Del: delete");
            if ui.button("Close").clicked() {
                out.close = true;
            }
        });

        let px_tick = self.zoom / TICKS_PER_STEP as f32;
        let pattern_end = pattern.length_ticks() as u32;
        let content_end = notes
            .iter()
            .map(Note::end)
            .max()
            .unwrap_or(0)
            .max(pattern_end)
            + 16 * TICKS_PER_STEP;
        let grid_w = content_end as f32 * px_tick;
        let grid_h = f32::from(KEYS) * self.key_h;

        let avail = ui.available_size();
        let roll_h = (avail.y - VELOCITY_H - 8.0).max(80.0);
        let mut scroll = egui::ScrollArea::both()
            .id_salt("piano_roll")
            .max_height(roll_h)
            .auto_shrink([false, false]);
        if !self.scrolled {
            // Start around C5.
            scroll = scroll.vertical_scroll_offset((f32::from(KEYS - 72) * self.key_h).max(0.0));
            self.scrolled = true;
        }
        let mut h_offset = 0.0;
        let scroll_out = scroll.show(ui, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(vec2(KEYBOARD_W + grid_w, grid_h), Sense::click_and_drag());
            let clip = ui.clip_rect();
            let grid = Rect::from_min_size(
                pos2(rect.left() + KEYBOARD_W, rect.top()),
                vec2(grid_w, grid_h),
            );
            let x_of = |t: u32| grid.left() + t as f32 * px_tick;
            let y_of = |k: u8| grid.top() + f32::from(KEYS - 1 - k) * self.key_h;
            let painter = ui.painter_at(clip);
            let visuals = ui.visuals().clone();

            // Background rows and grid.
            let first_key = (((grid.bottom() - clip.bottom()) / self.key_h)
                .floor()
                .max(0.0)) as u8;
            let last_key = ((((grid.bottom() - clip.top()) / self.key_h).ceil()) as u32)
                .min(u32::from(KEYS - 1)) as u8;
            for k in first_key..=last_key {
                let y = y_of(k);
                let row = Rect::from_min_size(pos2(grid.left(), y), vec2(grid_w, self.key_h));
                let base = if is_black(k) {
                    visuals.extreme_bg_color
                } else {
                    visuals.faint_bg_color
                };
                painter.rect_filled(row, CornerRadius::ZERO, base);
                if k % 12 == 0 {
                    painter.line_segment(
                        [
                            pos2(grid.left(), y + self.key_h),
                            pos2(grid.right(), y + self.key_h),
                        ],
                        Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.5)),
                    );
                }
            }
            let first_step = ((clip.left() - grid.left()) / self.zoom).floor().max(0.0) as u32;
            let last_step = ((clip.right() - grid.left()) / self.zoom).ceil() as u32;
            for s in first_step..=last_step.min(content_end / TICKS_PER_STEP) {
                let x = x_of(s * TICKS_PER_STEP);
                let w = if s % 16 == 0 {
                    1.5
                } else if s % 4 == 0 {
                    1.0
                } else {
                    0.5
                };
                let c =
                    visuals
                        .weak_text_color()
                        .gamma_multiply(if s % 4 == 0 { 0.6 } else { 0.25 });
                painter.line_segment(
                    [pos2(x, grid.top()), pos2(x, grid.bottom())],
                    Stroke::new(w, c),
                );
            }
            // Beyond the pattern end: dimmed (notes there do not play).
            let end_x = x_of(pattern_end);
            painter.rect_filled(
                Rect::from_min_max(pos2(end_x, grid.top()), grid.max),
                CornerRadius::ZERO,
                Color32::from_black_alpha(90),
            );
            painter.line_segment(
                [pos2(end_x, grid.top()), pos2(end_x, grid.bottom())],
                Stroke::new(2.0, visuals.warn_fg_color),
            );

            // Notes (visible ones only).
            let note_rect = |n: &Note| {
                Rect::from_min_max(
                    pos2(x_of(n.start), y_of(n.key)),
                    pos2(
                        x_of(n.end()).max(x_of(n.start) + 3.0),
                        y_of(n.key) + self.key_h,
                    ),
                )
            };
            for n in &notes {
                let r = note_rect(n);
                if !r.intersects(clip) {
                    continue;
                }
                let selected = self.selection.contains(n);
                let fill = Color32::from_rgb(235, 140, 60).gamma_multiply(0.45 + 0.55 * n.velocity);
                painter.rect_filled(r.shrink(0.5), CornerRadius::same(2), fill);
                let stroke = if selected {
                    Stroke::new(2.0, Color32::WHITE)
                } else {
                    Stroke::new(1.0, Color32::from_black_alpha(160))
                };
                painter.rect_stroke(
                    r.shrink(0.5),
                    CornerRadius::same(2),
                    stroke,
                    StrokeKind::Inside,
                );
            }
            if let Some(t) = playhead_tick {
                let x = x_of(t as u32);
                painter.line_segment(
                    [pos2(x, grid.top()), pos2(x, grid.bottom())],
                    Stroke::new(2.0, Color32::from_rgb(120, 200, 255)),
                );
            }

            // Keyboard, pinned to the left edge of the view.
            let kb_left = clip.left().max(rect.left());
            for k in first_key..=last_key {
                let y = y_of(k);
                let r = Rect::from_min_size(pos2(kb_left, y), vec2(KEYBOARD_W, self.key_h));
                let c = if is_black(k) {
                    Color32::from_gray(30)
                } else {
                    Color32::from_gray(220)
                };
                painter.rect_filled(r.shrink(0.5), CornerRadius::ZERO, c);
                if k % 12 == 0 {
                    painter.text(
                        pos2(r.right() - 3.0, r.center().y),
                        egui::Align2::RIGHT_CENTER,
                        key_name(k),
                        FontId::proportional(9.0),
                        Color32::from_gray(40),
                    );
                }
            }

            // Interaction.
            let pointer = response.interact_pointer_pos().or(response.hover_pos());
            let hit = |p: Pos2| notes.iter().rposition(|n| note_rect(n).contains(p));
            let to_tick = |x: f32| (x - grid.left()) / px_tick;
            let to_key = |y: f32| {
                (f32::from(KEYS - 1) - ((y - grid.top()) / self.key_h).floor()).clamp(0.0, 127.0)
                    as u8
            };
            let shift = ui.input(|i| i.modifiers.shift);

            if let Some(p) = pointer
                && p.x > grid.left()
                && let Some(i) = hit(p)
                && (note_rect(&notes[i]).right() - p.x) < EDGE_GRAB
            {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }

            if response.double_clicked()
                && let Some(p) = pointer
                && p.x > grid.left()
                && hit(p).is_none()
                && notes.len() < MAX_NOTES_PER_ROW
            {
                let n = Note {
                    start: self.snap_tick(to_tick(p.x)),
                    length: self.new_len.max(1),
                    key: to_key(p.y),
                    velocity: 0.8,
                };
                let mut new = notes.clone();
                new.push(n);
                out.edits.push((self.set(pattern, new), false));
                out.end_gesture = true;
                self.selection = vec![n];
            } else if response.clicked()
                && let Some(p) = pointer
            {
                match hit(p) {
                    Some(i) if shift => {
                        let n = notes[i];
                        if let Some(at) = self.selection.iter().position(|s| *s == n) {
                            self.selection.remove(at);
                        } else {
                            self.selection.push(n);
                        }
                    }
                    Some(i) => self.selection = vec![notes[i]],
                    None => self.selection.clear(),
                }
            }
            if response.secondary_clicked()
                && let Some(p) = pointer
                && let Some(i) = hit(p)
            {
                let mut new = notes.clone();
                let removed = new.remove(i);
                self.selection.retain(|s| *s != removed);
                out.edits.push((self.set(pattern, new), false));
                out.end_gesture = true;
            }

            // Where the button went down: by the time a drag is recognized
            // the pointer has already moved.
            let press = ui.input(|i| i.pointer.press_origin());
            if response.drag_started()
                && let Some(p) = press.or(response.interact_pointer_pos())
                && let Some(i) = hit(p)
            {
                let n = notes[i];
                if !self.selection.contains(&n) {
                    self.selection = vec![n];
                }
                let kind = if note_rect(&n).right() - p.x < EDGE_GRAB {
                    DragKind::Resize
                } else {
                    DragKind::Move
                };
                let dragged = notes
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| self.selection.contains(x))
                    .map(|(j, _)| j)
                    .collect();
                self.drag = Some(Drag {
                    kind,
                    original: notes.clone(),
                    dragged,
                    start: p,
                });
            }
            if let Some(d) = self.drag.clone()
                && d.kind != DragKind::Velocity
                && let Some(p) = response.interact_pointer_pos()
                && response.dragged()
            {
                let dt = (p.x - d.start.x) / px_tick;
                let dk = to_key(p.y) as i32 - to_key(d.start.y) as i32;
                let mut new = d.original.clone();
                let mut moved = Vec::new();
                for &j in &d.dragged {
                    let o = d.original[j];
                    let n = &mut new[j];
                    match d.kind {
                        DragKind::Move => {
                            n.start = self.snap_tick(o.start as f32 + dt);
                            n.key = (i32::from(o.key) + dk).clamp(0, 127) as u8;
                        }
                        DragKind::Resize => {
                            let end = self.snap_tick(o.end() as f32 + dt);
                            let min = if self.snap == 0 { 1 } else { self.snap };
                            n.length = end.saturating_sub(o.start).max(min);
                            self.new_len = n.length;
                        }
                        DragKind::Velocity => {}
                    }
                    moved.push(*n);
                }
                if new != notes {
                    out.edits.push((self.set(pattern, new), true));
                    self.selection = moved;
                }
            }
            if response.drag_stopped() && self.drag.take().is_some() {
                out.end_gesture = true;
            }
            h_offset = clip.left() - rect.left();
        });
        let _ = scroll_out;

        // Velocity lane, scrolled with the roll horizontally.
        let (lane, lane_resp) = ui.allocate_exact_size(
            vec2(ui.available_width(), VELOCITY_H),
            Sense::click_and_drag(),
        );
        let painter = ui.painter_at(lane);
        painter.rect_filled(lane, CornerRadius::ZERO, ui.visuals().extreme_bg_color);
        painter.text(
            pos2(lane.left() + 4.0, lane.top() + 2.0),
            egui::Align2::LEFT_TOP,
            "Velocity",
            FontId::proportional(10.0),
            ui.visuals().weak_text_color(),
        );
        let x0 = lane.left() + KEYBOARD_W - h_offset;
        let bar_x = |n: &Note| x0 + n.start as f32 * px_tick;
        for n in &notes {
            let x = bar_x(n);
            if x < lane.left() + KEYBOARD_W - 2.0 || x > lane.right() {
                continue;
            }
            let h = (lane.height() - 6.0) * n.velocity;
            let c = if self.selection.contains(n) {
                Color32::WHITE
            } else {
                Color32::from_rgb(235, 140, 60)
            };
            painter.line_segment(
                [
                    pos2(x + 1.0, lane.bottom()),
                    pos2(x + 1.0, lane.bottom() - h),
                ],
                Stroke::new(3.0, c),
            );
        }
        if (lane_resp.dragged() || lane_resp.clicked())
            && let Some(p) = lane_resp.interact_pointer_pos()
        {
            let v = ((lane.bottom() - p.y) / (lane.height() - 6.0)).clamp(0.0, 1.0);
            // The note whose bar is nearest the pointer (selected ones first).
            let near = |n: &Note| (bar_x(n) - p.x).abs() < 6.0;
            let targets: Vec<usize> = if self.selection.iter().any(near) {
                notes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| self.selection.contains(n))
                    .map(|(i, _)| i)
                    .collect()
            } else {
                notes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| near(n))
                    .map(|(i, _)| i)
                    .collect()
            };
            if !targets.is_empty() {
                let mut new = notes.clone();
                let mut sel = Vec::new();
                for i in targets {
                    new[i].velocity = (v * 100.0).round() / 100.0;
                    if self.selection.contains(&notes[i]) {
                        sel.push(new[i]);
                    }
                }
                if new != notes {
                    out.edits.push((self.set(pattern, new), true));
                    self.selection = sel;
                }
            }
        }
        if lane_resp.drag_stopped() || lane_resp.clicked() {
            out.end_gesture = true;
        }

        // Delete selected notes.
        if !self.selection.is_empty()
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            let new: Vec<Note> = notes
                .iter()
                .filter(|n| !self.selection.contains(n))
                .copied()
                .collect();
            self.selection.clear();
            out.edits.push((self.set(pattern, new), false));
            out.end_gesture = true;
        }
        out
    }

    fn set(&self, pattern: &Pattern, notes: Vec<Note>) -> Edit {
        Edit::SetNotes {
            pattern: pattern.id,
            channel: self.channel,
            notes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_and_black_keys() {
        assert_eq!(key_name(60), "C5");
        assert_eq!(key_name(61), "C#5");
        assert_eq!(key_name(0), "C0");
        assert!(is_black(61) && !is_black(64));
    }

    #[test]
    fn snapping() {
        let mut p = PianoRoll::new(ChannelId(1));
        assert_eq!(p.snap_tick(250.0), 240);
        assert_eq!(p.snap_tick(-5.0), 0);
        p.snap = 0;
        assert_eq!(p.snap_tick(250.4), 250);
    }
}
