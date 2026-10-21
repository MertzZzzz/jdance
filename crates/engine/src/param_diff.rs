// SPDX-License-Identifier: GPL-3.0-or-later
//! Detects edits that only change effect parameters, so they can be sent
//! as smoothed parameter commands instead of recompiling the snapshot
//! (which would rebuild effect buffers on every slider movement).

use jdance_domain::{InsertId, Project};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamChange {
    pub insert: InsertId,
    /// Index in the effect's parameter schema.
    pub index: u16,
    pub value: f32,
}

/// If `new` differs from `old` only in effect parameters that do not
/// change latency, returns those changes; otherwise `None` (a recompile is
/// needed). An empty list means the projects are equal.
pub fn param_only_changes(old: &Project, new: &Project) -> Option<Vec<ParamChange>> {
    let mut patched = old.clone();
    let mut changes = Vec::new();
    let new_tracks = std::iter::once(&new.mixer.master).chain(&new.mixer.tracks);
    let old_tracks =
        std::iter::once(&mut patched.mixer.master).chain(patched.mixer.tracks.iter_mut());
    for (o, n) in old_tracks.zip(new_tracks) {
        for (oi, ni) in o.inserts.iter_mut().zip(&n.inserts) {
            if oi.id != ni.id || oi.effect == ni.effect {
                continue;
            }
            if oi.effect.changes_latency(&ni.effect) {
                return None;
            }
            for (index, spec) in ni.effect.kind.params().iter().enumerate() {
                let v = ni.effect.get(spec.id);
                if oi.effect.get(spec.id) != v {
                    changes.push(ParamChange {
                        insert: ni.id,
                        index: index as u16,
                        value: v,
                    });
                }
            }
            oi.effect = ni.effect.clone();
        }
    }
    (patched == *new).then_some(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{Effect, EffectKind, Insert, MixerTrack, TrackId};

    fn project(effect: Effect) -> Project {
        let mut p = Project::new();
        let mut t = MixerTrack::new(TrackId(5), "T");
        t.inserts.push(Insert {
            id: InsertId(6),
            bypass: false,
            effect,
            sidechain: None,
        });
        p.mixer.tracks.push(t);
        p
    }

    #[test]
    fn detects_parameter_only_edits() {
        let a = project(Effect::new(EffectKind::Reverb));
        let b = project(Effect::new(EffectKind::Reverb).with("wet", 0.9));
        assert_eq!(
            param_only_changes(&a, &b),
            Some(vec![ParamChange {
                insert: InsertId(6),
                index: 4,
                value: 0.9
            }])
        );
        assert_eq!(param_only_changes(&a, &a), Some(vec![]));
        // Anything else needs a recompile.
        let mut c = b.clone();
        c.tempo_bpm = 100.0;
        assert_eq!(param_only_changes(&a, &c), None);
        let d = project(Effect::new(EffectKind::Compressor));
        let e = project(Effect::new(EffectKind::Compressor).with("lookahead", 2.0));
        assert_eq!(param_only_changes(&d, &e), None, "latency change");
        assert_eq!(param_only_changes(&a, &d), None, "effect type change");
    }
}
