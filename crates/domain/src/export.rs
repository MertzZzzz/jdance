// SPDX-License-Identifier: GPL-3.0-or-later
//! Export settings (spec "Export"). Not stored in the project: they are
//! per-export choices.

use crate::PatternId;

/// Default time rendered after the last note so effects can ring out.
pub const DEFAULT_EXPORT_TAIL_SECONDS: f32 = 5.0;
/// Upper bound for a user-chosen tail.
pub const MAX_EXPORT_TAIL_SECONDS: f32 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportRange {
    /// The playlist arrangement from 0 to its end (the loop range is
    /// ignored).
    Song,
    /// One pattern played `loops` times.
    Pattern { pattern: PatternId, loops: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    Pcm16,
    #[default]
    Pcm24,
    /// Written as is: values above 0 dBFS are kept (no hidden clamp).
    Float32,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 3] = [Self::Pcm16, Self::Pcm24, Self::Float32];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pcm16 => "16-bit PCM",
            Self::Pcm24 => "24-bit PCM",
            Self::Float32 => "32-bit float",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportOptions {
    pub range: ExportRange,
    /// Seconds rendered after the range end, `0..=MAX_EXPORT_TAIL_SECONDS`.
    pub tail_seconds: f32,
    pub format: ExportFormat,
}

impl ExportOptions {
    pub fn song() -> Self {
        Self {
            range: ExportRange::Song,
            tail_seconds: DEFAULT_EXPORT_TAIL_SECONDS,
            format: ExportFormat::default(),
        }
    }

    /// The tail clamped to the supported range; NaN becomes the default.
    pub fn tail(&self) -> f32 {
        if self.tail_seconds.is_nan() {
            DEFAULT_EXPORT_TAIL_SECONDS
        } else {
            self.tail_seconds.clamp(0.0, MAX_EXPORT_TAIL_SECONDS)
        }
    }

    /// A warning when effects are expected to ring longer than the tail.
    pub fn tail_warning(&self, estimated_tail_seconds: f32) -> Option<String> {
        (estimated_tail_seconds > self.tail() + 0.05).then(|| {
            format!(
                "Effects may ring for about {estimated_tail_seconds:.1} s, longer than the \
                 {:.1} s tail; the end of the render may be cut off.",
                self.tail()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_defaults_clamps_and_warns() {
        let mut o = ExportOptions::song();
        assert_eq!(o.tail(), 5.0);
        assert!(o.tail_warning(4.0).is_none());
        assert!(o.tail_warning(8.0).is_some());
        o.tail_seconds = 1e9;
        assert_eq!(o.tail(), MAX_EXPORT_TAIL_SECONDS);
        o.tail_seconds = -1.0;
        assert_eq!(o.tail(), 0.0);
        o.tail_seconds = f32::NAN;
        assert_eq!(o.tail(), DEFAULT_EXPORT_TAIL_SECONDS);
    }
}
