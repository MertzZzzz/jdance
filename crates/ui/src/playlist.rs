// SPDX-License-Identifier: GPL-3.0-or-later
//! Playlist editor: lanes with pattern and audio clips (spec 0.2.0 §2,
//! §4, §8; UI-VIS §7).
//!
//! Tools (shortcut): Draw (P), Paint (B), Delete (D), Slice (C), Select
//! (E). With Draw, Paint and Select a clip body drags (the selection
//! moves) and either edge resizes; Ctrl-drag selects with a frame in any
//! tool. Draw places one copy of the source, Paint a row of copies, Delete
//! removes what it touches, Slice cuts at the pointer. In every tool a
//! right click deletes the clip under it, and holding the right button
//! while moving deletes every clip passed over. The clip menu opens from
//! the marker at the start of a clip's name or with Shift + right click.
//! Every gesture is one undo step. Files dropped from the Browser or the
//! system become audio clips; below the last lane a new lane is made.
//! Double-click opens a clip's editor. Ruler (spec 0.2.4): the left button
//! takes the caret along; Shift-drag makes a time selection (the loop
//! range), moves one or moves its edge; a double click clears it. Alt
//! disables snapping while dragging.
//!
//! Automation clips (spec 0.2.0 §11) place a range of an automation
//! source like any clip; their points are edited in the Automation
//! Editor (double-click).

use crate::automation::AUTOMATION_COLOR;
use crate::automation_editor::envelope_line;
use crate::browser::BrowserItem;
use crate::icons::{self, Glyph};
use crate::project_browser::ProjectItem;
use crate::toolbar::bars_text;
use crate::{Host, theme};
use egui::{
    Align2, Color32, CornerRadius, FontId, PointerButton, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, pos2, vec2,
};
use jdance_commands::{Edge, Edit, audio_clip_edge, pattern_clip_edge};
use jdance_domain::{
    AssetId, AudioAsset, AudioClip, AutomationClip, AutomationId, ChannelId, ClipId, LaneId,
    PatternClip, PatternId, Project, SampleRef, TICKS_PER_STEP, TimeSignature, WHOLE_NOTE,
    frames_per_tick,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ROW_H: f32 = 32.0;
const RULER_H: f32 = 20.0;
const HEADER_W: f32 = 212.0;
const EDGE_PX: f32 = 6.0;
const NAME_H: f32 = 13.0;
const MARKER_W: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Draw,
    Paint,
    Delete,
    Slice,
    Select,
    /// Spec 0.2.9 §4: dragging an audio clip changes its speed.
    Speed,
}

impl Tool {
    const ALL: [(Self, Glyph, &'static str); 6] = [
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
            "Delete (D): click or drag over clips · in every tool the right button deletes",
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
        (
            Self::Speed,
            Glyph::Speed,
            "Speed (S): drag an audio clip to make it shorter (faster) or longer (slower); \
             double-click: normal speed",
        ),
    ];
}

/// What Draw and Paint place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Pattern(PatternId),
    Asset(AssetId),
    /// An audio channel: new clips are its instances (spec 0.2.2 ACH-043).
    Channel(ChannelId),
    Automation(AutomationId),
}

/// The state a clicked clip gives the copies Draw and Paint place (spec
/// 0.3.0 §9). Mute is not copied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Like {
    Pattern {
        offset: u64,
        duration: u64,
        looped: bool,
    },
    Audio {
        source_in: u64,
        source_out: u64,
        speed: f32,
        gain: f32,
        fade_in: u64,
        fade_out: u64,
    },
    Automation {
        offset: u64,
        duration: u64,
    },
}

impl Like {
    fn of(item: &Item) -> Self {
        match *item {
            Item::Pattern(c) => Like::Pattern {
                offset: c.offset,
                duration: c.duration,
                looped: c.looped,
            },
            Item::Audio(c) => Like::Audio {
                source_in: c.source_in,
                source_out: c.source_out,
                speed: c.speed,
                gain: c.gain,
                fade_in: c.fade_in,
                fade_out: c.fade_out,
            },
            Item::Automation(c) => Like::Automation {
                offset: c.offset,
                duration: c.duration,
            },
        }
    }

    /// `clip` with this state, when it is of the same kind.
    pub fn apply_audio(self, clip: AudioClip) -> AudioClip {
        match self {
            Like::Audio {
                source_in,
                source_out,
                speed,
                gain,
                fade_in,
                fade_out,
            } => AudioClip {
                source_in,
                source_out,
                speed,
                gain,
                fade_in,
                fade_out,
                ..clip
            },
            _ => clip,
        }
    }

    pub fn apply_pattern(self, clip: PatternClip) -> PatternClip {
        match self {
            Like::Pattern {
                offset,
                duration,
                looped,
            } => PatternClip {
                offset,
                duration,
                looped,
                ..clip
            },
            _ => clip,
        }
    }

    pub fn apply_automation(self, clip: AutomationClip) -> AutomationClip {
        match self {
            Like::Automation { offset, duration } => AutomationClip {
                offset,
                duration,
                ..clip
            },
            _ => clip,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    Edge(ClipId, Edge),
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
    /// The Speed tool on an audio clip: its end follows the pointer.
    Speed(ClipId),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    kind: DragKind,
    origin: Pos2,
}

/// View state kept by the app between frames.
#[derive(Debug)]
pub(crate) struct PlaylistView {
    /// Pixels per whole note.
    zoom: f32,
    selection: BTreeSet<ClipId>,
    drag: Option<Drag>,
    context_clip: Option<ClipId>,
    /// Snap grid in ticks (1 = off), from the toolbar.
    grid: u64,
    pub tool: Tool,
    pub source: Option<Source>,
    /// The clicked clip whose state the copies take (spec 0.3.0 §9);
    /// `None` = the source as it is.
    pub like: Option<Like>,
    /// Pattern current at the last frame: choosing another one makes it
    /// the source.
    last_pattern: Option<PatternId>,
    /// Clips touched by a Delete drag or a right-button sweep.
    erased: BTreeSet<ClipId>,
    /// Right-button delete gesture.
    rmb: crate::widgets::RightSweep,
    /// Lane that receives the next recording (spec 0.2.0 §7).
    pub armed: Option<LaneId>,
    /// The Song caret, drawn in song mode (spec 0.2.3 CUR-005).
    pub caret: Option<u64>,
    /// Caret and time-selection gestures on the ruler (spec 0.2.4).
    ruler: crate::ruler::Ruler,
    /// The take being recorded, drawn as a growing clip (REC-014).
    pub live: Option<LiveTake>,
    /// Scroll just enough to show this tick (new copies, spec 0.2.5 §1).
    pub reveal: Option<u64>,
    /// Zoom keeps the mouse rather than the caret in place (§2).
    pub zoom_to_mouse: bool,
    /// What a drag in progress would name and colour (§8).
    pub drop: Option<crate::drop_name::Dropped>,
    /// The clip area as of the last frame.
    view: Option<View>,
    /// Zoom before Shift+4 went to the widest view.
    zoom_back: Option<f32>,

    /// The editor keys go here (the app sets it, spec 0.2.7 §1 item 1).
    pub keys_active: bool,
    /// Where the Playlist was drawn, for the app's key focus.
    pub area: Rect,
    /// A press inside the Playlist this frame (spec 0.3.0 §1.3).
    pub pressed: bool,
    /// Times Ctrl+D showed that duplicating moved to Ctrl+B (spec 0.2.7
    /// §3 item 6: at most three).
    ctrl_d_hints: u8,

    /// Keep the playhead in view while playing (REC-017).
    pub follow: bool,
    /// The user scrolled or zoomed away; following waits until the
    /// playhead is visible again.
    follow_paused: bool,
}

/// Zoom range in pixels per whole note.
const MIN_ZOOM: f32 = 30.0;
const MAX_ZOOM: f32 = 480.0;
/// Page Up / Page Down zoom factor.
const ZOOM_STEP: f32 = 1.25;

/// The visible clip area of the last frame.
#[derive(Debug, Clone, Copy)]
struct View {
    /// Horizontal scroll offset in pixels.
    offset: f32,
    width: f32,
    /// Screen x of the visible area's left edge.
    left: f32,
    rect: Rect,
}

/// Left edge, in ticks, after zooming from `z` to `z2` pixels per tick so
/// tick `t` keeps its place on screen; never before 0 (spec 0.2.5 §2:
/// L' = T − (T − L)·z/z').
fn zoom_left(left: f32, z: f32, z2: f32, t: f32) -> f32 {
    (t - (t - left) * z / z2).max(0.0)
}

/// The take being recorded (spec 0.2.3 REC-014). Not a project object.
#[derive(Debug, Clone, Default)]
pub(crate) struct LiveTake {
    /// Lane of the take; `None` = the new-lane row under the lanes.
    pub lane: Option<LaneId>,
    /// Caret position where the take starts, in ticks.
    pub start: u64,
    pub name: String,
    /// The largest absolute sample per few hundred frames, oldest first.
    pub peaks: Vec<f32>,
}

impl Default for PlaylistView {
    fn default() -> Self {
        Self {
            zoom: 120.0,
            selection: BTreeSet::new(),
            drag: None,
            context_clip: None,
            grid: u64::from(TICKS_PER_STEP),
            tool: Tool::Draw,
            source: None,
            like: None,
            last_pattern: None,
            erased: BTreeSet::new(),
            rmb: crate::widgets::RightSweep::default(),
            armed: None,
            caret: None,
            ruler: crate::ruler::Ruler::default(),
            live: None,
            reveal: None,
            zoom_to_mouse: false,
            drop: None,
            view: None,
            zoom_back: None,
            follow: true,
            follow_paused: false,
            ctrl_d_hints: 0,
            keys_active: true,
            area: Rect::NOTHING,
            pressed: false,
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
        /// The state the copies take (spec 0.3.0 §9).
        like: Option<Like>,
    },
    Duplicate(Vec<ClipId>),
    MakeUnique(ClipId),
    /// Audio clips of one channel move to a new channel (ACH-024/025).
    MakeUniqueAudio(Vec<ClipId>),
    /// Select (and reveal) a channel in the Channel Rack.
    SelectChannel(ChannelId),
    /// The audio channel's file into a new sampler channel.
    ToSampler(ChannelId),
    AddLane,
    RenameLane(LaneId),
    /// The colour window for a lane (spec 0.2.5 §6).
    Color(LaneId),
    /// The dragged pattern or sample was dropped on this lane's header:
    /// its name and colour (spec 0.2.5 §8).
    NameFromDrop(LaneId),
    /// Default name and colour (§8 item 10).
    ResetLane(LaneId),
    /// Put the caret and the playhead at a position.
    Seek(u64),
    /// The caret during a ruler gesture: the playhead follows only while
    /// stopped (spec 0.2.4 CUR-012).
    CaretPreview(u64),
    /// Open the export with the Selection range (SEL-007).
    RenderSelection,
    /// A MIDI file as a new pattern and its clip (spec 0.3.0 §2.4);
    /// `lane: None` = a new lane.
    ImportMidi {
        path: PathBuf,
        lane: Option<LaneId>,
        start: u64,
    },
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
    /// Automate an audio clip's gain.
    AutomateGain(ClipId),
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
    /// A clicked pattern clip made its pattern the current one.
    pub select_pattern: Option<PatternId>,
}

fn color(c: [u8; 3]) -> Color32 {
    theme::rgb(c)
}

/// A length as whole bars, beats and ticks: "4:2:000".
fn length_text(ticks: u64, sig: TimeSignature) -> String {
    let (bars, beats, rest) = sig.split(ticks);
    format!("{bars}:{beats}:{rest:03}")
}

/// `c` with the speed that puts its end on `end_tick`, its start staying
/// (spec 0.2.9 §4.2): faster when shorter, within ×0.25…×4.
fn sped_up(project: &Project, c: &AudioClip, end_tick: i64) -> AudioClip {
    let Some((a, b)) = project.audio_clip_ticks(c) else {
        return *c;
    };
    let new_len = (end_tick as f64 - a).max(1.0);
    let speed = (f64::from(c.speed) * (b - a) / new_len) as f32;
    AudioClip {
        speed: speed.clamp(jdance_domain::MIN_CLIP_SPEED, jdance_domain::MAX_CLIP_SPEED),
        ..*c
    }
}

/// The length factor of the sampler playing `c` (1 for audio channels).
fn stretch_of(project: &Project, c: &AudioClip) -> f64 {
    project
        .channel(c.channel)
        .map_or(1.0, |ch| project.channel_stretch(ch))
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
    Automation(AutomationClip),
}

impl Item {
    fn id(&self) -> ClipId {
        match self {
            Item::Pattern(c) => c.id,
            Item::Audio(c) => c.id,
            Item::Automation(c) => c.id,
        }
    }

    /// What Draw and Paint place after a click on this clip (spec 0.2.7
    /// §2): its pattern, its audio channel (the sample) or its source.
    fn source(&self) -> Source {
        match self {
            Item::Pattern(c) => Source::Pattern(c.pattern),
            Item::Audio(c) => Source::Channel(c.channel),
            Item::Automation(c) => Source::Automation(c.source),
        }
    }
}

/// An automation clip as a non-looped pattern clip over a source of
/// `len` ticks: the same edge rules apply.
fn as_pattern_clip(c: &AutomationClip) -> PatternClip {
    PatternClip {
        id: c.id,
        lane: c.lane,
        pattern: PatternId(0),
        start: c.start,
        offset: c.offset,
        duration: c.duration,
        looped: false,
        muted: c.muted,
    }
}

fn span(project: &Project, item: &Item) -> Option<(f64, f64)> {
    match item {
        Item::Pattern(c) => {
            let (a, b) = c.interval();
            Some((a as f64, b as f64))
        }
        Item::Audio(c) => project.audio_clip_ticks(c),
        Item::Automation(c) => {
            let (a, b) = c.interval();
            Some((a as f64, b as f64))
        }
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
        .chain(pl.audio_clips.iter().map(|c| (c.lane, Item::Audio(*c))))
        .chain(
            pl.automation_clips
                .iter()
                .map(|c| (c.lane, Item::Automation(*c))),
        );
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
        Source::Channel(id) => {
            let ch = project.channel(id)?;
            let a = project.asset(ch.clip_asset_id()?)?.id;
            Some(source_len(project, Source::Asset(a))? * project.channel_stretch(ch))
        }
        Source::Automation(id) => project.automation_lane(id).map(|l| l.length as f64),
    }
}

/// Length in ticks of one copy of `source` with the state `like` (spec
/// 0.3.0 §9).
fn copy_len(project: &Project, source: Source, like: Option<Like>) -> Option<f64> {
    match (source, like) {
        (Source::Pattern(_), Some(Like::Pattern { duration, .. }))
        | (Source::Automation(_), Some(Like::Automation { duration, .. })) => Some(duration as f64),
        (Source::Channel(ch), Some(l @ Like::Audio { .. })) => {
            let asset = project.asset(project.channel(ch)?.clip_asset_id()?)?;
            let clip = l.apply_audio(jdance_commands::audio_channels::audio_clip(
                ClipId(0),
                LaneId(0),
                ch,
                asset,
                0,
            ));
            project.audio_clip_ticks(&clip).map(|(a, b)| b - a)
        }
        _ => source_len(project, source),
    }
}

fn source_name(project: &Project, source: Source) -> Option<String> {
    match source {
        Source::Pattern(id) => project.pattern(id).map(|p| p.name.clone()),
        Source::Asset(id) => project.asset(id).map(|a| a.name.clone()),
        Source::Channel(id) => project.channel(id).map(|c| c.name.clone()),
        Source::Automation(id) => project.automation_lane(id).map(|l| l.name.clone()),
    }
}

impl PlaylistView {
    /// `zoom` is pixels per whole note, so a signature change keeps the
    /// view's scale.
    fn px_per_tick(&self) -> f32 {
        self.zoom / WHOLE_NOTE as f32
    }

    /// Keeps the playhead at `x` in view: a page jump when it reaches the
    /// right edge. Always while recording, else with View › Follow; manual
    /// scrolling or zooming pauses it until the playhead shows again
    /// (REC-017).
    fn follow_playhead(&mut self, ui: &mut egui::Ui, x: Option<f32>) {
        let Some(x) = x else {
            self.follow_paused = false;
            return;
        };
        if !self.follow && self.live.is_none() {
            return;
        }
        let view = ui.clip_rect();
        let user = ui.rect_contains_pointer(view)
            && ui.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO || i.zoom_delta() != 1.0);
        if user {
            self.follow_paused = true;
            return;
        }
        let visible = x >= view.left() && x <= view.right();
        if self.follow_paused {
            self.follow_paused = !visible;
            return;
        }
        if x > view.right() - 4.0 || x < view.left() {
            ui.scroll_to_rect(
                Rect::from_min_size(pos2(x, view.top()), vec2(view.width(), 1.0)),
                Some(egui::Align::Min),
            );
        }
    }

    /// Makes the source of a clicked clip the active one (§2 items 1–3).
    fn activate(&mut self, item: &Item, out: &mut PlaylistOutput) {
        let source = item.source();
        self.source = Some(source);
        self.like = Some(Like::of(item));
        if let Source::Pattern(id) = source {
            self.last_pattern = Some(id);
            out.select_pattern = Some(id);
        }
    }

    pub(crate) fn retain_existing(&mut self, project: &Project) {
        let pl = &project.playlist;
        self.selection.retain(|id| pl.has_clip(*id));
        let valid = match self.source {
            Some(Source::Pattern(id)) => project.pattern(id).is_some(),
            Some(Source::Asset(id)) => project.asset(id).is_some(),
            Some(Source::Channel(id)) => project
                .channel(id)
                .is_some_and(|c| c.clip_asset_id().is_some() || c.sample().is_some()),
            Some(Source::Automation(id)) => project.automation_lane(id).is_some(),
            None => true,
        };
        if !valid {
            // The active source was removed: the first pattern, if any
            // (§2 item 12).
            self.source = project.patterns.first().map(|p| Source::Pattern(p.id));
            self.like = None;
        }
        if self.armed.is_some_and(|l| pl.lane(l).is_none()) {
            self.armed = None;
        }
    }

    /// Zoom keys and the mouse wheel over the clips (spec 0.2.5 §2):
    /// Ctrl+wheel, Page Up / Page Down, Shift+1…3 levels, Shift+4 the
    /// widest view and back. Shift+5 fits the time selection; returns its
    /// start then.
    fn zoom_input(&mut self, ui: &egui::Ui, project: &Project) -> Option<u64> {
        let ctx = ui.ctx();
        let over = self
            .view
            .zip(ctx.pointer_latest_pos())
            .is_some_and(|(v, p)| v.rect.contains(p));
        if over {
            let f = ctx.input(|i| i.zoom_delta());
            if f != 1.0 {
                self.zoom *= f;
            }
        }
        if ctx.text_edit_focused() {
            self.zoom = self.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
            return None;
        }
        let shift = |k| ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, k));
        let plain = |k| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));
        let mut fit = None;
        if plain(egui::Key::PageUp) {
            self.zoom *= ZOOM_STEP;
        }
        if plain(egui::Key::PageDown) {
            self.zoom /= ZOOM_STEP;
        }
        for (k, z) in [
            (egui::Key::Num1, 60.0),
            (egui::Key::Num2, 120.0),
            (egui::Key::Num3, 240.0),
        ] {
            if shift(k) {
                self.zoom = z;
            }
        }
        if shift(egui::Key::Num4) {
            match self.zoom_back.take() {
                Some(z) if self.zoom <= MIN_ZOOM => self.zoom = z,
                _ => {
                    self.zoom_back = Some(self.zoom);
                    self.zoom = MIN_ZOOM;
                }
            }
        }
        if shift(egui::Key::Num5)
            && let Some((a, b)) = project.playlist.loop_range.filter(|(a, b)| b > a)
            && let Some(v) = self.view
        {
            // The selection fills the view.
            self.zoom = v.width / (b - a) as f32 * WHOLE_NOTE as f32;
            fit = Some(a);
        }
        self.zoom = self.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        fit
    }

    /// The scroll offset that keeps the anchor in place after the zoom
    /// changed from `before` (spec 0.2.5 §2): the caret (or playhead) when
    /// it is in view, else the point under the mouse, else the centre;
    /// with Zoom to mouse the mouse first. `fit` is a tick to put at the
    /// left edge instead.
    fn zoom_offset(
        &self,
        before: f32,
        caret: Option<u64>,
        fit: Option<u64>,
        pointer: Option<Pos2>,
    ) -> Option<f32> {
        let v = self.view?;
        let z = before / WHOLE_NOTE as f32;
        let z2 = self.px_per_tick();
        if let Some(a) = fit {
            return Some(a as f32 * z2);
        }
        if (z2 - z).abs() < f32::EPSILON {
            return None;
        }
        let left = v.offset / z;
        let in_view = |t: f32| t >= left && t <= left + v.width / z;
        let mouse = pointer
            .filter(|p| v.rect.contains(*p))
            .map(|p| left + (p.x - v.left) / z);
        let caret = caret.map(|c| c as f32).filter(|t| in_view(*t));
        let centre = left + v.width / 2.0 / z;
        let anchor = if self.zoom_to_mouse {
            mouse.or(caret)
        } else {
            caret.or(mouse)
        }
        .unwrap_or(centre);
        Some(zoom_left(left, z, z2, anchor) * z2)
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
        playhead: Option<u64>,
        snap: u32,
        current_pattern: PatternId,
        host: &mut dyn Host,
        project_dir: Option<&Path>,
    ) -> PlaylistOutput {
        self.grid = u64::from(snap.max(1));
        if self.last_pattern != Some(current_pattern) {
            self.last_pattern = Some(current_pattern);
            if self.source != Some(Source::Pattern(current_pattern)) {
                self.like = None;
            }
            self.source = Some(Source::Pattern(current_pattern));
        }
        let mut out = PlaylistOutput::default();
        self.retain_existing(project);
        let zoom_before = self.zoom;
        self.header(ui, project);
        let fit = self.zoom_input(ui, project);
        let pointer = ui.ctx().pointer_latest_pos();
        let offset = self.zoom_offset(zoom_before, playhead.or(self.caret), fit, pointer);

        let mut view = None;
        egui::ScrollArea::vertical()
            .id_salt("playlist-v")
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    self.lane_headers(ui, project, &mut out);
                    let mut h = egui::ScrollArea::horizontal().id_salt("playlist-h");
                    if let Some(o) = offset {
                        h = h.horizontal_scroll_offset(o);
                    }
                    let o = h.show(ui, |ui| {
                        // Spec 0.3.0 §6: the view follows the mouse.
                        crate::widgets::middle_pan(ui);
                        crate::widgets::edge_scroll(ui, self.drag.is_some());
                        self.canvas(ui, project, playhead, host, project_dir, &mut out);
                    });
                    view = Some(View {
                        offset: o.state.clamped_offset().x,
                        width: o.inner_rect.width(),
                        left: o.inner_rect.left(),
                        rect: o.inner_rect,
                    });
                });
            });
        self.view = view;
        self.shortcuts(ui, project, &mut out);
        self.area = ui.min_rect();
        self.pressed = crate::widgets::pressed_in(ui, self.area);
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
                egui::Slider::new(&mut self.zoom, MIN_ZOOM..=MAX_ZOOM)
                    .logarithmic(true)
                    .show_value(false)
                    .text("Zoom"),
            )
            .on_hover_text(
                "Also Ctrl+wheel, Page Up / Page Down, Shift+1…4; Shift+5 fits the selection",
            );
            if !self.selection.is_empty() {
                ui.label(
                    RichText::new(format!("{} selected", self.selection.len())).color(theme::MUTED),
                );
            }
        });
    }

    /// Delete, Ctrl+B (duplicate), Ctrl+D (deselect) and tool keys, when
    /// no text field has focus (spec 0.2.7 §3).
    fn shortcuts(&mut self, ui: &egui::Ui, project: &Project, out: &mut PlaylistOutput) {
        let ctx = ui.ctx().clone();
        if !self.keys_active || ctx.text_edit_focused() {
            return;
        }
        let (del, dup, deselect) = ctx.input_mut(|i| {
            let del = crate::bindings::DELETE.take(i) || i.key_pressed(egui::Key::Backspace);
            let dup = i.consume_shortcut(&crate::bindings::DUPLICATE.shortcut());
            let deselect = i.consume_shortcut(&crate::bindings::DESELECT.shortcut());
            (del, dup, deselect)
        });
        if deselect {
            if !self.selection.is_empty() && self.ctrl_d_hints < 3 {
                self.ctrl_d_hints += 1;
                out.hint = Some(format!(
                    "{} clears the selection; duplicate is {}",
                    crate::bindings::DESELECT.label(),
                    crate::bindings::DUPLICATE.label()
                ));
            }
            self.selection.clear();
            return;
        }
        use crate::bindings as b;
        for (bnd, t) in [
            (b::TOOL_DRAW, Tool::Draw),
            (b::TOOL_PAINT, Tool::Paint),
            (b::TOOL_DELETE, Tool::Delete),
            (b::TOOL_SLICE, Tool::Slice),
            (b::TOOL_SELECT, Tool::Select),
            (b::TOOL_SPEED, Tool::Speed),
        ] {
            if ctx.input_mut(|i| bnd.take(i)) {
                self.tool = t;
            }
        }
        // Esc clears the selection (spec 0.2.5 §1, open question 4).
        if self.ruler.active().is_none() && ctx.input_mut(|i| b::CLEAR_SELECTION.take(i)) {
            self.selection.clear();
        }
        if self.selection.is_empty() {
            if dup {
                out.hint = Some("No clips selected".into());
            }
            return;
        }
        if del {
            out.edits.push(delete_edits(project, &self.selection));
            self.selection.clear();
        } else if dup {
            out.actions.push(PlaylistAction::Duplicate(
                self.selection.iter().copied().collect(),
            ));
        }
    }

    fn lane_headers(&mut self, ui: &mut egui::Ui, project: &Project, out: &mut PlaylistOutput) {
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
                // A pattern or sample dropped on the header names and
                // colours the lane; no clip is added (spec 0.2.5 §8).
                if let Some(d) = &self.drop {
                    let resp = child.interact(
                        row,
                        egui::Id::unique(("lane_drop", lane.id.0)),
                        Sense::hover(),
                    );
                    if resp.contains_pointer() {
                        crate::drop_name::feedback(&child, row, d);
                    }
                    if crate::drop_name::released_on(&resp) {
                        out.actions.push(PlaylistAction::NameFromDrop(lane.id));
                    }
                }
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
                        if ui.button("Color…").clicked() {
                            out.actions.push(PlaylistAction::Color(lane.id));
                            ui.close();
                        }
                        if ui.button("Reset name and color").clicked() {
                            out.actions.push(PlaylistAction::ResetLane(lane.id));
                            ui.close();
                        }
                        if ui.button("Remove lane").clicked() {
                            out.edits.push(Edit::RemoveLane(lane.id));
                            ui.close();
                        }
                    });
                let lamp = crate::widgets::lamp(ui, !lane.muted).on_hover_text(
                    "Mute lane (click) · solo (right-click; Ctrl+right-click adds to the solo)",
                );
                if lane.solo {
                    crate::widgets::solo_ring(ui, &lamp);
                }
                if lamp.clicked() {
                    out.edits.push(Edit::SetLaneMuted(lane.id, !lane.muted));
                }
                // Spec 0.3.0 §7: the right button solos.
                if lamp.secondary_clicked() {
                    let add = ui.input(|i| i.modifiers.command);
                    out.edits
                        .extend(jdance_commands::solo_lane(project, lane.id, add));
                }
                let mut s = lane.solo;
                if ui
                    .toggle_value(&mut s, RichText::new("S").size(11.0))
                    .on_hover_text("Solo lane")
                    .changed()
                {
                    out.edits.push(Edit::SetLaneSolo(lane.id, s));
                }
                let armed = self.armed == Some(lane.id)
                    || self.live.as_ref().is_some_and(|t| t.lane == Some(lane.id));
                if arm_button(ui, armed)
                    .on_hover_text("Arm for recording: the next take goes on this lane")
                    .clicked()
                {
                    self.armed = if armed { None } else { Some(lane.id) };
                }
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
        // Room for the caret and a take recorded past the end.
        let end = project
            .arrangement_end_ticks()
            .max(self.caret.unwrap_or(0))
            .max(playhead.unwrap_or(0));
        let sig = project.time_signature;
        let bar_ticks = u64::from(sig.bar_ticks());
        let bars = (end / bar_ticks + 8).max(16);
        let width = bars as f32 * bar_ticks as f32 * ppt;
        let n_lanes = pl.lanes.len();
        // One extra row below the lanes: dropping there adds a lane.
        let lanes_bottom = RULER_H + (n_lanes + 1) as f32 * ROW_H;
        let height = lanes_bottom;
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

        self.background(&painter, rect, sig, bars, n_lanes, lanes_bottom);
        self.ruler_input(ui, project, rect, ppt, out);
        let gesture = self.ruler.active().copied();
        if gesture.is_some() {
            // The ruler scrolls the view itself (CUR-013).
            self.follow_paused = true;
        }
        let selecting = gesture.filter(|g| g.selecting());
        let loop_range = match selecting {
            Some(g) => g.selection(),
            None => pl.loop_range,
        };
        if let Some((a, b)) = loop_range {
            let fill = if pl.loop_enabled || selecting.is_some() {
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
        if let Some(g) = selecting
            && let Some((a, b)) = g.selection()
        {
            // The selection being made, across the lanes (SEL-005).
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x_of(a), origin.y + RULER_H),
                    pos2(x_of(b), origin.y + lanes_bottom),
                ),
                CornerRadius::ZERO,
                theme::AMBER.gamma_multiply(0.08),
            );
            let text = if g.moving() {
                bars_text(a, sig)
            } else {
                length_text(b - a, sig)
            };
            painter.text(
                pos2(x_of(b) + 4.0, origin.y + 2.0),
                Align2::LEFT_TOP,
                text,
                FontId::monospace(10.0),
                theme::TEXT,
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

        if response.drag_started_by(PointerButton::Primary)
            && let Some(p) = press.or(response.interact_pointer_pos())
        {
            // The ruler has its own gestures (CUR-015).
            let kind = if p.y < origin.y + RULER_H {
                None
            } else if ctrl {
                Some(DragKind::Frame { add: shift })
            } else {
                match (hit(p), self.tool) {
                    (_, Tool::Delete) => {
                        self.erased.clear();
                        Some(DragKind::Erase)
                    }
                    (_, Tool::Slice) => None,
                    (Some((k, _)), Tool::Speed) => match items[k].item {
                        Item::Audio(c) => {
                            self.activate(&items[k].item, out);
                            self.selection = BTreeSet::from([c.id]);
                            Some(DragKind::Speed(c.id))
                        }
                        _ => {
                            out.hint = Some("Speed changes audio and sampler clips".into());
                            None
                        }
                    },
                    (None, Tool::Speed) => None,
                    (Some((k, edge)), _) => {
                        let id = items[k].item.id();
                        self.activate(&items[k].item, out);
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
                (Some(DragKind::Speed(id)), Item::Audio(c)) if id == c.id => {
                    Item::Audio(sped_up(project, &c, edge_tick))
                }
                (Some(DragKind::Edge(id, e)), Item::Audio(c)) if id == c.id => {
                    match project.clip_asset(&c) {
                        Some(a) => {
                            let at = (edge_tick as f64 * fpt).round() as i64;
                            Item::Audio(audio_clip_edge(
                                &c,
                                a,
                                project.timebase_rate,
                                stretch_of(project, &c),
                                e,
                                at,
                            ))
                        }
                        None => g.item,
                    }
                }
                (Some(DragKind::Move), Item::Automation(c)) if selected => {
                    Item::Automation(AutomationClip {
                        start: (c.start as i64 + dt).max(0) as u64,
                        ..c
                    })
                }
                (Some(DragKind::Edge(id, e)), Item::Automation(c)) if id == c.id => {
                    let len = project.automation_lane(c.source).map_or(1, |l| l.length);
                    let (start, offset, duration) =
                        pattern_clip_edge(&as_pattern_clip(&c), len, e, edge_tick);
                    Item::Automation(AutomationClip {
                        start,
                        offset,
                        duration,
                        ..c
                    })
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
                match (lane_at(p.y), copy_len(project, src, self.like)) {
                    (Some(l), Some(len)) if l < n_lanes => {
                        let a = snapped(p.x) as f64;
                        vec![(l, a, a + len)]
                    }
                    _ => Vec::new(),
                }
            }
            (Some(DragKind::Paint { lane, start }), Some(p), Some(src)) if lane < n_lanes => {
                match copy_len(project, src, self.like) {
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
            && response.dragged_by(PointerButton::Primary)
            && let Some(p) = pointer
            && let Some((k, _)) = hit(p)
        {
            self.erased.insert(items[k].item.id());
        }
        // Frame selection.
        if let (Some(DragKind::Frame { add }), Some(d), Some(p)) = (drag_kind, self.drag, pointer)
            && response.dragged_by(PointerButton::Primary)
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

        // Commit on release: one undo step per gesture.
        if response.drag_stopped_by(PointerButton::Primary)
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
                DragKind::Edge(id, _) | DragKind::Speed(id) => {
                    if let Some(g) = items.iter().find(|g| g.item.id() == id) {
                        let after = moved(g);
                        if after.start != g.start || after.end != g.end {
                            out.edits.push(commit(&after, &pl.lanes));
                        }
                    }
                }
                DragKind::Place | DragKind::Paint { .. } => {
                    if let (Some(&(lane, start, _)), Some(source)) = (ghosts.first(), self.source) {
                        out.actions.push(PlaylistAction::Place {
                            source,
                            lane: Some(pl.lanes[lane].id),
                            start: start as u64,
                            count: ghosts.len() as u32,
                            like: self.like,
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
                DragKind::Frame { .. } => {
                    // A frame over clips of one source makes it active;
                    // of several, the active source stays (§2 item 8).
                    let mut sources = items
                        .iter()
                        .filter(|g| self.selection.contains(&g.item.id()))
                        .map(|g| (g.item.source(), g.item));
                    if let Some((first, item)) = sources.next()
                        && sources.all(|(s, _)| s == first)
                    {
                        self.activate(&item, out);
                    }
                }
                DragKind::Move => {}
            }
        }

        // Right button: a click deletes the clip under it, holding it and
        // moving deletes every clip it passes over (one undo step), in
        // every tool. Shift + right click opens the clip menu instead.
        let visible = ui.clip_rect();
        let mut open_menu = None;
        let rmb_pressed = ui.input(|i| i.pointer.button_pressed(PointerButton::Secondary));
        if rmb_pressed
            && shift
            && self.drag.is_none()
            && let Some(p) = ui.ctx().pointer_latest_pos()
            && rect.contains(p)
            && visible.contains(p)
            && in_lanes(p)
        {
            open_menu = Some(hit(p).map(|(k, _)| items[k].item.id()));
        }
        let drag_idle = self.drag.is_none();
        let was_sweeping = self.rmb.active();
        let sweep = self.rmb.update(ui, |p| {
            !shift && drag_idle && visible.contains(p) && rect.contains(p) && in_lanes(p)
        });
        if let Some((points, ended)) = sweep {
            if !was_sweeping {
                self.erased.clear();
            }
            for p in points.into_iter().filter(|p| visible.contains(*p)) {
                if let Some((k, _)) = hit(p) {
                    self.erased.insert(items[k].item.id());
                }
            }
            if ended {
                let erased = std::mem::take(&mut self.erased);
                if !erased.is_empty() {
                    self.selection.retain(|id| !erased.contains(id));
                    out.edits.push(delete_edits(project, &erased));
                }
            }
        }

        // Clicks.
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            // The marker at the start of a clip's name opens its menu.
            let on_marker = hit(p).filter(|&(k, _)| {
                let g = &items[k];
                menu_marker(row_rect(g.lane, g.start, g.end)).is_some_and(|m| m.contains(p))
            });
            if let Some((k, _)) = on_marker {
                open_menu = Some(Some(items[k].item.id()));
            } else if in_lanes(p) {
                match (hit(p), self.tool) {
                    (Some((k, _)), Tool::Delete) => {
                        let id = items[k].item.id();
                        self.selection.remove(&id);
                        out.edits.push(delete_edits(project, &BTreeSet::from([id])));
                    }
                    (Some((k, _)), Tool::Slice) => {
                        self.activate(&items[k].item, out);
                        out.actions.push(PlaylistAction::Slice {
                            clip: items[k].item.id(),
                            tick: snapped(p.x) as u64,
                        });
                    }
                    (Some((k, _)), _) => {
                        let id = items[k].item.id();
                        // A click on a clip places nothing: it selects
                        // the clip and makes its source active (§2).
                        self.activate(&items[k].item, out);
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
                                like: self.like,
                            });
                        }
                        (_, None) => {
                            out.hint =
                                Some("No source to place: choose a pattern or a file".into());
                        }
                        _ => {}
                    },
                    (None, _) => self.selection.clear(),
                }
            }
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
            && let Some((k, _)) = hit(p)
        {
            match (self.tool, items[k].item) {
                // §4.5: the Speed tool's double-click is normal speed.
                (Tool::Speed, Item::Audio(c)) => {
                    if c.speed != 1.0 {
                        out.edits
                            .push(Edit::SetAudioClip(AudioClip { speed: 1.0, ..c }));
                    }
                }
                (_, item) => out.actions.push(PlaylistAction::Open(item.id())),
            }
        }

        // Files from the Browser or the system.
        let drop_at = |p: Pos2| -> (Option<LaneId>, u64) {
            let lane = lane_at(p.y).and_then(|l| pl.lanes.get(l)).map(|l| l.id);
            (lane, snapped(p.x) as u64)
        };
        let mut drop_ghost: Option<(Pos2, String)> = None;
        // While another widget is dragged this one is not hovered, so its
        // own pointer positions are empty: use the latest pointer.
        let dnd_pointer = ui.ctx().pointer_latest_pos().filter(|p| rect.contains(*p));
        if let Some(item) = response.dnd_hover_payload::<BrowserItem>()
            && let BrowserItem::Sample(path) | BrowserItem::Midi(path) = &*item
            && let Some(p) = dnd_pointer
            && in_lanes(p)
        {
            drop_ghost = Some((p, display_name(path)));
        }
        if let Some(item) = response.dnd_hover_payload::<ProjectItem>()
            && let Some(p) = dnd_pointer
            && in_lanes(p)
        {
            drop_ghost = source_name(project, item.0).map(|n| (p, n));
        }
        if let Some(item) = dnd_release::<ProjectItem>(&response)
            && let Some(p) = dnd_pointer
            && in_lanes(p)
        {
            let (lane, start) = drop_at(p);
            self.source = Some(item.0);
            self.like = None;
            out.actions.push(PlaylistAction::Place {
                source: item.0,
                lane,
                start,
                count: 1,
                like: None,
            });
        }
        if let Some(item) = dnd_release::<BrowserItem>(&response)
            && let Some(p) = dnd_pointer
            && in_lanes(p)
        {
            let (lane, start) = drop_at(p);
            match &*item {
                BrowserItem::Sample(path) => out.actions.push(PlaylistAction::ImportAudio {
                    path: path.clone(),
                    lane,
                    start,
                }),
                BrowserItem::Midi(path) => out.actions.push(PlaylistAction::ImportMidi {
                    path: path.clone(),
                    lane,
                    start,
                }),
                BrowserItem::Instrument(_) => {}
            }
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
                let start = snapped(p.x) as u64;
                // A MIDI file becomes a pattern (spec 0.3.0 §2.4).
                out.actions.push(if crate::browser::is_midi(&path) {
                    PlaylistAction::ImportMidi { path, lane, start }
                } else {
                    PlaylistAction::ImportAudio { path, lane, start }
                });
            }
        }

        if let Some(clip) = open_menu {
            self.context_clip = clip;
            if let Some(id) = clip
                && !self.selection.contains(&id)
            {
                self.selection = BTreeSet::from([id]);
            }
        }
        // Stays open while the curve and tension are edited.
        egui::Popup::context_menu(&response)
            .open_memory(open_menu.map(|_| egui::SetOpenCommand::Bool(true)))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
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
                    Item::Automation(c) => pl.admits_automation(c),
                };
            // Audio clips take their channel's color (ACH-041).
            let fill = match &g.item {
                Item::Audio(c) => project
                    .channel_index(c.channel)
                    .map(|i| {
                        color(jdance_domain::color_or_default(
                            project.channels[i].color,
                            i,
                        ))
                    })
                    .unwrap_or_else(|| color(lane.color)),
                // A pattern's own colour, else the lane's (spec 0.2.5 §9).
                Item::Pattern(c) => project
                    .pattern(c.pattern)
                    .and_then(|x| x.color)
                    .map_or_else(|| color(lane.color), color),
                _ => color(lane.color),
            };
            draw_clip(
                ui,
                &painter,
                project,
                &g,
                r,
                fill,
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
                Some(_) => bars_text(start, sig),
                None => format!("{} · new lane", bars_text(start, sig)),
            };
            let row = lane_at(p.y).unwrap_or(n_lanes);
            let r = Rect::from_min_size(
                pos2(x_of(start), origin.y + RULER_H + row as f32 * ROW_H + 2.0),
                vec2(140.0, ROW_H - 4.0),
            );
            painter.rect_filled(r, 4.0, theme::TEAL.gamma_multiply(0.3));
            painter.rect_stroke(r, 4.0, Stroke::new(1.0, theme::TEAL), StrokeKind::Inside);
            crate::widgets::fit_text(
                &painter,
                r.left_center() + vec2(5.0, 0.0),
                Align2::LEFT_CENTER,
                format!("{name} · {where_}"),
                FontId::proportional(11.0),
                theme::TEXT,
                r.width() - 10.0,
            );
        }

        if let (Some(t), Some(pos)) = (&self.live, playhead) {
            let row = t
                .lane
                .and_then(|l| pl.lanes.iter().position(|x| x.id == l))
                .unwrap_or(n_lanes);
            let r = row_rect(row, t.start as f64, pos.max(t.start) as f64);
            live_take(&painter, r, t);
        }
        self.follow_playhead(ui, playhead.map(x_of));
        if let Some(t) = self.reveal.take() {
            // Minimal scroll, zoom unchanged.
            let x = x_of(t);
            ui.scroll_to_rect(
                Rect::from_min_size(pos2(x, ui.clip_rect().top()), vec2(48.0, 1.0)),
                None,
            );
        }
        let caret = match gesture {
            Some(g) if self.caret.is_some() => Some(g.caret()),
            _ => self.caret,
        };
        if let Some(c) = caret {
            // Caret: a triangle on the ruler and a dashed line (CUR-005).
            let x = x_of(c);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x - 6.0, origin.y + RULER_H - 9.0),
                    pos2(x + 6.0, origin.y + RULER_H - 9.0),
                    pos2(x, origin.y + RULER_H),
                ],
                theme::AMBER,
                Stroke::NONE,
            ));
            painter.add(egui::Shape::dashed_line(
                &[pos2(x, origin.y + RULER_H), pos2(x, rect.bottom())],
                Stroke::new(1.0, theme::AMBER.gamma_multiply(0.7)),
                4.0,
                4.0,
            ));
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
            && let Some((_, edge)) = hit(p)
        {
            let icon = match (self.tool, edge) {
                (Tool::Delete, _) => egui::CursorIcon::NotAllowed,
                (Tool::Slice, _) => egui::CursorIcon::VerticalText,
                (Tool::Speed, _) => egui::CursorIcon::ResizeColumn,
                (_, Some(_)) => egui::CursorIcon::ResizeHorizontal,
                (_, None) => egui::CursorIcon::Grab,
            };
            ui.ctx().set_cursor_icon(icon);
        }
    }

    /// Caret and selection gestures on the ruler, its double click and
    /// its menu (spec 0.2.4 CUR-010…015, SEL-001…011).
    fn ruler_input(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        rect: Rect,
        ppt: f32,
        out: &mut PlaylistOutput,
    ) {
        let pl = &project.playlist;
        let sig = project.time_signature;
        let frame = crate::ruler::RulerFrame {
            rect: Rect::from_min_size(rect.min, vec2(rect.width(), RULER_H)),
            origin_x: rect.min.x,
            px_per_tick: ppt,
            grids: crate::ruler::Grids::new(self.grid, u64::from(sig.beat_ticks())),
            caret: self.caret.unwrap_or(0),
            selection: pl.loop_range,
            select: true,
            limit: None,
        };
        let (response, r) = self
            .ruler
            .ui(ui, egui::Id::unique("playlist-ruler"), &frame);
        if let Some(c) = r.preview {
            out.actions.push(PlaylistAction::CaretPreview(c));
        }
        if let Some(end) = r.end {
            out.actions.push(PlaylistAction::Seek(end.caret));
            if let Some(range) = end.selection {
                // Shift-selections turn the loop on, as before 0.2.4 (SEL-003).
                out.edits.push(Edit::SetLoop {
                    range: Some(range),
                    enabled: true,
                });
            }
        }
        if r.clear && pl.loop_range.is_some() {
            out.edits.push(Edit::SetLoop {
                range: None,
                enabled: false,
            });
        }
        response.context_menu(|ui| {
            let has = pl.loop_range.is_some_and(|(a, b)| b > a);
            if ui
                .add_enabled(has, egui::Button::new("Clear selection"))
                .on_hover_text("Also: double-click the ruler")
                .clicked()
            {
                out.edits.push(Edit::SetLoop {
                    range: None,
                    enabled: false,
                });
                ui.close();
            }
            let end = sig.ceil_to_bar(project.arrangement_end_ticks());
            if ui
                .add_enabled(end > 0, egui::Button::new("Select whole song"))
                .clicked()
            {
                out.edits.push(Edit::SetLoop {
                    range: Some((0, end)),
                    enabled: pl.loop_enabled,
                });
                ui.close();
            }
            if ui
                .add_enabled(has, egui::Button::new("Render selection…"))
                .clicked()
            {
                out.actions.push(PlaylistAction::RenderSelection);
                ui.close();
            }
        });
    }

    /// A gesture on the ruler is moving the caret.
    pub(crate) fn caret_dragging(&self) -> bool {
        self.ruler.active().is_some()
    }

    #[allow(clippy::too_many_arguments)]
    fn background(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        sig: TimeSignature,
        bars: u64,
        n_lanes: usize,
        lanes_bottom: f32,
    ) {
        let origin = rect.min;
        let ppt = self.px_per_tick();
        let bar_px = sig.bar_ticks() as f32 * ppt;
        let bottom = origin.y + lanes_bottom;
        painter.rect_filled(rect, CornerRadius::ZERO, theme::BG);
        // Every other bar slightly lighter: bars read without lines.
        for b in (0..bars).filter(|b| b % 2 == 1) {
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(origin.x + b as f32 * bar_px, origin.y + RULER_H),
                    pos2(origin.x + (b + 1) as f32 * bar_px, bottom),
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
        painter.rect_filled(
            Rect::from_min_size(origin, vec2(rect.width(), RULER_H)),
            CornerRadius::ZERO,
            theme::PANEL,
        );
        let beat_px = sig.beat_ticks() as f32 * ppt;
        let per_bar = u64::from(sig.numerator);
        // Bar numbers at least 28 px apart (SIG-012).
        let label_every = (28.0 / bar_px).ceil().max(1.0) as u64;
        for b in 0..=bars * per_bar {
            let x = origin.x + b as f32 * beat_px;
            let bar_line = b % per_bar == 0;
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
                if !(b / per_bar).is_multiple_of(label_every) {
                    continue;
                }
                painter.text(
                    pos2(x + 3.0, origin.y + 2.0),
                    Align2::LEFT_TOP,
                    format!("{}", b / per_bar + 1),
                    theme::mono(11.0),
                    theme::MUTED,
                );
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
        let muted = match (pl.clip(id), pl.audio_clip(id), pl.automation_clip(id)) {
            (Some(c), _, _) => c.muted,
            (_, Some(c), _) => c.muted,
            (_, _, Some(c)) => c.muted,
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
        if let Some(clip) = pl.audio_clip(id)
            && let Some(ch) = project.channel(clip.channel)
        {
            let n = project.channel_clips(ch.id).count();
            if ui
                .button(format!("Channel: {}", ch.name))
                .on_hover_text("Select it in the Channel Rack")
                .clicked()
            {
                out.actions.push(PlaylistAction::SelectChannel(ch.id));
                ui.close();
            }
            // Selected clips of the same channel go together (ACH-025).
            let same: Vec<ClipId> =
                std::iter::once(id)
                    .chain(self.selection.iter().copied().filter(|s| {
                        *s != id && pl.audio_clip(*s).is_some_and(|c| c.channel == ch.id)
                    }))
                    .collect();
            if ui
                .add_enabled(n > same.len(), egui::Button::new("Make unique"))
                .on_hover_text("A new channel for these clips, with the same file and settings")
                .clicked()
            {
                out.actions.push(PlaylistAction::MakeUniqueAudio(same));
                ui.close();
            }
            if ui
                .button("Send to sampler")
                .on_hover_text("A new sampler channel with this file; the clips stay")
                .clicked()
            {
                out.actions.push(PlaylistAction::ToSampler(ch.id));
                ui.close();
            }
            ui.menu_button("Mixer track", |ui| {
                ui.label(
                    RichText::new(format!("Routes the channel: affects {n} clip(s)"))
                        .size(11.0)
                        .color(theme::MUTED),
                );
                for t in project.mixer.all_tracks() {
                    let label =
                        format!("{} · {}", crate::rack::track_number(project, t.id), t.name);
                    if ui.selectable_label(t.id == ch.track, label).clicked() && t.id != ch.track {
                        out.edits.push(Edit::SetChannelTrack(ch.id, t.id));
                        ui.close();
                    }
                }
            });
        }
        if pl.audio_clip(id).is_some() {
            let automated = project
                .lane_for(&jdance_domain::AutomationTarget::AudioClipGain(id))
                .is_some();
            let label = if automated {
                "Edit gain automation…"
            } else {
                "Automate gain"
            };
            if ui.button(label).clicked() {
                out.actions.push(PlaylistAction::AutomateGain(id));
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
        if ui
            .add(egui::Button::new("Duplicate").shortcut_text(crate::bindings::DUPLICATE.label()))
            .clicked()
        {
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

pub(crate) fn display_name(path: &Path) -> String {
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
        Item::Automation(c) => Edit::SetAutomationClip(AutomationClip { lane, ..c }),
    }
}

/// One undo step removing `ids` (any clip kind).
fn delete_edits(project: &Project, ids: &BTreeSet<ClipId>) -> Edit {
    let pl = &project.playlist;
    Edit::Batch(
        ids.iter()
            .filter_map(|id| {
                if pl.clip(*id).is_some() {
                    Some(Edit::RemoveClip(*id))
                } else if pl.audio_clip(*id).is_some() {
                    Some(Edit::RemoveAudioClip(*id))
                } else if pl.automation_clip(*id).is_some() {
                    Some(Edit::RemoveAutomationClip(*id))
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
    if let Some(c) = pl.automation_clip(id) {
        return Some(Edit::SetAutomationClip(AutomationClip { muted, ..*c }));
    }
    pl.audio_clip(id)
        .map(|c| Edit::SetAudioClip(AudioClip { muted, ..*c }))
}

/// Record-arm toggle: a ring, filled when armed.
/// Lane of a take starting at `start` (REC-012): the armed lane; else the
/// first lane with no clip from `start` on; else `None`, a new lane.
pub(crate) fn record_lane(project: &Project, armed: Option<LaneId>, start: u64) -> Option<LaneId> {
    armed
        .filter(|l| project.playlist.lane(*l).is_some())
        .or_else(|| {
            project
                .playlist
                .lanes
                .iter()
                .map(|l| l.id)
                .find(|&l| !project.lane_busy(l, start, None, None))
        })
}

/// The growing clip of the take being recorded (REC-014): the record
/// color, its name and the waveform so far, spread over the clip.
fn live_take(painter: &egui::Painter, r: Rect, t: &LiveTake) {
    painter.rect_filled(r, 3.0, theme::RED.gamma_multiply(0.35));
    painter.rect_stroke(r, 3.0, Stroke::new(1.5, theme::RED), StrokeKind::Inside);
    let wave = Rect::from_min_max(pos2(r.left(), r.top() + NAME_H), r.max);
    let cols = wave.width().floor() as usize;
    if cols > 0 && !t.peaks.is_empty() {
        let mid = wave.center().y;
        let half = wave.height() / 2.0 - 1.0;
        let per = t.peaks.len() as f32 / cols as f32;
        for c in 0..cols {
            let a = (c as f32 * per) as usize;
            let b = (((c + 1) as f32 * per) as usize)
                .max(a + 1)
                .min(t.peaks.len());
            let Some(p) = t.peaks[a.min(t.peaks.len() - 1)..b]
                .iter()
                .copied()
                .reduce(f32::max)
            else {
                continue;
            };
            let h = (p.min(1.0) * half).max(0.5);
            let x = wave.left() + c as f32 + 0.5;
            painter.line_segment(
                [pos2(x, mid - h), pos2(x, mid + h)],
                Stroke::new(1.0, theme::TEXT.gamma_multiply(0.8)),
            );
        }
    }
    crate::widgets::fit_text(
        painter,
        r.left_top() + vec2(4.0, 1.0),
        Align2::LEFT_TOP,
        format!("● {}", t.name),
        FontId::proportional(11.0),
        theme::TEXT,
        r.width() - 8.0,
    );
}

fn arm_button(ui: &mut egui::Ui, armed: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let c = rect.center();
    let color = if armed {
        theme::RED
    } else if resp.hovered() {
        theme::TEXT
    } else {
        theme::MUTED
    };
    if armed {
        ui.painter().circle_filled(c, 5.5, color);
    } else {
        ui.painter().circle_stroke(c, 5.0, Stroke::new(1.5, color));
    }
    resp
}

/// What of type `T` is dropped on `response` this frame. Unlike
/// `Response::dnd_release_payload` it leaves a payload of another type in
/// place (egui drops it on a type mismatch).
pub(crate) fn dnd_release<T: std::any::Any + Send + Sync>(
    response: &egui::Response,
) -> Option<std::sync::Arc<T>> {
    egui::DragAndDrop::has_payload_of_type::<T>(&response.ctx)
        .then(|| response.dnd_release_payload::<T>())
        .flatten()
}

/// The menu marker at the start of a clip's name strip, if the clip is
/// wide enough to show one.
fn menu_marker(r: Rect) -> Option<Rect> {
    (r.width() >= 3.0 * EDGE_PX + 2.0 * MARKER_W).then(|| {
        Rect::from_min_size(
            pos2(r.left() + EDGE_PX - 2.0, r.top()),
            vec2(MARKER_W, NAME_H.min(r.height())),
        )
    })
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
            let Some(asset) = project.clip_asset(c) else {
                return;
            };
            let mut name = project
                .channel(c.channel)
                .map_or_else(|| asset.name.clone(), |ch| ch.name.clone());
            // §4.4: a clip at another speed says so.
            if c.speed != 1.0 {
                name = format!("{name}  ×{:.2}", c.speed);
            }
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
            (name, c.muted)
        }
        Item::Automation(c) => {
            let Some(source) = project.automation_lane(c.source) else {
                return;
            };
            // The source's envelope over the clip's range.
            let x_of = |t: u64| r.left() + (t as f32 - c.offset as f32) * ppt;
            let line = envelope_line(
                &source.points,
                body.shrink2(vec2(0.0, 1.0)),
                &x_of,
                (body.left(), body.right()),
            );
            let line: Vec<Pos2> = line
                .into_iter()
                .map(|p| pos2(p.x.clamp(body.left(), body.right()), p.y))
                .collect();
            let missing = !source.target.exists(project);
            let ink = if missing {
                theme::WARN
            } else if audible {
                AUTOMATION_COLOR
            } else {
                AUTOMATION_COLOR.gamma_multiply(0.5)
            };
            painter
                .with_clip_rect(body)
                .add(egui::Shape::line(line, Stroke::new(1.5, ink)));
            let name = if missing {
                format!("{} (target not found)", source.name)
            } else {
                source.name.clone()
            };
            (name, c.muted)
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
    let text_color = if audible {
        theme::ON_AMBER
    } else {
        theme::MUTED
    };
    let mut name_x = r.left().max(visible.left()) + 4.0;
    if let Some(m) = menu_marker(r) {
        let c = m.center();
        painter.add(egui::Shape::convex_polygon(
            vec![
                pos2(c.x - 3.5, c.y - 2.0),
                pos2(c.x + 3.5, c.y - 2.0),
                pos2(c.x, c.y + 2.5),
            ],
            text_color,
            Stroke::NONE,
        ));
        name_x = name_x.max(m.right() + 2.0);
    }
    painter
        .with_clip_rect(strip.intersect(ui.clip_rect()))
        .text(
            pos2(name_x, strip.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(10.5),
            text_color,
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, RawInput};

    #[test]
    fn the_speed_tool_shortens_a_clip_by_speeding_it_up() {
        let mut p = Project::new();
        p.tempo_bpm = 120.0;
        let asset = AudioAsset {
            id: AssetId(p.ids.next()),
            name: "a".into(),
            path: "a.wav".into(),
            hash: 0,
            size: 0,
            sample_rate: 48_000,
            channels: 1,
            frames: 48_000,
        };
        let ch = ChannelId(p.ids.next());
        p.channels.push(jdance_domain::Channel::new_clip_sampler(
            ch, "a", asset.id, "a.wav",
        ));
        p.assets.push(asset.clone());
        let lane = p.playlist.lanes[0].id;
        let c = jdance_commands::audio_channels::audio_clip(ClipId(99), lane, ch, &asset, 0);
        // One second at 120 BPM is two beats (1920 ticks).
        let (a, b) = p.audio_clip_ticks(&c).unwrap();
        assert!((b - a - 1920.0).abs() < 1e-6);
        let fast = sped_up(&p, &c, 960);
        assert_eq!(fast.speed, 2.0, "half as long, twice as fast");
        let (_, b) = p.audio_clip_ticks(&fast).unwrap();
        assert!((b - 960.0).abs() < 1e-6);
        assert_eq!(sped_up(&p, &c, 3_840).speed, 0.5);
        assert_eq!(
            sped_up(&p, &c, 1).speed,
            jdance_domain::MAX_CLIP_SPEED,
            "bounded"
        );
        assert_eq!(
            sped_up(&p, &c, 1_000_000).speed,
            jdance_domain::MIN_CLIP_SPEED
        );
    }

    #[test]
    fn a_take_goes_to_the_armed_lane_then_the_first_free_one() {
        let mut p = Project::new();
        let lanes: Vec<LaneId> = p.playlist.lanes.iter().map(|l| l.id).collect();
        let pat = p.patterns[0].clone();
        let bar = u64::from(p.time_signature.bar_ticks());
        // Lane 0 has a clip at bar 4; lane 1 one at bar 0 only.
        for (i, (lane, at)) in [(0, 4 * bar), (1, 0)].into_iter().enumerate() {
            p.playlist.clips.push(PatternClip::repeating(
                ClipId(900 + i as u64),
                lanes[lane],
                &pat,
                at,
                1,
            ));
        }
        assert_eq!(record_lane(&p, Some(lanes[3]), 0), Some(lanes[3]));
        // From bar 2: lane 0 still has a clip later on; lane 1 is free.
        assert_eq!(record_lane(&p, None, 2 * bar), Some(lanes[1]));
        assert_eq!(record_lane(&p, None, 0), Some(lanes[2]));
        // A removed armed lane does not count.
        assert_eq!(record_lane(&p, Some(LaneId(77_777)), 0), Some(lanes[2]));
        // Every lane busy: a new lane.
        for (i, l) in lanes.iter().enumerate() {
            p.playlist.clips.push(PatternClip::repeating(
                ClipId(950 + i as u64),
                *l,
                &pat,
                8 * bar,
                1,
            ));
        }
        assert_eq!(record_lane(&p, None, 0), None);
    }

    /// A drop checked for the wrong type first must still reach the
    /// right one (egui's own check drops a mismatched payload).
    #[test]
    fn dnd_release_keeps_a_payload_of_another_type() {
        let ctx = egui::Context::default();
        let p = pos2(50.0, 50.0);
        let mut got = (false, false);
        let frames = [
            vec![Event::PointerMoved(p)],
            vec![Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        ];
        for (k, events) in frames.into_iter().enumerate() {
            let input = RawInput {
                events,
                ..RawInput::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                if k == 0 {
                    egui::DragAndDrop::set_payload(ui.ctx(), 7_u32);
                }
                let (_, r) = ui.allocate_exact_size(vec2(200.0, 200.0), Sense::hover());
                if k == 1 {
                    got.0 = dnd_release::<String>(&r).is_some();
                    got.1 = dnd_release::<u32>(&r).is_some_and(|v| *v == 7);
                }
            });
            out.textures_delta.clear();
        }
        assert_eq!(got, (false, true));
    }
}

#[cfg(test)]
mod ruler_tests {
    use super::*;
    use crate::test_ui::{Driver, StubHost};
    use egui::{Modifiers, Pos2};

    /// Runs the gesture `points` (press at the first, release at the
    /// last) on the ruler and returns everything the Playlist reported.
    fn gesture(
        view: &mut PlaylistView,
        project: &Project,
        shift: bool,
        ticks: &[u64],
    ) -> PlaylistOutput {
        let mut d = Driver::new();
        let mut host = StubHost::default();
        let mut all = PlaylistOutput::default();
        let mut ui = |ui: &mut egui::Ui| {
            let out = view.show(
                ui,
                project,
                None,
                TICKS_PER_STEP,
                PatternId(0),
                &mut host,
                None,
            );
            all.edits.extend(out.edits);
            all.actions.extend(out.actions);
        };
        d.frame(vec![], &mut ui);
        let ruler = d.find_rect("Timeline ruler").expect("ruler");
        // Default zoom: 120 px per whole note.
        let at = |t: u64| {
            Pos2::new(
                ruler.left() + t as f32 * 120.0 / 3840.0 + 0.1,
                ruler.center().y,
            )
        };
        d.modifiers = if shift {
            Modifiers::SHIFT
        } else {
            Modifiers::NONE
        };
        let first = at(ticks[0]);
        d.frame(vec![d.moved(first)], &mut ui);
        d.frame(vec![d.button(first, true)], &mut ui);
        for &t in &ticks[1..] {
            d.frame(vec![d.moved(at(t))], &mut ui);
        }
        let last = at(*ticks.last().unwrap());
        d.frame(vec![d.button(last, false)], &mut ui);
        d.frame(vec![], &mut ui);
        all
    }

    fn project() -> Project {
        let mut p = Project::new();
        p.playlist = jdance_domain::Playlist::with_lanes(2, || 0);
        p
    }

    #[test]
    fn the_caret_follows_a_drag_on_the_ruler() {
        let p = project();
        let mut view = PlaylistView {
            caret: Some(0),
            ..PlaylistView::default()
        };
        let out = gesture(&mut view, &p, false, &[960, 1920, 3840, 7680]);
        let previews: Vec<u64> = out
            .actions
            .iter()
            .filter_map(|a| match a {
                PlaylistAction::CaretPreview(t) => Some(*t),
                _ => None,
            })
            .collect();
        assert_eq!(previews, [960, 1920, 3840, 7680], "every frame");
        assert!(out.actions.contains(&PlaylistAction::Seek(7680)));
        assert!(out.edits.is_empty(), "no selection, no clip frame");
    }

    #[test]
    fn a_shift_drag_makes_one_selection_and_turns_the_loop_on() {
        let p = project();
        let mut view = PlaylistView {
            caret: Some(0),
            ..PlaylistView::default()
        };
        let out = gesture(&mut view, &p, true, &[960, 2400, 4800]);
        assert_eq!(
            out.edits,
            [Edit::SetLoop {
                range: Some((960, 4800)),
                enabled: true
            }]
        );
        assert!(
            out.actions.contains(&PlaylistAction::Seek(960)),
            "caret on the start"
        );
    }

    #[test]
    fn a_shift_drag_inside_moves_the_selection_and_a_double_click_clears_it() {
        let mut p = project();
        p.playlist.loop_range = Some((3840, 7680));
        p.playlist.loop_enabled = true;
        let mut view = PlaylistView {
            caret: Some(0),
            ..PlaylistView::default()
        };
        let out = gesture(&mut view, &p, true, &[4800, 5760, 6720]);
        assert_eq!(
            out.edits,
            [Edit::SetLoop {
                range: Some((5760, 9600)),
                enabled: true
            }]
        );
        // Double click.
        let mut d = Driver::new();
        let mut host = StubHost::default();
        let mut edits = Vec::new();
        let mut ui = |ui: &mut egui::Ui| {
            let out = view.show(ui, &p, None, TICKS_PER_STEP, PatternId(0), &mut host, None);
            edits.extend(out.edits);
        };
        d.frame(vec![], &mut ui);
        let at = d.find("Timeline ruler").unwrap();
        d.click(at, &mut ui);
        d.click(at, &mut ui);
        assert_eq!(
            edits,
            [Edit::SetLoop {
                range: None,
                enabled: false
            }]
        );
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::*;
    use crate::test_ui::{Driver, StubHost};
    use egui::{Event, Key, Modifiers};

    #[test]
    fn the_left_edge_keeps_the_anchor_in_place_and_stays_after_zero() {
        // Anchor 400 px into the view at 1 px/tick, zoom ×2.
        let left = zoom_left(1000.0, 1.0, 2.0, 1400.0);
        assert_eq!((1400.0 - left) * 2.0, 400.0);
        assert_eq!(zoom_left(0.0, 2.0, 1.0, 100.0), 0.0, "never before 0");
    }

    fn key(k: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// Screen x of `tick` relative to the clip area, after each of `events`.
    fn run(view: &mut PlaylistView, events: Vec<Vec<Event>>, tick: u64) -> Vec<f32> {
        let mut p = Project::new();
        p.playlist = jdance_domain::Playlist::with_lanes(2, || 0);
        let mut d = Driver::new();
        let mut host = StubHost::default();
        let x = std::cell::Cell::new(0.0);
        let mut ui = |ui: &mut egui::Ui| {
            view.show(ui, &p, None, TICKS_PER_STEP, PatternId(0), &mut host, None);
            let v = view.view.unwrap();
            x.set(tick as f32 * view.px_per_tick() - v.offset);
        };
        d.frame(vec![], &mut ui);
        d.frame(vec![], &mut ui);
        let mut xs = Vec::new();
        for e in events {
            d.frame(e, &mut ui);
            d.frame(vec![], &mut ui);
            xs.push(x.get());
        }
        xs
    }

    #[test]
    fn page_keys_and_levels_keep_the_caret_where_it_is() {
        // 120 px per whole note: tick 12 800 is 400 px from the left.
        let caret = 12_800;
        let mut view = PlaylistView {
            caret: Some(caret),
            ..PlaylistView::default()
        };
        let up = || vec![key(Key::PageUp, Modifiers::NONE)];
        let xs = run(&mut view, vec![up(), up(), up(), up(), up()], caret);
        for x in &xs {
            assert!((x - 400.0).abs() <= 1.0, "{xs:?}");
        }
        let mut view = PlaylistView {
            caret: Some(caret),
            ..PlaylistView::default()
        };
        let xs = run(
            &mut view,
            vec![
                vec![key(Key::Num3, Modifiers::SHIFT)],
                vec![key(Key::Num2, Modifiers::SHIFT)],
                vec![key(Key::PageUp, Modifiers::NONE)],
            ],
            caret,
        );
        for x in &xs {
            assert!((x - 400.0).abs() <= 1.0, "{xs:?}");
        }
        assert_eq!(view.zoom, 120.0 * ZOOM_STEP);
    }

    #[test]
    fn a_caret_out_of_view_zooms_at_the_centre_and_shift_4_goes_back() {
        // Caret far right of the view: the centre (no mouse) stays.
        let mut view = PlaylistView {
            caret: Some(400_000),
            ..PlaylistView::default()
        };
        let start = view.zoom;
        run(&mut view, vec![vec![key(Key::Num4, Modifiers::SHIFT)]], 0);
        assert_eq!(view.zoom, MIN_ZOOM);
        run(&mut view, vec![vec![key(Key::Num4, Modifiers::SHIFT)]], 0);
        assert_eq!(view.zoom, start, "Shift+4 again: back");
        // At the song start the view does not go before 0.
        let mut view = PlaylistView {
            caret: Some(0),
            ..PlaylistView::default()
        };
        let xs = run(
            &mut view,
            vec![vec![key(Key::PageDown, Modifiers::NONE)]],
            0,
        );
        assert_eq!(xs, [0.0]);
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use crate::test_ui::{Driver, StubHost};
    use egui::{Event, Key, Modifiers};

    fn ctrl(k: Key) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND,
        }
    }

    /// The actions and hint after `key` with clip `0` of a one-clip
    /// playlist selected, and whether the selection is left.
    fn press(key: Key) -> (Vec<PlaylistAction>, Option<String>, bool) {
        let mut p = Project::new();
        p.playlist = jdance_domain::Playlist::with_lanes(1, || 0);
        let pattern = p.patterns[0].id;
        let lane = p.playlist.lanes[0].id;
        let id = ClipId(p.ids.next());
        p.playlist.clips.push(PatternClip {
            id,
            lane,
            pattern,
            start: 0,
            offset: 0,
            duration: u64::from(TICKS_PER_STEP) * 16,
            looped: true,
            muted: false,
        });
        let mut view = PlaylistView::default();
        view.selection.insert(id);
        let mut d = Driver::new();
        let mut host = StubHost::default();
        let (mut actions, mut hint) = (Vec::new(), None);
        let mut ui = |ui: &mut egui::Ui| {
            let out = view.show(ui, &p, None, TICKS_PER_STEP, pattern, &mut host, None);
            actions.extend(out.actions);
            hint = hint.take().or(out.hint);
        };
        d.frame(vec![], &mut ui);
        d.frame(vec![ctrl(key)], &mut ui);
        let left = !view.selection.is_empty();
        (actions, hint, left)
    }

    #[test]
    fn ctrl_b_duplicates_and_ctrl_d_only_deselects() {
        let (actions, _, left) = press(Key::B);
        assert!(
            matches!(actions.as_slice(), [PlaylistAction::Duplicate(ids)] if ids.len() == 1),
            "{actions:?}"
        );
        assert!(left);
        let (actions, hint, left) = press(Key::D);
        assert!(actions.is_empty(), "no copies: {actions:?}");
        assert!(!left, "deselected");
        assert!(
            hint.is_some_and(|h| h.contains("duplicate")),
            "the move is pointed out"
        );
    }
}

/// Spec 0.2.7 §2: the active source follows a left click on a clip.
#[cfg(test)]
mod source_tests {
    use super::*;
    use crate::test_ui::{Driver, StubHost};
    use egui::{Event, Key, Modifiers};

    struct Env {
        p: Project,
        view: PlaylistView,
        d: Driver,
        host: StubHost,
        current: PatternId,
        actions: Vec<PlaylistAction>,
    }

    const BAR: u64 = WHOLE_NOTE as u64;

    impl Env {
        /// Two patterns; a clip of the second one on lane 0, bar 1.
        fn new() -> Self {
            let mut p = Project::new();
            p.playlist = jdance_domain::Playlist::with_lanes(3, || 0);
            let second = PatternId(p.ids.next());
            p.patterns
                .push(jdance_domain::Pattern::new(second, "Pattern 2"));
            let lane = p.playlist.lanes[0].id;
            let id = ClipId(p.ids.next());
            p.playlist.clips.push(PatternClip {
                id,
                lane,
                pattern: second,
                start: BAR,
                offset: 0,
                duration: BAR,
                looped: true,
                muted: false,
            });
            let current = p.patterns[0].id;
            let mut e = Self {
                p,
                view: PlaylistView::default(),
                d: Driver::new(),
                host: StubHost::default(),
                current,
                actions: Vec::new(),
            };
            e.frame(vec![]);
            e.frame(vec![]);
            e
        }

        fn frame(&mut self, events: Vec<Event>) {
            let (view, p, host, current) = (&mut self.view, &self.p, &mut self.host, self.current);
            let mut out = PlaylistOutput::default();
            let mut ui = |ui: &mut egui::Ui| {
                out = view.show(ui, p, None, TICKS_PER_STEP, current, host, None);
            };
            self.d.frame(events, &mut ui);
            if let Some(id) = out.select_pattern {
                self.current = id;
            }
            self.actions.extend(out.actions);
        }

        /// Screen point of `tick` on `lane`.
        fn at(&self, lane: usize, tick: u64) -> egui::Pos2 {
            let v = self.view.view.unwrap();
            egui::pos2(
                v.left + tick as f32 * self.view.px_per_tick() - v.offset,
                v.rect.top() + RULER_H + ROW_H * lane as f32 + ROW_H / 2.0,
            )
        }

        fn click(&mut self, p: egui::Pos2) {
            let mut ui_events = vec![
                vec![self.d.moved(p)],
                vec![self.d.button(p, true)],
                vec![self.d.button(p, false)],
                vec![],
            ];
            for e in ui_events.drain(..) {
                self.frame(e);
            }
        }

        fn placed(&mut self) -> Vec<Source> {
            self.actions
                .drain(..)
                .filter_map(|a| match a {
                    PlaylistAction::Place { source, .. } => Some(source),
                    _ => None,
                })
                .collect()
        }
    }

    #[test]
    fn a_click_on_a_clip_makes_its_pattern_the_one_that_draw_places() {
        let mut e = Env::new();
        let first = e.p.patterns[0].id;
        let second = e.p.patterns[1].id;
        assert_eq!(e.view.source, Some(Source::Pattern(first)));
        // Draw is the default tool: a click on the clip only selects it.
        let on_clip = e.at(0, BAR + BAR / 2);
        e.click(on_clip);
        assert!(e.placed().is_empty(), "a click on a clip places nothing");
        assert_eq!(e.view.source, Some(Source::Pattern(second)));
        assert_eq!(e.current, second, "the current pattern follows");
        assert_eq!(e.view.selection.len(), 1);
        // Esc clears the outline, not the source.
        e.frame(vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }]);
        assert!(e.view.selection.is_empty());
        // Draw on an empty spot of another lane: pattern 2.
        let empty = e.at(2, 3 * BAR);
        e.click(empty);
        assert_eq!(e.placed(), [Source::Pattern(second)]);
    }

    #[test]
    fn a_removed_source_falls_back_to_the_first_pattern() {
        let mut e = Env::new();
        let second = e.p.patterns[1].id;
        e.view.source = Some(Source::Pattern(second));
        e.p.patterns.retain(|p| p.id != second);
        e.p.playlist.clips.clear();
        e.view.retain_existing(&e.p);
        assert_eq!(e.view.source, Some(Source::Pattern(e.p.patterns[0].id)));
    }

    #[test]
    fn audio_and_automation_clips_give_their_sources() {
        let c = jdance_domain::AudioClip {
            id: ClipId(1),
            lane: LaneId(2),
            channel: ChannelId(7),
            start: 0,
            source_in: 0,
            source_out: 10,
            gain: 1.0,
            muted: false,
            fade_in: 0,
            fade_out: 0,
            speed: 1.0,
        };
        assert_eq!(Item::Audio(c).source(), Source::Channel(ChannelId(7)));
    }
}
