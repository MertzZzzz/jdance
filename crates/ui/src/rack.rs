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
//! Audio channels (spec 0.2.2 ACH-030…036) show the waveform of their file
//! and the number of Playlist clips instead of steps: a click selects the
//! clips, a double-click opens the Sample Editor, dragging the strip onto
//! the Playlist places a new instance. The filter in the header shows all
//! channels, instruments only or audio only.
//!
//! Drawing collects edits into [`RackOutput`]; the caller applies them after
//! the frame so the document is never borrowed mutably while drawn.

use crate::widgets::{knob, lamp};
use crate::{Host, SampleStatus, theme};
use egui::{CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use jdance_commands::Edit;
use jdance_domain::{
    ChannelId, MAX_VOLUME, Pattern, PatternId, PluginTarget, Project, SampleRef, Step,
    TICKS_PER_STEP, color_or_default,
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
    /// Open the plugin picker for instruments.
    pub pick_instrument: bool,
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
    /// Select the Playlist clips of this audio channel (ACH-030).
    pub select_clips: Option<ChannelId>,
    /// Open the Sample Editor on the first clip of this audio channel.
    pub open_clip_editor: Option<ChannelId>,
    /// The audio channel's file into a new sampler channel.
    pub to_sampler: Option<ChannelId>,
    /// «Remove unused audio channels» (ACH-034).
    pub remove_unused: bool,
}

/// Which channels the rack lists (ACH-032); a view setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RackFilter {
    #[default]
    All,
    Instruments,
    Audio,
}

impl RackFilter {
    fn admits(self, ch: &jdance_domain::Channel) -> bool {
        match self {
            Self::All => true,
            Self::Instruments => !ch.is_audio(),
            Self::Audio => ch.is_audio(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Instruments => "Instruments",
            Self::Audio => "Audio",
        }
    }
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
    pub filter: RackFilter,
    /// A channel to scroll to and highlight until the given time (ACH-018).
    reveal: Option<(ChannelId, f64)>,
    scroll_pending: bool,
}

impl RackView {
    pub(crate) fn retain_existing(&mut self, project: &Project) {
        self.selection.retain(|id| project.channel(*id).is_some());
    }

    /// Scrolls to channel `id` and highlights it for a second; returns
    /// `false` when the filter hides it.
    pub(crate) fn reveal(&mut self, project: &Project, id: ChannelId, now: f64) -> bool {
        self.reveal = Some((id, now + 1.0));
        self.scroll_pending = true;
        project.channel(id).is_some_and(|c| self.filter.admits(c))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        pattern: PatternId,
        host: &mut dyn Host,
        dir: Option<&Path>,
        marks: PatternMarks,
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
        // The step playhead and the caret of this pattern, in Pattern and
        // in Song mode (spec 0.2.4 PCR-004, PCR-013).
        let step_of =
            |t: u64| ((t / u64::from(TICKS_PER_STEP)) as u32).min(pattern.length_steps - 1);
        let current_step = marks.playhead.map(step_of);
        let caret_step = marks.caret.map(step_of);

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

        self.header(ui, project, pattern, &mut out);
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
                let now = ui.input(|i| i.time);
                if self.reveal.is_some_and(|(_, until)| now > until) {
                    self.reveal = None;
                }
                for (index, ch) in project.channels.iter().enumerate() {
                    if !self.filter.admits(ch) {
                        continue;
                    }
                    let selected = self.selection.contains(&ch.id);
                    let revealed = self.reveal.is_some_and(|(id, _)| id == ch.id);
                    let color = theme::rgb(color_or_default(ch.color, index));
                    let frame = egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .corner_radius(6)
                        .fill(if revealed {
                            theme::BLUE.gamma_multiply(0.35)
                        } else if selected {
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
                            // Audio channels start at unity (ACH-001).
                            let default = if ch.is_audio() { 1.0 } else { 0.8 };
                            let r =
                                knob(ui, &mut vol, 0.0..=MAX_VOLUME, default, theme::AMBER, 30.0)
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
                            if ch.is_audio() {
                                audio_strip(ui, project, ch, color, host, dir, &mut out);
                            } else if notes {
                                notes_preview(ui, pattern, ch.id, color, &mut out);
                            } else {
                                self.steps(
                                    ui,
                                    pattern,
                                    ch.id,
                                    (current_step, caret_step),
                                    pointer,
                                    primary_down,
                                    &mut out,
                                );
                            }
                        });
                    });
                    let rect = row.response.rect;
                    if revealed && std::mem::take(&mut self.scroll_pending) {
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                    }
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
                            // An audio channel keeps its file: a new sampler.
                            // A sampler with clips keeps its file too.
                            crate::browser::BrowserItem::Sample(p)
                                if ch.instrument().is_none()
                                    && !ch.is_audio()
                                    && project.channel_clips(ch.id).next().is_none() =>
                            {
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
                            crate::browser::BrowserItem::Midi(_) => {}
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
                        && !ch.is_audio()
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
                    "Drop WAV or MP3 files here to add channels, or onto a channel to replace its sample",
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
                crate::browser::BrowserItem::Midi(_) => {}
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
        out: &mut RackOutput,
    ) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Channel Rack").font(theme::strong(14.0)));
            ui.label(RichText::new(&pattern.name).color(theme::MUTED));
            let per_bar = project.time_signature.steps_per_bar();
            let bars = if pattern.length_steps.is_multiple_of(per_bar) {
                format!("{}", pattern.length_steps / per_bar)
            } else {
                format!(
                    "{:.2}",
                    f64::from(pattern.length_steps) / f64::from(per_bar)
                )
            };
            ui.label(
                RichText::new(format!("{} steps · {bars} bar(s)", pattern.length_steps))
                    .color(theme::MUTED),
            );
            if !self.selection.is_empty() {
                ui.label(
                    RichText::new(format!("{} selected", self.selection.len())).color(theme::BLUE),
                );
            }
            egui::ComboBox::from_id_salt("rack_filter")
                .width(96.0)
                .selected_text(self.filter.name())
                .show_ui(ui, |ui| {
                    for f in [RackFilter::All, RackFilter::Instruments, RackFilter::Audio] {
                        ui.selectable_value(&mut self.filter, f, f.name());
                    }
                })
                .response
                .on_hover_text("Show all channels, instruments only or audio channels only");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let unused = project
                    .channels
                    .iter()
                    .filter(|c| c.is_audio() && project.channel_clips(c.id).next().is_none())
                    .count();
                if ui
                    .add_enabled(unused > 0, egui::Button::new("Remove unused audio"))
                    .on_hover_text(format!(
                        "Remove the {unused} audio channel(s) without clips in the Playlist; \
                         the files stay in the project"
                    ))
                    .clicked()
                {
                    out.remove_unused = true;
                }
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
                    if ui.button("Instrument (VST3)…").clicked() {
                        out.pick_instrument = true;
                        ui.close();
                    }
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
        let (kind, status) = match (ch.instrument(), ch.audio_asset()) {
            (Some(_), _) => ("VST3", None),
            (None, Some(_)) => ("Audio", None),
            (None, None) => ("Sampler", ch.sample().map(|s| host.sample_status(s, dir))),
        };
        // A sampler with a sample drags onto the Playlist (spec 0.2.9 §1.1).
        let placeable = ch.clip_asset_id().is_some() || ch.sample().is_some();
        let sense = if placeable {
            Sense::click_and_drag()
        } else {
            Sense::click()
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(NAME_W, STEP_H), sense);
        if placeable {
            resp.dnd_set_drag_payload(crate::project_browser::ProjectItem(
                crate::playlist::Source::Channel(ch.id),
            ));
        }
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
        // The kind on the right, the name in the room left of it.
        let kind_rect = crate::widgets::fit_text(
            painter,
            pos2(rect.right() - 6.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            kind_text,
            egui::FontId::proportional(10.5),
            kind_color,
            rect.width() / 2.0,
        );
        let left = rect.left() + 10.0;
        crate::widgets::fit_text(
            painter,
            pos2(left, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &ch.name,
            egui::FontId::proportional(13.0),
            theme::TEXT,
            kind_rect.left() - 6.0 - left,
        );
        let audio_file = ch.audio_asset().and_then(|a| project.asset(a));
        let hover = match (ch.instrument(), ch.sample(), &status) {
            _ if ch.is_audio() => match audio_file {
                Some(a) => format!(
                    "Audio channel: {}\n{:.2} s · {} Hz · {} clip(s) in the Playlist\nClick: settings · drag the waveform onto the Playlist",
                    a.name,
                    a.seconds(),
                    a.sample_rate,
                    project.channel_clips(ch.id).count()
                ),
                None => "Audio channel: the file is missing".to_owned(),
            },
            (Some(slot), _, _) => format!(
                "{} ({})\nDouble-click: open its editor",
                slot.name, slot.vendor
            ),
            (None, None, _) => "No sample — drop a WAV or MP3 file here or double-click".to_owned(),
            (
                None,
                Some(s),
                Some(Some(SampleStatus::Ready {
                    frames,
                    source_rate,
                    ..
                })),
            ) => {
                format!(
                    "{}\n{frames} frames, {source_rate} Hz{}\nDouble-click: sample settings · drag onto the Playlist: a clip of this sampler",
                    s.path,
                    match project.channel_clips(ch.id).count() {
                        0 => String::new(),
                        n => format!(" · {n} clip(s) in the Playlist"),
                    }
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
            if ch.is_audio() {
                out.open_settings = Some((ch.id, false));
            } else if ch.instrument().is_some() {
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
            if ch.is_audio() {
                if ui.button("Select clips in the Playlist").clicked() {
                    out.select_clips = Some(ch.id);
                    ui.close();
                }
                if ui.button("Sample Editor").clicked() {
                    out.open_clip_editor = Some(ch.id);
                    ui.close();
                }
                if ui
                    .button("Send to sampler")
                    .on_hover_text("A new sampler channel with this file; the clips stay")
                    .clicked()
                {
                    out.to_sampler = Some(ch.id);
                    ui.close();
                }
            } else if ui.button("Open Piano Roll").clicked() {
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
            } else if !ch.is_audio() {
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
            let clone = if ch.is_audio() {
                format!("Make unique — same file, no clips ({})", selection.len())
            } else {
                format!("Clone ({})", selection.len())
            };
            if ui.button(clone).clicked() {
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
        (current_step, caret_step): (Option<u32>, Option<u32>),
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
                if caret_step == Some(i) {
                    ui.painter().line_segment(
                        [
                            rect.left_top() + vec2(2.0, -2.0),
                            rect.right_top() + vec2(-2.0, -2.0),
                        ],
                        Stroke::new(2.0, theme::AMBER),
                    );
                }
            }
            resp.on_hover_text(format!(
                "Step {} · velocity {:.0}%",
                i + 1,
                step.velocity * 100.0
            ));
        }
    }
}

/// Playhead and caret of the shown pattern as pattern positions in ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PatternMarks {
    pub playhead: Option<u64>,
    pub caret: Option<u64>,
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

fn stem(path: &Path) -> String {
    path.file_stem()
        .map_or_else(|| "Sample".into(), |s| s.to_string_lossy().into_owned())
}

fn sample_ref(path: &Path) -> SampleRef {
    SampleRef {
        path: path.to_string_lossy().into_owned(),
    }
}

/// The step area of an audio channel (ACH-030): the file's waveform and
/// the number of Playlist clips. Click selects the clips, double-click
/// opens the Sample Editor, dragging places an instance on the Playlist.
fn audio_strip(
    ui: &mut egui::Ui,
    project: &Project,
    ch: &jdance_domain::Channel,
    color: egui::Color32,
    host: &dyn Host,
    dir: Option<&Path>,
    out: &mut RackOutput,
) {
    let width = (16.0 * (STEP_W + ui.spacing().item_spacing.x)).max(160.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, STEP_H), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(4), theme::RAISED);
    let asset = ch.audio_asset().and_then(|a| project.asset(a));
    let peaks = asset.and_then(|a| {
        host.sample_peaks(
            &SampleRef {
                path: a.path.clone(),
            },
            dir,
        )
    });
    let body = rect.shrink2(vec2(4.0, 3.0));
    match peaks.as_deref().filter(|p| !p.is_empty()) {
        Some(peaks) => {
            let mid = body.center().y;
            let half = body.height() / 2.0;
            let ink = color.gamma_multiply(0.85);
            let mut x = body.left().floor();
            while x < body.right() {
                let t0 = (x - body.left()) / body.width();
                let t1 = (x + 1.0 - body.left()) / body.width();
                let b0 = (t0 * peaks.len() as f32) as usize;
                let b1 = ((t1 * peaks.len() as f32).ceil() as usize).max(b0 + 1);
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
        None => {
            painter.text(
                body.left_center(),
                egui::Align2::LEFT_CENTER,
                if asset.is_some() {
                    "loading…"
                } else {
                    "file missing"
                },
                egui::FontId::proportional(10.5),
                theme::MUTED,
            );
        }
    }
    let clips = project.channel_clips(ch.id).count();
    let label = format!("{clips} clip{}", if clips == 1 { "" } else { "s" });
    let galley = painter.layout_no_wrap(label, egui::FontId::proportional(10.5), theme::TEXT);
    let at = pos2(
        rect.right() - galley.size().x - 8.0,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.rect_filled(
        Rect::from_min_size(at - vec2(4.0, 1.0), galley.size() + vec2(8.0, 2.0)),
        CornerRadius::same(3),
        theme::BG.gamma_multiply(0.85),
    );
    painter.galley(at, galley, theme::TEXT);
    let resp = resp.on_hover_text(
        "Click: select its clips in the Playlist · double-click: Sample Editor · \
         drag onto the Playlist: a new instance",
    );
    resp.dnd_set_drag_payload(crate::project_browser::ProjectItem(
        crate::playlist::Source::Channel(ch.id),
    ));
    if resp.double_clicked() {
        out.open_clip_editor = Some(ch.id);
    } else if resp.clicked() {
        out.select_clips = Some(ch.id);
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
