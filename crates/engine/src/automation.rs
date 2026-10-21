// SPDX-License-Identifier: GPL-3.0-or-later
//! Automation compiled for the audio thread (ADR-014, AUT-020, spec
//! 0.2.0 §11): sources sound only inside the windows of their clips.
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
    /// Samples from the source start.
    pos: i64,
    value: f32,
    curve: Curve,
    tension: f32,
}

/// Where a clip makes its source active: timeline `[start, end)` in
/// samples; `origin` is the timeline position of the source start.
#[derive(Debug, Clone, Copy)]
struct Window {
    start: u64,
    end: u64,
    source: usize,
    origin: i64,
}

/// All automation of one target (spec 0.2.0 §11): its sources and the
/// windows where clips make them active. Outside every window the target
/// keeps its manual value.
#[derive(Debug)]
pub struct AutomationRt {
    sources: Vec<Vec<PointRt>>,
    /// Sorted by start, not overlapping.
    windows: Vec<Window>,
    pub map: AutoMap,
}

impl AutomationRt {
    pub fn new(map: AutoMap) -> Self {
        Self {
            sources: Vec::new(),
            windows: Vec::new(),
            map,
        }
    }

    /// Adds a source's points; returns its index for [`Self::add_window`].
    pub fn add_source(&mut self, lane: &AutomationLane, to_sample: &impl Fn(u64) -> u64) -> usize {
        self.sources.push(
            lane.points
                .iter()
                .map(|p| PointRt {
                    pos: to_sample(p.tick) as i64,
                    value: p.value,
                    curve: p.curve,
                    tension: p.tension,
                })
                .collect(),
        );
        self.sources.len() - 1
    }

    /// Makes `source` active on timeline `[start, end)`, with source
    /// position `offset` (samples) at `start`.
    pub fn add_window(&mut self, source: usize, start: u64, end: u64, offset: u64) {
        if start < end && self.sources.get(source).is_some_and(|s| !s.is_empty()) {
            self.windows.push(Window {
                start,
                end,
                source,
                origin: start as i64 - offset as i64,
            });
        }
    }

    /// Sorts the windows; an overlap (rejected by the editor) is cut so
    /// the earlier clip ends where the later starts.
    pub fn finish(&mut self) {
        self.windows.sort_by_key(|w| w.start);
        for i in 1..self.windows.len() {
            let next = self.windows[i].start;
            let prev = &mut self.windows[i - 1];
            prev.end = prev.end.min(next);
        }
        self.windows.retain(|w| w.start < w.end);
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    fn window_at(&self, pos: u64) -> Option<&Window> {
        let i = self.windows.partition_point(|w| w.start <= pos);
        let w = self.windows.get(i.checked_sub(1)?)?;
        (pos < w.end).then_some(w)
    }

    /// Normalized value at timeline sample `pos`; `None` outside every
    /// clip. Inside, the first value holds before the first point and the
    /// last after the last (AUT-011).
    pub fn normalized_at(&self, pos: u64) -> Option<f32> {
        let w = self.window_at(pos)?;
        let points = &self.sources[w.source];
        let rel = pos as i64 - w.origin;
        let first = points.first()?;
        if rel < first.pos {
            return Some(first.value);
        }
        let i = points.partition_point(|p| p.pos <= rel);
        let a = points[i - 1];
        let Some(b) = points.get(i) else {
            return Some(a.value);
        };
        let t = (rel - a.pos) as f64 / (b.pos - a.pos) as f64;
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

    /// Where the value may jump inside `[from, to)`: window starts and
    /// envelope points, with the value from there on.
    pub fn breakpoints(&self, from: u64, to: u64) -> impl Iterator<Item = (u64, f32)> + '_ {
        let first = self.windows.partition_point(|w| w.end <= from);
        self.windows[first..]
            .iter()
            .take_while(move |w| w.start < to)
            .flat_map(move |w| {
                let (a, b) = (w.start.max(from), w.end.min(to));
                let points = &self.sources[w.source];
                let lo = a as i64 - w.origin;
                let k = points.partition_point(|p| p.pos < lo);
                let start = (w.start >= from)
                    .then(|| self.normalized_at(w.start).map(|v| (w.start, v)))
                    .flatten();
                start.into_iter().chain(
                    points[k..]
                        .iter()
                        .map(move |p| ((p.pos + w.origin) as u64, p.value))
                        .take_while(move |(pos, _)| *pos < b)
                        .filter(move |(pos, _)| *pos != w.start),
                )
            })
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
            name: "Pan".into(),
            target: AutomationTarget::TrackPan(TrackId(1)),
            length: 3840,
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
        let mut a = AutomationRt::new(AutoMap::Pan);
        let s = a.add_source(&lane, &|t| t * 25);
        a.add_window(s, 0, 100_000, 0);
        a.finish();
        assert_eq!(a.plain_at(0), Some(-1.0));
        assert_eq!(a.plain_at(12_000), Some(0.0));
        assert_eq!(a.plain_at(99_999), Some(1.0));
        assert_eq!(a.plain_at(100_000), None, "after the clip: manual value");
        let bp: Vec<_> = a.breakpoints(1, 30_000).collect();
        assert_eq!(bp, [(24_000, 1.0)]);
    }

    #[test]
    fn windows_place_a_range_of_the_source() {
        let lane = AutomationLane {
            id: AutomationId(1),
            name: "Pan".into(),
            target: AutomationTarget::TrackPan(TrackId(1)),
            length: 3840,
            points: vec![
                AutoPoint {
                    tick: 0,
                    value: 0.0,
                    curve: Curve::Linear,
                    tension: 0.0,
                },
                AutoPoint {
                    tick: 1000,
                    value: 1.0,
                    curve: Curve::Linear,
                    tension: 0.0,
                },
            ],
        };
        let mut a = AutomationRt::new(AutoMap::Raw);
        let s = a.add_source(&lane, &|t| t);
        // Source 500..1500 at 10 000, and the whole source at 20 000.
        a.add_window(s, 20_000, 23_840, 0);
        a.add_window(s, 10_000, 11_000, 500);
        a.finish();
        assert_eq!(a.normalized_at(9_999), None);
        assert_eq!(a.normalized_at(10_000), Some(0.5));
        assert_eq!(a.normalized_at(10_499), Some(0.999));
        assert_eq!(a.normalized_at(10_999), Some(1.0));
        assert_eq!(a.normalized_at(11_000), None);
        assert_eq!(a.normalized_at(20_000), Some(0.0));
        let bp: Vec<_> = a.breakpoints(0, 30_000).collect();
        assert_eq!(
            bp,
            [(10_000, 0.5), (10_500, 1.0), (20_000, 0.0), (21_000, 1.0)]
        );
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
