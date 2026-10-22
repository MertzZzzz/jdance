// SPDX-License-Identifier: GPL-3.0-or-later
//! Caret and time-selection gestures on a timeline ruler (spec 0.2.4
//! CUR-010…015, SEL-001…011, PCR-005).
//!
//! The left button on the ruler takes the caret along until it is
//! released. With Shift held at the press it makes a time selection
//! instead: from the press point to the pointer, or, pressed inside an
//! existing selection, it moves that selection, and near one of its
//! edges it moves that edge. The caret follows the pointer during the
//! gesture and lands on the start of the selection at the end. Escape
//! cancels; a double click clears the selection.
//!
//! [`RulerDrag`] is the pure gesture; [`Ruler`] reads egui input for it.

use egui::{Key, Modifiers, Pos2, Rect, Response, Sense, Ui, vec2};

/// Pointer distance to a selection edge that grabs the edge (SEL-009).
const EDGE_PX: f32 = 5.0;
/// Width of the zone at the view's sides that scrolls it (CUR-013).
const SCROLL_ZONE_PX: f32 = 24.0;
/// Scroll speed per frame at the very edge of the view.
const SCROLL_MAX_PX: f32 = 24.0;

/// Snap grids in ticks; 1 = off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Grids {
    /// The caret alone.
    pub caret: u64,
    /// Selection bounds and moves: never coarser than a beat (SEL-002).
    pub select: u64,
}

impl Grids {
    /// The caret on `grid` (0 = off); the selection on `grid` but at
    /// least on beats.
    pub(crate) fn new(grid: u64, beat: u64) -> Self {
        let grid = grid.max(1);
        Self {
            caret: grid,
            select: grid.min(beat.max(1)),
        }
    }

    pub(crate) const OFF: Self = Self {
        caret: 1,
        select: 1,
    };
}

fn snap(ticks: i64, grid: u64) -> i64 {
    let g = grid.max(1) as i64;
    (ticks as f64 / g as f64).round() as i64 * g
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Caret,
    /// A new selection from `anchor`.
    Select {
        anchor: u64,
    },
    /// The selection `orig` grabbed at tick `grab`.
    Move {
        grab: i64,
        orig: (u64, u64),
    },
    /// One edge of the selection follows; `fixed` is the other.
    Edge {
        fixed: u64,
    },
}

/// What a finished gesture leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RulerEnd {
    pub caret: u64,
    /// A new selection to store (one `SetLoop`), `None` = unchanged.
    pub selection: Option<(u64, u64)>,
}

/// A gesture in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RulerDrag {
    kind: Kind,
    caret_before: u64,
    selection_before: Option<(u64, u64)>,
    /// Upper bound of the caret, exclusive (the pattern length).
    limit: Option<u64>,
    caret: u64,
    selection: Option<(u64, u64)>,
}

impl RulerDrag {
    /// A press at tick `t`. `tol` is the edge grab distance in ticks;
    /// `select` allows selections (the Playlist ruler).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn press(
        t: i64,
        shift: bool,
        caret: u64,
        selection: Option<(u64, u64)>,
        tol: i64,
        grids: Grids,
        select: bool,
        limit: Option<u64>,
    ) -> Self {
        let kind = match (shift && select, selection.filter(|(a, b)| b > a)) {
            (false, _) => Kind::Caret,
            (true, Some((a, b))) if (t - a as i64).abs() <= tol => Kind::Edge { fixed: b },
            (true, Some((a, b))) if (t - b as i64).abs() <= tol => Kind::Edge { fixed: a },
            (true, Some((a, b))) if t > a as i64 && t < b as i64 => Kind::Move {
                grab: t,
                orig: (a, b),
            },
            (true, _) => Kind::Select {
                anchor: snap(t, grids.select).max(0) as u64,
            },
        };
        let mut d = Self {
            kind,
            caret_before: caret,
            selection_before: selection,
            limit,
            caret,
            selection,
        };
        d.drag(t, grids);
        d
    }

    /// The pointer is at tick `t`.
    pub(crate) fn drag(&mut self, t: i64, grids: Grids) {
        let at = |g: u64| snap(t, g).max(0) as u64;
        match self.kind {
            Kind::Caret => {
                let mut c = at(grids.caret);
                // The last grid point inside the pattern.
                if let Some(l) = self.limit
                    && c >= l
                {
                    c = l.saturating_sub(1) / grids.caret * grids.caret;
                }
                self.caret = c;
            }
            Kind::Select { anchor: fixed } | Kind::Edge { fixed } => {
                let end = at(grids.select);
                self.selection = Some((fixed.min(end), fixed.max(end)));
                self.caret = end;
            }
            Kind::Move { grab, orig: (a, b) } => {
                let delta = snap(t - grab, grids.select);
                let start = (a as i64 + delta).max(0) as u64;
                self.selection = Some((start, start + (b - a)));
                self.caret = start;
            }
        }
    }

    /// Caret shown during the gesture.
    pub(crate) fn caret(&self) -> u64 {
        self.caret
    }

    /// Selection shown during the gesture.
    pub(crate) fn selection(&self) -> Option<(u64, u64)> {
        self.selection
    }

    /// The gesture makes or moves a selection.
    pub(crate) fn selecting(&self) -> bool {
        !matches!(self.kind, Kind::Caret)
    }

    pub(crate) fn moving(&self) -> bool {
        matches!(self.kind, Kind::Move { .. })
    }

    /// Release: the caret and the selection to store (SEL-003/004/008).
    pub(crate) fn finish(&self) -> RulerEnd {
        match (self.kind, self.selection) {
            (Kind::Caret, _) => RulerEnd {
                caret: self.caret,
                selection: None,
            },
            (_, Some((a, b))) if b > a => RulerEnd {
                caret: a,
                selection: (self.selection != self.selection_before).then_some((a, b)),
            },
            // Zero length: a plain caret move.
            _ => RulerEnd {
                caret: self.caret,
                selection: None,
            },
        }
    }

    /// Escape: everything as before the press (SEL-006).
    pub(crate) fn cancel(&self) -> RulerEnd {
        RulerEnd {
            caret: self.caret_before,
            selection: None,
        }
    }
}

/// What the ruler reports for one frame.
#[derive(Debug, Default)]
pub(crate) struct RulerOutput {
    /// The caret moved during a gesture (or went back on Escape).
    pub preview: Option<u64>,
    /// The gesture ended on release.
    pub end: Option<RulerEnd>,
    /// Double click: clear the selection (SEL-011).
    pub clear: bool,
}

/// Geometry and state of a ruler for one frame.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RulerFrame {
    /// The ruler strip.
    pub rect: Rect,
    /// x of tick 0.
    pub origin_x: f32,
    pub px_per_tick: f32,
    pub grids: Grids,
    pub caret: u64,
    pub selection: Option<(u64, u64)>,
    /// Shift makes selections (the Playlist).
    pub select: bool,
    pub limit: Option<u64>,
}

impl RulerFrame {
    fn tick_at(&self, x: f32) -> i64 {
        ((x - self.origin_x) / self.px_per_tick).round() as i64
    }

    fn x_of(&self, t: u64) -> f32 {
        self.origin_x + t as f32 * self.px_per_tick
    }

    fn tol(&self) -> i64 {
        (EDGE_PX / self.px_per_tick).round() as i64
    }
}

/// Ruler input kept between frames.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ruler {
    drag: Option<RulerDrag>,
}

impl Ruler {
    pub(crate) fn active(&self) -> Option<&RulerDrag> {
        self.drag.as_ref()
    }

    /// Reads the input over `f.rect`; scrolls the enclosing scroll area
    /// near the sides of the visible part during a gesture.
    pub(crate) fn ui(
        &mut self,
        ui: &mut Ui,
        id: egui::Id,
        f: &RulerFrame,
    ) -> (Response, RulerOutput) {
        let response = ui.interact(f.rect, id, Sense::click_and_drag());
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "Timeline ruler"));
        let mut out = RulerOutput::default();
        let (pressed, down, shift, alt, latest) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.modifiers.shift,
                i.modifiers.alt,
                i.pointer.latest_pos(),
            )
        });
        let grids = if alt { Grids::OFF } else { f.grids };
        if self.drag.is_none()
            && pressed
            && let Some(p) = response.interact_pointer_pos().or(latest)
            && f.rect.contains(p)
            && response.is_pointer_button_down_on()
        {
            let d = RulerDrag::press(
                f.tick_at(p.x),
                shift,
                f.caret,
                f.selection,
                f.tol(),
                grids,
                f.select,
                f.limit,
            );
            out.preview = Some(d.caret());
            self.drag = Some(d);
        }
        if let Some(d) = &mut self.drag {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                out.preview = Some(d.cancel().caret);
                self.drag = None;
            } else {
                if let Some(p) = latest {
                    let before = d.caret();
                    d.drag(f.tick_at(p.x), grids);
                    if d.caret() != before {
                        out.preview = Some(d.caret());
                    }
                    autoscroll(ui, p);
                }
                if !down {
                    out.end = Some(d.finish());
                    self.drag = None;
                }
            }
        }
        if response.double_clicked() && f.select {
            out.clear = true;
        }
        // Cursors over a selection with Shift held (SEL-008).
        if self.drag.is_none()
            && f.select
            && shift
            && let Some(p) = response.hover_pos()
            && let Some((a, b)) = f.selection.filter(|(a, b)| b > a)
        {
            let (xa, xb) = (f.x_of(a), f.x_of(b));
            if (p.x - xa).abs() <= EDGE_PX || (p.x - xb).abs() <= EDGE_PX {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            } else if p.x > xa && p.x < xb {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        if self.drag.as_ref().is_some_and(RulerDrag::moving) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        (response, out)
    }
}

/// Scrolls the view when the pointer is near or past its sides (CUR-013);
/// faster closer to the edge.
fn autoscroll(ui: &mut Ui, p: Pos2) {
    let view = ui.clip_rect();
    let speed = |dist: f32| {
        (SCROLL_ZONE_PX - dist).clamp(0.0, 2.0 * SCROLL_ZONE_PX) / SCROLL_ZONE_PX * SCROLL_MAX_PX
    };
    let dx = if p.x < view.left() + SCROLL_ZONE_PX {
        speed(p.x - view.left())
    } else if p.x > view.right() - SCROLL_ZONE_PX {
        -speed(view.right() - p.x)
    } else {
        0.0
    };
    if dx != 0.0 {
        ui.scroll_with_delta(vec2(dx, 0.0));
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{PPQ, TICKS_PER_STEP, TimeSignature};

    const BEAT: u64 = PPQ as u64;

    fn bar_grids(sig: TimeSignature) -> Grids {
        Grids::new(u64::from(sig.bar_ticks()), u64::from(sig.beat_ticks()))
    }

    fn press(t: u64, shift: bool, sel: Option<(u64, u64)>, g: Grids) -> RulerDrag {
        RulerDrag::press(t as i64, shift, 0, sel, 10, g, true, None)
    }

    #[test]
    fn the_caret_follows_the_pointer_on_its_grid() {
        let g = Grids::new(u64::from(TICKS_PER_STEP), BEAT);
        let mut d = press(250, false, None, g);
        assert_eq!(d.caret(), 240);
        d.drag(1000, g);
        assert_eq!(d.caret(), 960);
        d.drag(-50, g);
        assert_eq!(d.caret(), 0, "never left of 0");
        d.drag(1003, Grids::OFF);
        assert_eq!(
            d.finish(),
            RulerEnd {
                caret: 1003,
                selection: None
            }
        );
    }

    #[test]
    fn selection_bounds_fall_on_beats_with_the_bar_snap() {
        // SEL-002: Snap Bar still lets the selection start on any beat.
        for sig in [TimeSignature::COMMON, TimeSignature::new(7, 8)] {
            let g = bar_grids(sig);
            let beat = u64::from(sig.beat_ticks());
            let mut d = press(beat + 3, true, None, g);
            d.drag((5 * beat + 7) as i64, g);
            assert_eq!(d.selection(), Some((beat, 5 * beat)));
            assert_eq!(d.caret(), 5 * beat, "the caret follows the pointer");
            assert_eq!(
                d.finish(),
                RulerEnd {
                    caret: beat,
                    selection: Some((beat, 5 * beat))
                },
                "the caret lands on the start (SEL-004)"
            );
        }
        assert_eq!(
            Grids::new(u64::from(TICKS_PER_STEP), BEAT).select,
            u64::from(TICKS_PER_STEP)
        );
    }

    #[test]
    fn a_selection_made_leftwards_and_one_of_zero_length() {
        let g = Grids::new(BEAT, BEAT);
        let mut d = press(8 * BEAT, true, None, g);
        d.drag((3 * BEAT) as i64, g);
        assert_eq!(d.finish().selection, Some((3 * BEAT, 8 * BEAT)));
        let d = press(8 * BEAT, true, None, g);
        assert_eq!(
            d.finish(),
            RulerEnd {
                caret: 8 * BEAT,
                selection: None
            }
        );
    }

    #[test]
    fn moving_keeps_the_length_and_the_offset_from_the_grid() {
        let g = Grids::new(BEAT, BEAT);
        let sel = Some((4 * BEAT + 10, 8 * BEAT + 10));
        let mut d = press(6 * BEAT, true, sel, g);
        assert!(d.moving());
        d.drag((8 * BEAT + 30) as i64, g);
        assert_eq!(d.selection(), Some((6 * BEAT + 10, 10 * BEAT + 10)));
        assert_eq!(d.caret(), 6 * BEAT + 10);
        d.drag(-(100 * BEAT as i64), g);
        assert_eq!(d.selection(), Some((0, 4 * BEAT)), "not left of 0");
        let end = d.finish();
        assert_eq!(
            end,
            RulerEnd {
                caret: 0,
                selection: Some((0, 4 * BEAT))
            }
        );
        // Not moved: no edit.
        let d = press(6 * BEAT, true, sel, g);
        assert_eq!(d.finish().selection, None);
    }

    #[test]
    fn an_edge_moves_alone_and_may_cross_the_other() {
        let g = Grids::new(BEAT, BEAT);
        let sel = Some((4 * BEAT, 8 * BEAT));
        let mut d = press(8 * BEAT + 5, true, sel, g);
        d.drag((10 * BEAT) as i64, g);
        assert_eq!(d.selection(), Some((4 * BEAT, 10 * BEAT)));
        d.drag((2 * BEAT) as i64, g);
        assert_eq!(d.selection(), Some((2 * BEAT, 4 * BEAT)));
        let mut d = press(4 * BEAT - 3, true, sel, g);
        d.drag((5 * BEAT) as i64, g);
        assert_eq!(d.finish().selection, Some((5 * BEAT, 8 * BEAT)));
    }

    #[test]
    fn escape_restores_and_shift_only_counts_at_the_press() {
        let g = Grids::new(BEAT, BEAT);
        let mut d = RulerDrag::press(BEAT as i64, true, 7, None, 10, g, true, None);
        d.drag((3 * BEAT) as i64, g);
        assert_eq!(
            d.cancel(),
            RulerEnd {
                caret: 7,
                selection: None
            }
        );
        // Without selections (Piano Roll) Shift drags the caret, within
        // the pattern.
        let mut d = RulerDrag::press(BEAT as i64, true, 0, None, 10, g, false, Some(4 * BEAT));
        assert!(!d.selecting());
        d.drag((9 * BEAT) as i64, g);
        assert_eq!(d.caret(), 3 * BEAT);
    }
}
