// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano Roll Tools dialogs (spec 0.2.0 §18): settings, a live preview
//! computed from the unchanged notes (never from an earlier preview),
//! Apply (one undo step), Cancel and Reset. Closing the window cancels.
//! The last settings of each tool are kept for the session.

use crate::theme;
use crate::toolbar::bars_text;
use egui::RichText;
use jdance_commands::note_tools::{
    self as nt, ArpDirection, ArticulateMode, ChopMode, ClawMode, GenScale, Input, Insertion, Mode,
    Outcome, RandomMode, Range, Tool,
};

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Grid choices in ticks, triplets included.
const GRIDS: [(u32, &str); 10] = [
    (3840, "1 bar"),
    (1920, "1/2"),
    (960, "1/4 (beat)"),
    (640, "1/4 triplet"),
    (480, "1/8"),
    (320, "1/8 triplet"),
    (240, "1/16 (step)"),
    (160, "1/16 triplet"),
    (120, "1/32"),
    (60, "1/64"),
];

#[derive(Debug, Clone)]
pub(crate) struct ToolDialog {
    pub tool: Tool,
    defaults: Tool,
    /// Settings and range the preview was computed for, and its result.
    cache: Option<(Tool, Range, Result<Option<Outcome>, String>)>,
    /// Generator range (editable); `None` = from the input.
    range: Option<Range>,
    /// Asking whether to apply without the notes that leave the pattern.
    confirm: bool,
    groove_text: String,
}

#[derive(Debug, Default)]
pub(crate) struct ToolOutput {
    pub apply: Option<Outcome>,
    pub close: bool,
}

fn memory_id(tool: &Tool) -> egui::Id {
    egui::Id::unique(("jdance_note_tool", tool.name()))
}

impl ToolDialog {
    /// A dialog for `tool`, starting from the settings used last in this
    /// session.
    pub fn new(ctx: &egui::Context, tool: Tool) -> Self {
        let last = ctx.data(|d| d.get_temp::<Tool>(memory_id(&tool)));
        Self {
            defaults: tool.clone(),
            tool: last.unwrap_or(tool),
            cache: None,
            range: None,
            confirm: false,
            groove_text: String::new(),
        }
    }

    fn range(&self, input: &Input) -> Range {
        self.range.unwrap_or_else(|| input.gen_range())
    }

    /// The preview for the current settings (recomputed when they change).
    pub fn preview(&mut self, input: &Input) -> Option<&Outcome> {
        let range = self.range(input);
        let stale = self
            .cache
            .as_ref()
            .is_none_or(|(t, r, _)| *t != self.tool || *r != range);
        if stale {
            let inp = Input {
                range: Some(range),
                ..*input
            };
            let result = nt::run(&self.tool, &inp);
            self.cache = Some((self.tool.clone(), range, result));
            self.confirm = false;
        }
        match &self.cache {
            Some((_, _, Ok(Some(o)))) => Some(o),
            _ => None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, input: &Input) -> ToolOutput {
        let mut out = ToolOutput::default();
        let mut open = true;
        let title = self.tool.name();
        egui::Window::new(title)
            .id(egui::Id::unique(("note_tool_window", title)))
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .open(&mut open)
            .show(ctx, |ui| {
                let scope = input.scope().len();
                let what = if self.tool.is_generator() {
                    let r = self.range(input);
                    format!(
                        "Range {} – {}",
                        bars_text(u64::from(r.start)),
                        bars_text(u64::from(r.end))
                    )
                } else if input.selected.is_empty() {
                    format!("All {scope} notes of this channel")
                } else {
                    format!("{scope} selected notes")
                };
                ui.label(RichText::new(what).color(theme::MUTED));
                ui.separator();
                let pattern_len = input.pattern_len;
                let mut range = self.range(input);
                settings(
                    ui,
                    &mut self.tool,
                    &mut range,
                    pattern_len,
                    &mut self.groove_text,
                );
                if range != self.range(input) {
                    self.range = Some(range);
                }
                ui.separator();
                self.preview(input);
                match &self.cache {
                    Some((_, _, Ok(None))) => {
                        ui.label(RichText::new("No notes to work on.").color(theme::MUTED));
                    }
                    Some((_, _, Err(e))) => {
                        ui.colored_label(theme::ERROR, e);
                    }
                    Some((_, _, Ok(Some(o)))) => {
                        let changed = o.select.len();
                        ui.label(format!(
                            "Preview: {} notes in the row, {changed} new or changed",
                            o.notes.len()
                        ));
                        for line in &o.info {
                            ui.colored_label(theme::WARN, line);
                        }
                    }
                    None => {}
                }
                ui.add_space(4.0);
                let ready = matches!(&self.cache, Some((_, _, Ok(Some(_)))));
                if self.confirm {
                    let dropped = match &self.cache {
                        Some((_, _, Ok(Some(o)))) => o.dropped,
                        _ => 0,
                    };
                    ui.colored_label(
                        theme::WARN,
                        format!("{dropped} note(s) would leave the pattern. Apply without them?"),
                    );
                }
                ui.horizontal(|ui| {
                    let label = if self.confirm {
                        "Apply anyway"
                    } else {
                        "Apply"
                    };
                    if ui.add_enabled(ready, egui::Button::new(label)).clicked()
                        && let Some((_, _, Ok(Some(o)))) = &self.cache
                    {
                        if o.dropped > 0 && !self.confirm {
                            self.confirm = true;
                        } else {
                            out.apply = Some(o.clone());
                            out.close = true;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        out.close = true;
                    }
                    if ui
                        .button("Reset")
                        .on_hover_text("Default settings")
                        .clicked()
                    {
                        self.tool = self.defaults.clone();
                        self.range = None;
                    }
                });
            });
        if !open {
            out.close = true;
        }
        if out.apply.is_some() {
            ctx.data_mut(|d| d.insert_temp(memory_id(&self.tool), self.tool.clone()));
        }
        out
    }
}

fn grid_combo(ui: &mut egui::Ui, salt: &str, grid: &mut u32) {
    let name = GRIDS
        .iter()
        .find(|(v, _)| v == grid)
        .map_or_else(|| format!("{grid} ticks"), |(_, n)| (*n).to_owned());
    egui::ComboBox::from_id_salt(salt)
        .selected_text(name)
        .show_ui(ui, |ui| {
            for (v, n) in GRIDS {
                ui.selectable_value(grid, v, n);
            }
        });
}

fn ticks(ui: &mut egui::Ui, v: &mut u32, max: u32) {
    ui.add(egui::DragValue::new(v).range(0..=max).suffix(" ticks"));
}

fn percent(ui: &mut egui::Ui, v: &mut u32, lo: u32, hi: u32) {
    ui.add(egui::DragValue::new(v).range(lo..=hi).suffix(" %"));
}

fn key_drag(ui: &mut egui::Ui, v: &mut u8) {
    ui.add(egui::DragValue::new(v).range(0..=127))
        .on_hover_text(crate::piano::key_name(*v));
}

fn root_combo(ui: &mut egui::Ui, salt: &str, root: &mut u8) {
    egui::ComboBox::from_id_salt(salt)
        .width(52.0)
        .selected_text(NAMES[usize::from(*root % 12)])
        .show_ui(ui, |ui| {
            for (i, n) in NAMES.iter().enumerate() {
                ui.selectable_value(root, i as u8, *n);
            }
        });
}

fn mode_combo(ui: &mut egui::Ui, salt: &str, mode: &mut Mode) {
    egui::ComboBox::from_id_salt(salt)
        .selected_text(mode.name())
        .show_ui(ui, |ui| {
            ui.selectable_value(mode, Mode::Major, Mode::Major.name());
            ui.selectable_value(mode, Mode::Minor, Mode::Minor.name());
        });
}

fn seed(ui: &mut egui::Ui, seed: &mut u64) {
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(seed));
        if ui
            .small_button("New")
            .on_hover_text("Another random variant")
            .clicked()
        {
            *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1) >> 16;
        }
    });
}

fn insertion(ui: &mut egui::Ui, v: &mut Insertion) {
    ui.horizontal(|ui| {
        ui.selectable_value(v, Insertion::ReplaceRange, "Replace range")
            .on_hover_text("Notes of this channel starting in the range are removed");
        ui.selectable_value(v, Insertion::AppendLayer, "Add a layer");
    });
}

fn arp_combo(ui: &mut egui::Ui, salt: &str, d: &mut ArpDirection) {
    let name = |d: ArpDirection| match d {
        ArpDirection::Up => "Up",
        ArpDirection::Down => "Down",
        ArpDirection::UpDown => "Up and down",
        ArpDirection::DownUp => "Down and up",
        ArpDirection::Random => "Random",
    };
    egui::ComboBox::from_id_salt(salt)
        .selected_text(name(*d))
        .show_ui(ui, |ui| {
            for v in [
                ArpDirection::Up,
                ArpDirection::Down,
                ArpDirection::UpDown,
                ArpDirection::DownUp,
                ArpDirection::Random,
            ] {
                ui.selectable_value(d, v, name(v));
            }
        });
}

/// Degrees as text "1 5 6 4".
fn degrees(ui: &mut egui::Ui, v: &mut Vec<u8>) {
    let mut text = v.iter().map(u8::to_string).collect::<Vec<_>>().join(" ");
    if ui
        .add(egui::TextEdit::singleline(&mut text).desired_width(140.0))
        .on_hover_text("Scale degrees 1–7, e.g. 1 5 6 4")
        .changed()
    {
        let parsed: Vec<u8> = text
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse::<u8>().ok())
            .filter(|d| (1..=7).contains(d))
            .take(64)
            .collect();
        if !parsed.is_empty() {
            *v = parsed;
        }
    }
}

fn range_ui(ui: &mut egui::Ui, r: &mut Range, pattern_len: u32) {
    ui.label("Range");
    ui.horizontal(|ui| {
        let mut start = r.start;
        let mut len = r.len();
        ui.add(
            egui::DragValue::new(&mut start)
                .range(0..=pattern_len.saturating_sub(1))
                .suffix(" from"),
        );
        ui.add(
            egui::DragValue::new(&mut len)
                .range(1..=pattern_len)
                .suffix(" ticks"),
        );
        r.start = start;
        r.end = (start + len).min(pattern_len);
    });
    ui.end_row();
}

/// The settings form of `tool`.
fn settings(
    ui: &mut egui::Ui,
    tool: &mut Tool,
    range: &mut Range,
    pattern_len: u32,
    groove_text: &mut String,
) {
    egui::Grid::new("note_tool_settings")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| match tool {
            Tool::QuickLegato | Tool::QuickQuantize | Tool::QuickQuantizeStarts | Tool::QuickChop => {
                ui.label("Uses the Piano Roll's snap grid.");
                ui.end_row();
            }
            Tool::Glue(s) => {
                ui.label("Gap tolerance");
                ticks(ui, &mut s.gap, 3840);
                ui.end_row();
            }
            Tool::Flip(s) => {
                ui.label("Flip");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.horizontal, "In time");
                    ui.checkbox(&mut s.vertical, "In pitch");
                });
                ui.end_row();
                ui.label("Pitch pivot");
                ui.horizontal(|ui| {
                    let mut custom = s.pivot.is_some();
                    ui.checkbox(&mut custom, "Custom");
                    match (custom, &mut s.pivot) {
                        (true, Some(p)) => key_drag(ui, p),
                        (true, None) => s.pivot = Some(60),
                        (false, _) => {
                            s.pivot = None;
                            ui.label(RichText::new("centre of the notes").color(theme::MUTED));
                        }
                    }
                });
                ui.end_row();
            }
            Tool::Articulate(s) => {
                ui.label("Mode");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.mode, ArticulateMode::Gate, "Gate");
                    ui.selectable_value(&mut s.mode, ArticulateMode::Legato, "Legato");
                });
                ui.end_row();
                if s.mode == ArticulateMode::Gate {
                    ui.label("Gate");
                    percent(ui, &mut s.gate_percent, 1, 200);
                    ui.end_row();
                }
                ui.label("Gap");
                ticks(ui, &mut s.gap, 960);
                ui.end_row();
                ui.label("Variation");
                percent(ui, &mut s.variation_percent, 0, 100);
                ui.end_row();
                ui.label("Seed");
                seed(ui, &mut s.seed);
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.prevent_overlap, "Stop at the next note");
                ui.end_row();
            }
            Tool::Quantize(s) => {
                ui.label("Grid");
                grid_combo(ui, "qz_grid", &mut s.grid);
                ui.end_row();
                ui.label("Strength");
                percent(ui, &mut s.strength, 0, 100);
                ui.end_row();
                ui.label("Move");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.starts, "Starts");
                    ui.checkbox(&mut s.ends, "Ends");
                });
                ui.end_row();
                if s.starts && !s.ends {
                    ui.label("");
                    ui.checkbox(&mut s.preserve_length, "Keep lengths");
                    ui.end_row();
                }
                ui.label("Swing");
                percent(ui, &mut s.swing, 0, 75);
                ui.end_row();
                ui.label("Window");
                ui.horizontal(|ui| {
                    ticks(ui, &mut s.window, 3840);
                    ui.label(RichText::new("0 = any distance").size(11.0).color(theme::MUTED));
                });
                ui.end_row();
                ui.label("Groove");
                ui.vertical(|ui| {
                    let mut on = s.groove.is_some();
                    if ui.checkbox(&mut on, "Custom groove (JSON)").changed() && !on {
                        s.groove = None;
                    }
                    if on {
                        if groove_text.is_empty() {
                            *groove_text = s.groove.as_ref().map_or_else(
                                || {
                                    r#"{"format":"jdance-groove","version":1,"length":960,"points":[0,250,480,730]}"#
                                        .to_owned()
                                },
                                nt::Groove::to_json,
                            );
                        }
                        ui.add(
                            egui::TextEdit::multiline(groove_text)
                                .desired_rows(2)
                                .font(theme::mono(11.0)),
                        );
                        match nt::Groove::from_json(groove_text) {
                            Ok(g) => s.groove = Some(g),
                            Err(e) => {
                                ui.colored_label(theme::ERROR, e);
                            }
                        }
                    }
                });
                ui.end_row();
            }
            Tool::Chop(s) => {
                ui.label("Split");
                ui.horizontal(|ui| {
                    let step = matches!(s.mode, ChopMode::Step(_));
                    if ui.selectable_label(step, "Every").clicked() && !step {
                        s.mode = ChopMode::Step(240);
                    }
                    if ui.selectable_label(!step, "Equal parts").clicked() && step {
                        s.mode = ChopMode::Parts(4);
                    }
                });
                ui.end_row();
                match &mut s.mode {
                    ChopMode::Step(t) => {
                        ui.label("Step");
                        grid_combo(ui, "chop_step", t);
                    }
                    ChopMode::Parts(n) => {
                        ui.label("Parts");
                        ui.add(egui::DragValue::new(n).range(1..=128));
                    }
                }
                ui.end_row();
                ui.label("Gate");
                percent(ui, &mut s.gate_percent, 1, 100);
                ui.end_row();
                ui.label("Every 2nd velocity");
                ui.add(egui::Slider::new(&mut s.alternate_velocity, 0.0..=2.0).text("×"));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.reverse_velocity, "Reverse the velocity pattern");
                ui.end_row();
            }
            Tool::Strum(s) => {
                ui.label("Direction");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.downward, false, "Low to high");
                    ui.selectable_value(&mut s.downward, true, "High to low");
                });
                ui.end_row();
                ui.label("Delay per note");
                ticks(ui, &mut s.delay, 960);
                ui.end_row();
                ui.label("Chord tolerance");
                ticks(ui, &mut s.tolerance, 480);
                ui.end_row();
                ui.label("Velocity ramp");
                ui.add(egui::DragValue::new(&mut s.velocity_ramp).range(-64..=64).suffix(" per note"));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.preserve_ends, "Keep the ends");
                ui.end_row();
            }
            Tool::Flam(s) => {
                ui.label("Offset");
                ticks(ui, &mut s.offset, 480);
                ui.end_row();
                ui.label("Grace length");
                ticks(ui, &mut s.grace_length, 480);
                ui.end_row();
                ui.label("Grace velocity");
                ui.add(egui::Slider::new(&mut s.grace_velocity, 0.05..=1.0).text("×"));
                ui.end_row();
                ui.label("At the pattern start");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.shift_pair, false, "Skip the grace");
                    ui.selectable_value(&mut s.shift_pair, true, "Shift the pair");
                });
                ui.end_row();
            }
            Tool::Limit(s) => {
                ui.label("Lowest / highest");
                ui.horizontal(|ui| {
                    key_drag(ui, &mut s.min);
                    key_drag(ui, &mut s.max);
                });
                ui.end_row();
                ui.label("Transpose");
                ui.add(egui::DragValue::new(&mut s.transpose).range(-127..=127).suffix(" st"));
                ui.end_row();
                ui.label("Out of range");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.fold, true, "Fold by octaves");
                    ui.selectable_value(&mut s.fold, false, "Clamp");
                });
                ui.end_row();
                ui.label("Scale");
                ui.horizontal(|ui| {
                    let mut on = s.scale.is_some();
                    ui.checkbox(&mut on, "Snap to");
                    if on {
                        let (mut root, mut mode) = s.scale.unwrap_or((0, Mode::Major));
                        root_combo(ui, "limit_root", &mut root);
                        mode_combo(ui, "limit_mode", &mut mode);
                        s.scale = Some((root, mode));
                    } else {
                        s.scale = None;
                    }
                });
                ui.end_row();
            }
            Tool::ScaleLevels(s) => {
                ui.label("Centre");
                ui.add(egui::DragValue::new(&mut s.center).range(1..=127));
                ui.end_row();
                ui.label("Multiplier");
                ui.add(egui::Slider::new(&mut s.multiplier, 0.0..=4.0));
                ui.end_row();
                ui.label("Offset");
                ui.add(egui::Slider::new(&mut s.offset, -1.0..=1.0));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.invert, "Invert");
                ui.end_row();
                ui.label("Curve");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.power, false, "Linear");
                    ui.selectable_value(&mut s.power, true, "Power");
                    if s.power {
                        ui.add(egui::DragValue::new(&mut s.exponent).range(0.05..=8.0).speed(0.02));
                    }
                });
                ui.end_row();
            }
            Tool::Arpeggiate(s) => {
                ui.label("Direction");
                arp_combo(ui, "arp_dir", &mut s.direction);
                ui.end_row();
                ui.label("Step");
                grid_combo(ui, "arp_step", &mut s.step);
                ui.end_row();
                ui.label("Gate");
                percent(ui, &mut s.gate_percent, 1, 100);
                ui.end_row();
                ui.label("Octaves");
                ui.add(egui::DragValue::new(&mut s.octaves).range(1..=4));
                ui.end_row();
                if s.direction == ArpDirection::Random {
                    ui.label("Seed");
                    seed(ui, &mut s.seed);
                    ui.end_row();
                }
            }
            Tool::ChordProgression(s) => {
                range_ui(ui, range, pattern_len);
                ui.label("Key");
                ui.horizontal(|ui| {
                    root_combo(ui, "cp_root", &mut s.tonic);
                    mode_combo(ui, "cp_mode", &mut s.mode);
                });
                ui.end_row();
                ui.label("Degrees");
                degrees(ui, &mut s.degrees);
                ui.end_row();
                ui.label("Chords");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.seventh, false, "Triads");
                    ui.selectable_value(&mut s.seventh, true, "Sevenths");
                });
                ui.end_row();
                ui.label("Chord length");
                grid_combo(ui, "cp_len", &mut s.chord_length);
                ui.end_row();
                ui.label("Octave / inversion");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut s.octave).range(0..=8));
                    ui.add(egui::DragValue::new(&mut s.inversion).range(0..=3));
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.close_voicing, "Smooth voice leading");
                ui.end_row();
                ui.label("Velocity");
                ui.add(egui::DragValue::new(&mut s.velocity).range(1..=127));
                ui.end_row();
                ui.label("Insert");
                insertion(ui, &mut s.insertion);
                ui.end_row();
            }
            Tool::Randomize(s) => {
                ui.label("Mode");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.mode, RandomMode::Transform, "Vary notes");
                    ui.selectable_value(&mut s.mode, RandomMode::Generate, "Generate");
                });
                ui.end_row();
                ui.label("Seed");
                seed(ui, &mut s.seed);
                ui.end_row();
                match s.mode {
                    RandomMode::Transform => {
                        ui.label("Pitch ±");
                        ui.add(egui::DragValue::new(&mut s.key_range).range(0..=24).suffix(" st"));
                        ui.end_row();
                        ui.label("Start ±");
                        ticks(ui, &mut s.start_range, 960);
                        ui.end_row();
                        ui.label("Length ±");
                        ticks(ui, &mut s.length_range, 960);
                        ui.end_row();
                        ui.label("Velocity ±");
                        ui.add(egui::DragValue::new(&mut s.velocity_range).range(0..=127));
                        ui.end_row();
                        ui.label("Scale");
                        ui.horizontal(|ui| {
                            let mut on = s.scale.is_some();
                            ui.checkbox(&mut on, "Keep in");
                            if on {
                                let (mut root, mut mode) = s.scale.unwrap_or((0, Mode::Major));
                                root_combo(ui, "rnd_root", &mut root);
                                mode_combo(ui, "rnd_mode", &mut mode);
                                s.scale = Some((root, mode));
                            } else {
                                s.scale = None;
                            }
                        });
                        ui.end_row();
                    }
                    RandomMode::Generate => {
                        range_ui(ui, range, pattern_len);
                        ui.label("Grid");
                        grid_combo(ui, "rnd_grid", &mut s.grid);
                        ui.end_row();
                        ui.label("Density");
                        ui.add(egui::Slider::new(&mut s.density, 0.0..=1.0));
                        ui.end_row();
                        ui.label("Pitch range");
                        ui.horizontal(|ui| {
                            key_drag(ui, &mut s.key_min);
                            key_drag(ui, &mut s.key_max);
                        });
                        ui.end_row();
                        ui.label("Scale");
                        ui.horizontal(|ui| {
                            root_combo(ui, "rnd_groot", &mut s.root);
                            ui.selectable_value(&mut s.gen_scale, GenScale::Major, "Major");
                            ui.selectable_value(&mut s.gen_scale, GenScale::Minor, "Minor");
                            ui.selectable_value(&mut s.gen_scale, GenScale::Chromatic, "Chromatic");
                        });
                        ui.end_row();
                        ui.label("Polyphony");
                        ui.add(egui::DragValue::new(&mut s.polyphony).range(1..=16));
                        ui.end_row();
                        ui.label("Length");
                        ui.horizontal(|ui| {
                            ticks(ui, &mut s.length_min, 3840);
                            ticks(ui, &mut s.length_max, 3840);
                        });
                        ui.end_row();
                        ui.label("Velocity");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut s.velocity_min).range(1..=127));
                            ui.add(egui::DragValue::new(&mut s.velocity_max).range(1..=127));
                        });
                        ui.end_row();
                        ui.label("Insert");
                        insertion(ui, &mut s.insertion);
                        ui.end_row();
                    }
                }
            }
            Tool::ClawMachine(s) => {
                ui.label("Period");
                grid_combo(ui, "claw_period", &mut s.period);
                ui.end_row();
                ui.label("Slices per period");
                ui.add(egui::DragValue::new(&mut s.slices).range(1..=64));
                ui.end_row();
                ui.label("Every Nth slice");
                ui.add(egui::DragValue::new(&mut s.every).range(1..=64));
                ui.end_row();
                ui.label("Phase");
                ui.add(egui::DragValue::new(&mut s.phase).range(0..=63));
                ui.end_row();
                ui.label("Marked slices");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.mode, ClawMode::Remove, "Remove");
                    ui.selectable_value(&mut s.mode, ClawMode::Gate, "Keep only");
                    ui.selectable_value(&mut s.mode, ClawMode::Shift, "Shift");
                });
                ui.end_row();
                if s.mode == ClawMode::Shift {
                    ui.label("Shift");
                    ui.add(egui::DragValue::new(&mut s.shift).range(-3840..=3840).suffix(" ticks"));
                    ui.end_row();
                }
            }
            Tool::RiffMachine(s) => {
                range_ui(ui, range, pattern_len);
                ui.label("Key");
                ui.horizontal(|ui| {
                    root_combo(ui, "riff_root", &mut s.root);
                    mode_combo(ui, "riff_mode", &mut s.mode);
                });
                ui.end_row();
                ui.label("Pitch range");
                ui.horizontal(|ui| {
                    key_drag(ui, &mut s.key_min);
                    key_drag(ui, &mut s.key_max);
                });
                ui.end_row();
                ui.label("Rhythm");
                ui.horizontal(|ui| {
                    grid_combo(ui, "riff_div", &mut s.division);
                    percent(ui, &mut s.density, 0, 100);
                });
                ui.end_row();
                ui.label("Chords (degrees)");
                degrees(ui, &mut s.progression);
                ui.end_row();
                ui.label("Chord length");
                grid_combo(ui, "riff_clen", &mut s.chord_length);
                ui.end_row();
                ui.label("Arpeggio");
                ui.horizontal(|ui| {
                    arp_combo(ui, "riff_arp", &mut s.arp);
                    ui.add(egui::DragValue::new(&mut s.octaves).range(1..=4).suffix(" oct"));
                });
                ui.end_row();
                ui.label("Gate");
                percent(ui, &mut s.gate_percent, 1, 100);
                ui.end_row();
                ui.label("Velocity");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut s.velocity).range(1..=127));
                    ui.add(egui::DragValue::new(&mut s.velocity_variation).range(0..=64).prefix("±"));
                });
                ui.end_row();
                ui.label("Timing ±");
                ticks(ui, &mut s.timing, 240);
                ui.end_row();
                ui.label("Seed");
                seed(ui, &mut s.seed);
                ui.end_row();
                ui.label("Insert");
                insertion(ui, &mut s.insertion);
                ui.end_row();
            }
        });
}

/// Every built-in tool in menu order (separators between groups), with
/// the dialog's starting settings for this grid.
pub(crate) fn menu_tools(grid: u32) -> Vec<Option<Tool>> {
    let g = grid.max(1);
    vec![
        Some(Tool::RiffMachine(nt::RiffMachine::default())),
        Some(Tool::ChordProgression(nt::ChordProgression::default())),
        None,
        Some(Tool::QuickLegato),
        Some(Tool::Articulate(nt::Articulate::default())),
        None,
        Some(Tool::QuickQuantize),
        Some(Tool::QuickQuantizeStarts),
        Some(Tool::Quantize(nt::Quantize {
            grid: g,
            ..nt::Quantize::default()
        })),
        None,
        Some(Tool::QuickChop),
        Some(Tool::Chop(nt::Chop {
            mode: ChopMode::Step(g),
            ..nt::Chop::default()
        })),
        Some(Tool::Glue(nt::Glue::default())),
        None,
        Some(Tool::Arpeggiate(nt::Arpeggiate::default())),
        Some(Tool::Strum(nt::Strum::default())),
        Some(Tool::Flam(nt::Flam::default())),
        Some(Tool::ClawMachine(nt::ClawMachine::default())),
        None,
        Some(Tool::Limit(nt::Limit::default())),
        Some(Tool::Flip(nt::Flip::default())),
        Some(Tool::Randomize(nt::Randomize::default())),
        Some(Tool::ScaleLevels(nt::ScaleLevels::default())),
    ]
}

/// Quick tools apply at once (a dialog opens only to confirm dropped
/// notes).
pub(crate) fn is_quick(tool: &Tool) -> bool {
    matches!(
        tool,
        Tool::QuickLegato | Tool::QuickQuantize | Tool::QuickQuantizeStarts | Tool::QuickChop
    )
}
