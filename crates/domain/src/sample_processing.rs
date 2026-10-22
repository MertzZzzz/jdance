// SPDX-License-Identifier: GPL-3.0-or-later
//! The advanced sampler's settings (spec 0.2.5 §4): declicking, time
//! stretching and pitch shifting, precomputed processing, cut groups,
//! fine tuning and level offsets.
//!
//! Precomputed processing and stretching change the sample data: they
//! are computed outside the audio thread into a new buffer, which a
//! channel finds under [`processed_ref`].

use crate::SampleRef;
use serde::{Deserialize, Serialize};

/// Fades where playback starts and ends, against clicks. Lengths are ms.
/// The names and lengths are ours; `Off` keeps projects as they sounded
/// before 0.2.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Declick {
    #[default]
    Off,
    /// No fade in; a 10 ms S-curve fade out inside the region.
    OutOnly,
    /// The attack kept (a fade in of a few samples); 10 ms out.
    Transient,
    /// As `Transient`, the fade out running 20 ms past the end.
    TransientBleed,
    /// 20 ms in, 20 ms out past the end.
    Generic,
    /// 100 ms in and out.
    Smooth,
    /// 200 ms in and out, for clips that join.
    Crossfade,
}

impl Declick {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::OutOnly,
        Self::Transient,
        Self::TransientBleed,
        Self::Generic,
        Self::Smooth,
        Self::Crossfade,
    ];

    /// Fade in, fade out (ms) and whether the fade out runs past the end
    /// of the region into the data after it.
    pub fn fades(self) -> (f32, f32, bool) {
        match self {
            Self::Off => (0.0, 0.0, false),
            Self::OutOnly => (0.0, 10.0, false),
            Self::Transient => (0.1, 10.0, false),
            Self::TransientBleed => (0.1, 20.0, true),
            Self::Generic => (20.0, 20.0, true),
            Self::Smooth => (100.0, 100.0, true),
            Self::Crossfade => (200.0, 200.0, true),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::OutOnly => "Fade out only",
            Self::Transient => "Keep the attack",
            Self::TransientBleed => "Keep the attack, fade past the end",
            Self::Generic => "Short fades (20 ms)",
            Self::Smooth => "Smooth fades (100 ms)",
            Self::Crossfade => "Crossfades (200 ms)",
        }
    }
}

/// How length and pitch change (spec 0.2.5 §4, our own algorithms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StretchMode {
    /// Pitch and length change together, like tape.
    Resample,
    /// Length and pitch change independently (overlap-add stretching).
    Stretch,
    /// Stretching whenever pitch or length change.
    #[default]
    Auto,
}

impl StretchMode {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Resample, Self::Stretch];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Resample => "Resampling",
            Self::Stretch => "Stretching",
        }
    }
}

/// The length the sample should have (the TIME control).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimeFit {
    /// No fitting.
    #[default]
    None,
    /// This many beats at the project tempo: refitted when the tempo
    /// changes.
    Beats { beats: f32 },
}

/// Stretching and pitch shifting (PITCH, MUL, TIME, mode).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stretch {
    pub mode: StretchMode,
    /// Pitch change in semitones, `-24..=24`.
    pub pitch: f32,
    /// Length multiplier, `0.25..=4`.
    pub mul: f32,
    pub time: TimeFit,
}

impl Default for Stretch {
    fn default() -> Self {
        Self {
            mode: StretchMode::Auto,
            pitch: 0.0,
            mul: 1.0,
            time: TimeFit::None,
        }
    }
}

impl Stretch {
    pub fn is_identity(&self) -> bool {
        self.pitch == 0.0 && self.mul == 1.0 && self.time == TimeFit::None
    }

    /// The length factor for a sample of `seconds` at `tempo_bpm`: TIME
    /// when set, else MUL.
    pub fn length_factor(&self, seconds: f64, tempo_bpm: f64) -> f64 {
        match self.time {
            TimeFit::Beats { beats } if seconds > 0.0 && tempo_bpm > 0.0 => {
                f64::from(beats) * 60.0 / tempo_bpm / seconds
            }
            _ => f64::from(self.mul),
        }
    }

    /// Whether stretching (rather than resampling) is used.
    pub fn stretches(&self) -> bool {
        match self.mode {
            StretchMode::Resample => false,
            StretchMode::Stretch => true,
            StretchMode::Auto => !self.is_identity(),
        }
    }
}

/// Processing applied to the sample data in advance (the Tools panel).
/// Fractions are of the sample's length; 0 turns a knob off.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Precompute {
    #[serde(default)]
    pub remove_dc: bool,
    #[serde(default)]
    pub reverse_polarity: bool,
    #[serde(default)]
    pub normalize: bool,
    /// The sound moves from the left to the right channel.
    #[serde(default)]
    pub fade_stereo: bool,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub swap_stereo: bool,
    /// New start, `0..1`.
    #[serde(default)]
    pub start: f32,
    /// New length, a fraction of what is left after `start`; 0 = all.
    #[serde(default)]
    pub length: f32,
    /// Fade in and out, `0..=0.5`.
    #[serde(default)]
    pub fade_in: f32,
    #[serde(default)]
    pub fade_out: f32,
    /// End blended into the start for smooth loops, `0..=0.5`.
    #[serde(default)]
    pub crossfade: f32,
    /// Trailing audio quieter than this (linear) is cut; 0 cuts only
    /// digital silence.
    #[serde(default)]
    pub trim: f32,
    /// Trim is on (the knob is not at its leftmost).
    #[serde(default)]
    pub trim_on: bool,
}

impl Precompute {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }
}

/// Offsets of the Instrument tab's levels (proposed meaning, spec 0.2.5
/// open question 8): PAN and VOL add to the channel's; MOD X and MOD Y
/// are stored for the note axes of a later version.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Levels {
    /// `-1..=1`.
    pub pan: f32,
    /// Gain factor offset `-1..=1` (−1 silences, +1 doubles).
    pub vol: f32,
    pub mod_x: f32,
    pub mod_y: f32,
}

/// What changes a channel's sample data, with the project tempo when the
/// length follows it. `None`: the file as it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Processing {
    pub precompute: Precompute,
    pub stretch: Stretch,
    /// Project tempo, used by [`TimeFit::Beats`].
    pub tempo_bpm: f64,
}

impl Processing {
    /// A stable 64-bit key of the processing (FNV-1a over the fields).
    pub fn key(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |bytes: &[u8]| {
            for b in bytes {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        let p = &self.precompute;
        eat(&[
            u8::from(p.remove_dc),
            u8::from(p.reverse_polarity),
            u8::from(p.normalize),
            u8::from(p.fade_stereo),
            u8::from(p.reverse),
            u8::from(p.swap_stereo),
            u8::from(p.trim_on),
        ]);
        for v in [
            p.start,
            p.length,
            p.fade_in,
            p.fade_out,
            p.crossfade,
            p.trim,
        ] {
            eat(&v.to_bits().to_le_bytes());
        }
        let s = &self.stretch;
        eat(&[s.mode as u8]);
        eat(&s.pitch.to_bits().to_le_bytes());
        eat(&s.mul.to_bits().to_le_bytes());
        if let TimeFit::Beats { beats } = s.time {
            eat(&beats.to_bits().to_le_bytes());
            eat(&self.tempo_bpm.to_bits().to_le_bytes());
        }
        h
    }
}

/// Separates the file path from the processing key in [`processed_ref`].
pub const PROCESSED_SEPARATOR: char = '\u{1F}';

/// The sample a channel plays: the file, or the file with `processing`
/// under its own key.
pub fn processed_ref(sample: &SampleRef, processing: Option<&Processing>) -> SampleRef {
    match processing {
        None => sample.clone(),
        Some(p) => SampleRef {
            path: format!("{}{PROCESSED_SEPARATOR}{:016x}", sample.path, p.key()),
        },
    }
}

/// The file path of a [`processed_ref`].
pub fn base_path(path: &str) -> &str {
    path.split(PROCESSED_SEPARATOR).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processing_keys_differ_and_follow_the_tempo_only_when_fitted() {
        let p = |tempo| Processing {
            precompute: Precompute {
                reverse: true,
                ..Precompute::default()
            },
            stretch: Stretch::default(),
            tempo_bpm: tempo,
        };
        assert_eq!(p(120.0).key(), p(90.0).key(), "no fit: tempo ignored");
        let mut fitted = p(120.0);
        fitted.stretch.time = TimeFit::Beats { beats: 4.0 };
        let mut slower = fitted;
        slower.tempo_bpm = 90.0;
        assert_ne!(fitted.key(), slower.key(), "fitted: a new tempo, new data");
        let s = SampleRef {
            path: "a.wav".into(),
        };
        let r = processed_ref(&s, Some(&fitted));
        assert_ne!(r, s);
        assert_eq!(base_path(&r.path), "a.wav");
        assert_eq!(processed_ref(&s, None), s);
    }

    #[test]
    fn length_factors_and_modes() {
        let mut s = Stretch::default();
        assert!(!s.stretches(), "Auto with nothing to do");
        s.pitch = 2.0;
        assert!(s.stretches());
        s.mode = StretchMode::Resample;
        assert!(!s.stretches());
        s.mul = 2.0;
        assert_eq!(s.length_factor(1.0, 120.0), 2.0);
        // Two seconds fitted to 8 beats at 120 BPM (4 s): twice as long.
        s.time = TimeFit::Beats { beats: 8.0 };
        assert_eq!(s.length_factor(2.0, 120.0), 2.0);
        assert_eq!(s.length_factor(2.0, 240.0), 1.0, "the tempo changes it");
    }
}
