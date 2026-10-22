// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless egui driver for UI tests: frames with pointer input and
//! lookup of widgets by their accessible label.

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};

pub(crate) struct Driver {
    pub ctx: egui::Context,
    pub modifiers: Modifiers,
    /// Accessible nodes of the last frame: label and bounds.
    nodes: Vec<(String, Rect)>,
}

impl Driver {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        Self {
            ctx,
            modifiers: Modifiers::NONE,
            nodes: Vec::new(),
        }
    }

    /// Runs one frame of `ui` with `events`.
    pub fn frame(&mut self, mut events: Vec<Event>, ui: &mut dyn FnMut(&mut egui::Ui)) {
        events.insert(0, Event::ModifiersChanged(self.modifiers));
        let input = RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
            ..RawInput::default()
        };
        let mut out = self.ctx.run_ui(input, |root| ui(root));
        out.textures_delta.clear();
        self.nodes.clear();
        if let Some(update) = out.platform_output.accesskit_update {
            for (_, node) in update.nodes {
                if let (Some(label), Some(b)) = (node.label(), node.bounds()) {
                    let r = Rect::from_min_max(
                        pos2(b.x0 as f32, b.y0 as f32),
                        pos2(b.x1 as f32, b.y1 as f32),
                    );
                    self.nodes.push((label.to_owned(), r));
                }
            }
        }
    }

    /// Centre of the last frame's widget labelled `label`.
    pub fn find(&self, label: &str) -> Option<Pos2> {
        self.nodes
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, r)| r.center())
    }

    /// Bounds of the last frame's widget labelled `label`.
    pub fn find_rect(&self, label: &str) -> Option<Rect> {
        self.nodes.iter().find(|(l, _)| l == label).map(|(_, r)| *r)
    }

    pub fn moved(&self, p: Pos2) -> Event {
        Event::PointerMoved(p)
    }

    pub fn button(&self, p: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed,
            modifiers: self.modifiers,
        }
    }

    /// A click at `p` over three frames (move, press, release).
    pub fn click(&mut self, p: Pos2, ui: &mut dyn FnMut(&mut egui::Ui)) {
        self.frame(vec![self.moved(p)], ui);
        self.frame(vec![self.button(p, true)], ui);
        self.frame(vec![self.button(p, false)], ui);
    }
}

/// A host with no audio: records seeks, reports a settable transport.
#[derive(Debug, Default)]
pub(crate) struct StubHost {
    pub transport: crate::host::TransportStatus,
    pub seeks: Vec<u64>,
    pub live_inputs: Option<(u16, Vec<String>)>,
}

impl crate::Host for StubHost {
    fn poll(&mut self) {}
    fn set_project(&mut self, _: &jdance_domain::Project, _: Option<&std::path::Path>) {}
    fn play(&mut self) {
        self.transport.playing = true;
    }
    fn stop(&mut self) {
        self.transport.playing = false;
    }
    fn set_mode(&mut self, _: crate::host::TransportMode) {}
    fn seek_ticks(&mut self, ticks: u64) {
        self.seeks.push(ticks);
    }
    fn transport(&self) -> crate::host::TransportStatus {
        self.transport
    }
    fn latency(&self) -> crate::host::LatencyInfo {
        crate::host::LatencyInfo::default()
    }
    fn meters(&self) -> crate::host::MeterReadings {
        crate::host::MeterReadings::default()
    }
    fn stream(&self) -> Option<crate::host::StreamStatus> {
        None
    }
    fn sample_status(
        &self,
        _: &jdance_domain::SampleRef,
        _: Option<&std::path::Path>,
    ) -> Option<crate::host::SampleStatus> {
        None
    }
    fn live_inputs(&self) -> Option<(u16, Vec<String>)> {
        self.live_inputs.clone()
    }
    fn backends(&self) -> Vec<(jdance_audio_api::BackendId, String)> {
        Vec::new()
    }
    fn devices(
        &self,
        _: &jdance_audio_api::BackendId,
    ) -> Result<Vec<jdance_audio_api::DeviceInfo>, jdance_audio_api::AudioError> {
        Ok(Vec::new())
    }
    fn capabilities(
        &self,
        _: &jdance_audio_api::DeviceKey,
    ) -> Result<jdance_audio_api::DeviceCapabilities, jdance_audio_api::AudioError> {
        Err(jdance_audio_api::AudioError::Backend("stub".into()))
    }
    fn saved_settings(
        &self,
        _: &jdance_audio_api::DeviceKey,
    ) -> Option<jdance_audio_api::DeviceSettings> {
        None
    }
    fn current_device(&self) -> Option<jdance_audio_api::DeviceKey> {
        None
    }
    fn open_device(
        &mut self,
        _: jdance_audio_api::DeviceKey,
        _: jdance_audio_api::DeviceSettings,
    ) -> Result<(), jdance_audio_api::AudioError> {
        Ok(())
    }
    fn close_device(&mut self) {}
    fn open_control_panel(
        &mut self,
        _: &jdance_audio_api::DeviceKey,
    ) -> Result<(), jdance_audio_api::AudioError> {
        Ok(())
    }
    fn export_audio(
        &mut self,
        _: &jdance_domain::Project,
        _: Option<&std::path::Path>,
        _: std::path::PathBuf,
        _: jdance_domain::ExportOptions,
        _: String,
        _: String,
    ) {
    }
    fn estimated_tail_seconds(&mut self, _: &jdance_domain::Project) -> f32 {
        0.0
    }
    fn take_notices(&mut self) -> Vec<crate::host::Notice> {
        Vec::new()
    }
}
