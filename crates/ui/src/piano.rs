// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano roll for one channel of one pattern (spec "DAW UI clean room",
//! §6).
//!
//! Tools: Draw (click adds, drag sets the new note's length), Paint (drag
//! adds a note on every grid cell), Select (drag selects with a frame),
//! Delete (click or drag over notes). With every tool a note can be
//! dragged (move, or resize at its right edge), a right click deletes the
//! note under it (holding the right button while moving deletes every
//! note passed over), and Ctrl-drag on empty space selects with a frame. Clicking the keyboard
//! plays the key. Every drag is one undo step; only visible notes are
//! drawn.

use crate::icons::{self, Glyph};
use crate::piano_dialogs::{Dialog, Props};
use crate::toolbar::{SNAPS, Snap, bars_text, snap_name};
use crate::{Host, theme};
use egui::{
    Color32, CornerRadius, Event, FontId, Key, PointerButton, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, pos2, vec2,
};
use jdance_commands::Edit;
use jdance_commands::notes::{self, Scale, ScaleMode};
use jdance_domain::{
    ChannelId, ChannelSource, MAX_NOTES_PER_ROW, MAX_PATTERN_STEPS, Note, Pattern, PatternId,
    Project, TICKS_PER_STEP, TimeSignature,
};

const KEYS: u8 = 128;
const KEYBOARD_W: f32 = 52.0;
const RULER_H: f32 = 20.0;
const LANE_H: f32 = 96.0;
const EDGE_GRAB: f32 = 6.0;
const NEW_VELOCITY: f32 = 0.8;
const SELECTED: Color32 = Color32::from_rgb(0x7D, 0xBB, 0xFF);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Draw,
    Paint,
    Delete,
    Slice,
    Select,
}

impl Tool {
    const ALL: [Self; 5] = [
        Self::Draw,
        Self::Paint,
        Self::Delete,
        Self::Slice,
        Self::Select,
    ];

    fn glyph(self) -> Glyph {
        match self {
            Self::Draw => Glyph::Draw,
            Self::Paint => Glyph::Paint,
            Self::Delete => Glyph::Delete,
            Self::Slice => Glyph::Slice,
            Self::Select => Glyph::Select,
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Draw => "Draw (P): click adds a note, drag sets its length; edges resize",
            Self::Paint => "Paint (B): drag adds a note on every grid cell",
            Self::Delete => {
                "Delete (D): click or drag over notes · in every tool the right button deletes"
            }
            Self::Slice => "Slice (C): click a note to cut it in two (both retrigger)",
            Self::Select => "Select (E): drag a frame · Shift adds",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    /// Right edge: the length changes.
    Resize,
    /// Left edge: start and length change, the end stays.
    ResizeLeft,
    /// Drawing a new note: its end follows the pointer.
    Create,
    Paint,
    Erase,
    /// Frame selection from `start`; `add` keeps the previous selection.
    Frame {
        add: bool,
    },
}

#[derive(Debug, Clone)]
struct Drag {
    kind: DragKind,
    /// Notes when the drag started and which of them are dragged.
    original: Vec<Note>,
    dragged: Vec<usize>,
    selection: Vec<Note>,
    start: Pos2,
}

/// A velocity lane drag: original notes and the note grabbed.
#[derive(Debug, Clone)]
struct LaneDrag {
    original: Vec<Note>,
    targets: Vec<usize>,
    grabbed: usize,
    /// The targets are the selection (and stay selected).
    selected: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PianoRoll {
    pub channel: ChannelId,
    /// Pixels per step.
    zoom: f32,
    key_h: f32,
    /// Snap in ticks (0 = off).
    snap: u32,
    /// The project signature, refreshed on every frame (SIG-003).
    sig: TimeSignature,
    /// Length of new notes in ticks (the last one placed or resized).
    new_len: u32,
    tool: Tool,
    selection: Vec<Note>,
    drag: Option<Drag>,
    lane_drag: Option<LaneDrag>,
    /// Several notes in the velocity lane: scale together (relative) or
    /// set to one value.
    relative: bool,
    clipboard: Vec<Note>,
    scale: Option<Scale>,
    dialog: Option<Dialog>,
    /// Key held on the keyboard, or of a note being drawn or dragged.
    sounding: Option<u8>,
    scrolled: bool,
    /// Grid tick under the pointer (paste position).
    hover_tick: Option<u32>,
    /// A clicked-in note sounds until this time.
    audition_until: f64,
    /// Last cell (tick, key) of the current Paint gesture.
    last_paint: Option<(u32, u8)>,
    /// Open Piano Roll Tools dialog (spec 0.2.0 §18).
    tool_dialog: Option<crate::note_tools_ui::ToolDialog>,
    /// Its preview: drawn instead of the notes until Apply or Cancel.
    preview: Option<Vec<Note>>,
    /// Grid of the quick tools: the last snap that was not off.
    last_grid: u32,
    script_dialog: Option<crate::scripts::ScriptDialog>,
    scripts: Vec<jdance_commands::note_tools::ScriptInfo>,
    /// Right-button delete gesture.
    rmb: crate::widgets::RightSweep,
    /// Caret gestures on the ruler (spec 0.2.4 PCR-005).
    ruler: crate::ruler::Ruler,
}

/// The caret moved on the Piano Roll ruler, as a pattern position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaretRequest {
    /// During the gesture.
    Preview(u64),
    /// On release.
    Seek(u64),
}

#[derive(Debug, Default)]
pub(crate) struct PianoOutput {
    pub edits: Vec<(Edit, bool)>,
    pub end_gesture: bool,
    pub close: bool,
    pub select_pattern: Option<PatternId>,
    pub snap: Option<Snap>,
    /// Tools > LFO: automation points for a range of the pattern.
    pub lfo: Option<jdance_commands::note_tools::Range>,
    /// Tools > Scripts.
    pub script: Option<crate::scripts::ScriptRequest>,
    /// The caret was moved on the ruler.
    pub caret: Option<CaretRequest>,
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

fn length_text(ticks: u32, beat: u32) -> String {
    if ticks.is_multiple_of(beat) {
        format!("{} beat(s)", ticks / beat)
    } else if beat.is_multiple_of(ticks) {
        format!("1/{} beat", beat / ticks)
    } else if ticks.is_multiple_of(TICKS_PER_STEP) {
        format!("{} steps", ticks / TICKS_PER_STEP)
    } else {
        format!("{ticks} ticks")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Left,
    Right,
}

/// Which resize edge `x` is on; narrow notes keep a body zone.
fn edge_of(r: &Rect, x: f32) -> Option<Edge> {
    let zone = EDGE_GRAB.min(r.width() / 3.0);
    if x - r.left() < zone {
        Some(Edge::Left)
    } else if r.right() - x < zone {
        Some(Edge::Right)
    } else {
        None
    }
}

/// `n` cut at `tick` into two notes with the same key and velocity; `None`
/// at or outside its edges.
fn slice_note(n: Note, tick: u32) -> Option<(Note, Note)> {
    (tick > n.start && tick < n.end()).then(|| {
        (
            Note {
                id: 0,
                length: tick - n.start,
                ..n
            },
            Note {
                id: 0,
                start: tick,
                length: n.end() - tick,
                ..n
            },
        )
    })
}

fn indices(notes: &[Note], selection: &[Note]) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter(|(_, n)| selection.contains(n))
        .map(|(i, _)| i)
        .collect()
}

impl PianoRoll {
    pub fn new(channel: ChannelId) -> Self {
        Self {
            channel,
            zoom: 28.0,
            key_h: 14.0,
            snap: TICKS_PER_STEP,
            sig: TimeSignature::COMMON,
            new_len: TICKS_PER_STEP,
            tool: Tool::Draw,
            selection: Vec::new(),
            drag: None,
            lane_drag: None,
            relative: true,
            clipboard: Vec::new(),
            scale: None,
            dialog: None,
            sounding: None,
            scrolled: false,
            hover_tick: None,
            audition_until: 0.0,
            last_paint: None,
            tool_dialog: None,
            preview: None,
            last_grid: TICKS_PER_STEP,
            script_dialog: None,
            scripts: Vec::new(),
            ruler: crate::ruler::Ruler::default(),
            rmb: crate::widgets::RightSweep::default(),
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

    /// The smallest note length / move step: the grid, or one tick.
    fn min_len(&self) -> u32 {
        self.snap.max(1)
    }

    fn sound(&mut self, host: &mut dyn Host, key: Option<u8>) {
        if self.sounding == key {
            return;
        }
        if let Some(k) = self.sounding.take() {
            host.live_note(self.channel, k, None);
        }
        if let Some(k) = key {
            host.live_note(self.channel, k, Some(NEW_VELOCITY));
            self.sounding = Some(k);
        }
    }

    fn set(&self, pattern: &Pattern, notes: Vec<Note>) -> Edit {
        Edit::SetNotes {
            pattern: pattern.id,
            channel: self.channel,
            notes,
        }
    }

    /// One undo step replacing the notes; selects `select` afterwards.
    fn commit(
        &mut self,
        out: &mut PianoOutput,
        pattern: &Pattern,
        notes: Vec<Note>,
        select: Vec<Note>,
    ) {
        out.edits.push((self.set(pattern, notes), false));
        out.end_gesture = true;
        self.selection = select;
    }

    /// The tools' view of the row: scope = selection, generator range =
    /// the selection's span (else the pattern).
    fn tool_input<'a>(
        &self,
        pattern: &Pattern,
        notes: &'a [Note],
        selected: &'a [usize],
    ) -> jdance_commands::note_tools::Input<'a> {
        let range = (!selected.is_empty()).then(|| jdance_commands::note_tools::Range {
            start: selected.iter().map(|&i| notes[i].start).min().unwrap_or(0),
            end: selected.iter().map(|&i| notes[i].end()).max().unwrap_or(0),
        });
        jdance_commands::note_tools::Input {
            notes,
            selected,
            pattern_len: pattern.length_ticks() as u32,
            grid: self.last_grid,
            range,
            signature: self.sig,
        }
    }

    /// Quick tools apply at once; the others open their dialog.
    fn start_tool(
        &mut self,
        ctx: &egui::Context,
        tool: jdance_commands::note_tools::Tool,
        pattern: &Pattern,
        notes: &[Note],
        out: &mut PianoOutput,
    ) {
        if crate::note_tools_ui::is_quick(&tool) {
            let sel = indices(notes, &self.selection);
            let input = self.tool_input(pattern, notes, &sel);
            match jdance_commands::note_tools::run(&tool, &input) {
                Ok(Some(o)) if o.dropped == 0 => {
                    let select = o.select.iter().map(|&i| o.notes[i]).collect();
                    self.commit(out, pattern, o.notes, select);
                    return;
                }
                Ok(None) => return,
                // Dropped notes need a confirmation; errors show there.
                _ => {}
            }
        }
        self.tool_dialog = Some(crate::note_tools_ui::ToolDialog::new(ctx, tool));
    }

    fn header(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        pattern: &Pattern,
        notes: &[Note],
        out: &mut PianoOutput,
    ) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Piano Roll").font(theme::strong(14.0)));
            egui::ComboBox::from_id_salt("piano_pattern")
                .selected_text(&pattern.name)
                .show_ui(ui, |ui| {
                    for p in &project.patterns {
                        if ui.selectable_label(p.id == pattern.id, &p.name).clicked() {
                            out.select_pattern = Some(p.id);
                        }
                    }
                })
                .response
                .on_hover_text("Pattern");
            let bars = pattern.length_steps.div_ceil(self.sig.steps_per_bar());
            if pattern.length_steps >= MAX_PATTERN_STEPS {
                // PAT-004: the pattern stops growing here.
                ui.label(RichText::new(format!("{bars} bars · limit")).color(theme::WARN))
                    .on_hover_text(
                        "Patterns are at most 256 steps; notes past the end are not added",
                    );
            } else {
                ui.label(RichText::new(format!("{bars} bar(s)")).color(theme::MUTED))
                    .on_hover_text("Grows when a note is placed past the end");
            }
            let label = |c: &jdance_domain::Channel| {
                let kind = match &c.source {
                    ChannelSource::Sampler { .. } => "Sampler",
                    ChannelSource::Plugin { .. } => "VST3",
                    ChannelSource::Audio { .. } => "Audio",
                };
                format!("{} · {kind}", c.name)
            };
            let current = project.channel(self.channel).map(label).unwrap_or_default();
            egui::ComboBox::from_id_salt("piano_channel")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    // Audio channels have no notes (ACH-003).
                    for c in project.channels.iter().filter(|c| !c.is_audio()) {
                        if ui
                            .selectable_label(c.id == self.channel, label(c))
                            .clicked()
                        {
                            self.channel = c.id;
                            self.selection.clear();
                        }
                    }
                })
                .response
                .on_hover_text("Channel");
            ui.separator();
            for t in Tool::ALL {
                if icons::tool_button(ui, t.glyph(), self.tool == t, t.hint()).clicked() {
                    self.tool = t;
                }
            }
            ui.separator();
            ui.label(RichText::new("SNAP").size(11.0).color(theme::MUTED));
            egui::ComboBox::from_id_salt("piano_snap")
                .selected_text(snap_name(self.snap, self.sig))
                .show_ui(ui, |ui| {
                    for v in SNAPS {
                        if ui
                            .selectable_label(v.ticks(self.sig) == self.snap, v.name())
                            .clicked()
                        {
                            out.snap = Some(v);
                        }
                    }
                });
            ui.add(
                egui::Slider::new(&mut self.zoom, 8.0..=96.0)
                    .show_value(false)
                    .text("Zoom"),
            );
            ui.separator();
            let selected = indices(notes, &self.selection);
            let target: Vec<Note> = if selected.is_empty() {
                notes.to_vec()
            } else {
                self.selection.clone()
            };
            let any = !notes.is_empty();
            let mut chosen = None;
            let scale = self.scale.unwrap_or(Scale {
                tonic: 0,
                mode: ScaleMode::Major,
            });
            let highlight = self.scale.is_some();
            let mut tool = None;
            let mut script = None;
            ui.menu_button("Tools", |ui| {
                let mut open = |ui: &mut egui::Ui, label: &str, d: Dialog, enabled: bool| {
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                        chosen = Some(d);
                        ui.close();
                    }
                };
                for t in crate::note_tools_ui::menu_tools(self.last_grid) {
                    let Some(t) = t else {
                        ui.separator();
                        continue;
                    };
                    let quick = crate::note_tools_ui::is_quick(&t);
                    let label = if quick {
                        t.name().to_owned()
                    } else {
                        format!("{}…", t.name())
                    };
                    let enabled = any
                        || t.is_generator()
                        || matches!(t, jdance_commands::note_tools::Tool::Randomize(_));
                    let tip = if quick {
                        format!(
                            "Applies at once on a {} grid",
                            crate::toolbar::snap_name(self.last_grid, self.sig)
                        )
                    } else {
                        "Opens with a preview; Apply is one undo step".to_owned()
                    };
                    if ui
                        .add_enabled(enabled, egui::Button::new(label))
                        .on_hover_text(tip)
                        .clicked()
                    {
                        tool = Some(t);
                        ui.close();
                    }
                }
                if ui
                    .button("LFO…")
                    .on_hover_text("Writes an LFO curve into an automation source")
                    .clicked()
                {
                    out.lfo = Some(jdance_commands::note_tools::Range {
                        start: 0,
                        end: pattern.length_ticks() as u32,
                    });
                    ui.close();
                }
                ui.separator();
                open(
                    ui,
                    "Transpose…",
                    Dialog::Transpose {
                        semitones: 0,
                        octaves: 0,
                    },
                    any,
                );
                open(ui, "Scale…", Dialog::Scale { scale, highlight }, true);
                open(
                    ui,
                    "Note properties…",
                    Dialog::Properties(Props::of(&target)),
                    any,
                );
                ui.separator();
                let has_last = crate::scripts::last_run(ui.ctx()).is_some();
                ui.menu_button("Scripts", |ui| {
                    script = crate::scripts::menu(ui, &self.scripts, has_last);
                });
            })
            .response
            .on_hover_text("Note tools: they work on the selection, or on every note");
            if let Some(d) = chosen {
                self.dialog = Some(d);
            }
            if let Some(t) = tool {
                self.start_tool(ui.ctx(), t, pattern, notes, out);
            }
            if script.is_some() {
                out.script = script;
            }
            if ui.button("Close").clicked() {
                out.close = true;
            }
        });
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        pattern: &Pattern,
        host: &mut dyn Host,
        marks: crate::rack::PatternMarks,
        snap: Snap,
    ) -> PianoOutput {
        let playhead_tick = marks.playhead.map(|t| t as f32);
        let caret = marks.caret;
        self.sig = project.time_signature;
        let snap = snap.ticks(self.sig);
        self.snap = snap;
        if snap > 0 {
            self.last_grid = snap;
        }
        let mut out = PianoOutput::default();
        let notes: Vec<Note> = pattern
            .row(self.channel)
            .map(|r| r.notes.clone())
            .unwrap_or_default();
        self.selection.retain(|s| notes.contains(s));
        let sel = indices(&notes, &self.selection);
        self.scripts = host.scripts();
        let input = self.tool_input(pattern, &notes, &sel);
        self.preview = match &mut self.tool_dialog {
            Some(d) => d.preview(&input).map(|o| o.notes.clone()),
            None => self
                .script_dialog
                .as_ref()
                .and_then(|d| d.preview().map(<[Note]>::to_vec)),
        };

        self.header(ui, project, pattern, &notes, &mut out);
        ui.add_space(4.0);

        let px_tick = self.zoom / TICKS_PER_STEP as f32;
        let key_h = self.key_h;
        let pattern_end = pattern.length_ticks() as u32;
        let content_end = notes
            .iter()
            .map(Note::end)
            .max()
            .unwrap_or(0)
            .max(pattern_end)
            .saturating_add(project.time_signature.bar_ticks())
            .min(MAX_PATTERN_STEPS * TICKS_PER_STEP);
        let grid_w = content_end as f32 * px_tick;
        let grid_h = f32::from(KEYS) * key_h + RULER_H;

        let avail = ui.available_size();
        let roll_h = (avail.y - LANE_H - 8.0).max(120.0);
        let mut scroll = egui::ScrollArea::both()
            .id_salt("piano_roll")
            .max_height(roll_h)
            .auto_shrink([false, false]);
        if !self.scrolled {
            // Start around C5.
            scroll = scroll.vertical_scroll_offset((f32::from(KEYS - 76) * key_h).max(0.0));
            self.scrolled = true;
        }
        let mut h_offset = 0.0;
        let scroll_out = scroll.show(ui, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(vec2(KEYBOARD_W + grid_w, grid_h), Sense::click_and_drag());
            let clip = ui.clip_rect();
            // The ruler stays at the top of the view.
            let ruler_top = clip.top().max(rect.top());
            let grid = Rect::from_min_size(
                pos2(rect.left() + KEYBOARD_W, rect.top() + RULER_H),
                vec2(grid_w, grid_h - RULER_H),
            );
            let kb_left = clip.left().max(rect.left());
            let x_of = |t: u32| grid.left() + t as f32 * px_tick;
            let y_of = |k: u8| grid.top() + f32::from(KEYS - 1 - k) * key_h;
            let painter = ui.painter_at(clip);

            // Rows: in-scale rows tinted when the scale helper is on.
            let first_key = (((grid.bottom() - clip.bottom()) / key_h).floor().max(0.0)) as u8;
            let last_key = ((((grid.bottom() - clip.top()) / key_h).ceil()) as u32)
                .min(u32::from(KEYS - 1)) as u8;
            for k in first_key..=last_key {
                let y = y_of(k);
                let row = Rect::from_min_size(pos2(grid.left(), y), vec2(grid_w, key_h));
                let base = match self.scale {
                    Some(s) if s.contains(k) => Color32::from_rgb(0x18, 0x24, 0x1F),
                    _ if is_black(k) => Color32::from_rgb(0x12, 0x14, 0x18),
                    _ => Color32::from_rgb(0x16, 0x19, 0x1D),
                };
                painter.rect_filled(row, CornerRadius::ZERO, base);
                let line = if k % 12 == 0 {
                    theme::LINE_STRONG
                } else {
                    Color32::from_rgb(0x1E, 0x23, 0x28)
                };
                painter.line_segment(
                    [pos2(grid.left(), y + key_h), pos2(grid.right(), y + key_h)],
                    Stroke::new(1.0, line),
                );
            }
            let first_step = ((clip.left() - grid.left()) / self.zoom).floor().max(0.0) as u32;
            let last_step = ((clip.right() - grid.left()) / self.zoom).ceil() as u32;
            for s in first_step..=last_step.min(content_end / TICKS_PER_STEP) {
                let x = x_of(s * TICKS_PER_STEP);
                let (w, c) = if s % self.sig.steps_per_bar() == 0 {
                    (1.5, theme::LINE_STRONG)
                } else if s % self.sig.steps_per_beat() == 0 {
                    (1.0, theme::LINE_STRONG)
                } else {
                    (1.0, Color32::from_rgb(0x22, 0x27, 0x2D))
                };
                painter.line_segment(
                    [pos2(x, grid.top()), pos2(x, grid.bottom())],
                    Stroke::new(w, c),
                );
            }
            // Beyond the pattern end: lightly shaded; a note placed there
            // grows the pattern to the next bar (PAT-001).
            let end_x = x_of(pattern_end);
            painter.rect_filled(
                Rect::from_min_max(pos2(end_x, grid.top()), grid.max),
                CornerRadius::ZERO,
                Color32::from_black_alpha(45),
            );
            painter.line_segment(
                [pos2(end_x, grid.top()), pos2(end_x, grid.bottom())],
                Stroke::new(1.5, theme::AMBER),
            );

            // Notes (visible ones only).
            let note_rect = |n: &Note| {
                Rect::from_min_max(
                    pos2(x_of(n.start), y_of(n.key) + 1.0),
                    pos2(
                        (x_of(n.end()) - 1.0).max(x_of(n.start) + 3.0),
                        y_of(n.key) + key_h - 1.0,
                    ),
                )
            };
            if let Some(preview) = &self.preview {
                // Tool preview: the result in teal over the dimmed notes.
                for n in &notes {
                    let r = note_rect(n);
                    if r.intersects(clip) {
                        painter.rect_filled(
                            r,
                            CornerRadius::same(3),
                            theme::MUTED.gamma_multiply(0.25),
                        );
                    }
                }
                for n in preview {
                    let r = note_rect(n);
                    if !r.intersects(clip) {
                        continue;
                    }
                    painter.rect_filled(
                        r,
                        CornerRadius::same(3),
                        theme::TEAL.gamma_multiply(0.35 + 0.5 * n.velocity),
                    );
                    painter.rect_stroke(
                        r,
                        CornerRadius::same(3),
                        Stroke::new(1.0, theme::TEAL),
                        StrokeKind::Inside,
                    );
                }
            }
            for n in notes.iter().filter(|_| self.preview.is_none()) {
                let r = note_rect(n);
                if !r.intersects(clip) {
                    continue;
                }
                let selected = self.selection.contains(n);
                let (fill, stroke) = if selected {
                    (
                        SELECTED,
                        Stroke::new(2.0, Color32::from_rgb(0xE9, 0xF3, 0xFF)),
                    )
                } else {
                    (
                        theme::AMBER.gamma_multiply(0.55 + 0.45 * n.velocity),
                        Stroke::new(1.0, Color32::from_rgb(0xFF, 0xD2, 0x7F)),
                    )
                };
                painter.rect_filled(r, CornerRadius::same(3), fill);
                painter.rect_stroke(r, CornerRadius::same(3), stroke, StrokeKind::Inside);
                if r.width() > 26.0 && key_h >= 12.0 {
                    painter.with_clip_rect(r.intersect(clip)).text(
                        pos2(r.left() + 4.0, r.center().y),
                        egui::Align2::LEFT_CENTER,
                        key_name(n.key),
                        FontId::proportional(9.5),
                        if selected {
                            Color32::from_rgb(0x06, 0x12, 0x1F)
                        } else {
                            theme::ON_AMBER
                        },
                    );
                }
            }
            if let Some(Drag {
                kind: DragKind::Frame { .. },
                start,
                ..
            }) = &self.drag
                && let Some(p) = response.interact_pointer_pos()
            {
                let r = Rect::from_two_pos(*start, p);
                painter.rect_filled(r, CornerRadius::ZERO, theme::BLUE.gamma_multiply(0.08));
                painter.rect_stroke(
                    r,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, SELECTED),
                    StrokeKind::Inside,
                );
            }
            if let Some(t) = playhead_tick {
                let x = x_of(t as u32);
                painter.line_segment(
                    [pos2(x, grid.top()), pos2(x, grid.bottom())],
                    Stroke::new(2.0, Color32::from_rgb(0xF2, 0xF4, 0xF6)),
                );
            }

            // Ruler (bars), pinned to the top.
            let ruler =
                Rect::from_min_size(pos2(clip.left(), ruler_top), vec2(clip.width(), RULER_H));
            painter.rect_filled(ruler, CornerRadius::ZERO, theme::PANEL);
            let bar = self.sig.bar_ticks();
            let bar_steps = self.sig.steps_per_bar();
            for b in (first_step / bar_steps)..=(last_step / bar_steps) {
                let x = x_of(b * bar);
                painter.line_segment(
                    [pos2(x, ruler_top + 12.0), pos2(x, ruler_top + RULER_H)],
                    Stroke::new(1.0, theme::LINE_STRONG),
                );
                painter.text(
                    pos2(x + 4.0, ruler_top + 2.0),
                    egui::Align2::LEFT_TOP,
                    (b + 1).to_string(),
                    theme::mono(11.0),
                    theme::MUTED,
                );
            }

            // Keyboard, pinned to the left edge of the view.
            for k in first_key..=last_key {
                let y = y_of(k);
                let r = Rect::from_min_size(pos2(kb_left, y), vec2(KEYBOARD_W, key_h));
                let held = self.sounding == Some(k);
                let (c, t) = if held {
                    (theme::AMBER, theme::ON_AMBER)
                } else if is_black(k) {
                    (Color32::from_rgb(0x1A, 0x1D, 0x21), theme::MUTED)
                } else {
                    (
                        Color32::from_rgb(0xD9, 0xDD, 0xE2),
                        Color32::from_rgb(0x1A, 0x1D, 0x21),
                    )
                };
                painter.rect_filled(r, CornerRadius::ZERO, c);
                painter.line_segment(
                    [r.left_bottom(), r.right_bottom()],
                    Stroke::new(1.0, theme::LINE),
                );
                if k % 12 == 0 || held {
                    painter.text(
                        pos2(r.right() - 4.0, r.center().y),
                        egui::Align2::RIGHT_CENTER,
                        key_name(k),
                        FontId::proportional(9.5),
                        t,
                    );
                }
            }
            painter.rect_filled(
                Rect::from_min_size(pos2(kb_left, ruler_top), vec2(KEYBOARD_W, RULER_H)),
                CornerRadius::ZERO,
                theme::PANEL,
            );

            // The caret: set and dragged on the ruler (spec 0.2.4 PCR-004/005).
            let frame = crate::ruler::RulerFrame {
                rect: Rect::from_min_max(
                    pos2(grid.left().max(kb_left + KEYBOARD_W), ruler_top),
                    pos2(grid.right().min(clip.right()), ruler_top + RULER_H),
                ),
                origin_x: grid.left(),
                px_per_tick: px_tick,
                grids: crate::ruler::Grids::new(
                    u64::from(snap.max(1)),
                    u64::from(self.sig.beat_ticks()),
                ),
                caret: caret.unwrap_or(0),
                selection: None,
                select: false,
                limit: Some(u64::from(pattern_end.max(1))),
            };
            let (_, r) = self.ruler.ui(
                ui,
                egui::Id::unique(("piano_ruler", self.channel.0)),
                &frame,
            );
            if let Some(c) = r.preview {
                out.caret = Some(CaretRequest::Preview(c));
            }
            if let Some(end) = r.end {
                out.caret = Some(CaretRequest::Seek(end.caret));
            }
            let shown = self.ruler.active().map(|d| d.caret()).or(caret);
            if let Some(c) = shown {
                let x = grid.left() + c as f32 * px_tick;
                if x >= kb_left + KEYBOARD_W {
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            pos2(x - 6.0, ruler_top + RULER_H - 9.0),
                            pos2(x + 6.0, ruler_top + RULER_H - 9.0),
                            pos2(x, ruler_top + RULER_H),
                        ],
                        theme::AMBER,
                        Stroke::NONE,
                    ));
                    painter.add(egui::Shape::dashed_line(
                        &[pos2(x, ruler_top + RULER_H), pos2(x, grid.bottom())],
                        Stroke::new(1.0, theme::AMBER.gamma_multiply(0.7)),
                        4.0,
                        4.0,
                    ));
                }
            }

            // Selection summary in the corner of the view.
            if !self.selection.is_empty() {
                let p = Props::of(&self.selection);
                let first = p
                    .start
                    .map(|s| bars_text(u64::from(s), self.sig))
                    .unwrap_or_default();
                let len = self
                    .selection
                    .iter()
                    .map(|n| n.length)
                    .min()
                    .map(|l| length_text(l, self.sig.beat_ticks()))
                    .unwrap_or_default();
                let key = p.key.map_or_else(|| "mixed".to_owned(), key_name);
                let vel = p
                    .velocity
                    .map_or_else(|| "mixed".to_owned(), |v| format!("{v:.2}"));
                let lines = [
                    format!("SELECTION · {} NOTE(S)", self.selection.len()),
                    format!("start {first} · len {len}"),
                    format!("key {key} · velocity {vel}"),
                ];
                let box_r = Rect::from_min_size(
                    pos2(clip.right() - 250.0, clip.bottom() - 74.0),
                    vec2(238.0, 62.0),
                );
                painter.rect_filled(box_r, CornerRadius::same(8), theme::PANEL);
                painter.rect_stroke(
                    box_r,
                    CornerRadius::same(8),
                    Stroke::new(1.0, theme::LINE),
                    StrokeKind::Inside,
                );
                for (i, l) in lines.iter().enumerate() {
                    painter.text(
                        pos2(box_r.left() + 10.0, box_r.top() + 8.0 + i as f32 * 16.0),
                        egui::Align2::LEFT_TOP,
                        l,
                        if i == 0 {
                            FontId::proportional(10.5)
                        } else {
                            theme::mono(11.5)
                        },
                        if i == 0 { theme::MUTED } else { theme::TEXT },
                    );
                }
            }

            // Interaction.
            let pointer = response.interact_pointer_pos().or(response.hover_pos());
            let on_keys = |p: Pos2| p.x < kb_left + KEYBOARD_W;
            let on_grid = |p: Pos2| !on_keys(p) && p.y > ruler_top + RULER_H;
            let hit = |p: Pos2| {
                on_grid(p)
                    .then(|| notes.iter().rposition(|n| note_rect(n).contains(p)))
                    .flatten()
            };
            let to_tick = |x: f32| (x - grid.left()) / px_tick;
            let to_key = |y: f32| {
                (f32::from(KEYS - 1) - ((y - grid.top()) / key_h).floor()).clamp(0.0, 127.0) as u8
            };
            let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
            self.hover_tick = response
                .hover_pos()
                .filter(|p| on_grid(*p))
                .map(|p| self.snap_tick(to_tick(p.x)));

            if let Some(p) = pointer
                && let Some(i) = hit(p)
            {
                let cursor = match (self.tool, edge_of(&note_rect(&notes[i]), p.x)) {
                    (Tool::Delete, _) => egui::CursorIcon::NotAllowed,
                    (Tool::Slice, _) => egui::CursorIcon::VerticalText,
                    (_, Some(_)) => egui::CursorIcon::ResizeHorizontal,
                    (_, None) => egui::CursorIcon::Grab,
                };
                ui.ctx().set_cursor_icon(cursor);
            }

            // Keyboard: press plays the key, dragging glides over keys.
            let down = response.is_pointer_button_down_on();
            let press = ui.input(|i| i.pointer.press_origin());
            let now = ui.input(|i| i.time);
            if self.drag.is_none() && down && press.is_some_and(on_keys) {
                self.sound(host, pointer.map(|p| to_key(p.y)));
            } else if self.drag.is_none() && !down && self.sounding.is_some() {
                if now >= self.audition_until {
                    self.sound(host, None);
                } else {
                    ui.ctx()
                        .request_repaint_after_secs((self.audition_until - now) as f32);
                }
            }

            // Clicks.
            if response.clicked()
                && let Some(p) = pointer
                && on_grid(p)
            {
                match (hit(p), self.tool) {
                    (Some(i), Tool::Delete) => {
                        let mut new = notes.clone();
                        new.remove(i);
                        let keep = self.selection.clone();
                        self.commit(&mut out, pattern, new, keep);
                    }
                    (Some(i), Tool::Slice) => {
                        if let Some((a, b)) = slice_note(notes[i], self.snap_tick(to_tick(p.x))) {
                            let mut new = notes.clone();
                            new[i] = a;
                            new.push(b);
                            self.commit(&mut out, pattern, new, vec![a, b]);
                        }
                    }
                    (Some(i), _) if shift => {
                        let n = notes[i];
                        if let Some(at) = self.selection.iter().position(|s| *s == n) {
                            self.selection.remove(at);
                        } else {
                            self.selection.push(n);
                        }
                    }
                    (Some(i), _) => self.selection = vec![notes[i]],
                    (None, Tool::Draw | Tool::Paint) if !ctrl => {
                        if notes.len() < MAX_NOTES_PER_ROW {
                            let n = Note {
                                id: 0,
                                start: self.snap_tick(to_tick(p.x)),
                                length: self.new_len.max(1),
                                key: to_key(p.y),
                                velocity: NEW_VELOCITY,
                            };
                            let mut new = notes.clone();
                            new.push(n);
                            self.commit(&mut out, pattern, new, vec![n]);
                            // A short audition of the new note.
                            self.sound(host, Some(n.key));
                            self.audition_until = now + 0.25;
                        }
                    }
                    (None, _) => self.selection.clear(),
                }
            }
            if response.double_clicked()
                && let Some(p) = pointer
                && let Some(i) = hit(p)
                && self.tool != Tool::Delete
            {
                self.selection = vec![notes[i]];
                self.dialog = Some(Dialog::Properties(Props::of(&[notes[i]])));
            }
            // Right button: a click deletes the note under it, holding it
            // and moving deletes every note it passes over (one undo
            // step), in every tool.
            let visible = ui.clip_rect();
            let drag_idle = self.drag.is_none();
            let sweep = self
                .rmb
                .update(ui, |p| drag_idle && visible.contains(p) && on_grid(p));
            if let Some((points, ended)) = sweep {
                let mut new = notes.clone();
                let mut removed = Vec::new();
                for p in points
                    .into_iter()
                    .filter(|p| visible.contains(*p) && on_grid(*p))
                {
                    if let Some(i) = new.iter().rposition(|n| note_rect(n).contains(p)) {
                        removed.push(new.remove(i));
                    }
                }
                if !removed.is_empty() {
                    self.selection.retain(|s| !removed.contains(s));
                    out.edits.push((self.set(pattern, new), true));
                }
                out.end_gesture |= ended;
            }

            // Drags start where the button went down: by the time a drag
            // is recognized the pointer has already moved.
            if response.drag_started_by(PointerButton::Primary)
                && let Some(p) = press.or(response.interact_pointer_pos())
                && on_grid(p)
            {
                let kind = match hit(p) {
                    _ if ctrl => DragKind::Frame { add: shift },
                    Some(_) if self.tool == Tool::Delete => DragKind::Erase,
                    Some(_) if self.tool == Tool::Slice => DragKind::Frame { add: shift },
                    Some(i) => match edge_of(&note_rect(&notes[i]), p.x) {
                        Some(Edge::Right) => DragKind::Resize,
                        Some(Edge::Left) => DragKind::ResizeLeft,
                        None => DragKind::Move,
                    },
                    None => match self.tool {
                        Tool::Draw => DragKind::Create,
                        Tool::Paint => DragKind::Paint,
                        Tool::Select | Tool::Slice => DragKind::Frame { add: shift },
                        Tool::Delete => DragKind::Erase,
                    },
                };
                let mut original = notes.clone();
                let mut dragged = Vec::new();
                match kind {
                    DragKind::Move | DragKind::Resize | DragKind::ResizeLeft => {
                        let n = notes[hit(p).expect("on a note")];
                        if !self.selection.contains(&n) {
                            self.selection = vec![n];
                        }
                        dragged = indices(&notes, &self.selection);
                        if kind == DragKind::Move {
                            self.sound(host, Some(n.key));
                        }
                    }
                    DragKind::Create if notes.len() < MAX_NOTES_PER_ROW => {
                        let n = Note {
                            id: 0,
                            start: self.snap_tick(to_tick(p.x)),
                            length: self.min_len(),
                            key: to_key(p.y),
                            velocity: NEW_VELOCITY,
                        };
                        dragged.push(original.len());
                        original.push(n);
                        self.sound(host, Some(n.key));
                    }
                    _ => {}
                }
                self.drag = Some(Drag {
                    kind,
                    original,
                    dragged,
                    selection: self.selection.clone(),
                    start: p,
                });
            }
            if let Some(d) = self.drag.clone()
                && let Some(p) = response.interact_pointer_pos()
                && response.dragged_by(PointerButton::Primary)
            {
                let dt = (p.x - d.start.x) / px_tick;
                let dk = i32::from(to_key(p.y)) - i32::from(to_key(d.start.y));
                match d.kind {
                    DragKind::Move | DragKind::Resize | DragKind::ResizeLeft | DragKind::Create => {
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
                                DragKind::ResizeLeft => {
                                    let end = o.end();
                                    let start = self
                                        .snap_tick(o.start as f32 + dt)
                                        .min(end.saturating_sub(1));
                                    n.start = start;
                                    n.length = end - start;
                                }
                                _ => {
                                    let end = self.snap_tick(o.end() as f32 + dt);
                                    n.length = end.saturating_sub(o.start).max(self.min_len());
                                    self.new_len = n.length;
                                }
                            }
                            moved.push(*n);
                        }
                        if d.kind == DragKind::Move
                            && let Some(first) = moved.first()
                        {
                            self.sound(host, Some(first.key));
                        }
                        if new != notes {
                            out.edits.push((self.set(pattern, new), true));
                            self.selection = moved;
                        }
                    }
                    DragKind::Paint => {
                        // Every cell from the last painted one to the
                        // pointer: a fast move leaves no gaps.
                        let cell = self.snap_tick(to_tick(p.x));
                        let key = to_key(p.y);
                        let from = self.last_paint.map_or(cell, |c| c.0);
                        // The grid, or the note length when snapping is off.
                        let step = if self.snap > 0 {
                            self.snap
                        } else {
                            self.new_len.max(1)
                        };
                        let (lo, hi) = (from.min(cell), from.max(cell));
                        let mut new = notes.clone();
                        let mut t = lo;
                        while t <= hi && new.len() < MAX_NOTES_PER_ROW && p.x > grid.left() {
                            if !new.iter().any(|n| n.key == key && n.start == t) {
                                new.push(Note {
                                    id: 0,
                                    start: t,
                                    length: self.new_len.max(1),
                                    key,
                                    velocity: NEW_VELOCITY,
                                });
                            }
                            t += step;
                        }
                        self.last_paint = Some((cell, key));
                        if new.len() != notes.len() {
                            out.edits.push((self.set(pattern, new), true));
                            self.sound(host, Some(key));
                        }
                    }
                    DragKind::Erase => {
                        if let Some(i) = hit(p) {
                            let mut new = notes.clone();
                            new.remove(i);
                            out.edits.push((self.set(pattern, new), true));
                        }
                    }
                    DragKind::Frame { add } => {
                        let r = Rect::from_two_pos(d.start, p);
                        let mut sel = if add { d.selection.clone() } else { Vec::new() };
                        for n in &notes {
                            if note_rect(n).intersects(r) && !sel.contains(n) {
                                sel.push(*n);
                            }
                        }
                        self.selection = sel;
                    }
                }
            }
            if response.drag_stopped_by(PointerButton::Primary) && self.drag.take().is_some() {
                self.last_paint = None;
                out.end_gesture = true;
                self.sound(host, None);
            }
            h_offset = clip.left() - rect.left();
        });
        let _ = scroll_out;

        self.velocity_lane(ui, pattern, &notes, px_tick, h_offset, &mut out);
        self.shortcuts(ui, pattern, &notes, &mut out);

        if let Some(req) = out.script.take() {
            use crate::scripts::{ScriptDialog, ScriptRequest};
            let last = crate::scripts::last_run(ui.ctx());
            match req {
                ScriptRequest::OpenFolder => {
                    if let Some(dir) = host.script_folder() {
                        host.reveal_in_file_manager(&dir);
                    }
                }
                ScriptRequest::Open(id) => {
                    if let Some(info) = self.scripts.iter().find(|s| s.id == id) {
                        self.script_dialog = Some(ScriptDialog::new(info.clone(), last.as_ref()));
                    }
                }
                ScriptRequest::RunLast => {
                    if let Some(l) = &last
                        && let Some(info) = self.scripts.iter().find(|s| s.id == l.id)
                    {
                        let mut d = ScriptDialog::new(info.clone(), Some(l));
                        // A changed script asks for its settings again.
                        d.auto_apply = info.hash == l.hash;
                        self.script_dialog = Some(d);
                    }
                }
            }
        }
        if let Some(mut d) = self.script_dialog.take() {
            let input = self.tool_input(pattern, &notes, &sel);
            if let Some(s) = d.preview_selection() {
                // Selection scripts preview by selecting.
                self.selection = s.iter().filter_map(|&i| notes.get(i).copied()).collect();
            }
            let r = d.show(ui.ctx(), host, &input);
            match r.apply {
                Some(jdance_commands::note_tools::ScriptOutput::Notes(o)) => {
                    let select = o.select.iter().map(|&i| o.notes[i]).collect();
                    self.commit(&mut out, pattern, o.notes, select);
                }
                Some(jdance_commands::note_tools::ScriptOutput::Selection(s)) => {
                    self.selection = s.iter().filter_map(|&i| notes.get(i).copied()).collect();
                }
                None => {}
            }
            if !r.close {
                self.script_dialog = Some(d);
            } else {
                self.preview = None;
            }
        }
        if let Some(mut d) = self.tool_dialog.take() {
            let input = self.tool_input(pattern, &notes, &sel);
            let r = d.show(ui.ctx(), &input);
            if let Some(o) = r.apply {
                let select = o.select.iter().map(|&i| o.notes[i]).collect();
                self.commit(&mut out, pattern, o.notes, select);
            }
            if !r.close {
                self.tool_dialog = Some(d);
            } else {
                self.preview = None;
            }
        }
        if let Some(mut d) = self.dialog.take() {
            let idx = indices(&notes, &self.selection);
            let r = d.show(ui.ctx(), &notes, &idx, self.sig);
            if let Some(h) = r.highlight {
                self.scale = h;
            }
            if let Some(new) = r.notes {
                // Keep the same notes selected after the change.
                let select: Vec<Note> = idx.iter().map(|&i| new[i]).collect();
                self.commit(&mut out, pattern, new, select);
            }
            if !r.close {
                self.dialog = Some(d);
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn velocity_lane(
        &mut self,
        ui: &mut egui::Ui,
        pattern: &Pattern,
        notes: &[Note],
        px_tick: f32,
        h_offset: f32,
        out: &mut PianoOutput,
    ) {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("lane_prop")
                .selected_text("Velocity")
                .show_ui(ui, |ui| {
                    let _ = ui.selectable_label(true, "Velocity");
                });
            ui.label(
                RichText::new(
                    "Pan, pitch and other note properties appear here once the engine supports them",
                )
                .size(11.0)
                .color(theme::MUTED),
            );
            ui.separator();
            ui.label(RichText::new("Several notes:").size(11.0).color(theme::MUTED));
            ui.selectable_value(&mut self.relative, true, "Relative")
                .on_hover_text("Dragging one selected note scales all selected velocities");
            ui.selectable_value(&mut self.relative, false, "Same value")
                .on_hover_text("Dragging sets every selected note to the same velocity");
        });
        let (lane, lane_resp) = ui.allocate_exact_size(
            vec2(ui.available_width(), LANE_H - 30.0),
            Sense::click_and_drag(),
        );
        let painter = ui.painter_at(lane);
        painter.rect_filled(
            lane,
            CornerRadius::ZERO,
            Color32::from_rgb(0x11, 0x13, 0x16),
        );
        painter.line_segment(
            [lane.left_top(), lane.right_top()],
            Stroke::new(2.0, theme::LINE),
        );
        let h_max = lane.height() - 10.0;
        let x0 = lane.left() + KEYBOARD_W - h_offset;
        let bar_x = |n: &Note| x0 + n.start as f32 * px_tick;
        for n in notes {
            let x = bar_x(n);
            if x < lane.left() + KEYBOARD_W - 2.0 || x > lane.right() {
                continue;
            }
            let h = h_max * n.velocity;
            let c = if self.selection.contains(n) {
                SELECTED
            } else {
                theme::AMBER
            };
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x, lane.bottom() - 4.0 - h),
                    pos2(x + 5.0, lane.bottom() - 4.0),
                ),
                CornerRadius::same(2),
                c,
            );
        }
        let value_at = |p: Pos2| ((lane.bottom() - 4.0 - p.y) / h_max).clamp(0.0, 1.0);
        let near = |n: &Note, p: Pos2| (bar_x(n) + 2.5 - p.x).abs() < 6.0;
        let press = ui.input(|i| i.pointer.press_origin());
        if (lane_resp.drag_started() || lane_resp.clicked())
            && let Some(p) = press.or(lane_resp.interact_pointer_pos())
        {
            // Selected notes first, then any note under the pointer.
            let grabbed = notes
                .iter()
                .position(|n| self.selection.contains(n) && near(n, p))
                .or_else(|| notes.iter().position(|n| near(n, p)));
            if let Some(g) = grabbed {
                let selected = self.selection.contains(&notes[g]);
                let targets = if selected {
                    indices(notes, &self.selection)
                } else {
                    vec![g]
                };
                self.lane_drag = Some(LaneDrag {
                    original: notes.to_vec(),
                    targets,
                    grabbed: g,
                    selected,
                });
            }
        }
        if let Some(d) = &self.lane_drag
            && let Some(p) = lane_resp.interact_pointer_pos()
            && (lane_resp.dragged() || lane_resp.clicked())
        {
            let v = value_at(p);
            let grabbed = d.original[d.grabbed].velocity;
            let mut new = d.original.clone();
            for &i in &d.targets {
                let o = d.original[i].velocity;
                let x = if self.relative && i != d.grabbed && grabbed > 0.0 {
                    o * v / grabbed
                } else {
                    v
                };
                new[i].velocity = (x.clamp(0.0, 1.0) * 100.0).round() / 100.0;
            }
            if new != notes {
                if d.selected {
                    self.selection = d.targets.iter().map(|&i| new[i]).collect();
                }
                out.edits.push((self.set(pattern, new), true));
            }
        }
        if lane_resp.drag_stopped() || lane_resp.clicked() {
            self.lane_drag = None;
            out.end_gesture = true;
        }
    }

    fn shortcuts(
        &mut self,
        ui: &mut egui::Ui,
        pattern: &Pattern,
        notes: &[Note],
        out: &mut PianoOutput,
    ) {
        if ui.ctx().egui_wants_keyboard_input() || self.dialog.is_some() {
            return;
        }
        let sel = indices(notes, &self.selection);
        let (copy, cut, paste) = ui.input(|i| {
            let has = |f: fn(&Event) -> bool| i.events.iter().any(f);
            (
                has(|e| matches!(e, Event::Copy)),
                has(|e| matches!(e, Event::Cut)),
                has(|e| matches!(e, Event::Paste(_))),
            )
        });
        let cmd = |k| egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, k);
        let (all, dup) = ui.input_mut(|i| {
            (
                i.consume_shortcut(&cmd(Key::A)),
                i.consume_shortcut(&cmd(Key::D)),
            )
        });
        let key = |k| ui.input(|i| i.key_pressed(k) && i.modifiers.is_none());
        let shift_key = |k| ui.input(|i| i.key_pressed(k) && i.modifiers.shift_only());
        for (k, t) in [
            (Key::P, Tool::Draw),
            (Key::B, Tool::Paint),
            (Key::D, Tool::Delete),
            (Key::C, Tool::Slice),
            (Key::E, Tool::Select),
        ] {
            if key(k) {
                self.tool = t;
            }
        }
        if all {
            self.selection = notes.to_vec();
        }
        if (copy || cut) && !sel.is_empty() {
            self.clipboard = self.selection.clone();
        }
        let delete = cut || key(Key::Delete) || key(Key::Backspace);
        if delete && !sel.is_empty() {
            let new = notes
                .iter()
                .filter(|n| !self.selection.contains(n))
                .copied()
                .collect();
            self.commit(out, pattern, new, Vec::new());
            return;
        }
        if paste && !self.clipboard.is_empty() {
            let at = self
                .hover_tick
                .unwrap_or_else(|| self.clipboard.iter().map(Note::end).max().unwrap_or(0));
            let (new, added) = notes::paste(notes, &self.clipboard, at);
            let select = added.iter().map(|&i| new[i]).collect();
            self.commit(out, pattern, new, select);
            return;
        }
        if dup && !sel.is_empty() {
            let (new, added) = notes::duplicate(notes, &sel, self.sig.beat_ticks());
            let select = added.iter().map(|&i| new[i]).collect();
            self.commit(out, pattern, new, select);
            return;
        }
        if sel.is_empty() {
            return;
        }
        // Arrows: transpose (Shift: octave) and move by the grid.
        let semis = if key(Key::ArrowUp) {
            1
        } else if key(Key::ArrowDown) {
            -1
        } else if shift_key(Key::ArrowUp) {
            12
        } else if shift_key(Key::ArrowDown) {
            -12
        } else {
            0
        };
        if semis != 0
            && let Some(new) = notes::transpose(notes, &sel, semis)
        {
            let select = sel.iter().map(|&i| new[i]).collect();
            self.commit(out, pattern, new, select);
            return;
        }
        let step = i64::from(self.snap.max(TICKS_PER_STEP));
        let dt = if key(Key::ArrowRight) {
            step
        } else if key(Key::ArrowLeft) {
            -step
        } else {
            0
        };
        let first = sel.iter().map(|&i| notes[i].start).min().unwrap_or(0);
        if dt != 0 && i64::from(first) + dt >= 0 {
            let mut new = notes.to_vec();
            for &i in &sel {
                new[i].start = (i64::from(new[i].start) + dt) as u32;
            }
            let select = sel.iter().map(|&i| new[i]).collect();
            self.commit(out, pattern, new, select);
        }
    }

    /// Releases a note still sounding (the roll is closing).
    pub fn release(&mut self, host: &mut dyn Host) {
        self.sound(host, None);
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
        assert_eq!(p.min_len(), 1);
    }

    #[test]
    fn lengths_read_in_beats() {
        assert_eq!(length_text(960, 960), "1 beat(s)");
        assert_eq!(length_text(480, 960), "1/2 beat");
        assert_eq!(length_text(1200, 960), "5 steps");
        assert_eq!(length_text(700, 960), "700 ticks");
        assert_eq!(length_text(960, 480), "2 beat(s)", "6/8 beat");
    }

    #[test]
    fn slicing_keeps_key_velocity_and_both_parts() {
        let n = Note {
            id: 0,
            start: 240,
            length: 960,
            key: 60,
            velocity: 0.7,
        };
        let (a, b) = slice_note(n, 480).unwrap();
        assert_eq!((a.start, a.length, b.start, b.length), (240, 240, 480, 720));
        assert_eq!((a.key, b.key, a.velocity, b.velocity), (60, 60, 0.7, 0.7));
        assert!(slice_note(n, 240).is_none() && slice_note(n, 1200).is_none());
    }
}

#[cfg(test)]
mod caret_tests {
    use super::*;
    use crate::test_ui::{Driver, StubHost};
    use egui::Pos2;

    /// PCR-005: the ruler takes the caret along, inside the pattern.
    #[test]
    fn the_ruler_drags_the_pattern_caret() {
        let mut p = Project::new();
        let ch = ChannelId(p.ids.next());
        p.channels
            .push(jdance_domain::Channel::new_sampler(ch, "c"));
        let pattern = p.patterns[0].clone();
        let len = pattern.length_ticks();
        let mut roll = PianoRoll::new(ch);
        let mut host = StubHost::default();
        let mut d = Driver::new();
        let mut requests = Vec::new();
        let marks = crate::rack::PatternMarks {
            playhead: None,
            caret: Some(0),
        };
        let mut ui = |ui: &mut egui::Ui| {
            let out = roll.show(ui, &p, &pattern, &mut host, marks, Snap::Step);
            requests.extend(out.caret);
        };
        d.frame(vec![], &mut ui);
        d.frame(vec![], &mut ui);
        let ruler = d.find_rect("Timeline ruler").expect("ruler");
        // 28 px per step (240 ticks) at the default zoom; the ruler starts
        // right of the keyboard, at tick 0.
        let at = |t: u64| {
            Pos2::new(
                ruler.left() + t as f32 * 28.0 / 240.0 + 0.1,
                ruler.center().y,
            )
        };
        d.frame(vec![d.moved(at(480))], &mut ui);
        d.frame(vec![d.button(at(480), true)], &mut ui);
        d.frame(vec![d.moved(at(960))], &mut ui);
        d.frame(vec![d.moved(at(len + 2000))], &mut ui);
        d.frame(vec![d.button(at(len + 2000), false)], &mut ui);
        assert_eq!(
            requests,
            [
                CaretRequest::Preview(480),
                CaretRequest::Preview(960),
                CaretRequest::Preview(len - 240),
                CaretRequest::Seek(len - 240),
            ],
            "kept inside the pattern"
        );
    }
}
