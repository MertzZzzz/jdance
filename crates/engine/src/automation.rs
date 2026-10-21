// SPDX-License-Identifier: GPL-3.0-or-later
//! Automation lanes compiled for the audio thread (ADR-014, AUT-020).
//!
//! Points are converted to sample positions for the snapshot's tempo;
//! values stay normalized and are mapped to the target's range when
//! read, so curves shape the normalized value as specified.

use jdance_domain::{AutomationLane, Curve, MAX_SEND_LEVEL, MAX_VOLUME, ParamSpec, curve_weight};

/// Automation resolution for gains and built-in parameters (AUT-021).
pub const AUTOMATION_STEP: usize = 32;
/// Spacing of intermediate VST3 points (AUT-030).
pub const PLUGIN_POINT_STEP: usize = 64;

#[derive(Debug, Clone, Copy)]
pub enum AutoMap {
    Volume,
    Pan,
    SendLevel,
    Param(&'static ParamSpec),
    Bypass,
    /// VST3: normalized as is.
    Raw,
}

#[derive(Debug, Clone, Copy)]
struct PointRt {
    pos: u64,
    value: f32,
    curve: Curve,
    tension: f32,
}

#[derive(Debug)]
pub struct AutomationRt {
    points: Vec<PointRt>,
    pub map: AutoMap,
}

impl AutomationRt {
    pub fn compile(lane: &AutomationLane, map: AutoMap, to_sample: &impl Fn(u64) -> u64) -> Self {
        Self {
            points: lane
                .points
                .iter()
                .map(|p| PointRt {
                    pos: to_sample(p.tick),
                    value: p.value,
                    curve: p.curve,
                    tension: p.tension,
                })
                .collect(),
            map,
        }
    }

    /// Normalized value at timeline sample `pos` (AUT-011).
    pub fn normalized_at(&self, pos: u64) -> Option<f32> {
        let first = self.points.first()?;
        if pos < first.pos {
            return Some(first.value);
        }
        let i = self.points.partition_point(|p| p.pos <= pos);
        let a = self.points[i - 1];
        let Some(b) = self.points.get(i) else {
            return Some(a.value);
        };
        let t = (pos - a.pos) as f64 / (b.pos - a.pos) as f64;
        Some(a.value + (b.value - a.value) * curve_weight(a.curve, a.tension, t) as f32)
    }

    /// Value in the target's units.
    pub fn plain_at(&self, pos: u64) -> Option<f32> {
        let n = self.normalized_at(pos)?;
        Some(match self.map {
            AutoMap::Volume => n * MAX_VOLUME,
            AutoMap::Pan => 2.0 * n - 1.0,
            AutoMap::SendLevel => n * MAX_SEND_LEVEL,
            AutoMap::Param(spec) => spec.from_normalized(n),
            AutoMap::Bypass => {
                if n >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            AutoMap::Raw => n,
        })
    }

    /// Envelope points with `pos` in `[from, to)`.
    pub fn breakpoints(&self, from: u64, to: u64) -> impl Iterator<Item = (u64, f32)> + '_ {
        let start = self.points.partition_point(|p| p.pos < from);
        self.points[start..]
            .iter()
            .take_while(move |p| p.pos < to)
            .map(|p| (p.pos, p.value))
    }
}

/// Where the block's samples sit on the timeline: up to a few segments
/// (a loop wrap starts a new one). Fixed size, no allocation.
#[derive(Debug, Clone, Copy)]
pub struct BlockTimeline {
    /// (block offset, timeline position) per segment start.
    segs: [(usize, u64); 8],
    len: usize,
}

impl BlockTimeline {
    pub fn new(pos: u64) -> Self {
        Self {
            segs: [(0, pos); 8],
            len: 1,
        }
    }

    pub fn push(&mut self, offset: usize, pos: u64) {
        if self.len < self.segs.len() {
            self.segs[self.len] = (offset, pos);
            self.len += 1;
        }
    }

    /// Timeline position of block offset `offset`.
    pub fn pos_at(&self, offset: usize) -> u64 {
        let segs = &self.segs[..self.len];
        let i = segs.partition_point(|s| s.0 <= offset).max(1) - 1;
        let (o, p) = segs[i];
        p + (offset - o) as u64
    }

    /// Segments as `(block offset, timeline start, length)` for a block of
    /// `n` samples.
    pub fn segments(&self, n: usize) -> impl Iterator<Item = (usize, u64, usize)> + '_ {
        let segs = &self.segs[..self.len];
        segs.iter().enumerate().map(move |(i, &(o, p))| {
            let end = segs.get(i + 1).map_or(n, |s| s.0);
            (o, p, end.saturating_sub(o))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{AutoPoint, AutomationId, AutomationTarget, TrackId};

    #[test]
    fn samples_mapping_and_breakpoints() {
        let lane = AutomationLane {
            id: AutomationId(1),
            target: AutomationTarget::TrackPan(TrackId(1)),
            points: vec![
                AutoPoint {
                    tick: 0,
                    value: 0.0,
                    curve: Curve::Linear,
                    tension: 0.0,
                },
                AutoPoint {
                    tick: 960,
                    value: 1.0,
                    curve: Curve::Linear,
                    tension: 0.0,
                },
            ],
        };
        // 960 ticks → 24 000 samples (120 BPM, 48 kHz).
        let a = AutomationRt::compile(&lane, AutoMap::Pan, &|t| t * 25);
        assert_eq!(a.plain_at(0), Some(-1.0));
        assert_eq!(a.plain_at(12_000), Some(0.0));
        assert_eq!(a.plain_at(99_999), Some(1.0));
        let bp: Vec<_> = a.breakpoints(1, 30_000).collect();
        assert_eq!(bp, [(24_000, 1.0)]);
    }

    #[test]
    fn block_timeline_follows_wraps() {
        let mut t = BlockTimeline::new(1_000);
        t.push(100, 0);
        assert_eq!(t.pos_at(0), 1_000);
        assert_eq!(t.pos_at(99), 1_099);
        assert_eq!(t.pos_at(100), 0);
        assert_eq!(t.pos_at(150), 50);
        let segs: Vec<_> = t.segments(256).collect();
        assert_eq!(segs, [(0, 1_000, 100), (100, 0, 156)]);
    }
}
