// SPDX-License-Identifier: GPL-3.0-or-later
//! Time signature of the project (spec 0.2.3 SIG-001…003).
//!
//! The tick and the step do not depend on it: [`PPQ`] ticks per quarter
//! note, a step is a sixteenth. Bars and beats are derived here and nowhere
//! else; `xtask check` rejects bar-length literals outside this module.

use crate::{PPQ, TICKS_PER_STEP};
use serde::{Deserialize, Serialize};

/// Beats per bar and the note value of a beat (SIG-001).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

pub const MAX_NUMERATOR: u8 = 16;
/// Ticks of a whole note (a note value, not a bar).
pub const WHOLE_NOTE: u32 = 4 * PPQ;
pub const DENOMINATORS: [u8; 4] = [2, 4, 8, 16];

/// Ready-made choices in the transport field (SIG-010).
pub const COMMON_SIGNATURES: [TimeSignature; 8] = [
    TimeSignature::new(2, 4),
    TimeSignature::new(3, 4),
    TimeSignature::COMMON,
    TimeSignature::new(5, 4),
    TimeSignature::new(6, 8),
    TimeSignature::new(7, 8),
    TimeSignature::new(9, 8),
    TimeSignature::new(12, 8),
];

impl Default for TimeSignature {
    fn default() -> Self {
        Self::COMMON
    }
}

impl std::fmt::Display for TimeSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

impl TimeSignature {
    /// 4/4.
    pub const COMMON: Self = Self::new(4, 4);

    pub const fn new(numerator: u8, denominator: u8) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    pub fn is_common(&self) -> bool {
        *self == Self::COMMON
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_NUMERATOR).contains(&self.numerator) {
            return Err(format!(
                "time signature numerator {} outside 1..={MAX_NUMERATOR}",
                self.numerator
            ));
        }
        if !DENOMINATORS.contains(&self.denominator) {
            return Err(format!(
                "time signature denominator {} is not 2, 4, 8 or 16",
                self.denominator
            ));
        }
        Ok(())
    }

    /// Ticks in one beat: `PPQ * 4 / denominator` (SIG-002).
    pub const fn beat_ticks(&self) -> u32 {
        PPQ * 4 / self.denominator as u32
    }

    /// Ticks in one bar.
    pub const fn bar_ticks(&self) -> u32 {
        self.beat_ticks() * self.numerator as u32
    }

    /// Steps (sixteenths) in one beat; always whole for the allowed
    /// denominators.
    pub const fn steps_per_beat(&self) -> u32 {
        self.beat_ticks() / TICKS_PER_STEP
    }

    /// Steps in one bar: `numerator * 16 / denominator`.
    pub const fn steps_per_bar(&self) -> u32 {
        self.bar_ticks() / TICKS_PER_STEP
    }

    /// 0-based bar, 0-based beat in the bar and tick in the beat.
    pub fn split(&self, ticks: u64) -> (u64, u32, u32) {
        let bar = u64::from(self.bar_ticks());
        let beat = u64::from(self.beat_ticks());
        let in_bar = ticks % bar;
        (ticks / bar, (in_bar / beat) as u32, (in_bar % beat) as u32)
    }

    /// The first tick of the bar at or after `ticks`.
    pub fn ceil_to_bar(&self, ticks: u64) -> u64 {
        ticks.div_ceil(u64::from(self.bar_ticks())) * u64::from(self.bar_ticks())
    }

    /// `steps` rounded up to whole bars.
    pub fn ceil_steps_to_bar(&self, steps: u32) -> u32 {
        steps.div_ceil(self.steps_per_bar()) * self.steps_per_bar()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_is_whole_steps_for_every_signature() {
        for d in DENOMINATORS {
            for n in 1..=MAX_NUMERATOR {
                let s = TimeSignature::new(n, d);
                s.validate().unwrap();
                assert_eq!(s.beat_ticks() * 4 * u32::from(d), PPQ * 16);
                assert_eq!(s.bar_ticks() % TICKS_PER_STEP, 0, "{s}");
                assert_eq!(s.steps_per_bar(), u32::from(n) * 16 / u32::from(d));
            }
        }
    }

    #[test]
    fn spec_examples() {
        let steps = |n, d| TimeSignature::new(n, d).steps_per_bar();
        assert_eq!(steps(4, 4), 16);
        assert_eq!(steps(3, 4), 12);
        assert_eq!(steps(6, 8), 12);
        assert_eq!(steps(7, 8), 14);
        assert_eq!(steps(5, 4), 20);
        assert_eq!(steps(3, 2), 24);
        assert_eq!(TimeSignature::COMMON.bar_ticks(), 4 * PPQ);
    }

    #[test]
    fn invalid_signatures_are_rejected() {
        assert!(TimeSignature::new(0, 4).validate().is_err());
        assert!(TimeSignature::new(17, 4).validate().is_err());
        assert!(TimeSignature::new(4, 3).validate().is_err());
        assert!(TimeSignature::new(4, 32).validate().is_err());
    }

    #[test]
    fn split_and_rounding() {
        let s = TimeSignature::new(3, 4);
        assert_eq!(s.split(0), (0, 0, 0));
        assert_eq!(s.split(u64::from(3 * PPQ) + u64::from(PPQ) + 5), (1, 1, 5));
        assert_eq!(s.ceil_to_bar(1), u64::from(3 * PPQ));
        assert_eq!(s.ceil_to_bar(0), 0);
        assert_eq!(s.ceil_steps_to_bar(13), 24);
        let e = TimeSignature::new(7, 8);
        assert_eq!(e.split(u64::from(e.beat_ticks()) * 8), (1, 1, 0));
    }
}
