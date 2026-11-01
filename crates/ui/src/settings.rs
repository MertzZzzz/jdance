// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio settings window, built from device capabilities (AUD-010/011).
//!
//! Device enumeration can be slow, so lists and capabilities are cached
//! and refreshed only on selection change or "Refresh".

use crate::Host;
use jdance_audio_api::{
    BackendId, BufferRequest, BufferSizeCaps, DeviceCapabilities, DeviceId, DeviceInfo, DeviceKey,
    DeviceSettings, StreamState,
};

pub(crate) struct SettingsWindow {
    backends: Vec<(BackendId, String)>,
    backend: Option<BackendId>,
    devices: Result<Vec<DeviceInfo>, String>,
    device: Option<DeviceId>,
    caps: Option<Result<DeviceCapabilities, String>>,
    /// Driver channel names of the selected device (ASIO-031).
    names: jdance_audio_api::ChannelNames,
    sample_rate: u32,
    buffer: BufferRequest,
    first_output: u16,
    message: Option<Result<String, String>>,
}

impl SettingsWindow {
    pub fn new(host: &dyn Host) -> Self {
        let backends = host.backends();
        let current = host.current_device();
        let backend = current
            .as_ref()
            .map(|k| k.backend.clone())
            .or_else(|| backends.first().map(|(id, _)| id.clone()));
        let mut w = Self {
            backends,
            backend,
            devices: Ok(Vec::new()),
            device: None,
            caps: None,
            names: Default::default(),
            sample_rate: 48_000,
            buffer: BufferRequest::Default,
            first_output: 0,
            message: None,
        };
        w.reload_devices(host, current.map(|k| k.device));
        w
    }

    fn key(&self) -> Option<DeviceKey> {
        Some(DeviceKey {
            backend: self.backend.clone()?,
            device: self.device.clone()?,
        })
    }

    fn reload_devices(&mut self, host: &dyn Host, prefer: Option<DeviceId>) {
        self.devices = match &self.backend {
            Some(b) => host.devices(b).map_err(|e| e.to_string()),
            None => Err("No audio backend is available".into()),
        };
        self.device = match &self.devices {
            Ok(list) => prefer
                .filter(|p| list.iter().any(|d| &d.id == p))
                .or_else(|| list.iter().find(|d| d.is_default).map(|d| d.id.clone()))
                .or_else(|| list.first().map(|d| d.id.clone())),
            Err(_) => None,
        };
        self.reload_caps(host);
    }

    fn reload_caps(&mut self, host: &dyn Host) {
        self.caps = self
            .key()
            .map(|k| host.capabilities(&k).map_err(|e| e.to_string()));
        self.names = self
            .key()
            .map(|k| host.channel_names(&k))
            .unwrap_or_default();
        let saved = self.key().and_then(|k| host.saved_settings(&k));
        if let Some(Ok(caps)) = &self.caps {
            let s = saved.unwrap_or(DeviceSettings {
                sample_rate: caps.default_sample_rate.unwrap_or(48_000),
                buffer: BufferRequest::Default,
                first_output_channel: 0,
            });
            self.sample_rate = s.sample_rate;
            self.buffer = if caps.can_change_buffer_from_app {
                s.buffer
            } else {
                BufferRequest::Default
            };
            self.first_output = s.first_output_channel;
        }
    }

    /// Returns `false` when the window was closed.
    pub fn show(&mut self, ctx: &egui::Context, host: &mut dyn Host) -> bool {
        let mut open = true;
        egui::Window::new(egui::RichText::new("Audio settings").font(crate::theme::strong(13.5)))
            .id(egui::Id::unique("audio_settings"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.set_width(560.0);
                self.contents(ui, host);
            });
        open
    }

    /// UI-RD §7 (board R4 · Audio settings): the stream banner, Device
    /// and Format cards (rate and buffer as segments, §10.13), the stream
    /// details, Close device on the left and Apply on the right.
    fn contents(&mut self, ui: &mut egui::Ui, host: &mut dyn Host) {
        use crate::icons::Glyph as G;
        use crate::theme;
        use egui::RichText;
        stream_banner(ui, host);
        ui.add_space(8.0);
        crate::widgets::card(ui, Some(G::Speaker), "Device", |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("audio-grid")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .min_col_width(90.0)
                .show(ui, |ui| {
                    ui.label("Backend");
                    let before = self.backend.clone();
                    let label = self
                        .backends
                        .iter()
                        .find(|(id, _)| Some(id) == self.backend.as_ref())
                        .map_or("—".to_owned(), |(_, n)| n.clone());
                    egui::ComboBox::from_id_salt("backend")
                        .width(400.0)
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            for (id, name) in &self.backends {
                                ui.selectable_value(&mut self.backend, Some(id.clone()), name);
                            }
                        });
                    ui.end_row();
                    if before != self.backend {
                        self.reload_devices(host, None);
                    }

                    ui.label("Device");
                    let before = self.device.clone();
                    ui.horizontal(|ui| {
                        match &self.devices {
                            Ok(list) => {
                                let label = list
                                    .iter()
                                    .find(|d| Some(&d.id) == self.device.as_ref())
                                    .map_or("—".to_owned(), |d| d.name.clone());
                                egui::ComboBox::from_id_salt("device")
                                    .width(364.0)
                                    .truncate()
                                    .selected_text(label)
                                    .show_ui(ui, |ui| {
                                        for d in list {
                                            let mut name = d.name.clone();
                                            if d.is_default {
                                                name.push_str(" (default)");
                                            }
                                            ui.selectable_value(
                                                &mut self.device,
                                                Some(d.id.clone()),
                                                name,
                                            );
                                        }
                                    });
                            }
                            Err(e) => {
                                ui.colored_label(theme::ERROR, e);
                            }
                        }
                        if crate::icons::icon_button_sized(
                            ui,
                            G::Undo,
                            false,
                            "Refresh the list of devices",
                            egui::vec2(28.0, 26.0),
                            13.0,
                        )
                        .clicked()
                        {
                            let keep = self.device.clone();
                            self.reload_devices(host, keep);
                        }
                    });
                    ui.end_row();
                    if before != self.device {
                        self.reload_caps(host);
                    }
                });
            let key = self.key();
            let has_panel = matches!(&self.caps, Some(Ok(c)) if c.has_control_panel);
            if has_panel {
                ui.horizontal(|ui| {
                    ui.add_space(106.0);
                    if crate::widgets::button(
                        ui,
                        Some(G::Settings),
                        "Driver control panel",
                        false,
                        crate::widgets::Look::Raised,
                        "Open the driver's own settings",
                    )
                    .clicked()
                        && let Some(k) = &key
                        && let Err(e) = host.open_control_panel(k)
                    {
                        self.message = Some(Err(e.to_string()));
                    }
                });
            }
            if self.backend.as_ref().is_some_and(|b| b.0 == "asio") {
                let mut on = host.release_on_blur();
                if crate::widgets::switch_row(
                    ui,
                    &mut on,
                    theme::LED_ON,
                    "Release the ASIO driver while jdance is inactive",
                )
                .on_hover_text(
                    "Closes the driver when the window loses focus and the transport is \
                     stopped, so other programs can use it; it reopens on return.",
                )
                .changed()
                {
                    host.set_release_on_blur(on);
                }
            }
        });
        ui.add_space(8.0);
        let caps = match self.caps.clone() {
            Some(Ok(caps)) => Some(caps),
            Some(Err(e)) => {
                ui.colored_label(theme::ERROR, e);
                None
            }
            None => None,
        };
        if let Some(caps) = &caps {
            crate::widgets::card(ui, Some(G::Sample), "Format", |ui| {
                ui.set_width(ui.available_width());
                egui::Grid::new("format-grid")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .min_col_width(90.0)
                    .show(ui, |ui| {
                        ui.label("Sample rate");
                        let rates = caps.sample_rates.offered();
                        if rates.is_empty() {
                            ui.label("No standard sample rate is supported by this device");
                        } else {
                            if !rates.contains(&self.sample_rate) {
                                self.sample_rate = caps
                                    .default_sample_rate
                                    .filter(|r| rates.contains(r))
                                    .unwrap_or(rates[0]);
                            }
                            // The common rates; those the device lacks are
                            // shown dimmed.
                            let mut shown: Vec<u32> = [44_100, 48_000, 88_200, 96_000]
                                .into_iter()
                                .chain(rates.iter().copied())
                                .collect();
                            shown.sort_unstable();
                            shown.dedup();
                            segments(ui, |ui| {
                                for r in shown {
                                    let ok = rates.contains(&r);
                                    let text = rate_label(r);
                                    let resp = ui.add_enabled_ui(ok, |ui| {
                                        crate::widgets::button(
                                            ui,
                                            None,
                                            &text,
                                            self.sample_rate == r,
                                            crate::widgets::Look::Blue,
                                            &format!("{r} Hz"),
                                        )
                                    });
                                    if resp.inner.clicked() {
                                        self.sample_rate = r;
                                    }
                                    if !ok {
                                        resp.response
                                            .on_disabled_hover_text("Not supported by this device");
                                    }
                                }
                            });
                        }
                        ui.end_row();

                        ui.label("Buffer size");
                        buffer_row(ui, caps, self.sample_rate, &mut self.buffer);
                        ui.end_row();

                        ui.label("Output pair");
                        let pairs = (caps.output_channels / 2).max(1);
                        if caps.output_channels <= 1 {
                            ui.label("Mono device: left and right are mixed");
                            self.first_output = 0;
                        } else {
                            self.first_output = self.first_output.min((pairs - 1) * 2);
                            egui::ComboBox::from_id_salt("pair")
                                .width(400.0)
                                .selected_text(self.names.output_pair_label(self.first_output))
                                .show_ui(ui, |ui| {
                                    for p in 0..pairs {
                                        ui.selectable_value(
                                            &mut self.first_output,
                                            p * 2,
                                            self.names.output_pair_label(p * 2),
                                        );
                                    }
                                });
                        }
                        ui.end_row();
                    });
                ui.label(
                    RichText::new(if caps.reports_latency {
                        "Latency is reported by the driver after the stream starts."
                    } else {
                        "This backend does not report latency."
                    })
                    .size(11.0)
                    .color(theme::MUTED),
                );
            });
            ui.add_space(8.0);
        }
        let active_names = if host.current_device() == self.key() {
            self.names.clone()
        } else {
            Default::default()
        };
        stream_details(ui, host, &active_names);
        ui.add_space(10.0);
        let key = self.key();
        let can_apply = key.is_some() && matches!(self.caps, Some(Ok(_)));
        // What differs from the running stream.
        let pending = match host.stream() {
            Some(st) if host.current_device() == key => {
                let rate = st.info.sample_rate != self.sample_rate;
                let buffer = st.info.requested_buffer != self.buffer;
                let pair = st.info.first_output_channel != self.first_output;
                match (rate, buffer, pair) {
                    (false, false, false) => None,
                    (true, false, false) => Some("Sample rate changed — not applied"),
                    (false, true, false) => Some("Buffer changed — not applied"),
                    (false, false, true) => Some("Output pair changed — not applied"),
                    _ => Some("Settings changed — not applied"),
                }
            }
            Some(_) => Some("Another device selected — not applied"),
            None => None,
        };
        ui.horizontal(|ui| {
            if crate::widgets::danger_button(ui, "Close device").clicked() {
                host.close_device();
                self.message = Some(Ok("Device closed".into()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::primary_button(ui, "Apply", can_apply).clicked() {
                    let settings = DeviceSettings {
                        sample_rate: self.sample_rate,
                        buffer: self.buffer,
                        first_output_channel: self.first_output,
                    };
                    self.message = Some(
                        host.open_device(key.clone().expect("checked"), settings)
                            .map(|()| "Stream started".to_owned())
                            .map_err(|e| e.to_string()),
                    );
                }
                if let Some(p) = pending {
                    ui.label(RichText::new(p).size(11.0).color(theme::AMBER));
                    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.5, theme::AMBER);
                }
            });
        });
        match &self.message {
            Some(Ok(m)) => {
                ui.label(RichText::new(m).size(11.0).color(theme::MUTED));
            }
            Some(Err(e)) => {
                ui.colored_label(theme::ERROR, e);
            }
            None => {}
        }
    }
}

/// «48k», «44.1k».
fn rate_label(r: u32) -> String {
    if r.is_multiple_of(1000) {
        format!("{}k", r / 1000)
    } else {
        format!("{:.1}k", f64::from(r) / 1000.0)
    }
}

/// A segmented group that wraps when it does not fit.
fn segments(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(crate::theme::FIELD)
        .stroke(egui::Stroke::new(1.0, crate::theme::LINE))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal_wrapped(add);
        });
}

/// The banner on top: a lamp, the state and the stream in one line, the
/// output latency on the right.
fn stream_banner(ui: &mut egui::Ui, host: &dyn Host) {
    use crate::theme;
    use egui::RichText;
    let (running, title, line, latency) = match host.stream() {
        None => (
            false,
            "No stream".to_owned(),
            "Choose a device and Apply".to_owned(),
            None,
        ),
        Some(s) => {
            let ok = matches!(s.state, StreamState::Running);
            let buffer = match (s.stats.max_frames, s.info.requested_buffer) {
                (Some(f), _) | (None, BufferRequest::Frames(f)) => format!(" · {f} smp"),
                _ => String::new(),
            };
            let title = match &s.state {
                StreamState::Running => "Stream running".to_owned(),
                StreamState::Failed(e) => format!("Stream failed: {e}"),
            };
            let latency = s.info.latency.map(|(_, o)| {
                format!(
                    "{:.1} ms",
                    f64::from(o) * 1000.0 / f64::from(s.info.sample_rate.max(1))
                )
            });
            (
                ok,
                title,
                format!(
                    "{} · {} · {} Hz{buffer}",
                    s.info.backend, s.info.device_name, s.info.sample_rate
                ),
                latency,
            )
        }
    };
    let (fill, stroke) = if running {
        (
            egui::Color32::from_rgb(0x15, 0x24, 0x1B),
            egui::Color32::from_rgb(0x2D, 0x5A, 0x3C),
        )
    } else {
        (theme::CARD, theme::CARD_LINE)
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke))
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                crate::widgets::lamp(ui, running);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(title).font(theme::strong(12.0)));
                    ui.add(
                        egui::Label::new(
                            RichText::new(line)
                                .font(theme::mono(10.0))
                                .color(theme::MUTED),
                        )
                        .truncate(),
                    );
                });
                if let Some(l) = latency {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(l)
                                .font(theme::mono(11.0))
                                .color(theme::LED_ON),
                        );
                    });
                }
            });
        });
}

/// State, callbacks, buffer, format and latency of the running stream in
/// a card of captioned values.
fn stream_details(ui: &mut egui::Ui, host: &dyn Host, names: &jdance_audio_api::ChannelNames) {
    use crate::theme;
    use egui::RichText;
    let Some(s) = host.stream() else {
        return;
    };
    let ms = |f: u32| f64::from(f) * 1000.0 / f64::from(s.info.sample_rate.max(1));
    let cells = [
        (
            "State",
            match &s.state {
                StreamState::Running => "running".to_owned(),
                StreamState::Failed(e) => format!("failed: {e}"),
            },
        ),
        ("Callbacks", s.stats.callbacks.to_string()),
        (
            "Observed buffer",
            match (s.stats.min_frames, s.stats.max_frames) {
                (Some(a), Some(b)) if a == b => format!("{a} smp"),
                (Some(a), Some(b)) => format!("{a}–{b} smp"),
                _ => "no callbacks yet".into(),
            },
        ),
        (
            "Sample format",
            format!("{:?}", s.info.sample_format).to_lowercase(),
        ),
        (
            "Latency in",
            s.info
                .latency
                .map_or("not reported".into(), |(i, _)| format!("{:.1} ms", ms(i))),
        ),
        (
            "Latency out",
            s.info
                .latency
                .map_or("not reported".into(), |(_, o)| format!("{:.1} ms", ms(o))),
        ),
        ("Channels", s.info.channels.to_string()),
        (
            "Output pair",
            names.output_pair_label(s.info.first_output_channel),
        ),
        (
            "Requested buffer",
            match s.info.requested_buffer {
                BufferRequest::Default => "auto".into(),
                BufferRequest::Frames(f) => format!("{f} smp"),
            },
        ),
    ];
    crate::widgets::card(ui, None, "", |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("active-grid")
            .num_columns(3)
            .spacing([24.0, 8.0])
            .min_col_width(150.0)
            .show(ui, |ui| {
                for (i, (k, v)) in cells.into_iter().enumerate() {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        crate::widgets::caption(ui, k);
                        ui.label(RichText::new(v).font(theme::mono(11.5)));
                    });
                    if i % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
    });
}

/// «256 frames (5.3 ms)» at `rate`.
fn frames_label(frames: u32, rate: u32) -> String {
    let ms = f64::from(frames) * 1000.0 / f64::from(rate.max(1));
    format!("{frames} frames ({ms:.1} ms)")
}

fn buffer_row(ui: &mut egui::Ui, caps: &DeviceCapabilities, rate: u32, buffer: &mut BufferRequest) {
    if !caps.can_change_buffer_from_app {
        let text = match &caps.buffer_size {
            BufferSizeCaps::FixedByDriver(f) => {
                format!("{} (set by the driver)", frames_label(*f, rate))
            }
            _ => "Chosen by the backend; it can't be set from the app".into(),
        };
        let hint = if caps.has_control_panel {
            "Use the driver control panel to change it."
        } else {
            "The value in use is shown under Active stream."
        };
        ui.label(text).on_hover_text(hint);
        *buffer = BufferRequest::Default;
        return;
    }
    let sizes = caps.buffer_size.offered();
    // UI-RD §10.13: the sizes as segments, Auto = the backend's default.
    segments(ui, |ui| {
        for s in sizes {
            if crate::widgets::button(
                ui,
                None,
                &s.to_string(),
                *buffer == BufferRequest::Frames(s),
                crate::widgets::Look::Blue,
                &frames_label(s, rate),
            )
            .clicked()
            {
                *buffer = BufferRequest::Frames(s);
            }
        }
        if crate::widgets::button(
            ui,
            None,
            "Auto",
            *buffer == BufferRequest::Default,
            crate::widgets::Look::Blue,
            "The backend's default",
        )
        .clicked()
        {
            *buffer = BufferRequest::Default;
        }
    });
}
