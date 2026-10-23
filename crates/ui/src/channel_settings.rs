// SPDX-License-Identifier: GPL-3.0-or-later
//! Channel Settings window (spec "DAW UI clean room", §11–§13).
//!
//! Tabs: Channel (host settings: name, color, level, pan, pitch, mixer
//! track, polyphony, mono), then for samplers Sample (file, waveform with
//! start / end / loop markers, root key, gain, playback mode) and Envelope
//! (ADSR), for plugin instruments Instrument (generic parameters and the
//! native editor). Audio channels (spec 0.2.2 ACH-031) have only the
//! Channel tab: level, pan, mixer track, the file and their clips.
//! Changing the mixer track only reroutes audio.

use crate::automation::AutomationRequest;
use crate::{Host, SampleStatus, theme};
use egui::{CornerRadius, Rect, RichText, Sense, Stroke, pos2, vec2};
use jdance_commands::Edit;
use jdance_domain::{
    Channel, ChannelId, MAX_CHANNEL_PITCH, MAX_ENVELOPE_MS, MAX_POLYPHONY, MAX_VOLUME, PALETTE,
    PlaybackMode, PluginTarget, Project, SamplerSettings, color_or_default,
};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Channel,
    Sample,
    Envelope,
    Instrument,
    /// The sampler's Instrument tab (spec 0.2.5 §4): levels, cut groups,
    /// root note.
    Keys,
}

/// Which marker a waveform drag moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Start,
    End,
    LoopStart,
    LoopEnd,
}

#[derive(Debug)]
pub(crate) struct ChannelSettingsWindow {
    pub channel: ChannelId,
    tab: Tab,
    /// Name being typed; committed on Enter or focus loss.
    name: Option<String>,
    dragging: Option<Marker>,
}

#[derive(Debug, Default)]
pub(crate) struct SettingsOutput {
    pub edits: Vec<(Edit, bool)>,
    pub end_gesture: bool,
    pub toggle_editor: bool,
    pub automation: Vec<AutomationRequest>,
    /// Open the Sample Selection Dialog for this channel.
    pub pick: Option<crate::library::PickFor>,
    /// Audio channel: select its Playlist clips / open the Sample Editor.
    pub select_clips: bool,
    pub open_clip_editor: bool,
}

use crate::piano::key_name;

impl ChannelSettingsWindow {
    pub(crate) fn new(channel: ChannelId) -> Self {
        Self {
            channel,
            tab: Tab::Channel,
            name: None,
            dragging: None,
        }
    }

    /// Opens on the sample tab (double-click on a sampler).
    pub(crate) fn sample(channel: ChannelId) -> Self {
        Self {
            tab: Tab::Sample,
            ..Self::new(channel)
        }
    }

    /// Opens on the instrument's parameters.
    pub(crate) fn instrument(channel: ChannelId) -> Self {
        Self {
            tab: Tab::Instrument,
            ..Self::new(channel)
        }
    }

    /// Draws the window; returns its output and whether it stays open.
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        host: &mut dyn Host,
        dir: Option<&Path>,
    ) -> (SettingsOutput, bool) {
        let mut out = SettingsOutput::default();
        let Some(index) = project.channel_index(self.channel) else {
            return (out, false);
        };
        let ch = &project.channels[index];
        let color = theme::rgb(color_or_default(ch.color, index));
        let instrument = ch.instrument().is_some();
        let audio = ch.is_audio();
        if instrument && matches!(self.tab, Tab::Sample | Tab::Envelope | Tab::Keys)
            || !instrument && self.tab == Tab::Instrument
            || audio
        {
            self.tab = Tab::Channel;
        }
        let mut open = true;
        egui::Window::new(
            RichText::new(format!(
                "{} · {}",
                ch.name,
                if instrument {
                    "Instrument"
                } else if audio {
                    "Audio"
                } else {
                    "Sampler"
                }
            ))
            .font(theme::strong(14.0)),
        )
        .id(egui::Id::unique(("channel_settings", self.channel.0)))
        .open(&mut open)
        .default_size([760.0, 520.0])
        .resizable(true)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                ui.painter().rect_filled(r, 3.0, color);
                let tabs: &[(Tab, &str)] = if instrument {
                    &[(Tab::Channel, "Channel"), (Tab::Instrument, "Instrument")]
                } else if audio {
                    &[(Tab::Channel, "Channel")]
                } else {
                    &[
                        (Tab::Channel, "Channel"),
                        (Tab::Sample, "Sample"),
                        (Tab::Keys, "Instrument"),
                        (Tab::Envelope, "Envelope"),
                    ]
                };
                for (tab, label) in tabs {
                    ui.selectable_value(&mut self.tab, *tab, *label);
                }
            });
            // The header stays while the sampler's tabs change (§4).
            if matches!(self.tab, Tab::Sample | Tab::Keys) {
                header(ui, project, ch, &mut out);
            }
            ui.separator();
            match self.tab {
                Tab::Channel => self.channel_tab(ui, project, index, &mut out),
                Tab::Sample => self.sample_tab(ui, ch, project.tempo_bpm, host, dir, &mut out),
                Tab::Keys => keys_tab(ui, project, ch, &mut out),
                Tab::Envelope => envelope_tab(ui, ch, &mut out),
                Tab::Instrument => instrument_tab(ui, project, ch, host, &mut out),
            }
        });
        (out, open)
    }

    fn channel_tab(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        index: usize,
        out: &mut SettingsOutput,
    ) {
        let ch = &project.channels[index];
        let id = ch.id;
        egui::Grid::new("channel_grid")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label("Name");
                let mut text = self.name.clone().unwrap_or_else(|| ch.name.clone());
                let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(240.0));
                if r.changed() {
                    self.name = Some(text.clone());
                }
                if r.lost_focus() {
                    let name = text.trim().to_owned();
                    if !name.is_empty() && name != ch.name {
                        out.edits.push((Edit::RenameChannel(id, name), false));
                    }
                    self.name = None;
                }
                ui.end_row();

                ui.label("Color");
                ui.horizontal(|ui| {
                    let current = color_or_default(ch.color, index);
                    for c in PALETTE {
                        let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                        ui.painter().rect_filled(rect, 4.0, theme::rgb(c));
                        if c == current {
                            ui.painter().rect_stroke(
                                rect.expand(2.0),
                                5.0,
                                Stroke::new(2.0, theme::TEXT),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if resp.clicked() && Some(c) != ch.color {
                            out.edits.push((Edit::SetChannelColor(id, Some(c)), false));
                        }
                    }
                });
                ui.end_row();

                ui.label("Volume");
                let mut vol = ch.volume;
                let r = ui.add(
                    egui::Slider::new(&mut vol, 0.0..=MAX_VOLUME)
                        .custom_formatter(|v, _| db_text(v as f32)),
                );
                if r.changed() {
                    out.edits.push((Edit::SetChannelVolume(id, vol), true));
                }
                out.end_gesture |= r.drag_stopped() || r.lost_focus();
                ui.end_row();

                ui.label("Pan");
                let mut pan = ch.pan;
                let r = ui.add(
                    egui::Slider::new(&mut pan, -1.0..=1.0).custom_formatter(|v, _| {
                        let v = (v * 100.0).round() as i32;
                        match v {
                            0 => "C".to_owned(),
                            v if v < 0 => format!("L{}", -v),
                            v => format!("R{v}"),
                        }
                    }),
                );
                if r.changed() {
                    out.edits.push((Edit::SetChannelPan(id, pan), true));
                }
                out.end_gesture |= r.drag_stopped() || r.lost_focus();
                ui.end_row();

                if ch.is_audio() {
                    audio_rows(ui, project, ch, out);
                    return;
                }
                let playback = |pitch: i32, polyphony: u16, mono: bool| Edit::SetChannelPlayback {
                    channel: id,
                    pitch,
                    polyphony,
                    mono,
                };
                ui.label("Pitch");
                let mut pitch = ch.pitch;
                let r = ui.add(
                    egui::DragValue::new(&mut pitch)
                        .range(-MAX_CHANNEL_PITCH..=MAX_CHANNEL_PITCH)
                        .suffix(" semitones"),
                );
                if r.changed() {
                    out.edits
                        .push((playback(pitch, ch.polyphony, ch.mono), true));
                }
                out.end_gesture |= r.drag_stopped() || r.lost_focus();
                ui.end_row();

                ui.label("Mixer track");
                egui::ComboBox::from_id_salt("settings_track")
                    .width(200.0)
                    .selected_text(track_label(project, ch.track))
                    .show_ui(ui, |ui| {
                        for t in project.mixer.all_tracks() {
                            if ui
                                .selectable_label(t.id == ch.track, track_label(project, t.id))
                                .clicked()
                                && t.id != ch.track
                            {
                                out.edits.push((Edit::SetChannelTrack(id, t.id), false));
                            }
                        }
                    });
                ui.end_row();

                ui.label("Polyphony");
                let mut poly = ch.polyphony;
                let r = ui.add(
                    egui::DragValue::new(&mut poly)
                        .range(0..=MAX_POLYPHONY)
                        .custom_formatter(|v, _| {
                            if v == 0.0 {
                                "No limit".to_owned()
                            } else {
                                format!("{v:.0} voices")
                            }
                        }),
                );
                if r.changed() {
                    out.edits.push((playback(ch.pitch, poly, ch.mono), true));
                }
                out.end_gesture |= r.drag_stopped() || r.lost_focus();
                ui.end_row();

                ui.label("Mono");
                let mut mono = ch.mono;
                if ui
                    .checkbox(&mut mono, "A new note cuts the sounding one")
                    .changed()
                {
                    out.edits
                        .push((playback(ch.pitch, ch.polyphony, mono), false));
                }
                ui.end_row();
            });
        ui.add_space(10.0);
        if ch.is_audio() {
            ui.label(
                RichText::new(
                    "Every clip of this file in the Playlist plays through this channel: level, \
                     pan and mixer track apply to all of them. Clip gain stays per clip.",
                )
                .size(12.0)
                .color(theme::MUTED),
            );
            return;
        }
        ui.label(
            RichText::new(
                "These are the host's channel settings, saved with the project. Changing the \
                 mixer track only reroutes the audio: the instrument and its preset stay.",
            )
            .size(12.0)
            .color(theme::MUTED),
        );
    }

    fn sample_tab(
        &mut self,
        ui: &mut egui::Ui,
        ch: &Channel,
        tempo: f64,
        host: &dyn Host,
        dir: Option<&Path>,
        out: &mut SettingsOutput,
    ) {
        let id = ch.id;
        let s = ch.sampler;
        ui.horizontal(|ui| {
            let path = ch.sample().map_or("No sample", |s| s.path.as_str());
            ui.label(RichText::new(path).font(theme::mono(12.0)));
            if ui
                .button("Load…")
                .on_hover_text("Choose from the library, the project or any file")
                .clicked()
            {
                out.pick = Some(crate::library::PickFor::Channel(id));
            }
        });
        let status = ch.sample().and_then(|smp| host.sample_status(smp, dir));
        let frames_rate = match &status {
            Some(SampleStatus::Ready {
                frames,
                source_rate,
                format,
            }) => {
                let secs = *frames as f64 / f64::from(*source_rate);
                ui.label(
                    RichText::new(format!(
                        "{secs:.2} s · {source_rate} Hz · {frames} frames · {}",
                        crate::browser::format_label(format)
                    ))
                    .font(theme::mono(11.0))
                    .color(theme::MUTED),
                );
                Some((*frames, *source_rate))
            }
            _ => None,
        };
        match (&status, ch.sample()) {
            (_, None) => {
                ui.label(
                    RichText::new("Load a WAV or MP3 file or drop one on the channel.")
                        .color(theme::MUTED),
                );
            }
            (Some(SampleStatus::Failed(e)), Some(smp)) => {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x21, 0x12, 0x0F))
                    .stroke(Stroke::new(1.0, egui::Color32::from_rgb(0x6B, 0x2E, 0x28)))
                    .corner_radius(8)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Missing sample").color(theme::ERROR).strong());
                        ui.label(format!(
                            "{} could not be loaded ({e}). The channel stays silent and keeps its settings.",
                            smp.path
                        ));
                    });
            }
            _ => {}
        }
        // The waveform shows the processed data once it is computed (§4).
        let peaks = ch
            .playback_sample(tempo)
            .and_then(|smp| host.sample_peaks(&smp, dir))
            .or_else(|| ch.sample().and_then(|smp| host.sample_peaks(smp, dir)));
        let mut edited = s;
        self.waveform(
            ui,
            peaks.as_deref().map(Vec::as_slice),
            &mut edited,
            frames_rate,
        );
        ui.add_space(8.0);
        ui.columns(3, |cols| {
            let ui = &mut cols[0];
            theme_label(ui, "Pitch and level");
            ui.label(format!(
                "Root note {} (Instrument tab)",
                key_name(edited.root_key)
            ));
            ui.horizontal(|ui| {
                ui.label("Gain");
                let r = ui.add(
                    egui::DragValue::new(&mut edited.gain_db)
                        .range(-24.0..=12.0)
                        .speed(0.1)
                        .suffix(" dB"),
                );
                out.end_gesture |= r.drag_stopped() || r.lost_focus();
            });
            ui.label(
                RichText::new("Other keys play faster or slower: pitch and time change together.")
                    .size(11.5)
                    .color(theme::MUTED),
            );
            let ui = &mut cols[1];
            theme_label(ui, "Playback");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut edited.mode, PlaybackMode::OneShot, "One-shot");
                ui.selectable_value(&mut edited.mode, PlaybackMode::Loop, "Loop");
            });
            ui.label(
                RichText::new(match edited.mode {
                    PlaybackMode::OneShot => {
                        "Plays from start to end; note length is ignored unless the envelope is on."
                    }
                    PlaybackMode::Loop => {
                        "Repeats the loop region while the note is held, then releases."
                    }
                })
                .size(11.5)
                .color(theme::MUTED),
            );
            let ui = &mut cols[2];
            theme_label(ui, "Markers");
            let pct = |v: f32| format!("{:.1}%", v * 100.0);
            let time = |v: f32| {
                frames_rate.map_or_else(
                    || pct(v),
                    |(f, r)| format!("{:.0} ms", f64::from(v) * f as f64 / f64::from(r) * 1000.0),
                )
            };
            ui.label(
                RichText::new(format!(
                    "Start {} · End {}",
                    time(edited.start),
                    time(edited.end)
                ))
                .font(theme::mono(11.5)),
            );
            if edited.mode == PlaybackMode::Loop {
                ui.label(
                    RichText::new(format!(
                        "Loop {} – {}",
                        time(edited.loop_start),
                        time(edited.loop_end)
                    ))
                    .font(theme::mono(11.5))
                    .color(theme::TEAL),
                );
            }
            if ui.button("Reset markers").clicked() {
                let d = SamplerSettings::default();
                edited.start = d.start;
                edited.end = d.end;
                edited.loop_start = d.loop_start;
                edited.loop_end = d.loop_end;
            }
        });
        ui.add_space(8.0);
        let mp3 = matches!(&status, Some(SampleStatus::Ready { format, .. }) if format.starts_with("MP3"));
        processing_ui(ui, &mut edited, frames_rate, tempo, mp3, out);
        if edited != s && edited.validate().is_ok() {
            let merging = self.dragging.is_some()
                || edited.gain_db != s.gain_db
                || edited.stretch != s.stretch
                || edited.precompute != s.precompute;
            out.edits.push((Edit::SetSampler(id, edited), merging));
        }
    }

    /// Waveform with draggable markers (start, end; loop in loop mode).
    fn waveform(
        &mut self,
        ui: &mut egui::Ui,
        peaks: Option<&[f32]>,
        s: &mut SamplerSettings,
        frames_rate: Option<(usize, u32)>,
    ) -> egui::Response {
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 200.0), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(
            rect,
            CornerRadius::same(8),
            egui::Color32::from_rgb(0x10, 0x12, 0x15),
        );
        let x_of = |f: f32| rect.left() + f * rect.width();
        let f_of = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        if let Some(peaks) = peaks.filter(|p| !p.is_empty()) {
            let cols = rect.width().max(1.0) as usize;
            let mid = rect.center().y;
            for c in 0..cols {
                let a = c * peaks.len() / cols;
                let b = ((c + 1) * peaks.len() / cols).max(a + 1).min(peaks.len());
                let v = peaks[a..b].iter().fold(0.0f32, |m, p| m.max(*p)).min(1.0);
                let h = (v * (rect.height() / 2.0 - 8.0)).max(0.5);
                let x = rect.left() + c as f32 + 0.5;
                painter.line_segment(
                    [pos2(x, mid - h), pos2(x, mid + h)],
                    Stroke::new(1.0, theme::BLUE.gamma_multiply(0.85)),
                );
            }
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No waveform",
                egui::FontId::proportional(12.0),
                theme::MUTED,
            );
        }
        // Outside the start / end markers is dimmed.
        let dim = egui::Color32::from_rgba_unmultiplied(14, 16, 19, 170);
        painter.rect_filled(
            Rect::from_min_max(rect.min, pos2(x_of(s.start), rect.bottom())),
            0.0,
            dim,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(x_of(s.end), rect.top()), rect.max),
            0.0,
            dim,
        );
        let looping = s.mode == PlaybackMode::Loop;
        if looping {
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x_of(s.loop_start), rect.top()),
                    pos2(x_of(s.loop_end), rect.bottom()),
                ),
                0.0,
                egui::Color32::from_rgba_unmultiplied(60, 201, 176, 26),
            );
        }
        let mut markers = vec![
            (Marker::Start, s.start, theme::AMBER),
            (Marker::End, s.end, theme::AMBER),
        ];
        if looping {
            markers.push((Marker::LoopStart, s.loop_start, theme::TEAL));
            markers.push((Marker::LoopEnd, s.loop_end, theme::TEAL));
        }
        for (m, v, c) in &markers {
            let x = x_of(*v);
            painter.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(2.0, *c),
            );
            let label = match m {
                Marker::Start => "Start",
                Marker::End => "End",
                Marker::LoopStart => "Loop",
                Marker::LoopEnd => "",
            };
            let y = if matches!(m, Marker::LoopStart | Marker::LoopEnd) {
                rect.bottom() - 14.0
            } else {
                rect.top() + 10.0
            };
            painter.text(
                pos2(x + 4.0, y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(11.0),
                *c,
            );
        }
        let _ = frames_rate;
        if resp.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            self.dragging = markers
                .iter()
                .map(|(m, v, _)| (*m, (x_of(*v) - p.x).abs()))
                .filter(|(_, d)| *d < 10.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(m, _)| m);
        }
        if let (Some(m), Some(p)) = (self.dragging, resp.interact_pointer_pos()) {
            let f = f_of(p.x);
            let gap = 0.001;
            match m {
                Marker::Start => s.start = f.min(s.end - gap),
                Marker::End => s.end = f.max(s.start + gap),
                Marker::LoopStart => s.loop_start = f.min(s.loop_end - gap),
                Marker::LoopEnd => s.loop_end = f.max(s.loop_start + gap),
            }
        }
        if resp.drag_stopped() {
            self.dragging = None;
        }
        if let Some(p) = resp.hover_pos()
            && markers
                .iter()
                .any(|(_, v, _)| (x_of(*v) - p.x).abs() < 10.0)
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        resp
    }
}

/// ON, PAN, VOL, PITCH and TRACK above the sampler's tabs (§4).
fn header(ui: &mut egui::Ui, project: &Project, ch: &Channel, out: &mut SettingsOutput) {
    let id = ch.id;
    ui.horizontal(|ui| {
        let mut on = !ch.muted;
        if ui
            .checkbox(&mut on, "ON")
            .on_hover_text("The channel plays")
            .changed()
        {
            out.edits.push((Edit::SetChannelMuted(id, !on), false));
        }
        ui.label("PAN");
        let mut pan = ch.pan;
        let r = ui.add(egui::DragValue::new(&mut pan).range(-1.0..=1.0).speed(0.01));
        if r.changed() {
            out.edits.push((Edit::SetChannelPan(id, pan), true));
        }
        out.end_gesture |= r.drag_stopped() || r.lost_focus();
        ui.label("VOL");
        let mut vol = ch.volume;
        let r = ui.add(
            egui::DragValue::new(&mut vol)
                .range(0.0..=MAX_VOLUME)
                .speed(0.01)
                .custom_formatter(|v, _| db_text(v as f32)),
        );
        if r.changed() {
            out.edits.push((Edit::SetChannelVolume(id, vol), true));
        }
        out.end_gesture |= r.drag_stopped() || r.lost_focus();
        ui.label("PITCH");
        let mut pitch = ch.pitch;
        let r = ui.add(
            egui::DragValue::new(&mut pitch)
                .range(-MAX_CHANNEL_PITCH..=MAX_CHANNEL_PITCH)
                .suffix(" st"),
        );
        if r.changed() {
            out.edits.push((
                Edit::SetChannelPlayback {
                    channel: id,
                    pitch,
                    polyphony: ch.polyphony,
                    mono: ch.mono,
                },
                true,
            ));
        }
        out.end_gesture |= r.drag_stopped() || r.lost_focus();
        ui.label("TRACK");
        egui::ComboBox::from_id_salt("sampler_header_track")
            .width(120.0)
            .selected_text(track_label(project, ch.track))
            .show_ui(ui, |ui| {
                for t in project.mixer.all_tracks() {
                    if ui
                        .selectable_label(t.id == ch.track, track_label(project, t.id))
                        .clicked()
                        && t.id != ch.track
                    {
                        out.edits.push((Edit::SetChannelTrack(id, t.id), false));
                    }
                }
            });
    });
}

/// Content, declicking, time stretching and the Tools panel of the
/// Sample tab (§4).
fn processing_ui(
    ui: &mut egui::Ui,
    e: &mut SamplerSettings,
    frames_rate: Option<(usize, u32)>,
    tempo: f64,
    mp3: bool,
    out: &mut SettingsOutput,
) {
    use jdance_domain::sample_processing::{Declick, StretchMode, TimeFit};
    let stopped = std::cell::Cell::new(false);
    let drag = |ui: &mut egui::Ui, v: &mut f32, lo: f32, hi: f32, speed: f64| {
        let r = ui.add(egui::DragValue::new(v).range(lo..=hi).speed(speed));
        stopped.set(stopped.get() | r.drag_stopped() || r.lost_focus());
        r.changed()
    };
    ui.columns(3, |cols| {
        let ui = &mut cols[0];
        theme_label(ui, "Content");
        ui.add_enabled(false, egui::Checkbox::new(&mut true, "Resample"))
            .on_disabled_hover_text("Always on: the sample is converted to the device rate.");
        ui.add_enabled_ui(!mp3, |ui| {
            ui.checkbox(&mut e.keep_on_disk, "Keep on disk")
                .on_hover_text(
                    "Stored with the project; the sample still loads into memory in this \
                     version. Turns the precomputed effects off.",
                )
                .on_disabled_hover_text("Not for MP3 files");
        });
        ui.add_space(6.0);
        theme_label(ui, "Declicking");
        egui::ComboBox::from_id_salt("declick")
            .width(200.0)
            .selected_text(e.declick.label())
            .show_ui(ui, |ui| {
                for d in Declick::ALL {
                    ui.selectable_value(&mut e.declick, d, d.label());
                }
            });

        let ui = &mut cols[1];
        theme_label(ui, "Time stretching");
        egui::Grid::new("stretch_grid")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("PITCH");
                let before = e.stretch.pitch;
                drag(ui, &mut e.stretch.pitch, -24.0, 24.0, 0.05);
                // Turning PITCH in resampling switches to Auto (§4).
                if e.stretch.pitch != before && e.stretch.mode == StretchMode::Resample {
                    e.stretch.mode = StretchMode::Auto;
                }
                ui.end_row();
                ui.label("MUL");
                drag(ui, &mut e.stretch.mul, 0.25, 4.0, 0.01);
                ui.end_row();
                ui.label("TIME");
                ui.horizontal(|ui| {
                    let mut fit = matches!(e.stretch.time, TimeFit::Beats { .. });
                    if ui
                        .checkbox(&mut fit, "Project tempo")
                        .on_hover_text("The length follows the project tempo")
                        .changed()
                    {
                        e.stretch.time = if fit {
                            // The sample's length in beats now, at least one.
                            let beats = frames_rate.map_or(4.0, |(f, r)| {
                                (f as f64 / f64::from(r.max(1)) * tempo / 60.0)
                                    .round()
                                    .max(1.0)
                            });
                            TimeFit::Beats {
                                beats: (beats as f32).min(1024.0),
                            }
                        } else {
                            TimeFit::None
                        };
                    }
                    if let TimeFit::Beats { beats } = &mut e.stretch.time {
                        let r = ui.add(
                            egui::DragValue::new(beats)
                                .range(0.25..=1024.0)
                                .speed(0.25)
                                .suffix(" beats"),
                        );
                        stopped.set(stopped.get() | r.drag_stopped() || r.lost_focus());
                    }
                });
                ui.end_row();
                ui.label("Mode");
                egui::ComboBox::from_id_salt("stretch_mode")
                    .width(120.0)
                    .selected_text(e.stretch.mode.label())
                    .show_ui(ui, |ui| {
                        for m in StretchMode::ALL {
                            ui.selectable_value(&mut e.stretch.mode, m, m.label());
                        }
                    });
                ui.end_row();
            });

        let ui = &mut cols[2];
        theme_label(ui, "Precomputed effects");
        ui.add_enabled_ui(!e.keep_on_disk, |ui| {
            let p = &mut e.precompute;
            egui::Grid::new("tools_flags")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.checkbox(&mut p.remove_dc, "Remove DC offset");
                    ui.checkbox(&mut p.reverse_polarity, "Reverse polarity");
                    ui.end_row();
                    ui.checkbox(&mut p.normalize, "Normalize");
                    ui.checkbox(&mut p.fade_stereo, "Fade stereo");
                    ui.end_row();
                    ui.checkbox(&mut p.reverse, "Reverse");
                    ui.checkbox(&mut p.swap_stereo, "Swap stereo");
                    ui.end_row();
                });
            egui::Grid::new("tools_knobs")
                .num_columns(2)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    let pct = |v: f64, _| format!("{:.1}%", v * 100.0);
                    for (label, v, hi) in [
                        ("SMP START", &mut p.start, 1.0),
                        ("LENGTH", &mut p.length, 1.0),
                        ("IN", &mut p.fade_in, 0.5),
                        ("OUT", &mut p.fade_out, 0.5),
                        ("CROSSFADE", &mut p.crossfade, 0.5),
                    ] {
                        ui.label(label);
                        let r = ui.add(
                            egui::DragValue::new(v)
                                .range(0.0..=hi)
                                .speed(0.002)
                                .custom_formatter(pct),
                        );
                        stopped.set(stopped.get() | r.drag_stopped() || r.lost_focus());
                        ui.end_row();
                    }
                    ui.checkbox(&mut p.trim_on, "TRIM");
                    let r = ui.add_enabled(
                        p.trim_on,
                        egui::DragValue::new(&mut p.trim)
                            .range(0.0..=1.0)
                            .speed(0.001)
                            .custom_formatter(|v, _| {
                                if v <= 0.0 {
                                    "silence".to_owned()
                                } else {
                                    db_text(v as f32)
                                }
                            }),
                    );
                    stopped.set(stopped.get() | r.drag_stopped() || r.lost_focus());
                    ui.end_row();
                });
        });
    });
    out.end_gesture |= stopped.get();
}

/// The sampler's Instrument tab: level offsets, cut groups, root note
/// and fine tuning (§4).
fn keys_tab(ui: &mut egui::Ui, project: &Project, ch: &Channel, out: &mut SettingsOutput) {
    let s = ch.sampler;
    let mut e = s;
    let mut stopped = false;
    theme_label(ui, "Levels adjustment");
    ui.horizontal(|ui| {
        for (label, v, hint) in [
            ("PAN", &mut e.levels.pan, "Added to the channel's pan"),
            ("VOL", &mut e.levels.vol, "Added to the channel's volume"),
            ("MOD X", &mut e.levels.mod_x, "Stored for a later version"),
            ("MOD Y", &mut e.levels.mod_y, "Stored for a later version"),
        ] {
            ui.label(label);
            let r = ui
                .add(egui::DragValue::new(v).range(-1.0..=1.0).speed(0.01))
                .on_hover_text(hint);
            stopped |= r.drag_stopped() || r.lost_focus();
        }
    });
    ui.add_space(8.0);
    theme_label(ui, "Group");
    ui.horizontal(|ui| {
        let group = |n: u8| {
            if n == 0 {
                "…".to_owned()
            } else {
                n.to_string()
            }
        };
        for (label, v, salt) in [("Cut", &mut e.cut, "cut"), ("By", &mut e.cut_by, "cut_by")] {
            ui.label(label);
            egui::ComboBox::from_id_salt(salt)
                .width(50.0)
                .selected_text(group(*v))
                .show_ui(ui, |ui| {
                    for n in 0..=MAX_CUT_GROUP {
                        ui.selectable_value(v, n, group(n));
                    }
                });
        }
        if ui
            .button("Cut self")
            .on_hover_text("Each new note cuts the one before")
            .clicked()
        {
            let n = free_cut_group(project);
            e.cut = n;
            e.cut_by = n;
        }
    });
    ui.label(
        RichText::new("Notes of this channel cut channels whose By is this channel's Cut.")
            .size(11.5)
            .color(theme::MUTED),
    );
    ui.add_space(8.0);
    theme_label(ui, "Root note");
    ui.horizontal(|ui| {
        ui.label(format!("Root note: {}", key_name(e.root_key)));
        if ui.button("Reset").on_hover_text("Back to C5").clicked() {
            e.root_key = jdance_domain::ROOT_KEY;
        }
        ui.checkbox(&mut e.main_pitch, "Enable main pitch")
            .on_hover_text("The channel's PITCH acts on notes");
        ui.checkbox(&mut e.add_to_key, "Add to key")
            .on_hover_text("Note keys change the pitch; off, every note plays at the root");
    });
    ui.horizontal(|ui| {
        ui.label("Fine tune");
        let r = ui.add(
            egui::Slider::new(&mut e.fine_tune, -100.0..=100.0)
                .suffix(" ct")
                .fixed_decimals(0),
        );
        stopped |= r.drag_stopped() || r.lost_focus();
    });
    if let Some(k) = root_keyboard(ui, e.root_key) {
        e.root_key = k;
    }
    ui.label(
        RichText::new("Right-click a key to make it the root note.")
            .size(11.5)
            .color(theme::MUTED),
    );
    if e != s && e.validate().is_ok() {
        let merging = e.levels != s.levels || e.fine_tune != s.fine_tune;
        out.edits.push((Edit::SetSampler(ch.id, e), merging));
    }
    out.end_gesture |= stopped;
}

/// Highest cut group number.
const MAX_CUT_GROUP: u8 = 32;

/// The lowest cut group no channel uses (for Cut self).
fn free_cut_group(project: &Project) -> u8 {
    (1..=MAX_CUT_GROUP)
        .find(|n| {
            !project
                .channels
                .iter()
                .any(|c| c.sampler.cut == *n || c.sampler.cut_by == *n)
        })
        .unwrap_or(MAX_CUT_GROUP)
}

/// Keys C2–C8 with the root highlighted; returns the right-clicked key.
fn root_keyboard(ui: &mut egui::Ui, root: u8) -> Option<u8> {
    const LO: u8 = 24;
    const HI: u8 = 96;
    let black = |k: u8| matches!(k % 12, 1 | 3 | 6 | 8 | 10);
    let whites = (LO..=HI).filter(|k| !black(*k)).count() as f32;
    let width = ui.available_width().min(760.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 56.0), Sense::click());
    let painter = ui.painter_at(rect);
    let w = rect.width() / whites;
    let mut white_rects = Vec::new();
    let mut black_rects = Vec::new();
    let mut i = 0.0;
    for k in LO..=HI {
        if black(k) {
            let x = rect.left() + i * w - w * 0.3;
            black_rects.push((
                k,
                Rect::from_min_size(pos2(x, rect.top()), vec2(w * 0.6, rect.height() * 0.6)),
            ));
        } else {
            white_rects.push((
                k,
                Rect::from_min_size(
                    pos2(rect.left() + i * w, rect.top()),
                    vec2(w, rect.height()),
                ),
            ));
            i += 1.0;
        }
    }
    for (k, r) in &white_rects {
        let fill = if *k == root {
            theme::AMBER
        } else {
            egui::Color32::from_rgb(0xD8, 0xDA, 0xDE)
        };
        painter.rect_filled(r.shrink(0.5), 1.0, fill);
        if k % 12 == 0 {
            painter.text(
                pos2(r.center().x, r.bottom() - 8.0),
                egui::Align2::CENTER_CENTER,
                key_name(*k),
                egui::FontId::proportional(9.0),
                egui::Color32::from_rgb(0x30, 0x33, 0x38),
            );
        }
    }
    for (k, r) in &black_rects {
        let fill = if *k == root {
            theme::AMBER
        } else {
            egui::Color32::from_rgb(0x1A, 0x1C, 0x20)
        };
        painter.rect_filled(*r, 1.0, fill);
    }
    if !resp.secondary_clicked() {
        return None;
    }
    let p = resp.interact_pointer_pos()?;
    black_rects
        .iter()
        .chain(&white_rects)
        .find(|(_, r)| r.contains(p))
        .map(|(k, _)| *k)
}

fn envelope_tab(ui: &mut egui::Ui, ch: &Channel, out: &mut SettingsOutput) {
    let s = ch.sampler;
    let mut e = s;
    ui.checkbox(&mut e.envelope, "Envelope on (note length is respected)");
    let width = ui.available_width().min(520.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 140.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        CornerRadius::same(8),
        egui::Color32::from_rgb(0x10, 0x12, 0x15),
    );
    let total = (e.attack_ms + e.decay_ms + e.release_ms).max(1.0) * 1.5;
    let hold = total / 3.0;
    let x = |ms: f32| rect.left() + 8.0 + ms / (total + hold) * (rect.width() - 16.0);
    let y = |v: f32| rect.bottom() - 8.0 - v * (rect.height() - 16.0);
    let pts = vec![
        pos2(x(0.0), y(0.0)),
        pos2(x(e.attack_ms), y(1.0)),
        pos2(x(e.attack_ms + e.decay_ms), y(e.sustain)),
        pos2(x(e.attack_ms + e.decay_ms + hold), y(e.sustain)),
        pos2(x(e.attack_ms + e.decay_ms + hold + e.release_ms), y(0.0)),
    ];
    let color = if e.envelope {
        theme::AMBER
    } else {
        theme::MUTED
    };
    painter.add(egui::Shape::line(pts, Stroke::new(2.0, color)));
    let mut stopped = false;
    egui::Grid::new("adsr")
        .num_columns(4)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            for (label, v, max) in [
                ("Attack", &mut e.attack_ms, MAX_ENVELOPE_MS),
                ("Decay", &mut e.decay_ms, MAX_ENVELOPE_MS),
                ("Release", &mut e.release_ms, MAX_ENVELOPE_MS),
            ] {
                ui.label(label);
                let r = ui.add(
                    egui::DragValue::new(v)
                        .range(0.0..=max)
                        .speed(1.0)
                        .suffix(" ms"),
                );
                stopped |= r.drag_stopped() || r.lost_focus();
                ui.end_row();
            }
            ui.label("Sustain");
            let mut pct = e.sustain * 100.0;
            let r = ui.add(
                egui::DragValue::new(&mut pct)
                    .range(0.0..=100.0)
                    .speed(0.5)
                    .suffix(" %"),
            );
            e.sustain = pct / 100.0;
            stopped |= r.drag_stopped() || r.lost_focus();
            ui.end_row();
        });
    if e != s && e.validate().is_ok() {
        out.edits
            .push((Edit::SetSampler(ch.id, e), e.envelope == s.envelope));
    }
    out.end_gesture |= stopped;
}

fn instrument_tab(
    ui: &mut egui::Ui,
    project: &Project,
    ch: &Channel,
    host: &mut dyn Host,
    out: &mut SettingsOutput,
) {
    let target = PluginTarget::Channel(ch.id);
    ui.horizontal(|ui| {
        let open = host.plugin_editor_open(target);
        let r = ui
            .selectable_label(open, "Native editor")
            .on_hover_text("Open the plugin's own window (right-click: scaling)");
        if r.clicked() {
            out.toggle_editor = true;
        }
        crate::mixer::editor_scaling_menu(&r, host, target, &ch.name);
    });
    crate::mixer::plugin_params_ui(
        ui,
        target,
        ch.instrument(),
        &project.automation,
        host,
        &mut out.automation,
    );
}

fn theme_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .size(11.0)
            .color(theme::MUTED)
            .extra_letter_spacing(0.6),
    );
}

fn track_label(project: &Project, id: jdance_domain::TrackId) -> String {
    let name = project.mixer.track(id).map_or("?", |t| t.name.as_str());
    format!("{} · {name}", crate::rack::track_number(project, id))
}

fn db_text(v: f32) -> String {
    if v <= 0.0 {
        "-inf dB".into()
    } else {
        format!("{:+.1} dB", 20.0 * v.log10())
    }
}

/// Mixer track, file and clips of an audio channel (ACH-031), inside the
/// Channel tab grid.
fn audio_rows(ui: &mut egui::Ui, project: &Project, ch: &Channel, out: &mut SettingsOutput) {
    let id = ch.id;
    ui.label("Mixer track");
    egui::ComboBox::from_id_salt("settings_track")
        .width(200.0)
        .selected_text(track_label(project, ch.track))
        .show_ui(ui, |ui| {
            for t in project.mixer.all_tracks() {
                if ui
                    .selectable_label(t.id == ch.track, track_label(project, t.id))
                    .clicked()
                    && t.id != ch.track
                {
                    out.edits.push((Edit::SetChannelTrack(id, t.id), false));
                }
            }
        });
    ui.end_row();
    ui.label("File");
    match ch.audio_asset().and_then(|a| project.asset(a)) {
        Some(a) => {
            ui.label(format!(
                "{}\n{:.2} s · {} Hz · {} channel(s)\n{}",
                a.name,
                a.seconds(),
                a.sample_rate,
                a.channels,
                a.path
            ));
        }
        None => {
            ui.colored_label(theme::ERROR, "missing");
        }
    }
    ui.end_row();
    let clips = project.channel_clips(id).count();
    ui.label("Clips");
    ui.horizontal(|ui| {
        ui.label(format!("{clips} in the Playlist"));
        if ui
            .add_enabled(clips > 0, egui::Button::new("Select them"))
            .clicked()
        {
            out.select_clips = true;
        }
        if ui
            .add_enabled(clips > 0, egui::Button::new("Sample Editor"))
            .clicked()
        {
            out.open_clip_editor = true;
        }
    });
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(db_text(1.0), "+0.0 dB");
        assert_eq!(db_text(0.0), "-inf dB");
    }
}
