// SPDX-License-Identifier: GPL-3.0-or-later
//! Toolbar pieces (spec "DAW UI clean room", §3): position display, snap
//! presets and the Tap Tempo window (§20).

use crate::theme;
use jdance_domain::{MAX_TEMPO_BPM, MIN_TEMPO_BPM, PPQ, TICKS_PER_STEP};

/// Ticks per 4/4 bar.
pub(crate) const BAR_TICKS: u64 = PPQ as u64 * 4;

/// `BBB:B:TTT` (bar, beat, tick within the beat), 1-based bar and beat.
pub(crate) fn bars_text(ticks: u64) -> String {
    let bar = ticks / BAR_TICKS + 1;
    let beat = ticks % BAR_TICKS / u64::from(PPQ) + 1;
    let tick = ticks % u64::from(PPQ);
    format!("{bar:03}:{beat}:{tick:03}")
}

/// `M:SS.mmm` at `bpm`.
pub(crate) fn time_text(ticks: u64, bpm: f64) -> String {
    let secs = ticks as f64 / f64::from(PPQ) * 60.0 / bpm;
    let ms = (secs * 1000.0).round() as u64;
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

/// Snap grid choices shared by the Playlist and the Piano Roll (ticks;
/// 0 = off).
pub(crate) const SNAPS: [(u32, &str); 6] = [
    (TICKS_PER_STEP * 16, "Bar"),
    (TICKS_PER_STEP * 4, "Beat"),
    (TICKS_PER_STEP, "1/4 beat"),
    (TICKS_PER_STEP / 2, "1/8 beat"),
    (TICKS_PER_STEP / 4, "1/16 beat"),
    (0, "Off"),
];

pub(crate) fn snap_name(snap: u32) -> &'static str {
    SNAPS
        .iter()
        .find(|(s, _)| *s == snap)
        .map_or("Custom", |(_, n)| n)
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
                    // The button takes keyboard focus so Space and Enter tap.
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
        assert_eq!(bars_text(0), "001:1:000");
        assert_eq!(bars_text(2 * BAR_TICKS + u64::from(PPQ) + 5), "003:2:005");
        assert_eq!(time_text(u64::from(PPQ) * 2, 120.0), "0:01.000");
        assert_eq!(time_text(u64::from(PPQ) * 250, 120.0), "2:05.000");
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
