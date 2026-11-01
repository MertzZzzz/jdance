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
use crate::widgets::Hint;
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
    /// Route to a free mixer track (spec 0.3.0 §5).
    pub free_track: bool,
    /// Open the channel's sample in the Sample Editor (UI-RD §10.4).
    pub open_sample_editor: bool,
    /// Show the channel's sample file in its folder.
    pub reveal_sample: bool,
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
        .default_size([840.0, 600.0])
        .resizable(true)
        .show(ctx, |ui| {
            // UI-RD §4: glyph tabs on the left, ON / PAN / VOL / PITCH /
            // TRACK on the right; the same strip on every tab.
            let tabs: &[(Tab, crate::icons::Glyph, &str)] = {
                use crate::icons::Glyph as G;
                if instrument {
                    &[
                        (Tab::Channel, G::Channel, "Channel"),
                        (Tab::Instrument, G::Plugins, "Instrument"),
                    ]
                } else if audio {
                    &[(Tab::Channel, G::Channel, "Channel")]
                } else {
                    &[
                        (Tab::Channel, G::Channel, "Channel"),
                        (Tab::Sample, G::Sample, "Sample"),
                        (Tab::Keys, G::PianoRoll, "Instrument"),
                        (Tab::Envelope, G::Envelope, "Envelope"),
                    ]
                }
            };
            ui.horizontal(|ui| {
                // §10.4: the previous / next channel of the same kind.
                let same_kind =
                    |c: &Channel| c.instrument().is_some() == instrument && c.is_audio() == audio;
                let ids: Vec<ChannelId> = project
                    .channels
                    .iter()
                    .filter(|c| same_kind(c))
                    .map(|c| c.id)
                    .collect();
                let at = ids.iter().position(|id| *id == self.channel).unwrap_or(0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let small = vec2(20.0, 18.0);
                    if ui
                        .add_enabled_ui(at > 0, |ui| {
                            crate::icons::icon_button_sized(
                                ui,
                                crate::icons::Glyph::Prev,
                                false,
                                "Previous channel",
                                small,
                                11.0,
                            )
                        })
                        .inner
                        .clicked()
                    {
                        self.channel = ids[at - 1];
                    }
                    if ui
                        .add_enabled_ui(at + 1 < ids.len(), |ui| {
                            crate::icons::icon_button_sized(
                                ui,
                                crate::icons::Glyph::Next,
                                false,
                                "Next channel",
                                small,
                                11.0,
                            )
                        })
                        .inner
                        .clicked()
                    {
                        self.channel = ids[at + 1];
                    }
                });
                let (r, _) = ui.allocate_exact_size(vec2(4.0, 38.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, color);
                for (tab, glyph, label) in tabs {
                    let on = self.tab == *tab;
                    let (r, resp) = ui.allocate_exact_size(vec2(66.0, 46.0), Sense::click());
                    let p = ui.painter();
                    if on {
                        p.rect(
                            r,
                            8.0,
                            theme::RAISED,
                            egui::Stroke::NONE,
                            egui::StrokeKind::Inside,
                        );
                        p.hline(
                            r.x_range().shrink(10.0),
                            r.bottom() - 1.0,
                            egui::Stroke::new(2.0, theme::AMBER),
                        );
                    } else if resp.hovered() {
                        p.rect_filled(r, 8.0, theme::RAISED.gamma_multiply(0.6));
                    }
                    let c = if on { theme::AMBER } else { theme::MUTED };
                    crate::icons::paint_with(
                        p,
                        *glyph,
                        egui::Rect::from_center_size(
                            r.center_top() + vec2(0.0, 16.0),
                            vec2(16.0, 16.0),
                        ),
                        c,
                    );
                    p.text(
                        r.center_bottom() - vec2(0.0, 7.0),
                        egui::Align2::CENTER_BOTTOM,
                        *label,
                        egui::FontId::proportional(10.5),
                        if on { theme::TEXT } else { theme::MUTED },
                    );
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Tab, true, on, *label)
                    });
                    if resp.clicked() {
                        self.tab = *tab;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    header(ui, project, ch, &mut out);
                });
            });
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
        // UI-RD §4 (board R3 · Sampler tabs, Channel).
        crate::widgets::caption(ui, "Name");
        let mut text = self.name.clone().unwrap_or_else(|| ch.name.clone());
        let r = ui.add(
            egui::TextEdit::singleline(&mut text)
                .desired_width(ui.available_width().min(460.0))
                .margin(egui::vec2(8.0, 6.0)),
        );
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
        ui.add_space(6.0);
        crate::widgets::caption(ui, "Color");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let current = color_or_default(ch.color, index);
            for c in PALETTE {
                let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                ui.painter().rect_filled(rect, 6.0, theme::rgb(c));
                if c == current {
                    ui.painter().rect_stroke(
                        rect.expand(2.0),
                        7.0,
                        Stroke::new(2.0, theme::TEXT),
                        egui::StrokeKind::Outside,
                    );
                }
                if resp.clicked() && Some(c) != ch.color {
                    out.edits.push((Edit::SetChannelColor(id, Some(c)), false));
                }
            }
        });
        ui.add_space(6.0);
        let playback = |pitch: i32, polyphony: u16, mono: bool| Edit::SetChannelPlayback {
            channel: id,
            pitch,
            polyphony,
            mono,
        };
        crate::widgets::card(ui, None, "", |ui| {
            ui.set_width(ui.available_width().min(460.0));
            let n = if ch.is_audio() { 2 } else { 3 };
            ui.columns(n, |cols| {
                cols[0].vertical_centered(|ui| {
                    let mut vol = ch.volume;
                    let default = if ch.is_audio() { 1.0 } else { 0.8 };
                    let r = crate::widgets::knob(
                        ui,
                        &mut vol,
                        0.0..=MAX_VOLUME,
                        default,
                        theme::AMBER,
                        34.0,
                    );
                    if r.changed() {
                        out.edits.push((Edit::SetChannelVolume(id, vol), true));
                    }
                    out.end_gesture |= r.drag_stopped();
                    ui.label(crate::widgets::caption_text("Volume").size(8.5));
                    ui.label(RichText::new(db_text(ch.volume)).font(theme::mono(10.5)));
                });
                cols[1].vertical_centered(|ui| {
                    let mut pan = ch.pan;
                    let r = crate::widgets::knob(ui, &mut pan, -1.0..=1.0, 0.0, theme::TEXT, 34.0);
                    if r.changed() {
                        out.edits.push((Edit::SetChannelPan(id, pan), true));
                    }
                    out.end_gesture |= r.drag_stopped();
                    ui.label(crate::widgets::caption_text("Pan").size(8.5));
                    let v = (ch.pan * 100.0).round() as i32;
                    let text = match v {
                        0 => "C".to_owned(),
                        v if v < 0 => format!("L{}", -v),
                        v => format!("R{v}"),
                    };
                    ui.label(RichText::new(text).font(theme::mono(10.5)));
                });
                if n == 3 {
                    cols[2].vertical_centered(|ui| {
                        let mut pitch = ch.pitch as f32;
                        let max = MAX_CHANNEL_PITCH as f32;
                        let r = crate::widgets::knob(
                            ui,
                            &mut pitch,
                            -max..=max,
                            0.0,
                            theme::TEXT,
                            34.0,
                        );
                        let pitch = pitch.round() as i32;
                        if r.changed() && pitch != ch.pitch {
                            out.edits
                                .push((playback(pitch, ch.polyphony, ch.mono), true));
                        }
                        out.end_gesture |= r.drag_stopped();
                        ui.label(crate::widgets::caption_text("Pitch").size(8.5));
                        ui.label(
                            RichText::new(format!("{:+} st", ch.pitch).replace("+0", "0"))
                                .font(theme::mono(10.5)),
                        );
                    });
                }
            });
        });
        ui.add_space(6.0);
        if ch.is_audio() {
            egui::Grid::new("channel_grid")
                .num_columns(2)
                .spacing([16.0, 10.0])
                .show(ui, |ui| audio_rows(ui, project, ch, out));
            ui.add_space(10.0);
            info_line(
                ui,
                "Every clip of this file in the Playlist plays through this channel: level, pan \
                 and mixer track apply to all of them. Clip gain stays per clip.",
            );
            return;
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                crate::widgets::caption(ui, "Mixer track");
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
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                crate::widgets::caption(ui, "Polyphony");
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
            });
            if ch.instrument().is_some() {
                ui.add_space(12.0);
                // Slides on plugins without per-note tuning (spec 0.3.6 §6).
                ui.vertical(|ui| {
                    crate::widgets::caption(ui, "Pitch bend range");
                    let mut range = ch.pitch_bend_range;
                    let r = ui
                        .add(
                            egui::DragValue::new(&mut range)
                                .range(1..=jdance_domain::MAX_PITCH_BEND_RANGE)
                                .suffix(" semitones"),
                        )
                        .on_hover_text(
                            "Slide notes reach plugins without per-note tuning as pitch bend: \
                             set the same range in the plugin",
                        );
                    if r.changed() {
                        out.edits.push((Edit::SetChannelBendRange(id, range), true));
                    }
                    out.end_gesture |= r.drag_stopped() || r.lost_focus();
                });
            }
        });
        ui.add_space(8.0);
        let mut mono = ch.mono;
        if crate::widgets::switch_row(
            ui,
            &mut mono,
            theme::LED_ON,
            "Mono — a new note cuts the sounding one",
        )
        .changed()
        {
            out.edits
                .push((playback(ch.pitch, ch.polyphony, mono), false));
        }
        ui.add_space(12.0);
        info_line(
            ui,
            "Host channel settings, saved with the project. Changing the mixer track only \
             reroutes the audio: the instrument and its preset stay.",
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
        use crate::icons::Glyph as G;
        let id = ch.id;
        let s = ch.sampler;
        let status = ch.sample().and_then(|smp| host.sample_status(smp, dir));
        let frames_rate = match &status {
            Some(SampleStatus::Ready {
                frames,
                source_rate,
                ..
            }) => Some((*frames, *source_rate)),
            _ => None,
        };
        let info = match &status {
            Some(SampleStatus::Ready {
                frames,
                source_rate,
                format,
            }) => format!(
                "{:.2} s · {:.1} kHz · {}",
                *frames as f64 / f64::from(*source_rate),
                f64::from(*source_rate) / 1000.0,
                crate::browser::format_label(format)
            ),
            _ => String::new(),
        };
        let mp3 = matches!(&status, Some(SampleStatus::Ready { format, .. }) if format.starts_with("MP3"));
        let mut edited = s;
        let stopped = std::cell::Cell::new(false);
        // UI-RD §4 (board R3 · Sampler): File, Content and Playback on
        // the left; Time stretching and Precomputed effects on the right;
        // the waveform across the bottom.
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            crate::widgets::card(ui, None, "", |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    crate::widgets::caption(ui, "File");
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    crate::icons::paint(ui, G::Sample, r, theme::AMBER);
                    let path = ch.sample().map_or("No sample", |s| s.path.as_str());
                    ui.add(
                        egui::Label::new(RichText::new(path).font(theme::mono(11.5))).truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let has = ch.sample().is_some();
                        let small = vec2(26.0, 24.0);
                        if ui
                            .add_enabled_ui(has, |ui| {
                                crate::icons::icon_button_sized(
                                    ui,
                                    G::Close,
                                    false,
                                    "Clear: the channel keeps its settings without a sample",
                                    small,
                                    13.0,
                                )
                            })
                            .inner
                            .clicked()
                        {
                            out.edits.push((
                                Edit::SetChannelSource {
                                    channel: id,
                                    source: jdance_domain::ChannelSource::Sampler { sample: None },
                                    clip_asset: ch.clip_asset,
                                },
                                false,
                            ));
                        }
                        if ui
                            .add_enabled_ui(has, |ui| {
                                crate::icons::icon_button_sized(
                                    ui,
                                    G::Search,
                                    false,
                                    "Show the file in its folder",
                                    small,
                                    13.0,
                                )
                            })
                            .inner
                            .clicked()
                        {
                            out.reveal_sample = true;
                        }
                        if crate::icons::icon_button_sized(
                            ui,
                            G::Folder,
                            false,
                            "Load: choose from the library, the project or any file",
                            small,
                            13.0,
                        )
                        .clicked()
                        {
                            out.pick = Some(crate::library::PickFor::Channel(id));
                        }
                    });
                });
                match (&status, ch.sample()) {
                    (_, None) => {
                        ui.label(
                            RichText::new("Load a WAV or MP3 file or drop one on the channel.")
                                .size(11.5)
                                .color(theme::MUTED),
                        );
                    }
                    (Some(SampleStatus::Failed(e)), Some(smp)) => {
                        ui.label(RichText::new("Missing sample").color(theme::ERROR).strong());
                        ui.label(
                            RichText::new(format!(
                                "{} could not be loaded ({e}). The channel stays silent and \
                                 keeps its settings.",
                                smp.path
                            ))
                            .size(11.5),
                        );
                    }
                    _ => {}
                }
            });
            ui.add_space(6.0);
            crate::widgets::card(ui, Some(G::Folder), "Content", |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!mp3, |ui| {
                        crate::widgets::switch_row(
                            ui,
                            &mut edited.keep_on_disk,
                            theme::LED_ON,
                            "Keep on disk",
                        )
                        .on_hover_text(
                            "Stored with the project; the sample still loads into memory in \
                             this version. Turns the precomputed effects off.",
                        )
                        .on_disabled_hover_text("Not for MP3 files");
                    });
                    ui.add_space(16.0);
                    ui.add_enabled_ui(false, |ui| {
                        crate::widgets::switch_row(ui, &mut true, theme::LED_ON, "Resample")
                    })
                    .response
                    .on_disabled_hover_text(
                        "Always on: the sample is converted to the device rate.",
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Declicking");
                    use jdance_domain::sample_processing::Declick;
                    egui::ComboBox::from_id_salt("declick")
                        .width(ui.available_width() - 8.0)
                        .selected_text(edited.declick.label())
                        .show_ui(ui, |ui| {
                            for d in Declick::ALL {
                                ui.selectable_value(&mut edited.declick, d, d.label());
                            }
                        });
                });
            });
            ui.add_space(6.0);
            crate::widgets::card(ui, Some(G::Play), "Playback", |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    crate::widgets::segmented(ui, |ui| {
                        for (m, g, name) in [
                            (PlaybackMode::OneShot, G::Next, "One-shot"),
                            (PlaybackMode::Loop, G::Loop, "Loop"),
                        ] {
                            if crate::widgets::button(
                                ui,
                                Some(g),
                                name,
                                edited.mode == m,
                                crate::widgets::Look::Blue,
                                "",
                            )
                            .clicked()
                            {
                                edited.mode = m;
                            }
                        }
                    });
                    ui.add_space(8.0);
                    let r = knob_cell(ui, "GAIN", &format!("{:+.1} dB", edited.gain_db), |ui| {
                        crate::widgets::knob(
                            ui,
                            &mut edited.gain_db,
                            -24.0..=12.0,
                            0.0,
                            theme::AMBER,
                            28.0,
                        )
                    });
                    stopped.set(stopped.get() | r.drag_stopped());
                });
                ui.label(
                    RichText::new(format!(
                        "{} Root note {} — Instrument tab.",
                        match edited.mode {
                            PlaybackMode::OneShot => {
                                "Plays from start to end; note length is ignored unless the \
                                 envelope is on."
                            }
                            PlaybackMode::Loop => {
                                "Repeats the loop region while the note is held, then releases."
                            }
                        },
                        key_name(edited.root_key)
                    ))
                    .size(11.0)
                    .color(theme::MUTED),
                );
                let pct = |v: f32| format!("{:.1}%", v * 100.0);
                let time = |v: f32| {
                    frames_rate.map_or_else(
                        || pct(v),
                        |(f, r)| {
                            format!("{:.0} ms", f64::from(v) * f as f64 / f64::from(r) * 1000.0)
                        },
                    )
                };
                ui.horizontal(|ui| {
                    let mut text =
                        format!("Start {} · End {}", time(edited.start), time(edited.end));
                    if edited.mode == PlaybackMode::Loop {
                        text += &format!(
                            " · Loop {} – {}",
                            time(edited.loop_start),
                            time(edited.loop_end)
                        );
                    }
                    ui.label(
                        RichText::new(text)
                            .font(theme::mono(10.5))
                            .color(theme::DIM),
                    );
                    if crate::widgets::button(
                        ui,
                        None,
                        "Reset markers",
                        false,
                        crate::widgets::Look::Raised,
                        "Start, end and loop back to the whole sample",
                    )
                    .clicked()
                    {
                        let d = SamplerSettings::default();
                        edited.start = d.start;
                        edited.end = d.end;
                        edited.loop_start = d.loop_start;
                        edited.loop_end = d.loop_end;
                    }
                });
            });
            let ui = &mut cols[1];
            processing_ui(ui, &mut edited, frames_rate, tempo, &stopped);
        });
        ui.add_space(6.0);
        // The waveform shows the processed data once it is computed (§4).
        let peaks = ch
            .playback_sample(tempo)
            .and_then(|smp| host.sample_peaks(&smp, dir))
            .or_else(|| ch.sample().and_then(|smp| host.sample_peaks(smp, dir)));
        let wave = self.waveform(
            ui,
            peaks.as_deref().map(Vec::as_slice),
            &mut edited,
            frames_rate,
        );
        // File details and "Open in Sample Editor" in the corner (§10.4).
        let corner = Rect::from_min_size(
            wave.rect.right_bottom() - vec2(34.0, 34.0),
            vec2(26.0, 26.0),
        );
        if ch.sample().is_some() {
            let editor = ui
                .put(corner, egui::Button::new("").frame(true).corner_radius(6))
                .hint("Open in Sample Editor");
            crate::icons::paint(ui, G::Undock, corner.shrink(6.0), theme::TEXT);
            if editor.clicked() {
                out.open_sample_editor = true;
            }
        }
        ui.painter().text(
            corner.left_center() - vec2(8.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            info,
            theme::mono(10.0),
            theme::MUTED,
        );
        out.end_gesture |= stopped.get();
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
            // UI-RD §4: the signal's envelope filled with a soft
            // gradient, a brighter outline on top.
            let cols = rect.width().max(1.0) as usize;
            let mid = rect.center().y;
            let mut top = Vec::with_capacity(cols);
            for c in 0..cols {
                let a = c * peaks.len() / cols;
                let b = ((c + 1) * peaks.len() / cols).max(a + 1).min(peaks.len());
                let v = peaks[a..b].iter().fold(0.0f32, |m, p| m.max(*p)).min(1.0);
                let h = (v * (rect.height() / 2.0 - 12.0)).max(0.5);
                let x = rect.left() + c as f32 + 0.5;
                let t = (h / (rect.height() / 2.0)).clamp(0.0, 1.0);
                painter.line_segment(
                    [pos2(x, mid - h), pos2(x, mid + h)],
                    Stroke::new(1.0, theme::BLUE.gamma_multiply(0.25 + 0.45 * t)),
                );
                top.push(pos2(x, mid - h));
            }
            let bottom: Vec<_> = top.iter().map(|p| pos2(p.x, 2.0 * mid - p.y)).collect();
            painter.add(egui::Shape::line(top, Stroke::new(1.2, theme::BLUE)));
            painter.add(egui::Shape::line(bottom, Stroke::new(1.2, theme::BLUE)));
            painter.line_segment(
                [pos2(rect.left(), mid), pos2(rect.right(), mid)],
                Stroke::new(1.0, theme::BLUE.gamma_multiply(0.35)),
            );
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
            // Labels are amber (teal for the loop) tags beside the line:
            // START to its right, END to its left.
            let label = match m {
                Marker::Start => "START",
                Marker::End => "END",
                Marker::LoopStart => "LOOP",
                Marker::LoopEnd => "",
            };
            if !label.is_empty() {
                let y = if matches!(m, Marker::LoopStart) {
                    rect.bottom() - 14.0
                } else {
                    rect.top() + 12.0
                };
                let galley =
                    painter.layout_no_wrap(label.to_owned(), theme::strong(9.5), theme::ON_AMBER);
                let size = galley.size() + vec2(8.0, 4.0);
                let left = if matches!(m, Marker::End) {
                    x - 3.0 - size.x
                } else {
                    x + 3.0
                };
                let tag = Rect::from_min_size(pos2(left, y - size.y / 2.0), size);
                painter.rect_filled(tag, 3.0, *c);
                painter.galley(tag.center() - galley.size() / 2.0, galley, theme::ON_AMBER);
            }
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
    // Laid out right to left: TRACK, PITCH, VOL, PAN, ON.
    let id = ch.id;
    let knob_cell = |ui: &mut egui::Ui, caption: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
        ui.allocate_ui_with_layout(
            vec2(42.0, 48.0),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                add(ui);
                ui.label(crate::widgets::caption_text(caption));
            },
        );
    };
    ui.spacing_mut().item_spacing.x = 8.0;
    // Spec 0.3.0 §5: a mixer track of its own in one click.
    if crate::icons::icon_button(
        ui,
        crate::icons::Glyph::Send,
        false,
        "Route this channel to the first free mixer track (a new one if none)",
    )
    .clicked()
    {
        out.free_track = true;
    }
    ui.allocate_ui_with_layout(
        vec2(72.0, 48.0),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            egui::ComboBox::from_id_salt("sampler_header_track")
                .width(64.0)
                .selected_text(
                    RichText::new(crate::rack::track_number(project, ch.track))
                        .font(theme::mono(13.0)),
                )
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
                })
                .response
                .on_hover_text(track_label(project, ch.track));
            ui.label(crate::widgets::caption_text("Track"));
        },
    );
    crate::widgets::divider(ui, 34.0);
    knob_cell(ui, "Pitch", &mut |ui| {
        let mut pitch = ch.pitch as f32;
        let lim = MAX_CHANNEL_PITCH as f32;
        let r = crate::widgets::knob(ui, &mut pitch, -lim..=lim, 0.0, theme::TEXT, 28.0)
            .on_hover_text(format!("Pitch {:+} semitones (double-click: 0)", ch.pitch));
        let pitch = pitch.round() as i32;
        if r.changed() && pitch != ch.pitch {
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
        out.end_gesture |= r.drag_stopped();
    });
    knob_cell(ui, "Vol", &mut |ui| {
        let mut vol = ch.volume;
        let r = crate::widgets::knob(ui, &mut vol, 0.0..=MAX_VOLUME, 0.8, theme::AMBER, 28.0)
            .on_hover_text(format!("Volume {}", db_text(ch.volume)));
        if r.changed() {
            out.edits.push((Edit::SetChannelVolume(id, vol), true));
        }
        out.end_gesture |= r.drag_stopped();
    });
    knob_cell(ui, "Pan", &mut |ui| {
        let mut pan = ch.pan;
        let r = crate::widgets::knob(ui, &mut pan, -1.0..=1.0, 0.0, theme::TEXT, 28.0)
            .on_hover_text(format!("Pan {:+.0}", ch.pan * 100.0));
        if r.changed() {
            out.edits.push((Edit::SetChannelPan(id, pan), true));
        }
        out.end_gesture |= r.drag_stopped();
    });
    knob_cell(ui, "On", &mut |ui| {
        let (r, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
        crate::widgets::led(ui.painter(), r.center(), 5.0, !ch.muted);
        if resp.on_hover_text("The channel plays").clicked() {
            out.edits
                .push((Edit::SetChannelMuted(id, !ch.muted), false));
        }
    });
}

/// Content, declicking, time stretching and the Tools panel of the
/// Sample tab (§4).
fn processing_ui(
    ui: &mut egui::Ui,
    e: &mut SamplerSettings,
    frames_rate: Option<(usize, u32)>,
    tempo: f64,
    stopped: &std::cell::Cell<bool>,
) {
    use crate::icons::Glyph as G;
    use jdance_domain::sample_processing::{StretchMode, TimeFit};
    let stop = |r: &egui::Response| stopped.set(stopped.get() | r.drag_stopped());
    crate::widgets::card(ui, Some(G::Speed), "Time stretching", |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let before = e.stretch.pitch;
            let r = knob_cell(ui, "PITCH", &format!("{:+.2}", e.stretch.pitch), |ui| {
                crate::widgets::knob(
                    ui,
                    &mut e.stretch.pitch,
                    -24.0..=24.0,
                    0.0,
                    theme::TEXT,
                    28.0,
                )
            });
            stop(&r);
            // Turning PITCH in resampling switches to Auto (§4).
            if e.stretch.pitch != before && e.stretch.mode == StretchMode::Resample {
                e.stretch.mode = StretchMode::Auto;
            }
            let r = knob_cell(ui, "MUL", &format!("×{:.2}", e.stretch.mul), |ui| {
                crate::widgets::knob(ui, &mut e.stretch.mul, 0.25..=4.0, 1.0, theme::TEXT, 28.0)
            });
            stop(&r);
            ui.vertical(|ui| {
                let mut fit = matches!(e.stretch.time, TimeFit::Beats { .. });
                if crate::widgets::switch_row(ui, &mut fit, theme::AMBER, "TIME")
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
            ui.add_space(8.0);
            ui.label("Mode");
            egui::ComboBox::from_id_salt("stretch_mode")
                .width(110.0)
                .selected_text(e.stretch.mode.label())
                .show_ui(ui, |ui| {
                    for m in StretchMode::ALL {
                        ui.selectable_value(&mut e.stretch.mode, m, m.label());
                    }
                });
        });
    });
    ui.add_space(6.0);
    crate::widgets::card(ui, Some(G::Tools), "Precomputed effects", |ui| {
        ui.set_width(ui.available_width());
        ui.add_enabled_ui(!e.keep_on_disk, |ui| {
            let p = &mut e.precompute;
            egui::Grid::new("tools_flags")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    let c = theme::BLUE;
                    crate::widgets::switch_row(ui, &mut p.remove_dc, c, "Remove DC offset");
                    crate::widgets::switch_row(ui, &mut p.reverse_polarity, c, "Reverse polarity");
                    ui.end_row();
                    crate::widgets::switch_row(ui, &mut p.normalize, c, "Normalize");
                    crate::widgets::switch_row(ui, &mut p.fade_stereo, c, "Fade stereo");
                    ui.end_row();
                    crate::widgets::switch_row(ui, &mut p.reverse, c, "Reverse");
                    crate::widgets::switch_row(ui, &mut p.swap_stereo, c, "Swap stereo");
                    ui.end_row();
                });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                let pct = |v: f32| format!("{:.0}%", v * 100.0);
                for (label, v, hi, default, color) in [
                    ("SMP START", &mut p.start, 1.0, 0.0, theme::TEXT),
                    ("LENGTH", &mut p.length, 1.0, 0.0, theme::AMBER),
                    ("IN", &mut p.fade_in, 0.5, 0.0, theme::TEXT),
                    ("OUT", &mut p.fade_out, 0.5, 0.0, theme::TEXT),
                    ("CROSSFADE", &mut p.crossfade, 0.5, 0.0, theme::TEXT),
                ] {
                    let text = if label == "LENGTH" && *v <= 0.0 {
                        "all".to_owned()
                    } else {
                        pct(*v)
                    };
                    let r = knob_cell(ui, label, &text, |ui| {
                        crate::widgets::knob(ui, v, 0.0..=hi, default, color, 26.0)
                    });
                    stop(&r);
                }
                let text = if !p.trim_on {
                    "off".to_owned()
                } else if p.trim <= 0.0 {
                    "silence".to_owned()
                } else {
                    db_text(p.trim)
                };
                let r = knob_cell(ui, "TRIM", &text, |ui| {
                    let r =
                        crate::widgets::knob(ui, &mut p.trim, 0.0..=1.0, 0.0, theme::TEXT, 26.0)
                            .on_hover_text(
                                "Trim the quiet ends; a right click turns trimming on / off",
                            );
                    if r.secondary_clicked() {
                        p.trim_on = !p.trim_on;
                    }
                    // Turning the knob turns trimming on.
                    if r.changed() {
                        p.trim_on = true;
                    }
                    r
                });
                stop(&r);
            });
        });
    });
}

/// A knob with its caption and value under it (UI-RD §4).
fn knob_cell(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    knob: impl FnOnce(&mut egui::Ui) -> egui::Response,
) -> egui::Response {
    ui.vertical(|ui| {
        ui.set_width(58.0);
        ui.spacing_mut().item_spacing.y = 1.0;
        let r = ui.vertical_centered(knob).inner;
        ui.vertical_centered(|ui| {
            ui.label(crate::widgets::caption_text(label).size(8.5));
            ui.label(
                RichText::new(value)
                    .font(theme::mono(9.5))
                    .color(theme::MUTED),
            );
        });
        r
    })
    .inner
}

/// The sampler's Instrument tab: level offsets, cut groups, root note
/// and fine tuning (§4).
fn keys_tab(ui: &mut egui::Ui, project: &Project, ch: &Channel, out: &mut SettingsOutput) {
    use crate::icons::Glyph as G;
    let s = ch.sampler;
    let mut e = s;
    let mut stopped = false;
    // UI-RD §4 (board R3 · Sampler tabs, Instrument).
    crate::widgets::caption(ui, "Levels adjustment");
    crate::widgets::card(ui, None, "", |ui| {
        ui.set_width(ui.available_width());
        ui.columns(4, |cols| {
            for (ui, (label, v, hint)) in cols.iter_mut().zip([
                ("PAN", &mut e.levels.pan, "Added to the channel's pan"),
                ("VOL", &mut e.levels.vol, "Added to the channel's volume"),
                ("MOD X", &mut e.levels.mod_x, "Stored for a later version"),
                ("MOD Y", &mut e.levels.mod_y, "Stored for a later version"),
            ]) {
                ui.vertical_centered(|ui| {
                    let r = crate::widgets::knob(ui, v, -1.0..=1.0, 0.0, theme::TEXT, 28.0)
                        .on_hover_text(hint);
                    stopped |= r.drag_stopped();
                    ui.label(crate::widgets::caption_text(label).size(8.5));
                    ui.label(RichText::new(format!("{:.2}", *v)).font(theme::mono(10.5)));
                });
            }
        });
    });
    ui.add_space(6.0);
    crate::widgets::caption(ui, "Cut group");
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
        let cut_self = e.cut != 0 && e.cut == e.cut_by;
        if crate::widgets::button(
            ui,
            Some(G::Slice),
            "Cut self",
            cut_self,
            crate::widgets::Look::Blue,
            "Each new note cuts the one before",
        )
        .clicked()
            && !cut_self
        {
            let n = free_cut_group(project);
            e.cut = n;
            e.cut_by = n;
        }
    });
    ui.label(
        RichText::new("Notes of this channel cut channels whose By is this channel's Cut.")
            .size(11.0)
            .color(theme::MUTED),
    );
    ui.add_space(6.0);
    crate::widgets::caption(ui, "Root note");
    ui.horizontal(|ui| {
        let root = key_name(e.root_key);
        crate::widgets::button(
            ui,
            None,
            &root,
            true,
            crate::widgets::Look::Amber,
            "Root note",
        );
        if crate::icons::icon_button_sized(
            ui,
            G::Undo,
            false,
            "Reset the root note to C5",
            vec2(26.0, 26.0),
            13.0,
        )
        .clicked()
        {
            e.root_key = jdance_domain::ROOT_KEY;
        }
        if crate::widgets::button(
            ui,
            None,
            "Main pitch",
            e.main_pitch,
            crate::widgets::Look::Blue,
            "The channel's PITCH acts on notes",
        )
        .clicked()
        {
            e.main_pitch = !e.main_pitch;
        }
        if crate::widgets::button(
            ui,
            None,
            "Add to key",
            e.add_to_key,
            crate::widgets::Look::Blue,
            "Note keys change the pitch; off, every note plays at the root",
        )
        .clicked()
        {
            e.add_to_key = !e.add_to_key;
        }
    });
    if let Some(k) = root_keyboard(ui, e.root_key) {
        e.root_key = k;
    }
    ui.label(
        RichText::new("Right-click a key to make it the root note.")
            .size(11.0)
            .color(theme::MUTED),
    );
    ui.horizontal(|ui| {
        ui.label("Fine tune");
        ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(120.0);
        let r = ui.add(
            egui::Slider::new(&mut e.fine_tune, -100.0..=100.0)
                .suffix(" ct")
                .fixed_decimals(0),
        );
        stopped |= r.drag_stopped() || r.lost_focus();
    });
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
        painter.rect_filled(r.shrink(0.5), 2.0, fill);
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
    // UI-RD §4 (board R3 · Sampler tabs): the switch, the graph with
    // draggable points (§10.5) and the knobs.
    ui.horizontal(|ui| {
        crate::widgets::switch_row(ui, &mut e.envelope, theme::AMBER, "Envelope on");
        ui.label(
            RichText::new("· note length is respected")
                .size(11.0)
                .color(theme::MUTED),
        );
    });
    ui.add_space(4.0);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 200.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        CornerRadius::same(9),
        egui::Color32::from_rgb(0x10, 0x12, 0x15),
        Stroke::new(1.0, theme::CARD_LINE),
        egui::StrokeKind::Inside,
    );
    for k in 1..4 {
        let gy = rect.top() + rect.height() * k as f32 / 4.0;
        painter.line_segment(
            [pos2(rect.left(), gy), pos2(rect.right(), gy)],
            Stroke::new(1.0, theme::LINE.gamma_multiply(0.5)),
        );
    }
    // The time scale stays put while a point is dragged (UI-RD §10.5).
    let scale_id = egui::Id::unique(("adsr_scale", ch.id.0));
    let frozen = ui.data(|d| d.get_temp::<f32>(scale_id));
    let span = frozen.unwrap_or_else(|| (s.attack_ms + s.decay_ms + s.release_ms).max(1.0) * 2.0);
    let hold = span / 4.0;
    let ms_per_px = span / (rect.width() - 24.0);
    let x = |ms: f32| rect.left() + 12.0 + ms / ms_per_px;
    let y = |v: f32| rect.bottom() - 18.0 - v * (rect.height() - 34.0);
    let pts = [
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
    // The area under the curve, fading down.
    let base = y(0.0);
    let mut fill = egui::Mesh::default();
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let top = color.gamma_multiply(0.35);
        let low = color.gamma_multiply(0.02);
        let i = fill.vertices.len() as u32;
        fill.colored_vertex(a, top);
        fill.colored_vertex(b, top);
        fill.colored_vertex(pos2(b.x, base), low);
        fill.colored_vertex(pos2(a.x, base), low);
        fill.add_triangle(i, i + 1, i + 2);
        fill.add_triangle(i, i + 2, i + 3);
    }
    painter.add(fill);
    for k in [2usize, 3] {
        painter.line_segment(
            [pts[k], pos2(pts[k].x, base)],
            Stroke::new(1.0, theme::LINE_STRONG),
        );
    }
    painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(2.0, color)));
    // Each phase named under its segment, when there is room.
    for (text, k) in [("A", 0usize), ("D", 1), ("S · note held", 2), ("R", 3)] {
        let (a, b) = (pts[k].x, pts[k + 1].x);
        if b - a < 8.0 * text.len() as f32 {
            continue;
        }
        painter.text(
            pos2((a + b) / 2.0, rect.bottom() - 8.0),
            egui::Align2::CENTER_CENTER,
            text,
            theme::mono(9.0),
            theme::DIM,
        );
    }
    // Draggable points: attack (time), decay (time and sustain level),
    // sustain end (level), release (time).
    let mut any_drag = false;
    let mut stopped = false;
    for (k, tip) in [
        (1usize, "Attack: drag left / right"),
        (2, "Decay and sustain: drag in any direction"),
        (3, "Sustain level: drag up / down"),
        (4, "Release: drag left / right"),
    ] {
        let resp = ui
            .interact(
                Rect::from_center_size(pts[k], vec2(16.0, 16.0)),
                egui::Id::unique(("adsr_point", ch.id.0, k)),
                Sense::drag(),
            )
            .hint(tip);
        let hot = resp.hovered() || resp.dragged();
        painter.circle(
            pts[k],
            if hot { 6.5 } else { 5.0 },
            if hot || k == 3 { color } else { theme::FIELD },
            Stroke::new(2.0, color),
        );
        if resp.dragged() {
            any_drag = true;
            let d = resp.drag_delta();
            let dms = d.x * ms_per_px;
            let dlevel = -d.y / (rect.height() - 34.0);
            match k {
                1 => e.attack_ms = (e.attack_ms + dms).clamp(0.0, MAX_ENVELOPE_MS),
                2 => {
                    e.decay_ms = (e.decay_ms + dms).clamp(0.0, MAX_ENVELOPE_MS);
                    e.sustain = (e.sustain + dlevel).clamp(0.0, 1.0);
                }
                3 => e.sustain = (e.sustain + dlevel).clamp(0.0, 1.0),
                _ => e.release_ms = (e.release_ms + dms).clamp(0.0, MAX_ENVELOPE_MS),
            }
        }
        stopped |= resp.drag_stopped();
    }
    if any_drag {
        ui.data_mut(|d| d.insert_temp(scale_id, span));
    } else {
        ui.data_mut(|d| d.remove_temp::<f32>(scale_id));
    }
    ui.add_space(6.0);
    crate::widgets::card(ui, None, "", |ui| {
        ui.set_width(ui.available_width());
        ui.columns(4, |cols| {
            let ms = |v: f32| {
                if v >= 1000.0 {
                    format!("{:.2} s", v / 1000.0)
                } else {
                    format!("{v:.0} ms")
                }
            };
            let d = SamplerSettings::default();
            let cells: [(&str, &mut f32, f32, f32); 4] = [
                ("ATTACK", &mut e.attack_ms, MAX_ENVELOPE_MS, d.attack_ms),
                ("DECAY", &mut e.decay_ms, MAX_ENVELOPE_MS, d.decay_ms),
                ("SUSTAIN", &mut e.sustain, 1.0, d.sustain),
                ("RELEASE", &mut e.release_ms, MAX_ENVELOPE_MS, d.release_ms),
            ];
            for (ui, (label, v, hi, default)) in cols.iter_mut().zip(cells) {
                let text = if label == "SUSTAIN" {
                    format!("{:.0}%", *v * 100.0)
                } else {
                    ms(*v)
                };
                ui.vertical_centered(|ui| {
                    // Times turn on a square-root scale: fine at the
                    // short end.
                    let r = if label == "SUSTAIN" {
                        crate::widgets::knob(ui, v, 0.0..=hi, default, theme::AMBER, 34.0)
                    } else {
                        let mut root = v.sqrt();
                        let r = crate::widgets::knob(
                            ui,
                            &mut root,
                            0.0..=hi.sqrt(),
                            default.sqrt(),
                            theme::AMBER,
                            34.0,
                        );
                        if r.changed() {
                            *v = (root * root).clamp(0.0, hi);
                        }
                        r
                    };
                    stopped |= r.drag_stopped();
                    ui.label(crate::widgets::caption_text(label).size(8.5));
                    ui.label(
                        RichText::new(text)
                            .font(theme::mono(10.5))
                            .color(theme::TEXT),
                    );
                });
            }
        });
    });
    ui.label(
        RichText::new("Drag the points on the graph or turn the knobs; double-click resets.")
            .size(11.0)
            .color(theme::MUTED),
    );
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
/// A muted note with an info glyph (UI-RD §4).
fn info_line(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
        crate::icons::paint(ui, crate::icons::Glyph::Help, r, theme::DIM);
        ui.label(RichText::new(text).size(11.0).color(theme::MUTED));
    });
}

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
