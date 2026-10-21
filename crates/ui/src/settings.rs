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
        egui::Window::new("Audio settings")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| self.contents(ui, host));
        open
    }

    fn contents(&mut self, ui: &mut egui::Ui, host: &mut dyn Host) {
        egui::Grid::new("audio-grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Backend");
                let before = self.backend.clone();
                let label = self
                    .backends
                    .iter()
                    .find(|(id, _)| Some(id) == self.backend.as_ref())
                    .map_or("—".to_owned(), |(_, n)| n.clone());
                egui::ComboBox::from_id_salt("backend")
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
                                .width(260.0)
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
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                    }
                    if ui.button("Refresh").clicked() {
                        let keep = self.device.clone();
                        self.reload_devices(host, keep);
                    }
                });
                ui.end_row();
                if before != self.device {
                    self.reload_caps(host);
                }

                let Some(Ok(caps)) = self.caps.clone() else {
                    if let Some(Err(e)) = &self.caps {
                        ui.label("");
                        ui.colored_label(ui.visuals().error_fg_color, e);
                        ui.end_row();
                    }
                    return;
                };

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
                    egui::ComboBox::from_id_salt("rate")
                        .selected_text(format!("{} Hz", self.sample_rate))
                        .show_ui(ui, |ui| {
                            for r in rates {
                                ui.selectable_value(&mut self.sample_rate, r, format!("{r} Hz"));
                            }
                        });
                }
                ui.end_row();

                ui.label("Buffer size");
                buffer_row(ui, &caps, &mut self.buffer);
                ui.end_row();

                ui.label("Output");
                let pairs = (caps.output_channels / 2).max(1);
                if caps.output_channels <= 1 {
                    ui.label("Mono device: left and right are mixed");
                    self.first_output = 0;
                } else {
                    self.first_output = self.first_output.min((pairs - 1) * 2);
                    egui::ComboBox::from_id_salt("pair")
                        .selected_text(pair_label(self.first_output))
                        .show_ui(ui, |ui| {
                            for p in 0..pairs {
                                ui.selectable_value(
                                    &mut self.first_output,
                                    p * 2,
                                    pair_label(p * 2),
                                );
                            }
                        });
                }
                ui.end_row();

                ui.label("Latency");
                ui.label(if caps.reports_latency {
                    "Reported by the driver after the stream starts"
                } else {
                    "Not reported by this backend"
                });
                ui.end_row();
            });

        ui.horizontal(|ui| {
            let key = self.key();
            let can_apply = key.is_some() && matches!(self.caps, Some(Ok(_)));
            if ui
                .add_enabled(can_apply, egui::Button::new("Apply"))
                .clicked()
            {
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
            if ui.button("Close device").clicked() {
                host.close_device();
                self.message = Some(Ok("Device closed".into()));
            }
            let has_panel = matches!(&self.caps, Some(Ok(c)) if c.has_control_panel);
            if has_panel
                && ui.button("Driver control panel").clicked()
                && let Some(k) = &key
                && let Err(e) = host.open_control_panel(k)
            {
                self.message = Some(Err(e.to_string()));
            }
        });
        match &self.message {
            Some(Ok(m)) => {
                ui.label(m);
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            None => {}
        }

        ui.separator();
        ui.strong("Active stream");
        match host.stream() {
            None => {
                ui.label("No stream is running.");
            }
            Some(s) => {
                egui::Grid::new("active-grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        let rows = [
                            ("Backend", s.info.backend.to_string()),
                            ("Device", s.info.device_name.clone()),
                            ("Sample rate", format!("{} Hz", s.info.sample_rate)),
                            ("Device channels", s.info.channels.to_string()),
                            ("Output pair", pair_label(s.info.first_output_channel)),
                            ("Sample format", format!("{:?}", s.info.sample_format)),
                            (
                                "Requested buffer",
                                match s.info.requested_buffer {
                                    BufferRequest::Default => "backend default".into(),
                                    BufferRequest::Frames(f) => format!("{f} frames"),
                                },
                            ),
                            (
                                "Observed buffer",
                                match (s.stats.min_frames, s.stats.max_frames) {
                                    (Some(a), Some(b)) => format!("{a}–{b} frames"),
                                    _ => "no callbacks yet".into(),
                                },
                            ),
                            ("Callbacks", s.stats.callbacks.to_string()),
                            (
                                "State",
                                match &s.state {
                                    StreamState::Running => "running".into(),
                                    StreamState::Failed(e) => format!("failed: {e}"),
                                },
                            ),
                        ];
                        for (k, v) in rows {
                            ui.label(k);
                            ui.label(v);
                            ui.end_row();
                        }
                    });
            }
        }
    }
}

fn pair_label(first: u16) -> String {
    format!("Outputs {} / {}", first + 1, first + 2)
}

fn buffer_row(ui: &mut egui::Ui, caps: &DeviceCapabilities, buffer: &mut BufferRequest) {
    if !caps.can_change_buffer_from_app {
        let text = match &caps.buffer_size {
            BufferSizeCaps::FixedByDriver(f) => format!("{f} frames (set by the driver)"),
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
    let label = match buffer {
        BufferRequest::Default => "Backend default".to_owned(),
        BufferRequest::Frames(f) => format!("{f} frames"),
    };
    egui::ComboBox::from_id_salt("buffer")
        .selected_text(label)
        .show_ui(ui, |ui| {
            ui.selectable_value(buffer, BufferRequest::Default, "Backend default");
            for s in sizes {
                ui.selectable_value(buffer, BufferRequest::Frames(s), format!("{s} frames"));
            }
        });
}
