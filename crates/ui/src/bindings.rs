// SPDX-License-Identifier: GPL-3.0-or-later
//! One table of the editing keys (spec 0.2.7 §1 item 4): command, the
//! windows it works in, keys and priority. Menus and tooltips take their
//! key labels from here; Ctrl is Cmd and Alt is Opt on macOS (item 3).
//! The table is data so a later version can let the user rebind keys.

use egui::{Key, KeyboardShortcut, ModifierNames, Modifiers};

/// Where a binding works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Playlist,
    PianoRoll,
    /// The same command in the Playlist and the Piano Roll (§3).
    Editors,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Priority {
    P0,
    P1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Binding {
    pub command: &'static str,
    pub scope: Scope,
    pub modifiers: Modifiers,
    pub key: Key,
    pub priority: Priority,
}

impl Binding {
    const fn new(command: &'static str, scope: Scope, modifiers: Modifiers, key: Key) -> Self {
        Self {
            command,
            scope,
            modifiers,
            key,
            priority: Priority::P0,
        }
    }

    pub fn shortcut(&self) -> KeyboardShortcut {
        KeyboardShortcut::new(self.modifiers, self.key)
    }

    /// "Ctrl+B", or "⌘B" on macOS.
    pub fn label(&self) -> String {
        let mac = cfg!(target_os = "macos");
        let names = if mac {
            ModifierNames::SYMBOLS
        } else {
            ModifierNames::NAMES
        };
        self.shortcut().format(&names, mac)
    }

    /// A plain letter: off while the computer keyboard plays notes (§1
    /// item 2).
    pub fn is_letter(&self) -> bool {
        self.modifiers.is_none() && self.key.symbol_or_name().len() == 1
    }

    /// Pressed this frame (consumed, so widgets do not see it).
    pub fn take(&self, i: &mut egui::InputState) -> bool {
        if self.modifiers.is_none() {
            i.consume_key(Modifiers::NONE, self.key)
        } else {
            i.consume_shortcut(&self.shortcut())
        }
    }
}

const CMD: Modifiers = Modifiers::COMMAND;
const NONE: Modifiers = Modifiers::NONE;
const SHIFT: Modifiers = Modifiers::SHIFT;
const CMD_SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: false,
    shift: true,
    mac_cmd: false,
    command: true,
};
const ALT: Modifiers = Modifiers::ALT;

use Scope::{Editors, PianoRoll};

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Self::Playlist => "Playlist",
            Self::PianoRoll => "Piano Roll",
            Self::Editors => "Playlist and Piano Roll",
        }
    }
}

/// The keys window: every binding by window.
pub(crate) fn keys_window(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("Keyboard shortcuts")
        .id(egui::Id::unique("keys_window"))
        .open(open)
        .default_size([420.0, 480.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for scope in [Editors, Scope::Playlist, PianoRoll] {
                    ui.add_space(6.0);
                    ui.strong(scope.name());
                    egui::Grid::new(("keys", scope.name()))
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            for b in ALL.iter().filter(|b| b.scope == scope) {
                                ui.monospace(b.label());
                                let text = if b.priority == Priority::P1 && b.key == Key::V {
                                    format!("{} (reserved)", b.command)
                                } else {
                                    b.command.to_owned()
                                };
                                ui.label(text);
                                ui.end_row();
                            }
                        });
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Hold Ctrl in Draw, Paint, Delete, Mute or Slice to select; hold Alt for no snap. \
                         Ctrl+Z undoes and, pressed again, redoes the last action.",
                    )
                    .size(11.0),
                );
            });
        });
}

pub(crate) const DUPLICATE: Binding = Binding::new("Duplicate", Editors, CMD, Key::B);
pub(crate) const DESELECT: Binding = Binding::new("Deselect", Editors, CMD, Key::D);
pub(crate) const SELECT_ALL: Binding = Binding::new("Select all", PianoRoll, CMD, Key::A);
pub(crate) const COPY: Binding = Binding::new("Copy", PianoRoll, CMD, Key::C);
pub(crate) const CUT: Binding = Binding::new("Cut", PianoRoll, CMD, Key::X);
pub(crate) const PASTE: Binding = Binding::new("Paste at the caret", PianoRoll, CMD, Key::V);
pub(crate) const DELETE: Binding = Binding::new("Delete", Editors, NONE, Key::Delete);
pub(crate) const UNDO_STEP: Binding = Binding::new(
    "Undo step by step",
    Editors,
    Modifiers {
        alt: true,
        ctrl: false,
        shift: false,
        mac_cmd: false,
        command: true,
    },
    Key::Z,
);
pub(crate) const TOOL_DRAW: Binding = Binding::new("Draw", Editors, NONE, Key::P);
pub(crate) const TOOL_PAINT: Binding = Binding::new("Paint", Editors, NONE, Key::B);
pub(crate) const TOOL_DELETE: Binding = Binding::new("Delete tool", Editors, NONE, Key::D);
pub(crate) const TOOL_MUTE: Binding = Binding::new("Mute tool", Editors, NONE, Key::T);
pub(crate) const TOOL_SELECT: Binding = Binding::new("Select", Editors, NONE, Key::E);
pub(crate) const TOOL_SLICE: Binding = Binding {
    priority: Priority::P1,
    ..Binding::new("Slice", Editors, NONE, Key::C)
};
pub(crate) const TOOL_SPEED: Binding = Binding::new("Speed tool", Scope::Playlist, NONE, Key::S);
pub(crate) const IMPORT_MIDI: Binding =
    Binding::new("Import a MIDI file at the caret", PianoRoll, CMD, Key::M);
pub(crate) const SNAP_TOGGLE: Binding =
    Binding::new("Snap on / off", PianoRoll, NONE, Key::Backspace);
pub(crate) const MOVE_LEFT: Binding =
    Binding::new("Move left by the snap", PianoRoll, SHIFT, Key::ArrowLeft);
pub(crate) const MOVE_RIGHT: Binding =
    Binding::new("Move right by the snap", PianoRoll, SHIFT, Key::ArrowRight);
pub(crate) const SEMITONE_UP: Binding =
    Binding::new("Up a semitone", PianoRoll, SHIFT, Key::ArrowUp);
pub(crate) const SEMITONE_DOWN: Binding =
    Binding::new("Down a semitone", PianoRoll, SHIFT, Key::ArrowDown);
pub(crate) const OCTAVE_UP: Binding = Binding::new("Up an octave", PianoRoll, CMD, Key::ArrowUp);
pub(crate) const OCTAVE_DOWN: Binding =
    Binding::new("Down an octave", PianoRoll, CMD, Key::ArrowDown);
pub(crate) const NUDGE_LEFT: Binding = Binding {
    priority: Priority::P1,
    ..Binding::new("Nudge left a pixel", PianoRoll, ALT, Key::ArrowLeft)
};
pub(crate) const NUDGE_RIGHT: Binding = Binding {
    priority: Priority::P1,
    ..Binding::new("Nudge right a pixel", PianoRoll, ALT, Key::ArrowRight)
};
pub(crate) const ZOOM_IN: Binding = Binding::new("Zoom in", Editors, NONE, Key::PageUp);
pub(crate) const ZOOM_OUT: Binding = Binding::new("Zoom out", Editors, NONE, Key::PageDown);
pub(crate) const PIANO_ROLL: Binding = Binding::new("Piano Roll", PianoRoll, NONE, Key::F7);
pub(crate) const CLOSE: Binding = Binding::new("Close", PianoRoll, NONE, Key::Escape);
pub(crate) const CLEAR_SELECTION: Binding =
    Binding::new("Clear selection", Scope::Playlist, NONE, Key::Escape);
pub(crate) const TYPING_PIANO: Binding =
    Binding::new("Computer keyboard as piano", PianoRoll, CMD, Key::T);
/// Reserved for the Piano Roll's own paste of MIDI (P2): not taken by
/// anything else.
pub(crate) const PASTE_MIDI: Binding = Binding {
    priority: Priority::P1,
    ..Binding::new("Paste MIDI at the caret", PianoRoll, CMD_SHIFT, Key::V)
};

/// Every binding, for the menus and the conflict test.
pub(crate) const ALL: &[Binding] = &[
    DUPLICATE,
    DESELECT,
    SELECT_ALL,
    COPY,
    CUT,
    PASTE,
    PASTE_MIDI,
    DELETE,
    UNDO_STEP,
    TOOL_DRAW,
    TOOL_PAINT,
    TOOL_DELETE,
    TOOL_MUTE,
    TOOL_SELECT,
    TOOL_SLICE,
    TOOL_SPEED,
    IMPORT_MIDI,
    SNAP_TOGGLE,
    MOVE_LEFT,
    MOVE_RIGHT,
    SEMITONE_UP,
    SEMITONE_DOWN,
    OCTAVE_UP,
    OCTAVE_DOWN,
    NUDGE_LEFT,
    NUDGE_RIGHT,
    ZOOM_IN,
    ZOOM_OUT,
    PIANO_ROLL,
    CLOSE,
    CLEAR_SELECTION,
    TYPING_PIANO,
];

/// Whether `b` works in `scope`.
#[cfg(test)]
pub(crate) fn applies(b: &Binding, scope: Scope) -> bool {
    b.scope == scope || b.scope == Editors || scope == Editors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_commands_share_keys_in_one_window() {
        for scope in [Scope::Playlist, PianoRoll] {
            let here: Vec<&Binding> = ALL.iter().filter(|b| applies(b, scope)).collect();
            for (i, a) in here.iter().enumerate() {
                for b in &here[i + 1..] {
                    assert!(
                        (a.modifiers, a.key) != (b.modifiers, b.key),
                        "{} and {} share {}",
                        a.command,
                        b.command,
                        a.label()
                    );
                }
            }
        }
    }

    #[test]
    fn labels_and_letters() {
        if !cfg!(target_os = "macos") {
            assert_eq!(DUPLICATE.label(), "Ctrl+B");
            assert_eq!(DESELECT.label(), "Ctrl+D");
        }
        assert!(TOOL_DRAW.is_letter() && !DUPLICATE.is_letter() && !ZOOM_IN.is_letter());
    }
}
