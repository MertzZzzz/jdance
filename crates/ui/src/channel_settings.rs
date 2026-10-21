// SPDX-License-Identifier: GPL-3.0-or-later
//! Channel Settings window (spec "DAW UI clean room", §11–§13).
//!
//! Tabs: Channel (host settings: name, color, level, pan, pitch, mixer
//! track, polyphony, mono), then for samplers Sample (file, waveform with
//! start / end / loop markers, root key, gain, playback mode) and Envelope
//! (ADSR), for plugin instruments Instrument (generic parameters and the
//! native editor). Changing the mixer track only reroutes audio.

use crate::automation::AutomationRequest;
use crate::{Host, SampleStatus, theme};
use egui::{CornerRadius, Rect, RichText, Sense, Stroke, pos2, vec2};
use jdance_commands::Edit;
use jdance_domain::{
    Channel, ChannelId, MAX_CHANNEL_PITCH, MAX_ENVELOPE_MS, MAX_POLYPHONY, MAX_VOLUME, PALETTE,
    PlaybackMode, PluginTarget, Project, SampleRef, SamplerSettings, color_or_default,
};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Channel,
    Sample,
    Envelope,
    Instrument,
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
        if instrument && matches!(self.tab, Tab::Sample | Tab::Envelope)
            || !instrument && self.tab == Tab::Instrument
        {
            self.tab = Tab::Channel;
        }
        let mut open = true;
        egui::Window::new(
            RichText::new(format!(
                "{} · {}",
                ch.name,
                if instrument { "Instrument" } else { "Sampler" }
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
                } else {
                    &[
                        (Tab::Channel, "Channel"),
                        (Tab::Sample, "Sample"),
                        (Tab::Envelope, "Envelope"),
                    ]
                };
                for (tab, label) in tabs {
                    ui.selectable_value(&mut self.tab, *tab, *label);
                }
            });
            ui.separator();
            match self.tab {
                Tab::Channel => self.channel_tab(ui, project, index, &mut out),
                Tab::Sample => self.sample_tab(ui, ch, host, dir, &mut out),
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
        host: &dyn Host,
        dir: Option<&Path>,
        out: &mut SettingsOutput,
    ) {
        let id = ch.id;
        let s = ch.sampler;
        ui.horizontal(|ui| {
            let path = ch.sample().map_or("No sample", |s| s.path.as_str());
            ui.label(RichText::new(path).font(theme::mono(12.0)));
            if ui.button("Load…").clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("WAV audio", &["wav", "wave"])
                    .pick_file()
            {
                let sample = SampleRef {
                    path: p.to_string_lossy().into_owned(),
                };
                out.edits
                    .push((Edit::SetChannelSample(id, Some(sample)), false));
            }
        });
        let status = ch.sample().and_then(|smp| host.sample_status(smp, dir));
        let frames_rate = match &status {
            Some(SampleStatus::Ready {
                frames,
                source_rate,
            }) => {
                let secs = *frames as f64 / f64::from(*source_rate);
                ui.label(
                    RichText::new(format!("{secs:.2} s · {source_rate} Hz · {frames} frames"))
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
                    RichText::new("Load a WAV file or drop one on the channel.")
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
        let peaks = ch.sample().and_then(|smp| host.sample_peaks(smp, dir));
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
            ui.horizontal(|ui| {
                ui.label("Root note");
                egui::ComboBox::from_id_salt("root_key")
                    .width(90.0)
                    .selected_text(format!(
                        "{} ({})",
                        key_name(edited.root_key),
                        edited.root_key
                    ))
                    .show_ui(ui, |ui| {
                        for k in (0..=127u8).rev() {
                            ui.selectable_value(
                                &mut edited.root_key,
                                k,
                                format!("{} ({k})", key_name(k)),
                            );
                        }
                    });
            });
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
        if edited != s && edited.validate().is_ok() {
            let merging = self.dragging.is_some() || edited.gain_db != s.gain_db;
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
        if ui
            .selectable_label(open, "Native editor")
            .on_hover_text("Open the plugin's own window")
            .clicked()
        {
            out.toggle_editor = true;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(db_text(1.0), "+0.0 dB");
        assert_eq!(db_text(0.0), "-inf dB");
    }
}
