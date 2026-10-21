// SPDX-License-Identifier: GPL-3.0-or-later
//! Dock / Undock (spec 0.2.0 §9): the Piano Roll, Sample Editor,
//! Automation Editor, Mixer and Project Browser can move into their own
//! native windows (egui viewports) of the same session; the Playlist stays
//! in the main window. Closing such a window hides the panel; nothing in
//! the project changes. The layout (which panels are undocked, their
//! position and size) is a user setting.
//!
//! Backends without multi-window support show undocked panels as windows
//! inside the main one (egui's embedded viewports).

use egui::{Pos2, Vec2, ViewportBuilder, ViewportClass, ViewportId};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Panel {
    PianoRoll,
    SampleEditor,
    AutomationEditor,
    Mixer,
    ProjectBrowser,
}

impl Panel {
    pub const ALL: [Panel; 5] = [
        Self::PianoRoll,
        Self::SampleEditor,
        Self::AutomationEditor,
        Self::Mixer,
        Self::ProjectBrowser,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::PianoRoll => "piano_roll",
            Self::SampleEditor => "sample_editor",
            Self::AutomationEditor => "automation_editor",
            Self::Mixer => "mixer",
            Self::ProjectBrowser => "project_browser",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PianoRoll => "Piano Roll",
            Self::SampleEditor => "Sample Editor",
            Self::AutomationEditor => "Automation Editor",
            Self::Mixer => "Mixer",
            Self::ProjectBrowser => "Project Browser",
        }
    }

    fn default_size(self) -> Vec2 {
        match self {
            Self::PianoRoll => Vec2::new(1000.0, 560.0),
            Self::SampleEditor => Vec2::new(800.0, 400.0),
            Self::AutomationEditor => Vec2::new(760.0, 340.0),
            Self::Mixer => Vec2::new(1100.0, 420.0),
            Self::ProjectBrowser => Vec2::new(260.0, 560.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    /// Outer position on the desktop; `None` = let the system place it.
    pub pos: Option<Pos2>,
    pub size: Vec2,
}

/// What happened to an undocked window this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowEvent {
    None,
    /// The window's close button: hide the panel.
    Closed,
    DockBack,
}

#[derive(Debug, Default)]
pub(crate) struct Layout {
    undocked: BTreeMap<Panel, Placement>,
    /// Set when the layout should be saved.
    pub changed: bool,
}

impl Layout {
    pub fn is_undocked(&self, p: Panel) -> bool {
        self.undocked.contains_key(&p)
    }

    pub fn undock(&mut self, p: Panel) {
        self.undocked.entry(p).or_insert(Placement {
            pos: None,
            size: p.default_size(),
        });
        self.changed = true;
    }

    pub fn dock(&mut self, p: Panel) {
        self.changed |= self.undocked.remove(&p).is_some();
    }

    /// Everything back in the main window.
    pub fn reset(&mut self) {
        self.undocked.clear();
        self.changed = true;
    }

    /// One line per undocked panel: `key x y w h` (`- -` for no position).
    pub fn to_text(&self) -> String {
        self.undocked
            .iter()
            .map(|(p, pl)| {
                let pos = pl
                    .pos
                    .map_or_else(|| "- -".to_owned(), |q| format!("{:.0} {:.0}", q.x, q.y));
                format!("{} {pos} {:.0} {:.0}\n", p.key(), pl.size.x, pl.size.y)
            })
            .collect()
    }

    /// Reads [`Self::to_text`]; unknown or damaged lines are skipped.
    pub fn from_text(text: &str) -> Self {
        let mut undocked = BTreeMap::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [key, x, y, w, h] = f[..] else { continue };
            let Some(p) = Panel::ALL.into_iter().find(|p| p.key() == key) else {
                continue;
            };
            let num = |s: &str| s.parse::<f32>().ok().filter(|v| v.is_finite());
            let pos = match (num(x), num(y)) {
                (Some(x), Some(y)) => Some(Pos2::new(x, y)),
                _ => None,
            };
            let (Some(w), Some(h)) = (num(w), num(h)) else {
                continue;
            };
            undocked.insert(
                p,
                Placement {
                    pos,
                    size: Vec2::new(w.clamp(200.0, 8000.0), h.clamp(150.0, 8000.0)),
                },
            );
        }
        Self {
            undocked,
            changed: false,
        }
    }

    /// Shows `contents` in the panel's own window (`instance` tells
    /// several windows of one kind apart). Positions outside the visible
    /// desktop are pulled back in.
    pub fn window(
        &mut self,
        ctx: &egui::Context,
        p: Panel,
        instance: u64,
        title: &str,
        contents: &mut dyn FnMut(&mut egui::Ui),
    ) -> WindowEvent {
        let Some(placement) = self.undocked.get(&p).copied() else {
            return WindowEvent::None;
        };
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        let mut builder = ViewportBuilder::default()
            .with_title(format!("{title} · jdance"))
            .with_inner_size(placement.size)
            .with_min_inner_size([280.0, 160.0]);
        if let Some(pos) = placement.pos {
            builder = builder.with_position(visible(pos, monitor));
        }
        let id = ViewportId::from_hash_of(("jdance_panel", p.key(), instance));
        let mut event = WindowEvent::None;
        let mut seen = None;
        ctx.show_viewport_immediate(id, builder, |ui, class| {
            let embedded = class == ViewportClass::EmbeddedWindow;
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("Dock back")
                        .on_hover_text("Return this panel to the main window")
                        .clicked()
                    {
                        event = WindowEvent::DockBack;
                    }
                    if embedded {
                        ui.label(
                            egui::RichText::new("(this system shows it inside the main window)")
                                .size(11.0)
                                .color(crate::theme::MUTED),
                        );
                    }
                });
                ui.separator();
                contents(ui);
            });
            let vp = ui.ctx().input(|i| {
                (
                    i.viewport().close_requested(),
                    i.viewport().outer_rect,
                    i.viewport().inner_rect,
                )
            });
            if vp.0 {
                event = WindowEvent::Closed;
            }
            seen = Some((vp.1, vp.2));
        });
        if let Some((outer, inner)) = seen
            && let Some(pl) = self.undocked.get_mut(&p)
        {
            let pos = outer.map(|r| r.min);
            let size = inner.map_or(pl.size, |r| r.size());
            if pos.is_some() && (pl.pos != pos || pl.size != size) {
                pl.pos = pos;
                pl.size = size;
                self.changed = true;
            }
        }
        match event {
            WindowEvent::DockBack => self.dock(p),
            WindowEvent::Closed => {}
            WindowEvent::None => {}
        }
        event
    }
}

/// `pos` pulled back so the title bar can be reached: monitors may sit
/// beside the main one (a generous band left and right of it), never
/// above or far below it.
fn visible(pos: Pos2, monitor: Option<Vec2>) -> Pos2 {
    let Some(m) = monitor.filter(|m| m.x > 0.0 && m.y > 0.0) else {
        return pos;
    };
    let (min_x, max_x) = (-m.x * 2.0 + 120.0, m.x * 3.0 - 120.0);
    let max_y = (m.y - 60.0).max(0.0);
    Pos2::new(pos.x.clamp(min_x, max_x), pos.y.clamp(0.0, max_y))
}

/// A docked editor window inside the main window, with an Undock button;
/// undocked, the native window. Returns whether it stays open.
pub(crate) fn editor_window(
    ctx: &egui::Context,
    layout: &mut Layout,
    p: Panel,
    instance: u64,
    title: &str,
    contents: &mut dyn FnMut(&mut egui::Ui),
) -> bool {
    if layout.is_undocked(p) {
        return layout.window(ctx, p, instance, title, contents) != WindowEvent::Closed;
    }
    let mut open = true;
    let mut undock = false;
    egui::Window::new(title)
        .id(egui::Id::unique(("jdance_editor", p.key(), instance)))
        .default_size(p.default_size())
        .resizable(true)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .small_button("Undock")
                    .on_hover_text("Move into its own window")
                    .clicked()
                {
                    undock = true;
                }
            });
            contents(ui);
        });
    if undock {
        layout.undock(p);
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_text_round_trip_and_damage() {
        let mut l = Layout::default();
        l.undock(Panel::Mixer);
        l.undock(Panel::PianoRoll);
        l.undocked.get_mut(&Panel::Mixer).unwrap().pos = Some(Pos2::new(10.0, 20.0));
        let text = l.to_text();
        let back = Layout::from_text(&text);
        assert_eq!(back.undocked, l.undocked);
        let damaged = Layout::from_text("mixer 1 2 x 4\nnope 1 2 3 4\npiano_roll - - 900 500\n");
        assert!(!damaged.is_undocked(Panel::Mixer));
        assert!(damaged.is_undocked(Panel::PianoRoll));
        l.reset();
        assert!(!l.is_undocked(Panel::Mixer) && l.changed);
    }

    #[test]
    fn off_screen_windows_come_back() {
        let m = Some(Vec2::new(1920.0, 1080.0));
        assert_eq!(visible(Pos2::new(100.0, 100.0), m), Pos2::new(100.0, 100.0));
        assert_eq!(visible(Pos2::new(100.0, 5000.0), m).y, 1020.0);
        assert!(visible(Pos2::new(99_999.0, 0.0), m).x < 6000.0);
        assert_eq!(visible(Pos2::new(5.0, -400.0), m).y, 0.0);
        assert_eq!(visible(Pos2::new(5.0, 5.0), None), Pos2::new(5.0, 5.0));
    }
}
