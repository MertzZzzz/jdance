// SPDX-License-Identifier: GPL-3.0-or-later
//! Toolbar pieces (spec "DAW UI clean room", §3): position display, snap
//! presets and the Tap Tempo window (§20).

use crate::theme;
use jdance_domain::{MAX_TEMPO_BPM, MIN_TEMPO_BPM, PPQ, TICKS_PER_STEP, TimeSignature};

/// `BBB:B:TTT` (bar, beat, tick within the beat), 1-based bar and beat, in
/// the project signature (SIG-003).
pub(crate) fn bars_text(ticks: u64, sig: TimeSignature) -> String {
    let (bar, beat, tick) = sig.split(ticks);
    format!("{:03}:{}:{tick:03}", bar + 1, beat + 1)
}

/// `M:SS.mmm` at `bpm`.
pub(crate) fn time_text(ticks: u64, bpm: f64) -> String {
    let secs = ticks as f64 / f64::from(PPQ) * 60.0 / bpm;
    let ms = (secs * 1000.0).round() as u64;
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

/// Snap grid choices shared by the Playlist and the Piano Roll. Bar and
/// beat follow the project signature, so the choice is kept, not its ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Snap {
    Bar,
    Beat,
    Step,
    HalfStep,
    QuarterStep,
    Off,
}

pub(crate) const SNAPS: [Snap; 6] = [
    Snap::Bar,
    Snap::Beat,
    Snap::Step,
    Snap::HalfStep,
    Snap::QuarterStep,
    Snap::Off,
];

impl Snap {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Snap::Bar => "Bar",
            Snap::Beat => "Beat",
            Snap::Step => "Step",
            Snap::HalfStep => "1/2 step",
            Snap::QuarterStep => "1/4 step",
            Snap::Off => "Off",
        }
    }

    /// Grid in ticks; 0 = off.
    pub(crate) fn ticks(self, sig: TimeSignature) -> u32 {
        match self {
            Snap::Bar => sig.bar_ticks(),
            Snap::Beat => sig.beat_ticks(),
            Snap::Step => TICKS_PER_STEP,
            Snap::HalfStep => TICKS_PER_STEP / 2,
            Snap::QuarterStep => TICKS_PER_STEP / 4,
            Snap::Off => 0,
        }
    }
}

/// Name of a grid given in ticks; "Custom" when no choice matches.
pub(crate) fn snap_name(ticks: u32, sig: TimeSignature) -> &'static str {
    SNAPS
        .iter()
        .find(|s| s.ticks(sig) == ticks)
        .map_or("Custom", |s| s.name())
}

/// The time signature field of the transport panel (SIG-010): shows
/// "4/4"; a click opens numerator, denominator and common choices. Returns
/// a new signature when one is chosen.
pub(crate) fn signature_field(ui: &mut egui::Ui, sig: TimeSignature) -> Option<TimeSignature> {
    let mut chosen = None;
    let button = ui
        .add(
            egui::Button::new(egui::RichText::new(sig.to_string()).font(theme::mono(14.0)))
                .fill(theme::FIELD)
                .min_size(egui::vec2(44.0, 24.0)),
        )
        .on_hover_text("Time signature of the project");
    egui::Popup::from_toggle_button_response(&button)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(170.0);
            ui.horizontal(|ui| {
                let mut n = sig.numerator;
                if ui
                    .add(egui::DragValue::new(&mut n).range(1..=jdance_domain::MAX_NUMERATOR))
                    .on_hover_text("Beats per bar")
                    .changed()
                {
                    chosen = Some(TimeSignature::new(n, sig.denominator));
                }
                ui.label("/");
                egui::ComboBox::from_id_salt("sig_denominator")
                    .width(44.0)
                    .selected_text(sig.denominator.to_string())
                    .show_ui(ui, |ui| {
                        for d in jdance_domain::DENOMINATORS {
                            if ui
                                .selectable_label(d == sig.denominator, d.to_string())
                                .clicked()
                            {
                                chosen = Some(TimeSignature::new(sig.numerator, d));
                            }
                        }
                    })
                    .response
                    .on_hover_text("Note value of a beat");
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                for c in jdance_domain::COMMON_SIGNATURES {
                    if ui.selectable_label(c == sig, c.to_string()).clicked() {
                        chosen = Some(c);
                    }
                }
            });
        });
    chosen.filter(|c| *c != sig)
}

/// Taps older than this start a new measurement.
const TAP_RESET_SECONDS: f64 = 2.0;
const TAP_WINDOW: usize = 8;

/// Tap Tempo: BPM from the average interval of the last taps.
#[derive(Debug, Default)]
pub(crate) struct TapTempo {
    taps: Vec<f64>,
}

impl TapTempo {
    pub(crate) fn tap(&mut self, now: f64) {
        if self
            .taps
            .last()
            .is_some_and(|t| now - t > TAP_RESET_SECONDS)
        {
            self.taps.clear();
        }
        self.taps.push(now);
        if self.taps.len() > TAP_WINDOW {
            self.taps.remove(0);
        }
    }

    pub(crate) fn reset(&mut self) {
        self.taps.clear();
    }

    pub(crate) fn count(&self) -> usize {
        self.taps.len()
    }

    /// Estimated tempo, clamped to the project range; needs two taps.
    pub(crate) fn bpm(&self) -> Option<f64> {
        let (first, last) = (self.taps.first()?, self.taps.last()?);
        let n = self.taps.len();
        if n < 2 || last <= first {
            return None;
        }
        let interval = (last - first) / (n - 1) as f64;
        Some((60.0 / interval).clamp(MIN_TEMPO_BPM, MAX_TEMPO_BPM))
    }

    /// Draws the window; returns a tempo to apply and whether it stays
    /// open.
    pub(crate) fn window(&mut self, ctx: &egui::Context, now: f64) -> (Option<f64>, bool) {
        let mut open = true;
        let mut apply = None;
        egui::Window::new("Tap Tempo")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    let tap = ui.add_sized(
                        [180.0, 100.0],
                        egui::Button::new(egui::RichText::new("Tap").size(20.0)),
                    );
                    // The button takes keyboard focus so Enter taps (Space
                    // is the transport everywhere, KEY-002).
                    if tap.clicked() {
                        self.tap(now);
                    }
                    let text = self
                        .bpm()
                        .map_or_else(|| "— BPM".to_owned(), |b| format!("{b:.1} BPM"));
                    ui.label(egui::RichText::new(text).font(theme::mono(28.0)));
                    ui.weak(format!(
                        "from the last {} taps · resets after {TAP_RESET_SECONDS:.0} s idle",
                        self.count()
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Reset").clicked() {
                            self.reset();
                        }
                        if ui
                            .add_enabled(
                                self.bpm().is_some(),
                                egui::Button::new("Apply to project"),
                            )
                            .clicked()
                        {
                            apply = self.bpm();
                        }
                    });
                });
            });
        (apply, open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions() {
        let common = TimeSignature::COMMON;
        let bar = u64::from(common.bar_ticks());
        assert_eq!(bars_text(0, common), "001:1:000");
        assert_eq!(bars_text(2 * bar + u64::from(PPQ) + 5, common), "003:2:005");
        // 6/8: a beat is an eighth.
        let six = TimeSignature::new(6, 8);
        assert_eq!(bars_text(u64::from(6 * PPQ / 2) + 480, six), "002:2:000");
        assert_eq!(time_text(u64::from(PPQ) * 2, 120.0), "0:01.000");
        assert_eq!(time_text(u64::from(PPQ) * 250, 120.0), "2:05.000");
    }

    #[test]
    fn snaps_follow_the_signature() {
        let three = TimeSignature::new(3, 4);
        assert_eq!(Snap::Bar.ticks(three), 3 * PPQ);
        assert_eq!(Snap::Beat.ticks(TimeSignature::new(7, 8)), PPQ / 2);
        assert_eq!(snap_name(3 * PPQ, three), "Bar");
        assert_eq!(snap_name(4 * PPQ, three), "Custom");
        assert_eq!(snap_name(TICKS_PER_STEP, three), "Step");
    }

    #[test]
    fn tap_tempo_averages_and_resets() {
        let mut t = TapTempo::default();
        t.tap(10.0);
        assert_eq!(t.bpm(), None, "one tap");
        t.tap(10.5);
        t.tap(11.0);
        t.tap(11.5);
        assert!((t.bpm().unwrap() - 120.0).abs() < 1e-9);
        t.tap(20.0);
        assert_eq!(t.count(), 1, "a pause starts over");
        for i in 1..20 {
            t.tap(20.0 + f64::from(i) * 0.25);
        }
        assert_eq!(t.count(), TAP_WINDOW);
        assert!((t.bpm().unwrap() - 240.0).abs() < 1e-9);
        t.reset();
        assert_eq!(t.bpm(), None);
    }
}
