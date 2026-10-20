// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer document model and routing graph rules (section 17).
//!
//! Routing is a directed graph over tracks (the master included) with
//! three edge kinds: a track's main out, its sends, and sidechain feeds
//! into insert effects. The graph must be acyclic (MIX-002).

use crate::{InsertId, SendId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Mixer tracks besides the master (MIX-003).
pub const MAX_TRACKS: usize = 128;
pub const MAX_SENDS_PER_TRACK: usize = 16;
pub const MAX_INSERTS_PER_TRACK: usize = 10;
/// Linear send gain limit, ≈ +6 dB (MIX-011).
pub const MAX_SEND_LEVEL: f32 = 2.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mixer {
    pub master: MixerTrack,
    pub tracks: Vec<MixerTrack>,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            master: MixerTrack::new(TrackId::MASTER, "Master"),
            tracks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixerTrack {
    pub id: TrackId,
    pub name: String,
    /// Linear gain, `0.0..=MAX_VOLUME`.
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    /// Destination of the main output; ignored for the master.
    pub main_out: TrackId,
    pub sends: Vec<Send>,
    pub inserts: Vec<Insert>,
}

impl MixerTrack {
    pub fn new(id: TrackId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            volume: 1.0,
            pan: 0.0,
            muted: false,
            main_out: TrackId::MASTER,
            sends: Vec::new(),
            inserts: Vec::new(),
        }
    }

    pub fn is_master(&self) -> bool {
        self.id == TrackId::MASTER
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Send {
    pub id: SendId,
    pub dest: TrackId,
    /// Linear gain, `0.0..=MAX_SEND_LEVEL`.
    pub level: f32,
    pub pre_fader: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insert {
    pub id: InsertId,
    pub bypass: bool,
    pub effect: Effect,
    pub sidechain: Option<Sidechain>,
}

/// Where a source track's signal is taken for a sidechain (MIX-010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tap {
    PreFader,
    PostFader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidechain {
    pub source: TrackId,
    pub tap: Tap,
}

/// Built-in effects. VST3 inserts are added in Milestone P1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    Compressor(CompressorParams),
}

impl Effect {
    pub fn name(&self) -> &'static str {
        match self {
            Effect::Compressor(_) => "Compressor",
        }
    }

    pub fn accepts_sidechain(&self) -> bool {
        match self {
            Effect::Compressor(_) => true,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Effect::Compressor(p) => p.validate(),
        }
    }
}

/// Feed-forward compressor with optional sidechain and lookahead (MIX-040).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompressorParams {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
    /// Introduces latency reported for delay compensation.
    pub lookahead_ms: f32,
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 120.0,
            makeup_db: 0.0,
            lookahead_ms: 0.0,
        }
    }
}

impl CompressorParams {
    pub const THRESHOLD_DB: (f32, f32) = (-60.0, 0.0);
    pub const RATIO: (f32, f32) = (1.0, 20.0);
    pub const ATTACK_MS: (f32, f32) = (0.0, 200.0);
    pub const RELEASE_MS: (f32, f32) = (1.0, 2000.0);
    pub const MAKEUP_DB: (f32, f32) = (0.0, 24.0);
    pub const LOOKAHEAD_MS: (f32, f32) = (0.0, 20.0);

    pub fn validate(&self) -> Result<(), String> {
        let fields = [
            ("threshold", self.threshold_db, Self::THRESHOLD_DB),
            ("ratio", self.ratio, Self::RATIO),
            ("attack", self.attack_ms, Self::ATTACK_MS),
            ("release", self.release_ms, Self::RELEASE_MS),
            ("makeup", self.makeup_db, Self::MAKEUP_DB),
            ("lookahead", self.lookahead_ms, Self::LOOKAHEAD_MS),
        ];
        for (name, v, (lo, hi)) in fields {
            if !(v.is_finite() && (lo..=hi).contains(&v)) {
                return Err(format!("compressor {name} {v} outside {lo}..={hi}"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingError {
    UnknownTrack(TrackId),
    DuplicateId(u64),
    /// The routing would contain a cycle; tracks in cycle order.
    Cycle(Vec<TrackId>),
    SelfRoute(TrackId),
    TooMany(&'static str),
    InvalidValue(String),
}

impl fmt::Display for RoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoutingError::UnknownTrack(t) => write!(f, "track {} does not exist", t.0),
            RoutingError::DuplicateId(id) => write!(f, "duplicate mixer ID {id}"),
            RoutingError::Cycle(path) => {
                let names: Vec<String> = path.iter().map(|t| t.0.to_string()).collect();
                write!(
                    f,
                    "routing would create a feedback loop through tracks {}",
                    names.join(" → ")
                )
            }
            RoutingError::SelfRoute(t) => write!(f, "track {} cannot route to itself", t.0),
            RoutingError::TooMany(what) => write!(f, "too many {what}"),
            RoutingError::InvalidValue(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for RoutingError {}

impl Mixer {
    pub fn all_tracks(&self) -> impl Iterator<Item = &MixerTrack> {
        std::iter::once(&self.master).chain(&self.tracks)
    }

    pub fn has_track(&self, id: TrackId) -> bool {
        self.track(id).is_some()
    }

    pub fn track(&self, id: TrackId) -> Option<&MixerTrack> {
        self.all_tracks().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut MixerTrack> {
        if id == TrackId::MASTER {
            Some(&mut self.master)
        } else {
            self.tracks.iter_mut().find(|t| t.id == id)
        }
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// Outgoing routing edges of `t`: (destination, kind) where the
    /// destination receives signal from `t`.
    fn edges(&self) -> BTreeMap<TrackId, BTreeSet<TrackId>> {
        let mut out: BTreeMap<TrackId, BTreeSet<TrackId>> = BTreeMap::new();
        for t in self.all_tracks() {
            let e = out.entry(t.id).or_default();
            if !t.is_master() {
                e.insert(t.main_out);
            }
            e.extend(t.sends.iter().map(|s| s.dest));
        }
        for t in self.all_tracks() {
            for sc in t.inserts.iter().filter_map(|i| i.sidechain) {
                out.entry(sc.source).or_default().insert(t.id);
            }
        }
        out
    }

    /// Track IDs in processing order: every track comes after all tracks
    /// that feed it. The master is always last.
    pub fn processing_order(&self) -> Result<Vec<TrackId>, RoutingError> {
        let edges = self.edges();
        let mut indegree: BTreeMap<TrackId, usize> = self.all_tracks().map(|t| (t.id, 0)).collect();
        for dests in edges.values() {
            for d in dests {
                *indegree.get_mut(d).ok_or(RoutingError::UnknownTrack(*d))? += 1;
            }
        }
        // Kahn's algorithm, visiting in document order for stable output.
        let doc_order: Vec<TrackId> = self
            .tracks
            .iter()
            .map(|t| t.id)
            .chain([TrackId::MASTER])
            .collect();
        let mut ready: Vec<TrackId> = doc_order
            .iter()
            .copied()
            .filter(|t| indegree[t] == 0)
            .collect();
        let mut order = Vec::with_capacity(doc_order.len());
        while let Some(t) = ready.first().copied() {
            ready.remove(0);
            order.push(t);
            for d in edges.get(&t).into_iter().flatten() {
                let n = indegree.get_mut(d).expect("checked");
                *n -= 1;
                if *n == 0 {
                    let pos = doc_order.iter().position(|x| x == d).expect("known track");
                    let at = ready
                        .iter()
                        .position(|r| doc_order.iter().position(|x| x == r) > Some(pos))
                        .unwrap_or(ready.len());
                    ready.insert(at, *d);
                }
            }
        }
        if order.len() != doc_order.len() {
            return Err(RoutingError::Cycle(self.find_cycle(&edges)));
        }
        debug_assert_eq!(order.last(), Some(&TrackId::MASTER));
        Ok(order)
    }

    fn find_cycle(&self, edges: &BTreeMap<TrackId, BTreeSet<TrackId>>) -> Vec<TrackId> {
        // Iterative DFS with colors; returns the first cycle found.
        #[derive(Clone, Copy, PartialEq)]
        enum C {
            White,
            Grey,
            Black,
        }
        let mut color: BTreeMap<TrackId, C> = self.all_tracks().map(|t| (t.id, C::White)).collect();
        for start in self.all_tracks().map(|t| t.id) {
            if color[&start] != C::White {
                continue;
            }
            let mut stack: Vec<(TrackId, Vec<TrackId>)> = vec![(
                start,
                edges.get(&start).into_iter().flatten().copied().collect(),
            )];
            color.insert(start, C::Grey);
            while let Some((node, pending)) = stack.last_mut() {
                let node = *node;
                match pending.pop() {
                    Some(next) => match color.get(&next).copied() {
                        Some(C::Grey) => {
                            let from = stack.iter().position(|(n, _)| *n == next).unwrap_or(0);
                            return stack[from..]
                                .iter()
                                .map(|(n, _)| *n)
                                .chain([next])
                                .collect();
                        }
                        Some(C::White) => {
                            color.insert(next, C::Grey);
                            let succ = edges.get(&next).into_iter().flatten().copied().collect();
                            stack.push((next, succ));
                        }
                        _ => {}
                    },
                    None => {
                        color.insert(node, C::Black);
                        stack.pop();
                    }
                }
            }
        }
        Vec::new()
    }

    /// Full structural validation: IDs, references, limits, values and
    /// acyclicity.
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.master.id != TrackId::MASTER {
            return Err(RoutingError::InvalidValue(
                "master track must have ID 0".into(),
            ));
        }
        if self.tracks.len() > MAX_TRACKS {
            return Err(RoutingError::TooMany("mixer tracks"));
        }
        let mut ids = BTreeSet::new();
        for t in self.all_tracks() {
            let dup = |id: u64, ids: &mut BTreeSet<u64>| {
                if ids.insert(id) {
                    Ok(())
                } else {
                    Err(RoutingError::DuplicateId(id))
                }
            };
            dup(t.id.0, &mut ids)?;
            if !t.is_master() && t.id == TrackId::MASTER {
                return Err(RoutingError::DuplicateId(0));
            }
            if t.sends.len() > MAX_SENDS_PER_TRACK {
                return Err(RoutingError::TooMany("sends on one track"));
            }
            if t.inserts.len() > MAX_INSERTS_PER_TRACK {
                return Err(RoutingError::TooMany("inserts on one track"));
            }
            let finite_in = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
            if !finite_in(t.volume, 0.0, crate::MAX_VOLUME) || !finite_in(t.pan, -1.0, 1.0) {
                return Err(RoutingError::InvalidValue(format!(
                    "track {} volume/pan out of range",
                    t.id.0
                )));
            }
            if !t.is_master() {
                if t.main_out == t.id {
                    return Err(RoutingError::SelfRoute(t.id));
                }
                if !self.has_track(t.main_out) {
                    return Err(RoutingError::UnknownTrack(t.main_out));
                }
            }
            for s in &t.sends {
                dup(s.id.0, &mut ids)?;
                if s.dest == t.id {
                    return Err(RoutingError::SelfRoute(t.id));
                }
                if !self.has_track(s.dest) {
                    return Err(RoutingError::UnknownTrack(s.dest));
                }
                if !finite_in(s.level, 0.0, MAX_SEND_LEVEL) {
                    return Err(RoutingError::InvalidValue(format!(
                        "send {} level out of range",
                        s.id.0
                    )));
                }
            }
            for i in &t.inserts {
                dup(i.id.0, &mut ids)?;
                i.effect.validate().map_err(RoutingError::InvalidValue)?;
                if let Some(sc) = i.sidechain {
                    if !i.effect.accepts_sidechain() {
                        return Err(RoutingError::InvalidValue(format!(
                            "{} has no sidechain input",
                            i.effect.name()
                        )));
                    }
                    if sc.source == t.id {
                        return Err(RoutingError::SelfRoute(t.id));
                    }
                    if !self.has_track(sc.source) {
                        return Err(RoutingError::UnknownTrack(sc.source));
                    }
                }
            }
        }
        self.processing_order().map(|_| ())
    }

    /// Tracks that can receive signal from `from` (main out, send or
    /// sidechain source) without creating a cycle. Used by the UI to offer
    /// only valid destinations (MIX-021).
    pub fn valid_destinations(&self, from: TrackId) -> Vec<TrackId> {
        let edges = self.edges();
        // Destinations must not be upstream of `from`, i.e. `from` must not
        // be reachable from them.
        self.all_tracks()
            .map(|t| t.id)
            .filter(|d| *d != from && !reaches(&edges, *d, from))
            .collect()
    }

    /// Tracks whose signal may feed a sidechain on `to` without a cycle.
    pub fn valid_sidechain_sources(&self, to: TrackId) -> Vec<TrackId> {
        let edges = self.edges();
        self.all_tracks()
            .map(|t| t.id)
            .filter(|s| *s != to && !reaches(&edges, to, *s))
            .collect()
    }
}

fn reaches(edges: &BTreeMap<TrackId, BTreeSet<TrackId>>, from: TrackId, to: TrackId) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(n) = stack.pop() {
        if n == to {
            return true;
        }
        if seen.insert(n) {
            stack.extend(edges.get(&n).into_iter().flatten().copied());
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer(n: u64) -> Mixer {
        let mut m = Mixer::default();
        for i in 1..=n {
            m.tracks.push(MixerTrack::new(TrackId(i), format!("T{i}")));
        }
        m
    }

    fn send(id: u64, dest: u64) -> Send {
        Send {
            id: SendId(id),
            dest: TrackId(dest),
            level: 1.0,
            pre_fader: false,
        }
    }

    #[test]
    fn order_puts_sources_before_destinations_and_master_last() {
        let mut m = mixer(3);
        // T1 → T2 (main out), T3 sends to T1, master last.
        m.tracks[0].main_out = TrackId(2);
        m.tracks[2].sends.push(send(100, 1));
        let order = m.processing_order().unwrap();
        let pos = |id: u64| order.iter().position(|t| t.0 == id).unwrap();
        assert!(pos(3) < pos(1) && pos(1) < pos(2));
        assert_eq!(order.last(), Some(&TrackId::MASTER));
        assert!(m.validate().is_ok());
    }

    #[test]
    fn cycles_are_detected_with_path() {
        let mut m = mixer(2);
        m.tracks[0].main_out = TrackId(2);
        m.tracks[1].sends.push(send(100, 1));
        match m.validate() {
            Err(RoutingError::Cycle(path)) => {
                assert_eq!(path.first(), path.last());
                assert!(path.contains(&TrackId(1)) && path.contains(&TrackId(2)));
            }
            other => panic!("expected cycle, got {other:?}"),
        }
    }

    #[test]
    fn sidechain_edges_count_for_cycles() {
        let mut m = mixer(2);
        m.tracks[0].main_out = TrackId(2);
        // T2's signal feeding a compressor on T1 closes T1 → T2 → T1.
        m.tracks[0].inserts.push(Insert {
            id: InsertId(50),
            bypass: false,
            effect: Effect::Compressor(CompressorParams::default()),
            sidechain: Some(Sidechain {
                source: TrackId(2),
                tap: Tap::PreFader,
            }),
        });
        assert!(matches!(m.validate(), Err(RoutingError::Cycle(_))));
        assert!(!m.valid_sidechain_sources(TrackId(1)).contains(&TrackId(2)));
        assert!(!m.valid_destinations(TrackId(2)).contains(&TrackId(1)));
    }

    #[test]
    fn master_sidechain_into_track_is_a_cycle() {
        let mut m = mixer(1);
        assert!(
            !m.valid_sidechain_sources(TrackId(1))
                .contains(&TrackId::MASTER)
        );
        m.tracks[0].inserts.push(Insert {
            id: InsertId(50),
            bypass: false,
            effect: Effect::Compressor(CompressorParams::default()),
            sidechain: Some(Sidechain {
                source: TrackId::MASTER,
                tap: Tap::PostFader,
            }),
        });
        assert!(matches!(m.validate(), Err(RoutingError::Cycle(_))));
    }

    #[test]
    fn structural_errors() {
        let mut m = mixer(1);
        m.tracks[0].main_out = TrackId(9);
        assert_eq!(m.validate(), Err(RoutingError::UnknownTrack(TrackId(9))));
        let mut m = mixer(1);
        m.tracks[0].sends.push(send(100, 1));
        assert_eq!(m.validate(), Err(RoutingError::SelfRoute(TrackId(1))));
        let mut m = mixer(2);
        m.tracks[0].sends.push(send(2, 2));
        assert_eq!(m.validate(), Err(RoutingError::DuplicateId(2)));
        let mut m = mixer(1);
        m.tracks[0].sends.push(Send {
            level: 9.0,
            ..send(100, 0)
        });
        assert!(matches!(m.validate(), Err(RoutingError::InvalidValue(_))));
    }
}
