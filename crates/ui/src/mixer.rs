// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer panel: one strip per bus.
//!
//! Strip layout follows the signal flow: inserts → gain/balance →
//! mute/solo → bus meter; the master strip adds the safety limiter and the
//! output meter. Effect controls are generated from each effect's
//! parameter schema. Drawing only collects edits; routing menus offer
//! only destinations that keep the graph acyclic (MIX-021).

use crate::{Host, LatencyInfo, MeterReadings, PluginParamView};
use egui::{Color32, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use jdance_commands::Edit;
use jdance_domain::{
    Effect, EffectKind, Insert, LimiterSettings, MAX_INSERTS_PER_TRACK, MAX_SEND_LEVEL,
    MAX_SENDS_PER_TRACK, MAX_TRACKS, MAX_VOLUME, Mapping, Mixer, MixerTrack, ParamUnit, Sidechain,
    Tap, TrackId,
};
use jdance_dsp::Processor;
use jdance_plugin_api::PluginDescriptor;

const STRIP_WIDTH: f32 = 190.0;
const FADER_HEIGHT: f32 = 110.0;

/// Mixer changes that need new IDs or dialogs, applied by the caller.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MixerAction {
    AddTrack,
    AddSend { from: TrackId, to: TrackId },
    AddEffect(TrackId, EffectKind),
    AddPlugin(TrackId, PluginDescriptor),
    Rename(TrackId),
}

#[derive(Debug, Default)]
pub(crate) struct MixerOutput {
    pub edits: Vec<(Edit, bool)>,
    pub end_gesture: bool,
    pub actions: Vec<MixerAction>,
}

fn db_label(gain: f32) -> String {
    if gain <= 0.0 {
        "-inf dB".into()
    } else {
        format!("{:+.1} dB", 20.0 * gain.log10())
    }
}

fn track_name(mixer: &Mixer, id: TrackId) -> String {
    mixer
        .track(id)
        .map_or_else(|| format!("#{}", id.0), |t| t.name.clone())
}

/// Vertical stereo peak meter; −60..+6 dB, red above 0 dBFS.
fn meter(ui: &mut egui::Ui, height: f32, (l, r): (f32, f32)) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(2), ui.visuals().extreme_bg_color);
    let frac = |v: f32| {
        if v <= 0.0 {
            0.0
        } else {
            ((20.0 * v.log10() + 60.0) / 66.0).clamp(0.0, 1.0)
        }
    };
    for (i, v) in [l, r].into_iter().enumerate() {
        let x0 = rect.left() + 1.0 + i as f32 * 5.5;
        let top = rect.bottom() - frac(v) * rect.height();
        let bar = Rect::from_min_max(pos2(x0, top), pos2(x0 + 4.5, rect.bottom()));
        let color = if v > 1.0 {
            Color32::from_rgb(230, 70, 60)
        } else if v > 0.5 {
            Color32::from_rgb(230, 200, 60)
        } else {
            Color32::from_rgb(90, 190, 110)
        };
        painter.rect_filled(bar, CornerRadius::ZERO, color);
    }
    // 0 dBFS mark.
    let y0 = rect.bottom() - (60.0 / 66.0) * rect.height();
    painter.line_segment(
        [pos2(rect.left(), y0), pos2(rect.right(), y0)],
        Stroke::new(1.0, ui.visuals().weak_text_color()),
    );
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    latency: &LatencyInfo,
    meters: &MeterReadings,
    host: &mut dyn Host,
) -> MixerOutput {
    let mut out = MixerOutput::default();
    let plugins = host.plugin_effects();
    ui.horizontal(|ui| {
        ui.strong("Mixer");
        ui.label(format!(
            "· output latency {}",
            latency.format(latency.total)
        ));
        if latency.clamped {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "· delay compensation limit reached; some paths are not fully aligned",
            );
        }
    });
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for track in mixer.all_tracks() {
                strip(ui, mixer, track, latency, meters, &plugins, host, &mut out);
                ui.separator();
            }
            if mixer.tracks.len() < MAX_TRACKS && ui.button("+ Track").clicked() {
                out.actions.push(MixerAction::AddTrack);
            }
        });
    });
    out
}

#[allow(clippy::too_many_arguments)]
fn strip(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    t: &MixerTrack,
    latency: &LatencyInfo,
    meters: &MeterReadings,
    plugins: &[PluginDescriptor],
    host: &mut dyn Host,
    out: &mut MixerOutput,
) {
    ui.vertical(|ui| {
        ui.set_width(STRIP_WIDTH);
        ui.horizontal(|ui| {
            let name = ui
                .add(egui::Button::new(RichText::new(&t.name).strong()).truncate())
                .on_hover_text("Double-click to rename; right-click for more");
            if name.double_clicked() {
                out.actions.push(MixerAction::Rename(t.id));
            }
            if !t.is_master() {
                name.context_menu(|ui| {
                    if ui.button("Rename…").clicked() {
                        out.actions.push(MixerAction::Rename(t.id));
                        ui.close();
                    }
                    if ui.button("Remove track").clicked() {
                        out.edits.push((Edit::RemoveTrack(t.id), false));
                        ui.close();
                    }
                });
            }
            let mut muted = t.muted;
            if ui
                .toggle_value(&mut muted, "M")
                .on_hover_text("Mute")
                .changed()
            {
                out.edits.push((Edit::SetTrackMuted(t.id, muted), false));
            }
            if !t.is_master() {
                let mut solo = t.solo;
                if ui
                    .toggle_value(&mut solo, "S")
                    .on_hover_text("Solo (sources and the path to the master stay audible)")
                    .changed()
                {
                    out.edits.push((Edit::SetTrackSolo(t.id, solo), false));
                }
            }
        });

        // Inserts come first in the signal flow.
        for (index, ins) in t.inserts.iter().enumerate() {
            insert_ui(ui, mixer, t, ins, index, host, out);
        }
        if t.inserts.len() < MAX_INSERTS_PER_TRACK {
            ui.menu_button(
                format!("+ Effect ({}/{MAX_INSERTS_PER_TRACK})", t.inserts.len()),
                |ui| {
                    for kind in EffectKind::ALL {
                        if ui.button(kind.name()).clicked() {
                            out.actions.push(MixerAction::AddEffect(t.id, kind));
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.menu_button("VST3", |ui| {
                        if plugins.is_empty() {
                            ui.label("No plugins found. See Plugins.");
                        }
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for d in plugins {
                                    let label = format!("{} ({})", d.plugin.name, d.plugin.vendor);
                                    if ui.button(label).clicked() {
                                        out.actions.push(MixerAction::AddPlugin(t.id, d.clone()));
                                        ui.close();
                                    }
                                }
                            });
                    });
                },
            );
        }
        ui.separator();

        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = FADER_HEIGHT;
            let mut vol = t.volume;
            let r = ui.add(
                egui::Slider::new(&mut vol, 0.0..=MAX_VOLUME)
                    .vertical()
                    .show_value(false),
            );
            if r.changed() {
                out.edits.push((Edit::SetTrackVolume(t.id, vol), true));
            }
            out.end_gesture |= r.drag_stopped();
            let levels = if t.is_master() {
                meters.output
            } else {
                meters.bus(t.id)
            };
            meter(ui, FADER_HEIGHT, levels);
            ui.vertical(|ui| {
                ui.label(db_label(t.volume));
                ui.spacing_mut().slider_width = 80.0;
                let mut pan = t.pan;
                let r = ui
                    .add(egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false))
                    .on_hover_text(format!(
                        "Balance {:+.0} (double-click to center)",
                        t.pan * 100.0
                    ));
                if r.changed() {
                    out.edits.push((Edit::SetTrackPan(t.id, pan), true));
                }
                if r.double_clicked() {
                    out.edits.push((Edit::SetTrackPan(t.id, 0.0), false));
                }
                out.end_gesture |= r.drag_stopped();
                if let Some(l) = latency.track(t.id).filter(|l| *l > 0) {
                    ui.weak(format!("latency {}", latency.format(l)));
                }
            });
        });

        if t.is_master() {
            limiter_ui(ui, &mixer.limiter, meters.limiter_gain, out);
        } else {
            ui.horizontal(|ui| {
                ui.label("Out");
                egui::ComboBox::from_id_salt(("out", t.id.0))
                    .width(110.0)
                    .selected_text(track_name(mixer, t.main_out))
                    .show_ui(ui, |ui| {
                        for d in mixer.valid_destinations(t.id) {
                            if ui
                                .selectable_label(d == t.main_out, track_name(mixer, d))
                                .clicked()
                                && d != t.main_out
                            {
                                out.edits.push((Edit::SetTrackMainOut(t.id, d), false));
                            }
                        }
                    });
            });
        }

        ui.separator();
        sends_ui(ui, mixer, t, out);
    });
}

fn limiter_ui(ui: &mut egui::Ui, lim: &LimiterSettings, gain: f32, out: &mut MixerOutput) {
    let mut s = *lim;
    let mut changed = false;
    let mut stopped = false;
    ui.horizontal(|ui| {
        changed |= ui
            .checkbox(&mut s.enabled, "Safety limiter")
            .on_hover_text(
                "Sample-peak protection on the master output; not a mastering or true-peak limiter",
            )
            .changed();
        if s.enabled {
            let gr = if gain < 1.0 {
                -20.0 * gain.max(1e-6).log10()
            } else {
                0.0
            };
            ui.label(format!("GR {gr:.1} dB"));
        }
    });
    if s.enabled {
        ui.horizontal(|ui| {
            ui.label("Ceiling");
            let r = ui.add(
                egui::DragValue::new(&mut s.ceiling_db)
                    .range(LimiterSettings::CEILING_DB.0..=LimiterSettings::CEILING_DB.1)
                    .speed(0.05)
                    .suffix(" dBFS"),
            );
            changed |= r.changed();
            stopped |= r.drag_stopped() || r.lost_focus();
        });
        ui.horizontal(|ui| {
            ui.label("Release");
            let r = ui.add(
                egui::DragValue::new(&mut s.release_ms)
                    .range(LimiterSettings::RELEASE_MS.0..=LimiterSettings::RELEASE_MS.1)
                    .speed(1.0)
                    .suffix(" ms"),
            );
            changed |= r.changed();
            stopped |= r.drag_stopped() || r.lost_focus();
        });
    }
    if changed && s != *lim {
        out.edits.push((Edit::SetLimiter(s), true));
    }
    out.end_gesture |= stopped;
}

fn sends_ui(ui: &mut egui::Ui, mixer: &Mixer, t: &MixerTrack, out: &mut MixerOutput) {
    for s in &t.sends {
        ui.horizontal(|ui| {
            ui.label(format!("Send to {}", track_name(mixer, s.dest)));
            if ui.small_button("x").on_hover_text("Remove send").clicked() {
                out.edits.push((Edit::RemoveSend(s.id), false));
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = 90.0;
            let mut level = s.level;
            let r = ui
                .add(egui::Slider::new(&mut level, 0.0..=MAX_SEND_LEVEL).show_value(false))
                .on_hover_text(db_label(s.level));
            if r.changed() {
                out.edits.push((Edit::SetSendLevel(s.id, level), true));
            }
            out.end_gesture |= r.drag_stopped();
            let mut pre = s.pre_fader;
            if ui
                .toggle_value(&mut pre, "Pre")
                .on_hover_text("Pre-fader send (not affected by fader or mute)")
                .changed()
            {
                out.edits.push((Edit::SetSendPreFader(s.id, pre), false));
            }
        });
    }
    if t.sends.len() < MAX_SENDS_PER_TRACK {
        let dests: Vec<TrackId> = mixer
            .valid_destinations(t.id)
            .into_iter()
            .filter(|d| !t.sends.iter().any(|s| s.dest == *d))
            .collect();
        if !dests.is_empty() {
            ui.menu_button("+ Send", |ui| {
                for d in dests {
                    if ui.button(track_name(mixer, d)).clicked() {
                        out.actions.push(MixerAction::AddSend { from: t.id, to: d });
                        ui.close();
                    }
                }
            });
        }
    }
}

fn insert_ui(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    t: &MixerTrack,
    ins: &Insert,
    index: usize,
    host: &mut dyn Host,
    out: &mut MixerOutput,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(ins.effect.name()).strong());
        let mut on = !ins.bypass;
        if ui
            .toggle_value(&mut on, "On")
            .on_hover_text("Bypass crossfades to dry; the effect's tail is dropped")
            .changed()
        {
            out.edits.push((Edit::SetInsertBypass(ins.id, !on), false));
        }
        if ins.sidechain.is_some() {
            ui.colored_label(Color32::from_rgb(120, 170, 235), "SC");
        }
        if index > 0
            && ui
                .small_button("Up")
                .on_hover_text("Move earlier in the chain")
                .clicked()
        {
            out.edits.push((Edit::MoveInsert(ins.id, index - 1), false));
        }
        if index + 1 < t.inserts.len()
            && ui
                .small_button("Dn")
                .on_hover_text("Move later in the chain")
                .clicked()
        {
            out.edits.push((Edit::MoveInsert(ins.id, index + 1), false));
        }
        if ui
            .small_button("x")
            .on_hover_text("Remove effect")
            .clicked()
        {
            out.edits.push((Edit::RemoveInsert(ins.id), false));
        }
    });
    egui::CollapsingHeader::new("Parameters")
        .id_salt(("insert", ins.id.0))
        .default_open(false)
        .show(ui, |ui| {
            if ins.effect.kind == EffectKind::ParametricEq {
                eq_graph(ui, &ins.effect);
            }
            if ins.effect.kind == EffectKind::Plugin {
                plugin_params_ui(ui, ins, host);
            } else {
                params_ui(ui, ins, out);
            }
            if ins.effect.accepts_sidechain() {
                sidechain_ui(ui, mixer, t, ins, out);
            }
        });
}

/// Generic editor for a plugin (PLG-064): every parameter with the
/// plugin's own value text. Read only while the plugin is not loaded.
fn plugin_params_ui(ui: &mut egui::Ui, ins: &Insert, host: &mut dyn Host) {
    if let Some(slot) = &ins.effect.plugin {
        ui.small(format!("{} {} · VST3", slot.vendor, slot.version));
    }
    let Some(params) = host.plugin_params(ins.id) else {
        let why = host
            .plugin_problem(ins.id)
            .unwrap_or_else(|| "not loaded".into());
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("{why}. Audio passes through; the saved state is kept."),
        );
        return;
    };
    if params.is_empty() {
        ui.label("No parameters.");
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt(("plugin_params", ins.id.0))
        .max_height(240.0)
        .show(ui, |ui| {
            egui::Grid::new(("plugin_grid", ins.id.0))
                .num_columns(2)
                .show(ui, |ui| {
                    for p in &params {
                        plugin_param_row(ui, ins, p, host);
                        ui.end_row();
                    }
                });
        });
}

fn plugin_param_row(ui: &mut egui::Ui, ins: &Insert, p: &PluginParamView, host: &mut dyn Host) {
    let name = if p.info.units.is_empty() {
        p.info.name.clone()
    } else {
        format!("{} ({})", p.info.name, p.info.units)
    };
    ui.label(name);
    if p.info.flags.read_only {
        ui.label(&p.text);
        return;
    }
    ui.horizontal(|ui| {
        let steps = p.info.step_count;
        let mut v = p.value;
        let changed = if steps == 1 {
            let mut on = v >= 0.5;
            let r = ui.checkbox(&mut on, "");
            v = if on { 1.0 } else { 0.0 };
            r.changed()
        } else if steps > 1 {
            let mut k = (v * f64::from(steps)).round() as u32;
            let r = ui.add(egui::Slider::new(&mut k, 0..=steps).show_value(false));
            v = f64::from(k) / f64::from(steps);
            r.changed()
        } else {
            ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false))
                .changed()
        };
        ui.label(&p.text);
        if changed {
            host.set_plugin_param(ins.id, p.info.id, v);
        }
    });
}

fn params_ui(ui: &mut egui::Ui, ins: &Insert, out: &mut MixerOutput) {
    let effect = &ins.effect;
    let mut edited: Option<Effect> = None;
    let mut stopped = false;
    egui::Grid::new(("params", ins.id.0))
        .num_columns(2)
        .show(ui, |ui| {
            for spec in effect.kind.params() {
                let current = effect.get(spec.id);
                ui.label(spec.name);
                let mut v = current;
                let suffix = match spec.unit {
                    ParamUnit::Decibels => " dB",
                    ParamUnit::Hertz => " Hz",
                    ParamUnit::Milliseconds => " ms",
                    ParamUnit::Seconds => " s",
                    ParamUnit::Ratio => ":1",
                    ParamUnit::Fraction | ParamUnit::Q | ParamUnit::Toggle | ParamUnit::Choice => {
                        ""
                    }
                };
                let r = match spec.mapping {
                    Mapping::Toggle => {
                        let mut b = v >= 0.5;
                        let r = ui.checkbox(&mut b, "");
                        v = if b { 1.0 } else { 0.0 };
                        r
                    }
                    Mapping::Choice(labels) => {
                        let mut idx = v as usize;
                        let r = egui::ComboBox::from_id_salt(("choice", ins.id.0, spec.id))
                            .selected_text(labels.get(idx).copied().unwrap_or("?"))
                            .show_ui(ui, |ui| {
                                for (i, label) in labels.iter().enumerate() {
                                    ui.selectable_value(&mut idx, i, *label);
                                }
                            })
                            .response;
                        v = idx as f32;
                        r
                    }
                    Mapping::Log | Mapping::Linear => {
                        let fraction = spec.unit == ParamUnit::Fraction;
                        let mut slider = egui::Slider::new(&mut v, spec.min..=spec.max)
                            .logarithmic(spec.mapping == Mapping::Log)
                            .suffix(suffix);
                        if fraction {
                            slider = slider.custom_formatter(|x, _| format!("{:.0}%", x * 100.0));
                        }
                        ui.add(slider)
                    }
                };
                stopped |= r.drag_stopped() || r.lost_focus();
                if v != current {
                    edited = Some(
                        edited
                            .take()
                            .unwrap_or_else(|| effect.clone())
                            .with(spec.id, v),
                    );
                }
                ui.end_row();
            }
        });
    if let Some(e) = edited {
        out.edits.push((Edit::SetInsertEffect(ins.id, e), true));
    }
    out.end_gesture |= stopped;
}

fn sidechain_ui(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    t: &MixerTrack,
    ins: &Insert,
    out: &mut MixerOutput,
) {
    ui.horizontal(|ui| {
        ui.label("Sidechain");
        let current = ins.sidechain.map(|s| s.source);
        let label = current.map_or("None".to_owned(), |s| track_name(mixer, s));
        egui::ComboBox::from_id_salt(("sc", ins.id.0))
            .width(90.0)
            .selected_text(label)
            .show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), "None").clicked() && current.is_some() {
                    out.edits
                        .push((Edit::SetInsertSidechain(ins.id, None), false));
                }
                for s in mixer.valid_sidechain_sources(t.id) {
                    if ui
                        .selectable_label(current == Some(s), track_name(mixer, s))
                        .clicked()
                        && current != Some(s)
                    {
                        let tap = ins.sidechain.map_or(Tap::PreFader, |sc| sc.tap);
                        out.edits.push((
                            Edit::SetInsertSidechain(ins.id, Some(Sidechain { source: s, tap })),
                            false,
                        ));
                    }
                }
            });
    });
    if let Some(sc) = ins.sidechain {
        let mut post = sc.tap == Tap::PostFader;
        if ui
            .checkbox(&mut post, "Post-fader tap")
            .on_hover_text("Take the source after its fader instead of before")
            .changed()
        {
            let tap = if post { Tap::PostFader } else { Tap::PreFader };
            out.edits.push((
                Edit::SetInsertSidechain(ins.id, Some(Sidechain { tap, ..sc })),
                false,
            ));
        }
    }
}

/// Frequency response of the EQ settings, 20 Hz–20 kHz, ±24 dB.
fn eq_graph(ui: &mut egui::Ui, effect: &Effect) {
    const SR: u32 = 48_000;
    let mut eq = jdance_dsp::ParametricEq::new();
    for (i, spec) in EffectKind::ParametricEq.params().iter().enumerate() {
        eq.set_param(i, effect.get(spec.id));
    }
    eq.prepare(SR, 64);
    eq.settle();
    let (rect, _) = ui.allocate_exact_size(vec2(STRIP_WIDTH - 10.0, 70.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(3), ui.visuals().extreme_bg_color);
    let to_y = |db: f64| rect.center().y - (db / 24.0) as f32 * (rect.height() / 2.0 - 2.0);
    painter.line_segment(
        [pos2(rect.left(), to_y(0.0)), pos2(rect.right(), to_y(0.0))],
        Stroke::new(1.0, ui.visuals().weak_text_color()),
    );
    let points: Vec<egui::Pos2> = (0..=100)
        .map(|i| {
            let t = i as f64 / 100.0;
            let f = 20.0 * 1000f64.powf(t);
            let db = eq.response_db(f).clamp(-24.0, 24.0);
            pos2(rect.left() + t as f32 * rect.width(), to_y(db))
        })
        .collect();
    painter.add(egui::Shape::line(
        points,
        Stroke::new(1.5, Color32::from_rgb(235, 120, 60)),
    ));
    painter.rect_stroke(
        rect,
        CornerRadius::same(3),
        ui.visuals().widgets.noninteractive.bg_stroke,
        StrokeKind::Inside,
    );
}
