// SPDX-License-Identifier: GPL-3.0-or-later
//! Channel Rack with the step sequencer (spec "DAW UI clean room", §5).
//!
//! Row: mute lamp, pan and volume knobs, mixer track, name (color, type,
//! sample state), then the steps, or a preview of the piano-roll notes.
//! Steps are one-step notes at the channel's root key; a click toggles,
//! dragging paints the same state over several steps as one undo step,
//! the wheel over a lit step changes its velocity (fill height).
//! Click a name to select, Ctrl to add, Shift for a range; the context
//! menu acts on the selection.
//!
//! Drawing collects edits into [`RackOutput`]; the caller applies them after
//! the frame so the document is never borrowed mutably while drawn.

use crate::widgets::{knob, lamp};
use crate::{Host, SampleStatus, theme};
use egui::{CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use jdance_commands::Edit;
use jdance_domain::{
    ChannelId, MAX_VOLUME, Pattern, PatternId, PluginTarget, Project, SampleRef, Step,
    color_or_default,
};
use jdance_plugin_api::PluginDescriptor;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const STEP_W: f32 = 26.0;
const STEP_H: f32 = 28.0;
const NAME_W: f32 = 150.0;
/// Velocity change per wheel notch.
const WHEEL_VELOCITY: f32 = 0.05;

#[derive(Debug, Default)]
pub(crate) struct RackOutput {
    pub edits: Vec<(Edit, bool)>,
    pub end_gesture: bool,
    /// New channels to create: name and optional sample.
    pub new_channels: Vec<(String, Option<SampleRef>)>,
    pub rename: Option<crate::RenameTarget>,
    /// Open the piano roll for this channel.
    pub open_piano: Option<ChannelId>,
    /// New instrument channels.
    pub new_instruments: Vec<PluginDescriptor>,
    /// Toggle the instrument's editor window / parameter window.
    pub toggle_editor: Option<ChannelId>,
    pub show_params: Option<ChannelId>,
    /// Open Channel Settings (`true`: on the sample tab).
    pub open_settings: Option<(ChannelId, bool)>,
    pub clone: Vec<ChannelId>,
    /// Channels to delete after the user confirms.
    pub delete: Vec<ChannelId>,
    /// Open the Sample Selection Dialog for this purpose.
    pub pick: Option<crate::library::PickFor>,
}

/// A step-painting drag: the state painted and the steps touched so far.
#[derive(Debug, Clone)]
struct Paint {
    pattern: PatternId,
    on: bool,
    cells: BTreeMap<(ChannelId, u32), Step>,
}

/// Rack state kept between frames.
#[derive(Debug, Default)]
pub(crate) struct RackView {
    pub selection: BTreeSet<ChannelId>,
    /// Last clicked channel, the anchor of Shift ranges.
    anchor: Option<ChannelId>,
    paint: Option<Paint>,
}

impl RackView {
    pub(crate) fn retain_existing(&mut self, project: &Project) {
        self.selection.retain(|id| project.channel(*id).is_some());
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        pattern: PatternId,
        host: &dyn Host,
        dir: Option<&Path>,
        pattern_mode: bool,
    ) -> RackOutput {
        let mut out = RackOutput::default();
        self.retain_existing(project);
        let Some(pattern) = project
            .pattern(pattern)
            .or_else(|| project.patterns.first())
        else {
            ui.label("No pattern");
            return out;
        };
        let transport = host.transport();
        // The step playhead is shown when this pattern is what plays.
        let current_step = transport
            .loop_pos
            .filter(|_| transport.playing && pattern_mode)
            .map(|p| ((p * pattern.length_steps as f32) as u32).min(pattern.length_steps - 1));

        let dropped: Vec<std::path::PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        let hovering_files = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let pointer = ui.ctx().input(|i| i.pointer.hover_pos());
        let primary_down = ui.ctx().input(|i| i.pointer.primary_down());
        let mut dropped_on_row = false;

        self.header(ui, project, pattern, host, &mut out);
        egui::ScrollArea::both()
            .id_salt("rack")
            .max_height(ui.available_height())
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let label = |ui: &mut egui::Ui, w: f32, t: &str| {
                        ui.add_sized(
                            [w, 14.0],
                            egui::Label::new(RichText::new(t).size(10.5).color(theme::MUTED)),
                        );
                    };
                    label(ui, 20.0, "ON");
                    label(ui, 30.0, "PAN");
                    label(ui, 30.0, "VOL");
                    label(ui, 40.0, "MIXER");
                    label(ui, NAME_W, "CHANNEL");
                    ui.label(
                        RichText::new("STEPS · velocity is the fill height; wheel to change")
                            .size(10.5)
                            .color(theme::MUTED),
                    );
                });
                for (index, ch) in project.channels.iter().enumerate() {
                    let selected = self.selection.contains(&ch.id);
                    let color = theme::rgb(color_or_default(ch.color, index));
                    let frame = egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .corner_radius(6)
                        .fill(if selected {
                            egui::Color32::from_rgb(0x1A, 0x27, 0x38)
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .stroke(if selected {
                            Stroke::new(1.0, egui::Color32::from_rgb(0x2F, 0x5C, 0x8F))
                        } else {
                            Stroke::NONE
                        });
                    let row = frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if lamp(ui, !ch.muted)
                                .on_hover_text(if ch.muted {
                                    "Muted: click to unmute"
                                } else {
                                    "Click to mute"
                                })
                                .clicked()
                            {
                                out.edits
                                    .push((Edit::SetChannelMuted(ch.id, !ch.muted), false));
                            }
                            let mut pan = ch.pan;
                            let r = knob(ui, &mut pan, -1.0..=1.0, 0.0, theme::TEXT, 30.0)
                                .on_hover_text(format!(
                                    "Pan {} (drag; double-click centers)",
                                    pan_text(ch.pan)
                                ));
                            if r.changed() {
                                out.edits.push((Edit::SetChannelPan(ch.id, pan), true));
                            }
                            out.end_gesture |= r.drag_stopped();
                            let mut vol = ch.volume;
                            let r = knob(ui, &mut vol, 0.0..=MAX_VOLUME, 0.8, theme::AMBER, 30.0)
                                .on_hover_text(format!(
                                    "Volume {:.0}% (drag; double-click resets)",
                                    ch.volume * 100.0
                                ));
                            if r.changed() {
                                out.edits.push((Edit::SetChannelVolume(ch.id, vol), true));
                            }
                            out.end_gesture |= r.drag_stopped();
                            self.track_box(ui, project, ch.id, ch.track, &mut out);
                            self.name_button(ui, project, index, color, host, dir, &mut out);
                            let notes = pattern.row(ch.id).is_some_and(|r| !r.notes.is_empty());
                            if notes {
                                notes_preview(ui, pattern, ch.id, color, &mut out);
                            } else {
                                self.steps(
                                    ui,
                                    pattern,
                                    ch.id,
                                    current_step,
                                    pointer,
                                    primary_down,
                                    &mut out,
                                );
                            }
                        });
                    });
                    let rect = row.response.rect;
                    let dragging_in = row
                        .response
                        .dnd_hover_payload::<crate::browser::BrowserItem>()
                        .is_some();
                    if let Some(item) = row
                        .response
                        .dnd_release_payload::<crate::browser::BrowserItem>()
                    {
                        dropped_on_row = true;
                        match &*item {
                            crate::browser::BrowserItem::Sample(p) if ch.instrument().is_none() => {
                                out.edits.push((
                                    Edit::SetChannelSample(ch.id, Some(sample_ref(p))),
                                    false,
                                ));
                            }
                            crate::browser::BrowserItem::Sample(p) => {
                                out.new_channels.push((stem(p), Some(sample_ref(p))));
                            }
                            crate::browser::BrowserItem::Instrument(d) => {
                                out.new_instruments.push(d.clone())
                            }
                        }
                    }
                    if (hovering_files || dragging_in) && pointer.is_some_and(|p| rect.contains(p))
                    {
                        ui.painter().rect_stroke(
                            rect.expand(2.0),
                            CornerRadius::same(6),
                            Stroke::new(2.0, theme::BLUE),
                            StrokeKind::Outside,
                        );
                    }
                    if ch.instrument().is_none()
                        && let (Some(p), Some(file)) = (pointer, dropped.first())
                        && rect.contains(p)
                    {
                        out.edits
                            .push((Edit::SetChannelSample(ch.id, Some(sample_ref(file))), false));
                        dropped_on_row = true;
                    }
                }
                ui.add_space(6.0);
                ui.label(
                RichText::new(
                    "Drop WAV files here to add channels, or onto a channel to replace its sample",
                )
                .size(11.0)
                .color(theme::MUTED),
            );
            });

        // Browser items dropped on empty rack space become new channels.
        // A rect test, not a widget: an overlay would swallow the scroll.
        let released = ui.input(|i| i.pointer.any_released());
        if !dropped_on_row
            && released
            && ui.rect_contains_pointer(ui.min_rect())
            && let Some(item) =
                egui::DragAndDrop::take_payload::<crate::browser::BrowserItem>(ui.ctx())
        {
            match &*item {
                crate::browser::BrowserItem::Sample(p) => {
                    out.new_channels.push((stem(p), Some(sample_ref(p))));
                }
                crate::browser::BrowserItem::Instrument(d) => out.new_instruments.push(d.clone()),
            }
        }

        // Commit a finished paint drag as one undo step.
        if !primary_down && let Some(paint) = self.paint.take() {
            let edits: Vec<Edit> = paint
                .cells
                .into_iter()
                .map(|((channel, index), step)| Edit::SetStep {
                    pattern: paint.pattern,
                    channel,
                    index,
                    step,
                })
                .collect();
            if !edits.is_empty() {
                out.edits.push((Edit::Batch(edits), false));
            }
        }

        // System drops count only over the rack (the playlist takes its own).
        let over_rack = ui
            .ctx()
            .pointer_latest_pos()
            .is_some_and(|p| ui.max_rect().contains(p));
        if !dropped_on_row && over_rack {
            for file in dropped {
                let name = file
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Sample".into());
                out.new_channels.push((name, Some(sample_ref(&file))));
            }
        }
        out
    }

    fn header(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        pattern: &Pattern,
        host: &dyn Host,
        out: &mut RackOutput,
    ) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Channel Rack").font(theme::strong(14.0)));
            ui.label(RichText::new(&pattern.name).color(theme::MUTED));
            let bars = f64::from(pattern.length_steps) / 16.0;
            ui.label(
                RichText::new(format!("{} steps · {bars} bar(s)", pattern.length_steps))
                    .color(theme::MUTED),
            );
            if !self.selection.is_empty() {
                ui.label(
                    RichText::new(format!("{} selected", self.selection.len())).color(theme::BLUE),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("+ Channel", |ui| {
                    if ui.button("Sampler channel").clicked() {
                        out.new_channels
                            .push((format!("Channel {}", project.channels.len() + 1), None));
                        ui.close();
                    }
                    if ui.button("Sampler from a sample…").clicked() {
                        out.pick = Some(crate::library::PickFor::NewChannel);
                        ui.close();
                    }
                    ui.menu_button("Instrument (VST3)", |ui| {
                        let list = host.plugin_instruments();
                        if list.is_empty() {
                            ui.label("No VST3 instruments found. See Options → Plugins.");
                        }
                        for d in list {
                            if ui
                                .button(format!("{} ({})", d.plugin.name, d.plugin.vendor))
                                .clicked()
                            {
                                out.new_instruments.push(d);
                                ui.close();
                            }
                        }
                    });
                });
            });
        });
    }

    fn track_box(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        id: ChannelId,
        track: jdance_domain::TrackId,
        out: &mut RackOutput,
    ) {
        let number = track_number(project, track);
        let name = project
            .mixer
            .track(track)
            .map_or_else(|| "?".to_owned(), |t| t.name.clone());
        egui::ComboBox::from_id_salt(("route", id.0))
            .width(40.0)
            .selected_text(RichText::new(format!("{number:<2}")).font(theme::mono(12.0)))
            .show_ui(ui, |ui| {
                for t in project.mixer.all_tracks() {
                    let label = format!("{} · {}", track_number(project, t.id), t.name);
                    if ui.selectable_label(t.id == track, label).clicked() && t.id != track {
                        out.edits.push((Edit::SetChannelTrack(id, t.id), false));
                    }
                }
            })
            .response
            .on_hover_text(format!("Mixer track: {name}"));
    }

    #[allow(clippy::too_many_arguments)]
    fn name_button(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        index: usize,
        color: egui::Color32,
        host: &dyn Host,
        dir: Option<&Path>,
        out: &mut RackOutput,
    ) {
        let ch = &project.channels[index];
        let (kind, status) = match ch.instrument() {
            Some(_) => ("VST3", None),
            None => ("Sampler", ch.sample().map(|s| host.sample_status(s, dir))),
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(NAME_W, STEP_H), Sense::click());
        let painter = ui.painter();
        let fill = if resp.hovered() {
            egui::Color32::from_rgb(0x2E, 0x34, 0x3B)
        } else {
            theme::RAISED
        };
        painter.rect_filled(rect, CornerRadius::same(5), fill);
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(4.0, rect.height())),
            CornerRadius {
                nw: 5,
                sw: 5,
                ne: 0,
                se: 0,
            },
            color,
        );
        let galley_rect = rect.shrink2(vec2(10.0, 0.0));
        painter.text(
            pos2(galley_rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            truncate(&ch.name, 15),
            egui::FontId::proportional(13.0),
            theme::TEXT,
        );
        let kind_color = match status {
            Some(Some(SampleStatus::Failed(_))) => theme::ERROR,
            Some(None) => theme::WARN,
            Some(Some(SampleStatus::Loading)) => theme::MUTED,
            _ => theme::MUTED,
        };
        let kind_text = match status {
            Some(None) if ch.sample().is_none() => "Empty",
            Some(Some(SampleStatus::Failed(_))) => "Missing",
            _ => kind,
        };
        painter.text(
            pos2(rect.right() - 6.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            kind_text,
            egui::FontId::proportional(10.5),
            kind_color,
        );
        let hover = match (ch.instrument(), ch.sample(), &status) {
            (Some(slot), _, _) => format!(
                "{} ({})\nDouble-click: open its editor",
                slot.name, slot.vendor
            ),
            (None, None, _) => "No sample — drop a WAV file here or double-click".to_owned(),
            (
                None,
                Some(s),
                Some(Some(SampleStatus::Ready {
                    frames,
                    source_rate,
                })),
            ) => {
                format!(
                    "{}\n{frames} frames, {source_rate} Hz\nDouble-click: sample settings",
                    s.path
                )
            }
            (None, Some(s), Some(Some(SampleStatus::Failed(e)))) => {
                format!("{}\nFailed: {e}", s.path)
            }
            (None, Some(s), _) => format!("{}\nLoading…", s.path),
        };
        let resp = resp.on_hover_text(hover);
        if resp.clicked() {
            self.click(project, ch.id, ui.input(|i| i.modifiers));
        }
        if resp.double_clicked() {
            if ch.instrument().is_some() {
                out.toggle_editor = Some(ch.id);
            } else {
                out.open_settings = Some((ch.id, true));
            }
        }
        if resp.secondary_clicked() && !self.selection.contains(&ch.id) {
            self.selection.clear();
            self.selection.insert(ch.id);
            self.anchor = Some(ch.id);
        }
        let selection: Vec<ChannelId> = if self.selection.contains(&ch.id) {
            project
                .channels
                .iter()
                .map(|c| c.id)
                .filter(|id| self.selection.contains(id))
                .collect()
        } else {
            vec![ch.id]
        };
        resp.context_menu(|ui| {
            if ui.button("Open Piano Roll").clicked() {
                out.open_piano = Some(ch.id);
                ui.close();
            }
            if ui.button("Channel settings…").clicked() {
                out.open_settings = Some((ch.id, false));
                ui.close();
            }
            if ch.instrument().is_some() {
                let open = host.plugin_editor_open(PluginTarget::Channel(ch.id));
                if ui
                    .button(if open {
                        "Close instrument editor"
                    } else {
                        "Open instrument editor"
                    })
                    .clicked()
                {
                    out.toggle_editor = Some(ch.id);
                    ui.close();
                }
                if ui.button("Instrument parameters").clicked() {
                    out.show_params = Some(ch.id);
                    ui.close();
                }
            } else {
                if ui.button("Load sample…").clicked() {
                    out.pick = Some(crate::library::PickFor::Channel(ch.id));
                    ui.close();
                }
                if ui
                    .add_enabled(ch.sample().is_some(), egui::Button::new("Clear sample"))
                    .clicked()
                {
                    out.edits.push((Edit::SetChannelSample(ch.id, None), false));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Rename / color…").clicked() {
                out.rename = Some(crate::RenameTarget::Channel(ch.id));
                ui.close();
            }
            if ui.button(format!("Clone ({})", selection.len())).clicked() {
                out.clone.extend(selection.iter().copied());
                ui.close();
            }
            let n = project.channels.len();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(index > 0, egui::Button::new("Move up"))
                    .clicked()
                {
                    out.edits.push((Edit::MoveChannel(ch.id, index - 1), false));
                }
                if ui
                    .add_enabled(index + 1 < n, egui::Button::new("Move down"))
                    .clicked()
                {
                    out.edits.push((Edit::MoveChannel(ch.id, index + 1), false));
                }
            });
            ui.menu_button("Route to mixer track", |ui| {
                for t in project.mixer.all_tracks() {
                    let label = format!("{} · {}", track_number(project, t.id), t.name);
                    if ui.button(label).clicked() {
                        out.edits.push((
                            Edit::Batch(
                                selection
                                    .iter()
                                    .map(|id| Edit::SetChannelTrack(*id, t.id))
                                    .collect(),
                            ),
                            false,
                        ));
                        ui.close();
                    }
                }
            });
            ui.separator();
            if ui
                .button(RichText::new(format!("Delete ({})…", selection.len())).color(theme::ERROR))
                .clicked()
            {
                out.delete = selection.clone();
                ui.close();
            }
        });
    }

    fn click(&mut self, project: &Project, id: ChannelId, mods: egui::Modifiers) {
        if mods.shift
            && let Some(anchor) = self.anchor
            && let (Some(a), Some(b)) = (project.channel_index(anchor), project.channel_index(id))
        {
            let (lo, hi) = (a.min(b), a.max(b));
            self.selection = project.channels[lo..=hi].iter().map(|c| c.id).collect();
            return;
        }
        if mods.command {
            if !self.selection.remove(&id) {
                self.selection.insert(id);
            }
        } else if self.selection.len() == 1 && self.selection.contains(&id) {
            self.selection.clear();
        } else {
            self.selection.clear();
            self.selection.insert(id);
        }
        self.anchor = Some(id);
    }

    #[allow(clippy::too_many_arguments)]
    fn steps(
        &mut self,
        ui: &mut egui::Ui,
        pattern: &Pattern,
        channel: ChannelId,
        current_step: Option<u32>,
        pointer: Option<egui::Pos2>,
        primary_down: bool,
        out: &mut RackOutput,
    ) {
        ui.spacing_mut().item_spacing.x = 3.0;
        for i in 0..pattern.length_steps {
            let stored = pattern.step(channel, i).unwrap_or_default();
            let step = self
                .paint
                .as_ref()
                .filter(|p| p.pattern == pattern.id)
                .and_then(|p| p.cells.get(&(channel, i)).copied())
                .unwrap_or(stored);
            let (rect, resp) =
                ui.allocate_exact_size(vec2(STEP_W, STEP_H), Sense::click_and_drag());
            if resp.drag_started() || resp.is_pointer_button_down_on() && self.paint.is_none() {
                if ui.input(|i| i.pointer.primary_down()) {
                    let on = !stored.on;
                    let mut cells = BTreeMap::new();
                    cells.insert((channel, i), Step { on, ..stored });
                    self.paint = Some(Paint {
                        pattern: pattern.id,
                        on,
                        cells,
                    });
                }
            } else if primary_down
                && let Some(paint) = &mut self.paint
                && paint.pattern == pattern.id
                && pointer.is_some_and(|p| rect.contains(p))
            {
                paint.cells.entry((channel, i)).or_insert(Step {
                    on: paint.on,
                    ..stored
                });
            }
            if resp.hovered() && stored.on {
                let scroll: f32 = ui.input(|i| {
                    i.raw
                        .events
                        .iter()
                        .filter_map(|e| match e {
                            egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                            _ => None,
                        })
                        .sum()
                });
                if scroll != 0.0 {
                    let v = (stored.velocity + WHEEL_VELOCITY * scroll.signum()).clamp(0.05, 1.0);
                    if v != stored.velocity {
                        out.edits.push((
                            Edit::SetStep {
                                pattern: pattern.id,
                                channel,
                                index: i,
                                step: Step {
                                    velocity: v,
                                    ..stored
                                },
                            },
                            false,
                        ));
                    }
                }
            }
            if ui.is_rect_visible(rect) {
                draw_step(ui, rect, step, i, current_step == Some(i), resp.hovered());
            }
            resp.on_hover_text(format!(
                "Step {} · velocity {:.0}%",
                i + 1,
                step.velocity * 100.0
            ));
        }
    }
}

fn draw_step(ui: &egui::Ui, rect: Rect, step: Step, index: u32, playhead: bool, hovered: bool) {
    let painter = ui.painter();
    let alt = (index / 4) % 2 == 1;
    let base = if alt {
        egui::Color32::from_rgb(0x22, 0x27, 0x2D)
    } else {
        egui::Color32::from_rgb(0x2B, 0x31, 0x38)
    };
    let r = CornerRadius::same(4);
    if step.on {
        painter.rect_filled(rect, r, theme::AMBER);
        let dark = rect.height() * (1.0 - step.velocity);
        if dark > 0.5 {
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(rect.width(), dark)),
                CornerRadius {
                    nw: 4,
                    ne: 4,
                    sw: 0,
                    se: 0,
                },
                egui::Color32::from_rgba_unmultiplied(14, 16, 19, 110),
            );
        }
    } else {
        painter.rect_filled(rect, r, base);
    }
    let stroke = if playhead {
        Stroke::new(2.0, theme::TEXT)
    } else if hovered {
        Stroke::new(1.0, theme::MUTED)
    } else {
        Stroke::new(1.0, egui::Color32::from_rgb(0x36, 0x3D, 0x45))
    };
    painter.rect_stroke(rect, r, stroke, StrokeKind::Inside);
}

/// Miniature of the channel's notes in the pattern; click opens the
/// Piano Roll.
fn notes_preview(
    ui: &mut egui::Ui,
    pattern: &Pattern,
    channel: ChannelId,
    color: egui::Color32,
    out: &mut RackOutput,
) {
    let width = pattern.length_steps as f32 * (STEP_W + 3.0) - 3.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, STEP_H), Sense::click());
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(4),
        egui::Color32::from_rgb(0x14, 0x17, 0x1B),
        Stroke::new(1.0, theme::LINE),
        StrokeKind::Inside,
    );
    let notes = pattern.row(channel).map_or(&[][..], |r| &r.notes[..]);
    let len = pattern.length_ticks().max(1) as f32;
    let (lo, hi) = notes
        .iter()
        .fold((u8::MAX, 0u8), |(a, b), n| (a.min(n.key), b.max(n.key)));
    let span = f32::from(hi.saturating_sub(lo).max(1));
    for n in notes {
        let x0 = rect.left() + n.start as f32 / len * rect.width();
        let x1 = rect.left()
            + u64::from(n.end()).min(pattern.length_ticks()) as f32 / len * rect.width();
        let y = rect.bottom() - 6.0 - f32::from(n.key - lo) / span * (rect.height() - 12.0);
        painter.rect_filled(
            Rect::from_min_max(pos2(x0, y - 2.0), pos2(x1.max(x0 + 2.0), y + 2.0)),
            CornerRadius::same(1),
            color,
        );
    }
    painter.text(
        pos2(rect.right() - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("piano roll · {} notes", notes.len()),
        egui::FontId::proportional(10.5),
        theme::MUTED,
    );
    if resp.on_hover_text("Click to open the Piano Roll").clicked() {
        out.open_piano = Some(channel);
    }
}

/// `M` for the master, else the 1-based track number.
pub(crate) fn track_number(project: &Project, id: jdance_domain::TrackId) -> String {
    if id == jdance_domain::TrackId::MASTER {
        return "M".into();
    }
    project
        .mixer
        .tracks
        .iter()
        .position(|t| t.id == id)
        .map_or_else(|| "?".into(), |i| format!("{:02}", i + 1))
}

fn pan_text(pan: f32) -> String {
    let v = (pan * 100.0).round() as i32;
    match v {
        0 => "C".into(),
        v if v < 0 => format!("L{}", -v),
        v => format!("R{v}"),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map_or_else(|| "Sample".into(), |s| s.to_string_lossy().into_owned())
}

fn sample_ref(path: &Path) -> SampleRef {
    SampleRef {
        path: path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(pan_text(0.0), "C");
        assert_eq!(pan_text(-0.25), "L25");
        assert_eq!(pan_text(1.0), "R100");
        assert_eq!(truncate("Kick", 15), "Kick");
        assert_eq!(truncate("A very long channel name", 8), "A very …");
        let mut p = Project::new();
        p.mixer.tracks.push(jdance_domain::MixerTrack::new(
            jdance_domain::TrackId(7),
            "Drums",
        ));
        assert_eq!(track_number(&p, jdance_domain::TrackId::MASTER), "M");
        assert_eq!(track_number(&p, jdance_domain::TrackId(7)), "01");
    }

    #[test]
    fn selection_clicks() {
        let mut p = Project::new();
        let ids: Vec<ChannelId> = (0..4)
            .map(|i| {
                let id = ChannelId(p.ids.next());
                p.channels
                    .push(jdance_domain::Channel::new_sampler(id, format!("c{i}")));
                id
            })
            .collect();
        let mut v = RackView::default();
        let none = egui::Modifiers::NONE;
        v.click(&p, ids[1], none);
        assert_eq!(v.selection, BTreeSet::from([ids[1]]));
        v.click(&p, ids[3], egui::Modifiers::SHIFT);
        assert_eq!(v.selection, BTreeSet::from([ids[1], ids[2], ids[3]]));
        v.click(&p, ids[2], egui::Modifiers::COMMAND);
        assert_eq!(v.selection, BTreeSet::from([ids[1], ids[3]]));
        v.click(&p, ids[0], none);
        assert_eq!(v.selection, BTreeSet::from([ids[0]]));
        v.click(&p, ids[0], none);
        assert!(
            v.selection.is_empty(),
            "clicking the only selected one deselects"
        );
    }
}
