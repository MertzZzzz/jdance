// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer (UI-VIS §8): vertical strips (number, name and meter, mute,
//! pan, fader, delay mark, route switch) and the inspector of the
//! selected track (effect slots, output or limiter, sends). Effect controls are generated from each effect's
//! parameter schema. Drawing only collects edits; routing menus offer
//! only destinations that keep the graph acyclic (MIX-021).

use crate::automation::{self, AutomationRequest};
use crate::{Host, LatencyInfo, MeterReadings, PluginParamView};
use egui::{Color32, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use jdance_commands::Edit;
use jdance_domain::{
    AutomationLane, AutomationTarget, Effect, EffectKind, Insert, LimiterSettings,
    MAX_INSERTS_PER_TRACK, MAX_SEND_LEVEL, MAX_SENDS_PER_TRACK, MAX_TRACKS, MAX_VOLUME, Mapping,
    Mixer, MixerTrack, ParamUnit, PluginTarget, Sidechain, Tap, TrackId, TrackInput,
};
use jdance_dsp::Processor;
use jdance_plugin_api::PluginDescriptor;

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
    pub automation: Vec<AutomationRequest>,
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

/// Strip widths of UI-VIS §8.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StripView {
    #[default]
    Compact,
    Wide,
    WideFx,
}

impl StripView {
    fn width(self) -> f32 {
        match self {
            Self::Compact => 56.0,
            Self::Wide => 72.0,
            Self::WideFx => 96.0,
        }
    }

    fn fader(self) -> f32 {
        match self {
            Self::Compact => 120.0,
            _ => 180.0,
        }
    }
}

/// Mixer view state kept by the app: the selected track and the strip
/// width.
#[derive(Debug)]
pub(crate) struct MixerView {
    pub selected: TrackId,
    pub view: StripView,
    /// The feedback warning was confirmed this session (MON-002).
    monitor_warned: bool,
    /// Track waiting for the feedback warning before monitoring.
    confirm_monitor: Option<TrackId>,
}

impl Default for MixerView {
    fn default() -> Self {
        Self {
            selected: TrackId::MASTER,
            view: StripView::Compact,
            monitor_warned: false,
            confirm_monitor: None,
        }
    }
}

/// The headphones toggle of a track with an input (MON-001); the first
/// time in a session it asks about feedback first (MON-002).
fn monitor_toggle(ui: &mut egui::Ui, t: &MixerTrack, view: &mut MixerView, out: &mut MixerOutput) {
    let Some(input) = t.input.filter(|_| !t.is_master()) else {
        return;
    };
    let mut on = t.monitor;
    let r = ui
        .toggle_value(
            &mut on,
            RichText::new(format!("Mon · {}", input.short_label())).size(10.0),
        )
        .on_hover_text("Hear the input through this track (its effects, fader and sends)");
    if r.changed() {
        if on && !view.monitor_warned {
            view.confirm_monitor = Some(t.id);
        } else {
            out.edits.push((Edit::SetTrackMonitor(t.id, on), false));
        }
    }
}

/// The feedback warning before the first monitoring (MON-002, MON-020).
fn monitor_warning(
    ctx: &egui::Context,
    host: &mut dyn Host,
    view: &mut MixerView,
    out: &mut MixerOutput,
) {
    let Some(id) = view.confirm_monitor else {
        return;
    };
    egui::Modal::new(egui::Id::unique("monitor_warning")).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.heading("Monitor the input?");
        ui.label(
            "With speakers and an open microphone this can feed back loudly. Use headphones.",
        );
        let direct = host.recording_settings().monitor;
        if direct {
            ui.colored_label(
                crate::theme::WARN,
                "The direct monitor of Options › Recording turns off, so the input is not heard twice.",
            );
        }
        ui.horizontal(|ui| {
            if ui.button("Turn monitoring on").clicked() {
                if direct {
                    let mut s = host.recording_settings();
                    s.monitor = false;
                    let _ = host.set_recording_settings(s);
                }
                out.edits.push((Edit::SetTrackMonitor(id, true), false));
                view.monitor_warned = true;
                view.confirm_monitor = None;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                view.confirm_monitor = None;
            }
        });
    });
}

const INSPECTOR_W: f32 = 300.0;
const SCALE_W: f32 = 28.0;

/// The mixer (UI-VIS §8): a band of vertical strips (master first) and
/// the inspector of the selected track on the right.
pub(crate) fn show(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    automation: &[AutomationLane],
    latency: &LatencyInfo,
    meters: &MeterReadings,
    host: &mut dyn Host,
    view: &mut MixerView,
) -> MixerOutput {
    let mut out = MixerOutput::default();
    if mixer.track(view.selected).is_none() {
        view.selected = TrackId::MASTER;
    }
    let plugins = host.plugin_effects();
    ui.horizontal(|ui| {
        ui.strong("Mixer");
        for (v, name) in [
            (StripView::Compact, "Compact"),
            (StripView::Wide, "Wide"),
            (StripView::WideFx, "Wide + FX"),
        ] {
            ui.selectable_value(&mut view.view, v, name);
        }
        if mixer.tracks.len() < MAX_TRACKS && ui.button("+ Track").clicked() {
            out.actions.push(MixerAction::AddTrack);
        }
        ui.label(
            RichText::new(format!("output latency {}", latency.format(latency.total)))
                .color(crate::theme::MUTED),
        );
        if latency.clamped {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "delay compensation limit reached; some paths are not fully aligned",
            );
        }
    });
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        let band_w = (ui.available_width() - INSPECTOR_W - 12.0).max(200.0);
        ui.vertical(|ui| {
            ui.set_width(band_w);
            egui::ScrollArea::horizontal()
                .id_salt("mixer_strips")
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        scale(ui, view.view);
                        for (i, track) in mixer.all_tracks().enumerate() {
                            strip(
                                ui, mixer, automation, track, i, latency, meters, view, &mut out,
                            );
                        }
                    });
                });
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.set_width(INSPECTOR_W);
            egui::ScrollArea::vertical()
                .id_salt("mixer_inspector")
                .max_height(height)
                .show(ui, |ui| {
                    if let Some(t) = mixer.track(view.selected) {
                        inspector(
                            ui, mixer, automation, t, latency, meters, &plugins, host, view,
                            &mut out,
                        );
                    }
                });
        });
    });
    monitor_warning(ui.ctx(), host, view, &mut out);
    out
}

/// dB scale left of the master (UI-VIS-091), aligned with the strips'
/// peak meters (in the name block).
fn scale(ui: &mut egui::Ui, view: StripView) {
    let (rect, _) = ui.allocate_exact_size(vec2(SCALE_W, STRIP_TOP + view.fader()), Sense::hover());
    let p = ui.painter();
    // The meter rectangle of a strip: see `strip`.
    let (top, h) = (rect.top() + 16.0 + 6.0, 100.0 - 8.0);
    for db in [6.0f32, 0.0, -6.0, -12.0, -24.0, -36.0, -48.0] {
        let y = top + h * (1.0 - (db + 60.0) / 66.0);
        p.text(
            pos2(rect.right() - 2.0, y),
            egui::Align2::RIGHT_CENTER,
            if db == 0.0 {
                "0".to_owned()
            } else {
                format!("{db:+.0}")
            },
            crate::theme::mono(9.5),
            crate::theme::MUTED,
        );
    }
    p.text(
        pos2(rect.right() - 2.0, top + h),
        egui::Align2::RIGHT_BOTTOM,
        "∞",
        crate::theme::mono(9.5),
        crate::theme::MUTED,
    );
}

/// Height above the fader: number, name and meter, lamp, pan.
const STRIP_TOP: f32 = 16.0 + 100.0 + 18.0 + 30.0;

#[allow(clippy::too_many_arguments)]
fn strip(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    automation: &[AutomationLane],
    t: &MixerTrack,
    index: usize,
    latency: &LatencyInfo,
    meters: &MeterReadings,
    view: &mut MixerView,
    out: &mut MixerOutput,
) {
    use crate::theme;
    let width = view.view.width();
    let selected = view.selected == t.id;
    let color = theme::rgb(jdance_domain::color_or_default(t.color, index));
    let frame = egui::Frame::new()
        .fill(if selected {
            theme::BLUE_BG.gamma_multiply(0.3)
        } else {
            theme::PANEL
        })
        .corner_radius(CornerRadius::same(4));
    frame.show(ui, |ui| {
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            // 1. Number.
            let number = if t.is_master() {
                "M".to_owned()
            } else {
                format!("{index}")
            };
            let (r, resp) = ui.allocate_exact_size(vec2(width, 16.0), Sense::click());
            if selected {
                ui.painter().rect_filled(r, 3.0, theme::BLUE_BG);
            }
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                number,
                theme::mono(11.0),
                theme::TEXT,
            );
            // A track feeding the selected one gets an amber dot.
            let feeds = !selected
                && (t.main_out == view.selected || t.sends.iter().any(|s| s.dest == view.selected));
            if feeds {
                ui.painter()
                    .circle_filled(pos2(r.right() - 6.0, r.center().y), 2.5, theme::AMBER);
            }
            if resp.clicked() {
                view.selected = t.id;
            }
            // 2. Name (rotated) and peak meter.
            let (r, resp) = ui.allocate_exact_size(vec2(width, 100.0), Sense::click());
            let p = ui.painter();
            p.rect_filled(
                Rect::from_min_size(r.min, vec2(r.width(), 3.0)),
                CornerRadius::ZERO,
                color,
            );
            let galley = p.layout_no_wrap(
                t.name.clone(),
                egui::FontId::proportional(12.0),
                theme::TEXT,
            );
            let text = egui::epaint::TextShape::new(
                pos2(r.left() + 6.0, r.bottom() - 4.0),
                galley,
                theme::TEXT,
            )
            .with_angle(-std::f32::consts::FRAC_PI_2);
            p.with_clip_rect(r).add(text);
            let levels = if t.is_master() {
                meters.output
            } else {
                meters.bus(t.id)
            };
            let mr = Rect::from_min_size(
                pos2(r.right() - 14.0, r.top() + 6.0),
                vec2(10.0, r.height() - 8.0),
            );
            draw_meter(ui.painter(), mr, levels, t.muted);
            let resp = resp.on_hover_text(format!("{} · double-click or F2 to rename", t.name));
            if resp.clicked() {
                view.selected = t.id;
            }
            if resp.double_clicked() {
                out.actions.push(MixerAction::Rename(t.id));
            }
            resp.context_menu(|ui| {
                if ui.button("Rename…").clicked() {
                    out.actions.push(MixerAction::Rename(t.id));
                    ui.close();
                }
                if !t.is_master() && ui.button("Remove track").clicked() {
                    out.edits.push((Edit::RemoveTrack(t.id), false));
                    ui.close();
                }
            });
            // 3. Mute lamp (right-click: solo).
            ui.horizontal(|ui| {
                ui.add_space((width - 20.0) / 2.0);
                let lamp = crate::widgets::lamp(ui, !t.muted).on_hover_text(if t.is_master() {
                    "Mute"
                } else {
                    "Mute (click) · solo (right-click)"
                });
                if lamp.clicked() {
                    out.edits.push((Edit::SetTrackMuted(t.id, !t.muted), false));
                }
                if lamp.secondary_clicked() && !t.is_master() {
                    out.edits.push((Edit::SetTrackSolo(t.id, !t.solo), false));
                }
                if t.solo {
                    ui.painter().text(
                        lamp.rect.right_top(),
                        egui::Align2::LEFT_TOP,
                        "S",
                        theme::mono(9.0),
                        theme::AMBER,
                    );
                }
            });
            // Input and monitoring (INP-001, MON-001).
            if t.input.is_some() {
                ui.horizontal(|ui| monitor_toggle(ui, t, view, out));
            }
            // 4. Pan.
            ui.horizontal(|ui| {
                ui.add_space((width - 26.0) / 2.0);
                let mut pan = t.pan;
                let r = crate::widgets::knob(ui, &mut pan, -1.0..=1.0, 0.0, theme::BLUE, 26.0)
                    .on_hover_text(format!("Pan {:+.0} (double-click: centre)", t.pan * 100.0));
                if r.changed() {
                    out.edits.push((Edit::SetTrackPan(t.id, pan), true));
                    view.selected = t.id;
                }
                automation::attach(
                    ui,
                    &r,
                    automation,
                    AutomationTarget::TrackPan(t.id),
                    (t.pan + 1.0) / 2.0,
                    &mut out.automation,
                );
                out.end_gesture |= r.drag_stopped();
            });
            ui.add_space(4.0);
            // 6. Fader.
            ui.horizontal(|ui| {
                ui.add_space((width - 22.0) / 2.0);
                ui.spacing_mut().slider_width = view.view.fader();
                let mut vol = t.volume;
                let r = ui.add(
                    egui::Slider::new(&mut vol, 0.0..=MAX_VOLUME)
                        .vertical()
                        .show_value(false),
                );
                let r = r.on_hover_text(db_label(t.volume));
                if r.changed() && !automation::secondary_press(ui) {
                    out.edits.push((Edit::SetTrackVolume(t.id, vol), true));
                }
                if r.clicked() || r.drag_started() {
                    view.selected = t.id;
                }
                automation::attach(
                    ui,
                    &r,
                    automation,
                    AutomationTarget::TrackVolume(t.id),
                    t.volume / MAX_VOLUME,
                    &mut out.automation,
                );
                out.end_gesture |= r.drag_stopped();
            });
            if view.view != StripView::Compact {
                ui.label(
                    RichText::new(format!("{}\n{:+.0}", db_label(t.volume), t.pan * 100.0))
                        .font(theme::mono(9.5))
                        .color(theme::MUTED),
                );
            }
            ui.add_space(4.0);
            // 8. PDC hourglass and 9/10. route from the selected track.
            ui.horizontal(|ui| {
                let lat = latency.track(t.id).unwrap_or(0);
                let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                hourglass(
                    ui.painter(),
                    r,
                    if lat > 0 { theme::WARN } else { theme::MUTED },
                );
                resp.on_hover_text(if lat > 0 {
                    format!("Plugin delay {}", latency.format(lat))
                } else {
                    "No plugin delay".to_owned()
                });
                if !selected {
                    route_toggle(ui, mixer, view.selected, t, out);
                }
            });
            if let Some(src) = mixer.track(view.selected)
                && let Some(send) = src.sends.iter().find(|s| s.dest == t.id)
            {
                ui.horizontal(|ui| {
                    ui.add_space((width - 20.0) / 2.0);
                    let mut level = send.level;
                    let r = crate::widgets::knob(
                        ui,
                        &mut level,
                        0.0..=MAX_SEND_LEVEL,
                        1.0,
                        theme::AMBER,
                        20.0,
                    )
                    .on_hover_text(format!(
                        "Send from {}: {}",
                        src.name,
                        db_label(send.level)
                    ));
                    if r.changed() {
                        out.edits.push((Edit::SetSendLevel(send.id, level), true));
                    }
                    out.end_gesture |= r.drag_stopped();
                });
            }
            if view.view == StripView::WideFx {
                ui.add_space(4.0);
                for ins in &t.inserts {
                    let name = ins.effect.name();
                    let c = if ins.bypass {
                        theme::MUTED
                    } else {
                        theme::TEXT
                    };
                    ui.add(egui::Label::new(RichText::new(name).size(10.5).color(c)).truncate());
                }
            }
        });
    });
}

/// Sand-glass PDC mark.
fn hourglass(p: &egui::Painter, r: Rect, color: Color32) {
    let s = Stroke::new(1.2, color);
    let (l, rt, t, b) = (
        r.left() + 4.0,
        r.right() - 4.0,
        r.top() + 2.0,
        r.bottom() - 2.0,
    );
    p.line_segment([pos2(l, t), pos2(rt, t)], s);
    p.line_segment([pos2(l, b), pos2(rt, b)], s);
    p.line_segment([pos2(l, t), pos2(rt, b)], s);
    p.line_segment([pos2(rt, t), pos2(l, b)], s);
}

/// Route switch on strip `t` for a send from the selected track
/// (UI-VIS-092 №9): filled amber = send, outline = none.
fn route_toggle(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    from: TrackId,
    t: &MixerTrack,
    out: &mut MixerOutput,
) {
    use crate::theme;
    let Some(src) = mixer.track(from) else {
        return;
    };
    let send = src.sends.iter().find(|s| s.dest == t.id);
    let main = src.main_out == t.id && from != TrackId::MASTER;
    let valid = mixer.valid_destinations(from).contains(&t.id);
    let (r, resp) = ui.allocate_exact_size(vec2(20.0, 16.0), Sense::click());
    let tri = vec![
        pos2(r.center().x, r.top() + 2.0),
        pos2(r.right() - 3.0, r.bottom() - 2.0),
        pos2(r.left() + 3.0, r.bottom() - 2.0),
    ];
    let p = ui.painter();
    if send.is_some() || main {
        p.add(egui::Shape::convex_polygon(tri, theme::AMBER, Stroke::NONE));
    } else {
        let c = if valid { theme::MUTED } else { theme::LINE };
        p.add(egui::Shape::closed_line(tri, Stroke::new(1.2, c)));
    }
    let tip = if main {
        format!("{} goes to {} (its output)", src.name, t.name)
    } else if send.is_some() {
        format!("Send {} → {}: click to remove", src.name, t.name)
    } else if valid {
        format!("Click: send {} → {}", src.name, t.name)
    } else {
        "Would create a loop".to_owned()
    };
    let resp = resp.on_hover_text(tip);
    if resp.clicked() && !main {
        match send {
            Some(s) => out.edits.push((Edit::RemoveSend(s.id), false)),
            None if valid && src.sends.len() < MAX_SENDS_PER_TRACK => {
                out.actions.push(MixerAction::AddSend { from, to: t.id });
            }
            None => {}
        }
    }
    resp.context_menu(|ui| {
        if valid && from != TrackId::MASTER && ui.button("Route only here").clicked() {
            out.edits.push((Edit::SetTrackMainOut(from, t.id), false));
            for s in &src.sends {
                out.edits.push((Edit::RemoveSend(s.id), false));
            }
            ui.close();
        }
    });
}

/// Peak meter in `r`; −60..+6 dB, red above 0 dBFS, half bright when
/// muted.
fn draw_meter(p: &egui::Painter, r: Rect, (l, rr): (f32, f32), muted: bool) {
    p.rect_filled(r, CornerRadius::same(2), crate::theme::FIELD);
    let frac = |v: f32| {
        if v <= 0.0 {
            0.0
        } else {
            ((20.0 * v.log10() + 60.0) / 66.0).clamp(0.0, 1.0)
        }
    };
    let alpha = if muted { 0.5 } else { 1.0 };
    for (i, v) in [l, rr].into_iter().enumerate() {
        let x0 = r.left() + 1.0 + i as f32 * (r.width() / 2.0);
        let top = r.bottom() - frac(v) * r.height();
        let bar = Rect::from_min_max(pos2(x0, top), pos2(x0 + r.width() / 2.0 - 1.5, r.bottom()));
        let color = if v > 1.0 {
            crate::theme::RED
        } else if v > 0.5 {
            crate::theme::AMBER
        } else {
            crate::theme::GREEN
        };
        p.rect_filled(bar, CornerRadius::ZERO, color.gamma_multiply(alpha));
    }
}

/// Input of a track (INP-001/002) with its monitoring and delay (MON-004).
fn input_ui(
    ui: &mut egui::Ui,
    t: &MixerTrack,
    latency: &LatencyInfo,
    host: &mut dyn Host,
    view: &mut MixerView,
    out: &mut MixerOutput,
) {
    let live = host.live_inputs();
    let (count, names) = live.clone().unwrap_or_default();
    let label = |i: TrackInput| {
        let name = |c: u16| names.get(usize::from(c)).filter(|n| !n.is_empty());
        let base = i.short_label();
        match i {
            TrackInput::Mono(c) => name(c).map_or(base.clone(), |n| format!("{base} ({n})")),
            TrackInput::Stereo(c) => match (name(c), name(c + 1)) {
                (Some(a), Some(b)) => format!("{base} ({a}/{b})"),
                _ => base,
            },
        }
    };
    ui.horizontal(|ui| {
        ui.label("Input");
        let missing = t.input.is_some_and(|i| i.needed() > count);
        let text = match t.input {
            None => RichText::new("(none)"),
            Some(i) if missing => RichText::new(format!("{} (not on device)", i.short_label()))
                .color(crate::theme::WARN),
            Some(i) => RichText::new(label(i)),
        };
        let combo = egui::ComboBox::from_id_salt(("track_input", t.id.0))
            .width(170.0)
            .selected_text(text)
            .show_ui(ui, |ui| {
                let mut choose = |ui: &mut egui::Ui, v: Option<TrackInput>, text: String| {
                    if ui.selectable_label(t.input == v, text).clicked() && t.input != v {
                        out.edits.push((Edit::SetTrackInput(t.id, v), false));
                    }
                };
                choose(ui, None, "(none)".into());
                for c in (0..count.saturating_sub(1)).step_by(2) {
                    choose(
                        ui,
                        Some(TrackInput::Stereo(c)),
                        label(TrackInput::Stereo(c)),
                    );
                }
                for c in 0..count {
                    choose(ui, Some(TrackInput::Mono(c)), label(TrackInput::Mono(c)));
                }
            })
            .response;
        if live.is_none() {
            combo.on_hover_text(
                "No input is open: choose the input device in Options › Recording; tracks use \
                 its channels",
            );
        }
    });
    if t.input.is_some() {
        ui.horizontal(|ui| {
            monitor_toggle(ui, t, view, out);
            if t.monitor
                && let Some(s) = host.stream()
            {
                // Input + output of the driver (or two buffers), plus the
                // track's plugin delay (MON-004).
                let rate = f64::from(s.info.sample_rate.max(1));
                let (frames, estimate) = match s.info.latency {
                    Some((i, o)) => (u64::from(i) + u64::from(o), false),
                    None => (2 * u64::from(s.stats.last_frames.unwrap_or(0)), true),
                };
                let pdc = u64::from(latency.track(t.id).unwrap_or(0));
                let ms = (frames + pdc) as f64 * 1000.0 / rate;
                let text = if estimate {
                    format!("≈ {ms:.1} ms (estimate)")
                } else {
                    format!("{ms:.1} ms")
                };
                ui.label(RichText::new(text).color(crate::theme::MUTED))
                    .on_hover_text(
                        "Delay of what you hear: input, output and this track's plugins",
                    );
            }
        });
    }
}

/// Inspector of the selected track (UI-VIS-093): input, effect slots,
/// output, sends.
#[allow(clippy::too_many_arguments)]
fn inspector(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    automation: &[AutomationLane],
    t: &MixerTrack,
    latency: &LatencyInfo,
    meters: &MeterReadings,
    plugins: &[PluginDescriptor],
    host: &mut dyn Host,
    view: &mut MixerView,
    out: &mut MixerOutput,
) {
    ui.label(RichText::new(format!("Mixer › {}", t.name)).strong());
    if !t.is_master() {
        input_ui(ui, t, latency, host, view, out);
    }
    ui.separator();
    ui.label(RichText::new("Effects").color(crate::theme::MUTED));
    for (index, ins) in t.inserts.iter().enumerate() {
        insert_ui(ui, mixer, automation, t, ins, index, host, out);
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
    if t.is_master() {
        limiter_ui(ui, &mixer.limiter, meters.limiter_gain, out);
    } else {
        ui.horizontal(|ui| {
            ui.label("Output");
            egui::ComboBox::from_id_salt(("out", t.id.0))
                .width(140.0)
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
        ui.separator();
        sends_ui(ui, mixer, automation, t, out);
    }
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

fn sends_ui(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    automation: &[AutomationLane],
    t: &MixerTrack,
    out: &mut MixerOutput,
) {
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
            if r.changed() && !automation::secondary_press(ui) {
                out.edits.push((Edit::SetSendLevel(s.id, level), true));
            }
            automation::attach(
                ui,
                &r,
                automation,
                AutomationTarget::SendLevel(s.id),
                s.level / MAX_SEND_LEVEL,
                &mut out.automation,
            );
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

#[allow(clippy::too_many_arguments)]
fn insert_ui(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    automation: &[AutomationLane],
    t: &MixerTrack,
    ins: &Insert,
    index: usize,
    host: &mut dyn Host,
    out: &mut MixerOutput,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(ins.effect.name()).strong());
        let mut on = !ins.bypass;
        let r = ui
            .toggle_value(&mut on, "On")
            .on_hover_text("Bypass crossfades to dry; the effect's tail is dropped");
        if r.changed() {
            out.edits.push((Edit::SetInsertBypass(ins.id, !on), false));
        }
        automation::attach(
            ui,
            &r,
            automation,
            AutomationTarget::InsertBypass(ins.id),
            if ins.bypass { 1.0 } else { 0.0 },
            &mut out.automation,
        );
        if ins.sidechain.is_some() {
            ui.colored_label(Color32::from_rgb(120, 170, 235), "SC");
        }
        if ins.effect.kind == EffectKind::Plugin {
            let open = host.plugin_editor_open(PluginTarget::Insert(ins.id));
            if ui
                .selectable_label(open, "Edit")
                .on_hover_text("Open the plugin's own editor window")
                .clicked()
            {
                let title = format!("{} — {}", ins.effect.name(), t.name);
                host.toggle_plugin_editor(PluginTarget::Insert(ins.id), &title);
            }
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
                plugin_params_ui(
                    ui,
                    PluginTarget::Insert(ins.id),
                    ins.effect.plugin.as_ref(),
                    automation,
                    host,
                    &mut out.automation,
                );
            } else {
                params_ui(ui, ins, automation, out);
            }
            if ins.effect.accepts_sidechain() {
                sidechain_ui(ui, mixer, t, ins, out);
            }
        });
}

/// Generic editor for a plugin (PLG-064): every parameter with the
/// plugin's own value text. Read only while the plugin is not loaded.
pub(crate) fn plugin_params_ui(
    ui: &mut egui::Ui,
    target: PluginTarget,
    slot: Option<&jdance_domain::PluginSlot>,
    automation: &[AutomationLane],
    host: &mut dyn Host,
    requests: &mut Vec<AutomationRequest>,
) {
    if let Some(slot) = slot {
        ui.small(format!("{} {} · VST3", slot.vendor, slot.version));
    }
    let Some(params) = host.plugin_params(target) else {
        let why = host
            .plugin_problem(target)
            .unwrap_or_else(|| "not loaded".into());
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("{why}. Audio passes through; the saved state is kept."),
        );
        if host.plugin_held_back(target) && ui.button("Load again").clicked() {
            host.retry_plugin(target);
        }
        return;
    };
    if params.is_empty() {
        ui.label("No parameters.");
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt(("plugin_params", target))
        .max_height(240.0)
        .show(ui, |ui| {
            egui::Grid::new(("plugin_grid", target))
                .num_columns(2)
                .show(ui, |ui| {
                    for p in &params {
                        plugin_param_row(ui, target, p, automation, host, requests);
                        ui.end_row();
                    }
                });
        });
}

fn plugin_param_row(
    ui: &mut egui::Ui,
    target: PluginTarget,
    p: &PluginParamView,
    automation: &[AutomationLane],
    host: &mut dyn Host,
    requests: &mut Vec<AutomationRequest>,
) {
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
        let r = if steps == 1 {
            let mut on = v >= 0.5;
            let r = ui.checkbox(&mut on, "");
            v = if on { 1.0 } else { 0.0 };
            r
        } else if steps > 1 {
            let mut k = (v * f64::from(steps)).round() as u32;
            let r = ui.add(egui::Slider::new(&mut k, 0..=steps).show_value(false));
            v = f64::from(k) / f64::from(steps);
            r
        } else {
            ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false))
        };
        if p.info.flags.automatable {
            automation::attach(
                ui,
                &r,
                automation,
                AutomationTarget::PluginParam {
                    target,
                    param: p.info.id,
                },
                p.value as f32,
                requests,
            );
        }
        if r.changed() && !automation::secondary_press(ui) {
            host.set_plugin_param(target, p.info.id, v);
        }
        // The value text doubles as a text field (PLG-064).
        let id = egui::Id::unique(("plugin_text", target, p.info.id));
        let edit_id = id.with("edit");
        let editing: Option<String> = ui.data(|d| d.get_temp(id));
        match editing {
            None => {
                if ui
                    .add(egui::Button::new(&p.text).frame(false))
                    .on_hover_text("Click to type a value")
                    .clicked()
                {
                    ui.data_mut(|d| d.insert_temp(id, String::new()));
                    ui.memory_mut(|m| m.request_focus(edit_id));
                }
            }
            Some(mut text) => {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .id(edit_id)
                        .hint_text(&p.text)
                        .desired_width(70.0),
                );
                if r.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && let Some(v) = host.plugin_param_from_text(target, p.info.id, &text)
                    {
                        host.set_plugin_param(target, p.info.id, v);
                    }
                    ui.data_mut(|d| d.remove::<String>(id));
                } else {
                    ui.data_mut(|d| d.insert_temp(id, text));
                }
            }
        }
    });
}

pub(crate) fn unit_suffix(unit: ParamUnit) -> &'static str {
    match unit {
        ParamUnit::Decibels => " dB",
        ParamUnit::Hertz => " Hz",
        ParamUnit::Milliseconds => " ms",
        ParamUnit::Seconds => " s",
        ParamUnit::Ratio => ":1",
        ParamUnit::Fraction | ParamUnit::Q | ParamUnit::Toggle | ParamUnit::Choice => "",
    }
}

fn params_ui(
    ui: &mut egui::Ui,
    ins: &Insert,
    automation: &[AutomationLane],
    out: &mut MixerOutput,
) {
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
                let suffix = unit_suffix(spec.unit);
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
                automation::attach(
                    ui,
                    &r,
                    automation,
                    AutomationTarget::EffectParam {
                        insert: ins.id,
                        param: spec.id.to_owned(),
                    },
                    spec.to_normalized(current),
                    &mut out.automation,
                );
                if v != current && !automation::secondary_press(ui) {
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
    let (rect, _) = ui.allocate_exact_size(vec2(INSPECTOR_W - 20.0, 80.0), Sense::hover());
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
