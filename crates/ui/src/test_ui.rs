// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless egui driver for UI tests: frames with pointer input and
//! lookup of widgets by their accessible label.

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};

pub(crate) struct Driver {
    pub ctx: egui::Context,
    pub modifiers: Modifiers,
    /// Accessible nodes of the last frame: label and bounds.
    nodes: Vec<(String, Rect)>,
}

impl Driver {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        Self {
            ctx,
            modifiers: Modifiers::NONE,
            nodes: Vec::new(),
        }
    }

    /// Runs one frame of `ui` with `events`.
    pub fn frame(&mut self, mut events: Vec<Event>, ui: &mut dyn FnMut(&mut egui::Ui)) {
        events.insert(0, Event::ModifiersChanged(self.modifiers));
        let input = RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
            ..RawInput::default()
        };
        let mut out = self.ctx.run_ui(input, |root| ui(root));
        out.textures_delta.clear();
        self.nodes.clear();
        if let Some(update) = out.platform_output.accesskit_update {
            for (_, node) in update.nodes {
                if let (Some(label), Some(b)) = (node.label(), node.bounds()) {
                    let r = Rect::from_min_max(
                        pos2(b.x0 as f32, b.y0 as f32),
                        pos2(b.x1 as f32, b.y1 as f32),
                    );
                    self.nodes.push((label.to_owned(), r));
                }
            }
        }
    }

    /// Centre of the last frame's widget labelled `label`.
    pub fn find(&self, label: &str) -> Option<Pos2> {
        self.nodes
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, r)| r.center())
    }

    pub fn moved(&self, p: Pos2) -> Event {
        Event::PointerMoved(p)
    }

    pub fn button(&self, p: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed,
            modifiers: self.modifiers,
        }
    }

    /// A click at `p` over three frames (move, press, release).
    pub fn click(&mut self, p: Pos2, ui: &mut dyn FnMut(&mut egui::Ui)) {
        self.frame(vec![self.moved(p)], ui);
        self.frame(vec![self.button(p, true)], ui);
        self.frame(vec![self.button(p, false)], ui);
    }
}
