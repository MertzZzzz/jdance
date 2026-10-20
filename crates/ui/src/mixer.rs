// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer panel: one strip per track (MIX-001…MIX-034).
//!
//! Like the rack, drawing only collects edits; routing menus offer only
//! destinations that keep the graph acyclic (MIX-021), and the commands
//! layer validates again.

use crate::LatencyInfo;
use egui::{Color32, RichText};
use jdance_commands::Edit;
use jdance_domain::{
    CompressorParams, Effect, Insert, MAX_INSERTS_PER_TRACK, MAX_SEND_LEVEL, MAX_SENDS_PER_TRACK,
    MAX_TRACKS, MAX_VOLUME, Mixer, MixerTrack, Sidechain, Tap, TrackId,
};

const STRIP_WIDTH: f32 = 170.0;

/// Mixer changes that need new IDs, applied by the caller.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MixerAction {
    AddTrack,
    AddSend { from: TrackId, to: TrackId },
    AddCompressor(TrackId),
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

pub(crate) fn show(ui: &mut egui::Ui, mixer: &Mixer, latency: &LatencyInfo) -> MixerOutput {
    let mut out = MixerOutput::default();
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
                strip(ui, mixer, track, latency, &mut out);
                ui.separator();
            }
            if mixer.tracks.len() < MAX_TRACKS && ui.button("+ Track").clicked() {
                out.actions.push(MixerAction::AddTrack);
            }
        });
    });
    out
}

fn strip(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    t: &MixerTrack,
    latency: &LatencyInfo,
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
        });

        ui.horizontal(|ui| {
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

        if !t.is_master() {
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
        for ins in &t.inserts {
            insert_ui(ui, mixer, t, ins, out);
        }
        if t.inserts.len() < MAX_INSERTS_PER_TRACK && ui.small_button("+ Compressor").clicked() {
            out.actions.push(MixerAction::AddCompressor(t.id));
        }

        ui.separator();
        for s in &t.sends {
            ui.horizontal(|ui| {
                ui.label(format!("→ {}", track_name(mixer, s.dest)));
                if ui.small_button("x").on_hover_text("Remove send").clicked() {
                    out.edits.push((Edit::RemoveSend(s.id), false));
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = 80.0;
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
    });
}

fn insert_ui(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    t: &MixerTrack,
    ins: &Insert,
    out: &mut MixerOutput,
) {
    let Effect::Compressor(p) = &ins.effect;
    let header = egui::CollapsingHeader::new("Parameters")
        .id_salt(("insert", ins.id.0))
        .default_open(false);
    ui.horizontal(|ui| {
        ui.label(RichText::new(ins.effect.name()).strong());
        let mut on = !ins.bypass;
        if ui
            .toggle_value(&mut on, "On")
            .on_hover_text("Bypass keeps the effect's latency")
            .changed()
        {
            out.edits.push((Edit::SetInsertBypass(ins.id, !on), false));
        }
        if ins.sidechain.is_some() {
            ui.colored_label(Color32::from_rgb(120, 170, 235), "SC");
        }
        if ui
            .small_button("x")
            .on_hover_text("Remove effect")
            .clicked()
        {
            out.edits.push((Edit::RemoveInsert(ins.id), false));
        }
    });
    header.show(ui, |ui| {
        let mut q = *p;
        let mut changed = false;
        let mut stopped = false;
        let mut field = |ui: &mut egui::Ui,
                         label: &str,
                         v: &mut f32,
                         range: (f32, f32),
                         suffix: &str,
                         speed: f64| {
            ui.horizontal(|ui| {
                ui.label(label);
                let r = ui.add(
                    egui::DragValue::new(v)
                        .range(range.0..=range.1)
                        .speed(speed)
                        .suffix(suffix),
                );
                changed |= r.changed();
                stopped |= r.drag_stopped() || r.lost_focus();
            });
        };
        field(
            ui,
            "Threshold",
            &mut q.threshold_db,
            CompressorParams::THRESHOLD_DB,
            " dB",
            0.2,
        );
        field(
            ui,
            "Ratio",
            &mut q.ratio,
            CompressorParams::RATIO,
            ":1",
            0.05,
        );
        field(
            ui,
            "Attack",
            &mut q.attack_ms,
            CompressorParams::ATTACK_MS,
            " ms",
            0.2,
        );
        field(
            ui,
            "Release",
            &mut q.release_ms,
            CompressorParams::RELEASE_MS,
            " ms",
            1.0,
        );
        field(
            ui,
            "Makeup",
            &mut q.makeup_db,
            CompressorParams::MAKEUP_DB,
            " dB",
            0.1,
        );
        field(
            ui,
            "Lookahead",
            &mut q.lookahead_ms,
            CompressorParams::LOOKAHEAD_MS,
            " ms",
            0.05,
        );
        if changed && q != *p {
            out.edits
                .push((Edit::SetInsertEffect(ins.id, Effect::Compressor(q)), true));
        }
        out.end_gesture |= stopped;

        ui.horizontal(|ui| {
            ui.label("Sidechain");
            let current = ins.sidechain.map(|s| s.source);
            let label = current.map_or("None".to_owned(), |s| track_name(mixer, s));
            egui::ComboBox::from_id_salt(("sc", ins.id.0))
                .width(90.0)
                .selected_text(label)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(current.is_none(), "None").clicked() && current.is_some()
                    {
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
                                Edit::SetInsertSidechain(
                                    ins.id,
                                    Some(Sidechain { source: s, tap }),
                                ),
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
    });
}
