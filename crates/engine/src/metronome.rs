// SPDX-License-Identifier: GPL-3.0-or-later
//! Metronome and recording count-in (spec 0.2.3 MET-001…023).
//!
//! The click is synthesized here: a short sine with an exponential decay,
//! higher and 3 dB louder on the first beat of a bar (MET-010). Clicks are
//! scheduled sample-accurately from the timeline positions of the block
//! (MET-011) and added to the device output after the master stage, so
//! they never reach tracks, meters of tracks or offline renders (MET-012).

use crate::automation::BlockTimeline;

/// Length of one click.
const CLICK_SECONDS: f32 = 0.03;
/// Decay time constant of the click envelope.
const DECAY_SECONDS: f32 = 0.006;
const FREQ: f32 = 1_000.0;
const ACCENT_FREQ: f32 = 1_500.0;
/// +3 dB on the first beat of a bar.
const ACCENT_GAIN: f32 = 1.412_537_6;
/// Click triggers handled per block; beats closer than that are skipped.
const MAX_TRIGGERS: usize = 16;

/// Default level, −6 dB (MET-003).
pub const DEFAULT_METRONOME_GAIN: f32 = 0.501_187_2;

#[derive(Debug, Clone, Copy, Default)]
struct Click {
    /// Samples rendered so far; `len` when silent.
    at: usize,
    len: usize,
    phase_step: f32,
    decay: f32,
    amp: f32,
}

impl Click {
    fn start(&mut self, sample_rate: f32, accent: bool, gain: f32) {
        let freq = if accent { ACCENT_FREQ } else { FREQ };
        self.at = 0;
        self.len = (CLICK_SECONDS * sample_rate) as usize;
        self.phase_step = std::f32::consts::TAU * freq / sample_rate;
        self.decay = (-1.0 / (DECAY_SECONDS * sample_rate)).exp();
        self.amp = if accent { gain * ACCENT_GAIN } else { gain };
    }

    /// Adds the click to `out[from..to]`, scaled by `fade` when given.
    fn render_add(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        from: usize,
        to: usize,
        fade: Option<&[f32]>,
    ) {
        for i in from..to {
            if self.at >= self.len {
                return;
            }
            let t = self.at as f32;
            let v = self.amp * (t * self.phase_step).sin() * self.decay.powf(t);
            let v = fade.map_or(v, |f| v * f[i]);
            l[i] += v;
            r[i] += v;
            self.at += 1;
        }
    }
}

/// Metronome state of the processor.
#[derive(Debug)]
pub(crate) struct Metronome {
    sample_rate: f32,
    pub on: bool,
    pub gain: f32,
    click: Click,
    /// A count-in is running (it ends a beat after its last click).
    counting: bool,
    /// Count-in beats still to come.
    count_left: u32,
    count_total: u32,
    /// Samples since the count-in started.
    count_pos: u64,
    triggers: [(usize, bool); MAX_TRIGGERS],
    n_triggers: usize,
}

impl Metronome {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            on: false,
            gain: DEFAULT_METRONOME_GAIN,
            click: Click::default(),
            counting: false,
            count_left: 0,
            count_total: 0,
            count_pos: 0,
            triggers: [(0, false); MAX_TRIGGERS],
            n_triggers: 0,
        }
    }

    pub fn start_count_in(&mut self, beats: u32) {
        self.counting = beats > 0;
        self.count_left = beats;
        self.count_total = beats;
        self.count_pos = 0;
    }

    pub fn cancel_count_in(&mut self) {
        self.counting = false;
        self.count_left = 0;
    }

    pub fn counting(&self) -> bool {
        self.counting
    }

    /// Beats of the count-in still to come, for display.
    pub fn count_left(&self) -> u32 {
        self.count_left
    }

    /// Samples until the count-in ends, so the processor can start the
    /// transport exactly on a block boundary (MET-023).
    pub fn count_remaining(&self, beat_samples: f64) -> u64 {
        let total = (f64::from(self.count_total) * beat_samples).round() as u64;
        total.saturating_sub(self.count_pos)
    }

    fn push(&mut self, offset: usize, accent: bool) {
        if self.n_triggers < MAX_TRIGGERS {
            self.triggers[self.n_triggers] = (offset, accent);
            self.n_triggers += 1;
        }
    }

    /// Schedules clicks for a playing block whose positions are
    /// `timeline`; beats are `beat_samples` long, `per_bar` to a bar.
    pub fn schedule_playing(
        &mut self,
        timeline: &BlockTimeline,
        n: usize,
        beat_samples: f64,
        per_bar: u32,
    ) {
        if !self.on || beat_samples <= 0.0 {
            return;
        }
        for (offset, start, len) in timeline.segments(n) {
            let end = start + len as u64;
            let mut k = (start as f64 / beat_samples).ceil() as u64;
            // Rounding may put beat k - 1 at `start` itself.
            if k > 0 && ((k - 1) as f64 * beat_samples).round() as u64 >= start {
                k -= 1;
            }
            loop {
                let at = (k as f64 * beat_samples).round() as u64;
                if at >= end {
                    break;
                }
                if at >= start {
                    let accent = k.is_multiple_of(u64::from(per_bar.max(1)));
                    self.push(offset + (at - start) as usize, accent);
                }
                k += 1;
            }
        }
    }

    /// Schedules count-in clicks for a stopped block of `n` samples and
    /// advances the count-in. Returns true when it ended with this block.
    pub fn schedule_count_in(&mut self, n: usize, beat_samples: f64, per_bar: u32) -> bool {
        if !self.counting {
            return false;
        }
        let end = self.count_pos + n as u64;
        for k in (self.count_total - self.count_left)..self.count_total {
            let at = (f64::from(k) * beat_samples).round() as u64;
            if at >= end {
                break;
            }
            if at >= self.count_pos {
                self.push((at - self.count_pos) as usize, (k % per_bar.max(1)) == 0);
                self.count_left -= 1;
            }
        }
        self.count_pos = end;
        if self.count_remaining(beat_samples) == 0 {
            self.cancel_count_in();
            return true;
        }
        false
    }

    /// Renders the scheduled clicks (and the tail of a running one) into
    /// the block output; `fade` is the Stop fade when it applies.
    pub fn render(&mut self, l: &mut [f32], r: &mut [f32], fade: Option<&[f32]>) {
        let n = l.len().min(r.len());
        let mut cursor = 0;
        for i in 0..self.n_triggers {
            let (at, accent) = self.triggers[i];
            let at = at.min(n);
            self.click.render_add(l, r, cursor, at, fade);
            self.click.start(self.sample_rate, accent, self.gain);
            cursor = at;
        }
        self.n_triggers = 0;
        self.click.render_add(l, r, cursor, n, fade);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onsets(l: &[f32]) -> Vec<usize> {
        // A click starts at sin(0) = 0, so look for the first non-zero
        // sample after a silent gap.
        let mut out = Vec::new();
        let mut quiet = usize::MAX / 2;
        for (i, v) in l.iter().enumerate() {
            if v.abs() > 1e-6 {
                if quiet > 100 {
                    out.push(i - 1);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        out
    }

    #[test]
    fn clicks_land_on_beats_with_an_accent_per_bar() {
        let mut m = Metronome::new(48_000);
        m.on = true;
        // 3 beats a bar, 12 000 samples a beat.
        let n = 1000;
        let mut l = vec![0.0; 48_000];
        let mut r = vec![0.0; 48_000];
        for b in 0..48 {
            let tl = BlockTimeline::new(b as u64 * n as u64);
            m.schedule_playing(&tl, n, 12_000.0, 3);
            let (bl, br) = (&mut l[b * n..(b + 1) * n], &mut r[b * n..(b + 1) * n]);
            m.render(bl, br, None);
        }
        assert_eq!(onsets(&l), [0, 12_000, 24_000, 36_000]);
        let peak = |from: usize| {
            l[from..from + 1_000]
                .iter()
                .fold(0.0f32, |a, v| a.max(v.abs()))
        };
        assert!(peak(0) > peak(12_000) * 1.3, "accent on beat 1");
        assert!((peak(36_000) - peak(0)).abs() < 1e-3, "next bar accented");
        assert!(peak(0) <= DEFAULT_METRONOME_GAIN * ACCENT_GAIN + 1e-6);
    }

    #[test]
    fn a_wrap_inside_the_block_schedules_from_both_segments() {
        let mut m = Metronome::new(48_000);
        m.on = true;
        let mut tl = BlockTimeline::new(11_500);
        // Loop back to 0 at block offset 500.
        tl.push(500, 0);
        m.schedule_playing(&tl, 1000, 12_000.0, 4);
        assert_eq!(&m.triggers[..m.n_triggers], &[(500, true)]);
    }

    #[test]
    fn count_in_counts_beats_and_ends_on_the_boundary() {
        let mut m = Metronome::new(48_000);
        m.start_count_in(6);
        let beat = 1_000.5;
        let mut ended_at = None;
        let mut clicks = 0;
        let mut pos = 0u64;
        for _ in 0..100 {
            let n = (m.count_remaining(beat) as usize).min(256);
            if n == 0 {
                break;
            }
            let done = m.schedule_count_in(n, beat, 3);
            clicks += m.n_triggers;
            m.n_triggers = 0;
            pos += n as u64;
            if done {
                ended_at = Some(pos);
                break;
            }
        }
        assert_eq!(clicks, 6);
        assert_eq!(ended_at, Some((6.0 * beat).round() as u64));
        assert!(!m.counting());
    }
}
