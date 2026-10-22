// SPDX-License-Identifier: GPL-3.0-or-later
//! Global keys (UI-VIS-100, spec 0.2.3 KEY-001…005): read the same way in
//! every window of the app, the undocked panels included.

use egui::{Key, KeyboardShortcut, Modifiers};

/// Global keys pressed in one window this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GlobalKeys {
    pub space: bool,
    pub undo: bool,
    pub redo: bool,
    pub save: bool,
    pub save_as: bool,
    pub open: bool,
    pub new: bool,
    pub link: bool,
    pub metronome: bool,
    pub count_in: bool,
}

impl GlobalKeys {
    pub fn merge(&mut self, o: Self) {
        self.space |= o.space;
        self.undo |= o.undo;
        self.redo |= o.redo;
        self.save |= o.save;
        self.save_as |= o.save_as;
        self.open |= o.open;
        self.new |= o.new;
        self.link |= o.link;
        self.metronome |= o.metronome;
        self.count_in |= o.count_in;
    }
}

/// Takes the global keys of the window `ctx` is drawing. Space is consumed
/// so a focused button does not also take it; it is left alone while a
/// text field edits or a modal dialog is open (KEY-003), and key repeat
/// does not count (KEY-005).
pub(crate) fn take(ctx: &egui::Context) -> GlobalKeys {
    let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
    let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
    // Only text editing keeps Space: a focused button must not (KEY-003).
    let typing = ctx.text_edit_focused();
    let modal = ctx.memory(|m| m.top_modal_layer().is_some());
    ctx.input_mut(|i| {
        // egui matches shortcuts "logically" (extra Shift is ignored), so
        // the Shift variants must be consumed before the plain ones.
        let redo = i.consume_shortcut(&cmd_shift(Key::Z)) || i.consume_shortcut(&cmd(Key::Y));
        let save_as = i.consume_shortcut(&cmd_shift(Key::S));
        let mut keys = GlobalKeys {
            redo,
            undo: i.consume_shortcut(&cmd(Key::Z)),
            save_as,
            save: i.consume_shortcut(&cmd(Key::S)),
            open: i.consume_shortcut(&cmd(Key::O)),
            new: i.consume_shortcut(&cmd(Key::N)),
            link: i.consume_shortcut(&cmd(Key::L)),
            metronome: i.consume_shortcut(&cmd(Key::M)),
            count_in: i.consume_shortcut(&cmd(Key::P)),
            space: false,
        };
        if !typing && !modal {
            let pressed = i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: Key::Space,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.is_none()
                )
            });
            // Removes the press (and its repeats) for the widgets.
            i.consume_key(Modifiers::NONE, Key::Space);
            keys.space = pressed;
        }
        keys
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, RawInput};

    fn press(key: Key, modifiers: Modifiers, repeat: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers,
        }
    }

    fn run(events: Vec<Event>, ui: impl FnMut(&mut egui::Ui)) -> GlobalKeys {
        let ctx = egui::Context::default();
        let mut ui = ui;
        let mut keys = GlobalKeys::default();
        let input = RawInput {
            events,
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |root| {
            ui(root);
            keys = take(root.ctx());
        });
        out.textures_delta.clear();
        keys
    }

    #[test]
    fn space_and_shortcuts_are_read_once_and_repeats_ignored() {
        let k = run(vec![press(Key::Space, Modifiers::NONE, false)], |_| {});
        assert!(k.space);
        // Held down: egui marks the next presses as repeats (KEY-005).
        let ctx = egui::Context::default();
        let mut got = Vec::new();
        for _ in 0..3 {
            let input = RawInput {
                events: vec![press(Key::Space, Modifiers::NONE, false)],
                ..RawInput::default()
            };
            let mut out = ctx.run_ui(input, |ui| got.push(take(ui.ctx()).space));
            out.textures_delta.clear();
        }
        assert_eq!(got, [true, false, false]);
        let k = run(
            vec![
                press(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT, false),
                press(Key::M, Modifiers::COMMAND, false),
            ],
            |_| {},
        );
        assert!(k.redo && !k.undo && k.metronome && !k.space);
    }

    #[test]
    fn a_focused_button_does_not_take_space() {
        let ctx = egui::Context::default();
        let (mut clicked, mut keys) = (false, GlobalKeys::default());
        for events in [vec![], vec![press(Key::Space, Modifiers::NONE, false)]] {
            let input = RawInput {
                events,
                ..RawInput::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                keys = take(ui.ctx());
                let b = ui.button("Panic");
                b.request_focus();
                clicked |= b.clicked();
            });
            out.textures_delta.clear();
        }
        assert!(
            keys.space && !clicked,
            "KEY-003: space {} clicked {clicked}",
            keys.space
        );
    }

    #[test]
    fn space_types_into_text_fields() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let mut keys = GlobalKeys::default();
        for events in [vec![], vec![press(Key::Space, Modifiers::NONE, false)]] {
            let input = RawInput {
                events,
                ..RawInput::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let r = ui.text_edit_singleline(&mut text);
                r.request_focus();
                keys = take(ui.ctx());
            });
            out.textures_delta.clear();
        }
        assert!(!keys.space, "a focused text field keeps Space (KEY-003)");
    }
}
