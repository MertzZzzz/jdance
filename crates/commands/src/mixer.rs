// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer edits.
//!
//! Parameter edits (volume, pan, send level, effect parameters) have exact
//! cheap inverses and merge during drags. Structural edits are applied,
//! then the whole routing graph is validated (MIX-002); on failure the
//! project is restored unchanged. Their inverse is a snapshot of the mixer
//! and of the channel routing, which keeps multi-object changes (e.g.
//! removing a track rewires channels, sends and sidechains) exactly
//! undoable.

use crate::{Edit, EditError, History, check_range};
use jdance_domain::{
    ChannelId, Effect, Insert, InsertId, MAX_SEND_LEVEL, MAX_VOLUME, Mixer, MixerTrack, Project,
    RoutingError, Send, SendId, TrackId,
};

fn tracks_mut(m: &mut Mixer) -> impl Iterator<Item = &mut MixerTrack> {
    std::iter::once(&mut m.master).chain(m.tracks.iter_mut())
}

fn send_mut(m: &mut Mixer, id: SendId) -> Result<&mut Send, EditError> {
    tracks_mut(m)
        .flat_map(|t| t.sends.iter_mut())
        .find(|s| s.id == id)
        .ok_or(EditError::NoSuchMixerObject(id.0))
}

fn insert_mut(m: &mut Mixer, id: InsertId) -> Result<&mut Insert, EditError> {
    tracks_mut(m)
        .flat_map(|t| t.inserts.iter_mut())
        .find(|i| i.id == id)
        .ok_or(EditError::NoSuchMixerObject(id.0))
}

fn track_mut(p: &mut Project, id: TrackId) -> Result<&mut MixerTrack, EditError> {
    p.mixer
        .track_mut(id)
        .ok_or(EditError::Routing(RoutingError::UnknownTrack(id)))
}

fn channel_tracks(p: &Project) -> Vec<(ChannelId, TrackId)> {
    p.channels.iter().map(|c| (c.id, c.track)).collect()
}

fn restore_channel_tracks(p: &mut Project, routes: &[(ChannelId, TrackId)]) {
    for (c, t) in routes {
        if let Some(ch) = p.channel_mut(*c) {
            ch.track = *t;
        }
    }
}

/// Runs a structural change with validation and snapshot inverse.
fn structural(
    p: &mut Project,
    f: impl FnOnce(&mut Project) -> Result<(), EditError>,
) -> Result<Edit, EditError> {
    let mixer = p.mixer.clone();
    let routes = channel_tracks(p);
    let automation = p.automation.clone();
    let existed: Vec<bool> = automation.iter().map(|l| l.target.exists(p)).collect();
    let result = f(p).and_then(|()| p.validate_routing().map_err(EditError::Routing));
    match result {
        Ok(()) => {
            // Lanes of removed objects go with them (AUT-050); lanes that
            // already had no target (missing plugin) stay.
            let mut i = 0;
            let p_ref: &Project = p;
            let keep: Vec<bool> = p_ref
                .automation
                .iter()
                .map(|l| {
                    let k = !existed.get(i).copied().unwrap_or(false) || l.target.exists(p_ref);
                    i += 1;
                    k
                })
                .collect();
            let mut k = keep.into_iter();
            p.automation.retain(|_| k.next().unwrap_or(true));
            Ok(Edit::RestoreMixer {
                mixer: Box::new(mixer),
                channel_tracks: routes,
                automation,
            })
        }
        Err(e) => {
            p.mixer = mixer;
            restore_channel_tracks(p, &routes);
            p.automation = automation;
            Err(e)
        }
    }
}

pub(crate) fn apply(edit: &Edit, p: &mut Project) -> Result<Edit, EditError> {
    match edit {
        Edit::SetChannelTrack(ch, track) => {
            if !p.mixer.has_track(*track) {
                return Err(EditError::Routing(RoutingError::UnknownTrack(*track)));
            }
            let c = p.channel_mut(*ch).ok_or(EditError::NoSuchChannel(*ch))?;
            Ok(Edit::SetChannelTrack(
                *ch,
                std::mem::replace(&mut c.track, *track),
            ))
        }
        Edit::SetTrackVolume(id, v) => {
            let v = check_range(*v, 0.0, MAX_VOLUME)?;
            let t = track_mut(p, *id)?;
            Ok(Edit::SetTrackVolume(
                *id,
                std::mem::replace(&mut t.volume, v),
            ))
        }
        Edit::SetTrackPan(id, v) => {
            let v = check_range(*v, -1.0, 1.0)?;
            let t = track_mut(p, *id)?;
            Ok(Edit::SetTrackPan(*id, std::mem::replace(&mut t.pan, v)))
        }
        Edit::SetTrackMuted(id, m) => {
            let t = track_mut(p, *id)?;
            Ok(Edit::SetTrackMuted(
                *id,
                std::mem::replace(&mut t.muted, *m),
            ))
        }
        Edit::SetTrackSolo(id, solo) => {
            let t = track_mut(p, *id)?;
            if t.is_master() {
                return Err(EditError::Routing(RoutingError::InvalidValue(
                    "the master track cannot be soloed".into(),
                )));
            }
            Ok(Edit::SetTrackSolo(
                *id,
                std::mem::replace(&mut t.solo, *solo),
            ))
        }
        Edit::RenameTrack(id, name) => {
            let t = track_mut(p, *id)?;
            Ok(Edit::RenameTrack(
                *id,
                std::mem::replace(&mut t.name, name.clone()),
            ))
        }
        Edit::SetSendLevel(id, v) => {
            let v = check_range(*v, 0.0, MAX_SEND_LEVEL)?;
            let s = send_mut(&mut p.mixer, *id)?;
            Ok(Edit::SetSendLevel(*id, std::mem::replace(&mut s.level, v)))
        }
        Edit::SetInsertBypass(id, b) => {
            let i = insert_mut(&mut p.mixer, *id)?;
            Ok(Edit::SetInsertBypass(
                *id,
                std::mem::replace(&mut i.bypass, *b),
            ))
        }
        Edit::SetInsertEffect(id, effect) => {
            effect
                .validate()
                .map_err(|m| EditError::Routing(RoutingError::InvalidValue(m)))?;
            let i = insert_mut(&mut p.mixer, *id)?;
            if i.sidechain.is_some() && !effect.accepts_sidechain() {
                return Err(EditError::Routing(RoutingError::InvalidValue(format!(
                    "{} has no sidechain input",
                    effect.name()
                ))));
            }
            Ok(Edit::SetInsertEffect(
                *id,
                std::mem::replace(&mut i.effect, effect.clone()),
            ))
        }

        Edit::AddTrack { track, index } => structural(p, |p| {
            if p.mixer.has_track(track.id) || track.id == TrackId::MASTER {
                return Err(EditError::Routing(RoutingError::DuplicateId(track.id.0)));
            }
            let at = (*index).min(p.mixer.tracks.len());
            p.mixer.tracks.insert(at, track.clone());
            Ok(())
        }),
        Edit::RemoveTrack(id) => structural(p, |p| {
            if *id == TrackId::MASTER {
                return Err(EditError::Routing(RoutingError::InvalidValue(
                    "the master track cannot be removed".into(),
                )));
            }
            let index = p
                .mixer
                .track_index(*id)
                .ok_or(EditError::Routing(RoutingError::UnknownTrack(*id)))?;
            p.mixer.tracks.remove(index);
            for c in p.channels.iter_mut().filter(|c| c.track == *id) {
                c.track = TrackId::MASTER;
            }
            for t in tracks_mut(&mut p.mixer) {
                if t.main_out == *id {
                    t.main_out = TrackId::MASTER;
                }
                t.sends.retain(|s| s.dest != *id);
                for i in &mut t.inserts {
                    if i.sidechain.is_some_and(|sc| sc.source == *id) {
                        i.sidechain = None;
                    }
                }
            }
            Ok(())
        }),
        Edit::SetTrackMainOut(id, dest) => structural(p, |p| {
            if *id == TrackId::MASTER {
                return Err(EditError::Routing(RoutingError::InvalidValue(
                    "the master track has no main output to route".into(),
                )));
            }
            track_mut(p, *id)?.main_out = *dest;
            Ok(())
        }),
        Edit::AddSend { track, send } => structural(p, |p| {
            track_mut(p, *track)?.sends.push(*send);
            Ok(())
        }),
        Edit::RemoveSend(id) => structural(p, |p| {
            let before: usize = p.mixer.all_tracks().map(|t| t.sends.len()).sum();
            for t in tracks_mut(&mut p.mixer) {
                t.sends.retain(|s| s.id != *id);
            }
            let after: usize = p.mixer.all_tracks().map(|t| t.sends.len()).sum();
            if before == after {
                return Err(EditError::NoSuchMixerObject(id.0));
            }
            Ok(())
        }),
        Edit::SetSendPreFader(id, pre) => structural(p, |p| {
            send_mut(&mut p.mixer, *id)?.pre_fader = *pre;
            Ok(())
        }),
        Edit::AddInsert {
            track,
            insert,
            index,
        } => structural(p, |p| {
            let t = track_mut(p, *track)?;
            let at = (*index).min(t.inserts.len());
            t.inserts.insert(at, insert.clone());
            Ok(())
        }),
        Edit::RemoveInsert(id) => structural(p, |p| {
            let before: usize = p.mixer.all_tracks().map(|t| t.inserts.len()).sum();
            for t in tracks_mut(&mut p.mixer) {
                t.inserts.retain(|i| i.id != *id);
            }
            let after: usize = p.mixer.all_tracks().map(|t| t.inserts.len()).sum();
            if before == after {
                return Err(EditError::NoSuchMixerObject(id.0));
            }
            Ok(())
        }),
        Edit::MoveInsert(id, to) => structural(p, |p| {
            let t = tracks_mut(&mut p.mixer)
                .find(|t| t.inserts.iter().any(|i| i.id == *id))
                .ok_or(EditError::NoSuchMixerObject(id.0))?;
            let from = t
                .inserts
                .iter()
                .position(|i| i.id == *id)
                .expect("found above");
            let ins = t.inserts.remove(from);
            let at = (*to).min(t.inserts.len());
            t.inserts.insert(at, ins);
            Ok(())
        }),
        Edit::SetLimiter(settings) => {
            settings
                .validate()
                .map_err(|m| EditError::Routing(RoutingError::InvalidValue(m)))?;
            Ok(Edit::SetLimiter(std::mem::replace(
                &mut p.mixer.limiter,
                *settings,
            )))
        }
        Edit::SetInsertSidechain(id, sc) => structural(p, |p| {
            insert_mut(&mut p.mixer, *id)?.sidechain = *sc;
            Ok(())
        }),
        Edit::RestoreMixer {
            mixer,
            channel_tracks,
            automation,
        } => structural(p, |p| {
            p.mixer = (**mixer).clone();
            restore_channel_tracks(p, channel_tracks);
            p.automation = automation.clone();
            Ok(())
        }),
        _ => unreachable!("non-mixer edit {edit:?} routed to mixer::apply"),
    }
}

/// Allocates an ID and appends a mixer track routed to the master.
pub fn add_track(
    history: &mut History,
    p: &mut Project,
    name: impl Into<String>,
) -> Result<TrackId, EditError> {
    let id = TrackId(p.ids.next());
    let index = p.mixer.tracks.len();
    history.execute(
        p,
        Edit::AddTrack {
            track: MixerTrack::new(id, name),
            index,
        },
    )?;
    Ok(id)
}

/// Allocates an ID and adds a post-fader send at unity gain.
pub fn add_send(
    history: &mut History,
    p: &mut Project,
    from: TrackId,
    to: TrackId,
) -> Result<SendId, EditError> {
    let id = SendId(p.ids.next());
    history.execute(
        p,
        Edit::AddSend {
            track: from,
            send: Send {
                id,
                dest: to,
                level: 1.0,
                pre_fader: false,
            },
        },
    )?;
    Ok(id)
}

/// Allocates an ID and appends an insert effect.
pub fn add_insert(
    history: &mut History,
    p: &mut Project,
    track: TrackId,
    effect: Effect,
) -> Result<InsertId, EditError> {
    let id = InsertId(p.ids.next());
    let index = p.mixer.track(track).map_or(0, |t| t.inserts.len());
    history.execute(
        p,
        Edit::AddInsert {
            track,
            insert: Insert {
                id,
                bypass: false,
                effect,
                sidechain: None,
            },
            index,
        },
    )?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_sampler_channel;
    use jdance_domain::{EffectKind, Sidechain, Tap};

    fn comp() -> Effect {
        Effect::new(EffectKind::Compressor)
    }

    fn lane(target: jdance_domain::AutomationTarget) -> jdance_domain::AutomationLane {
        use jdance_domain::{AutoPoint, Curve};
        jdance_domain::AutomationLane {
            id: jdance_domain::AutomationId(0),
            target,
            points: vec![
                AutoPoint {
                    tick: 3840,
                    value: 0.2,
                    curve: Curve::Linear,
                    tension: 0.0,
                },
                AutoPoint {
                    tick: 0,
                    value: 1.0,
                    curve: Curve::Hold,
                    tension: 0.0,
                },
            ],
        }
    }

    #[test]
    fn automation_lanes_follow_their_objects() {
        use jdance_domain::{AutomationId, AutomationTarget};
        let mut p = Project::new();
        let mut h = History::default();
        let a = add_track(&mut h, &mut p, "A").unwrap();
        let id = AutomationId(p.ids.next());
        h.execute(
            &mut p,
            Edit::SetAutomation {
                id,
                lane: Some(lane(AutomationTarget::TrackVolume(a))),
            },
        )
        .unwrap();
        assert_eq!(p.automation[0].points[0].tick, 0, "sorted");
        assert_eq!(p.automation[0].id, id);
        // One lane per target.
        let other = AutomationId(p.ids.next());
        assert!(
            h.execute(
                &mut p,
                Edit::SetAutomation {
                    id: other,
                    lane: Some(lane(AutomationTarget::TrackVolume(a))),
                },
            )
            .is_err()
        );
        // A lane whose plugin is missing (no such insert) is kept.
        let orphan = AutomationId(p.ids.next());
        p.automation.push(jdance_domain::AutomationLane {
            id: orphan,
            ..lane(AutomationTarget::InsertBypass(InsertId(9999)))
        });
        h.execute(&mut p, Edit::RemoveTrack(a)).unwrap();
        assert_eq!(p.automation.len(), 1, "the track's lane went with it");
        assert_eq!(p.automation[0].id, orphan);
        h.undo(&mut p).unwrap();
        assert_eq!(p.automation.len(), 2, "undo restores both");
        assert!(p.automation_lane(id).is_some());
        // Remove the lane itself, then undo.
        h.execute(&mut p, Edit::SetAutomation { id, lane: None })
            .unwrap();
        assert!(p.automation_lane(id).is_none());
        h.undo(&mut p).unwrap();
        assert!(p.automation_lane(id).is_some());
    }

    #[test]
    fn cycle_is_rejected_and_project_unchanged() {
        let mut p = Project::new();
        let mut h = History::default();
        let a = add_track(&mut h, &mut p, "A").unwrap();
        let b = add_track(&mut h, &mut p, "B").unwrap();
        h.execute(&mut p, Edit::SetTrackMainOut(a, b)).unwrap();
        let before = p.clone();
        let err = add_send(&mut h, &mut p, b, a).unwrap_err();
        assert!(
            matches!(err, EditError::Routing(RoutingError::Cycle(_))),
            "{err:?}"
        );
        assert_eq!(p.mixer, before.mixer);
        // The sidechain variant of the same loop.
        let ins = add_insert(&mut h, &mut p, a, comp()).unwrap();
        let sc = Some(Sidechain {
            source: b,
            tap: Tap::PreFader,
        });
        assert!(
            h.execute(&mut p, Edit::SetInsertSidechain(ins, sc))
                .is_err()
        );
        assert_eq!(p.mixer.track(a).unwrap().inserts[0].sidechain, None);
    }

    #[test]
    fn remove_track_rewires_and_undo_restores_everything() {
        let mut p = Project::new();
        let mut h = History::default();
        let ch = add_sampler_channel(&mut h, &mut p, "Kick").unwrap();
        let drums = add_track(&mut h, &mut p, "Drums").unwrap();
        let bass = add_track(&mut h, &mut p, "Bass").unwrap();
        let verb = add_track(&mut h, &mut p, "Reverb").unwrap();
        h.execute(&mut p, Edit::SetChannelTrack(ch, drums)).unwrap();
        add_send(&mut h, &mut p, bass, drums).unwrap();
        h.execute(&mut p, Edit::SetTrackMainOut(verb, drums))
            .unwrap();
        let ins = add_insert(&mut h, &mut p, bass, comp()).unwrap();
        let sc = Some(Sidechain {
            source: drums,
            tap: Tap::PreFader,
        });
        // Drums is downstream of Bass via the send, so this is a cycle.
        assert!(
            h.execute(&mut p, Edit::SetInsertSidechain(ins, sc))
                .is_err()
        );

        let before = p.clone();
        h.execute(&mut p, Edit::RemoveTrack(drums)).unwrap();
        assert_eq!(p.channel(ch).unwrap().track, TrackId::MASTER);
        assert_eq!(p.mixer.track(verb).unwrap().main_out, TrackId::MASTER);
        assert!(p.mixer.track(bass).unwrap().sends.is_empty());
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap();
        assert!(!p.mixer.has_track(drums));
    }

    #[test]
    fn sidechain_from_parallel_track_is_allowed() {
        let mut p = Project::new();
        let mut h = History::default();
        let kick = add_track(&mut h, &mut p, "Kick").unwrap();
        let bass = add_track(&mut h, &mut p, "Bass").unwrap();
        let ins = add_insert(&mut h, &mut p, bass, comp()).unwrap();
        let sc = Some(Sidechain {
            source: kick,
            tap: Tap::PreFader,
        });
        h.execute(&mut p, Edit::SetInsertSidechain(ins, sc))
            .unwrap();
        assert_eq!(p.mixer.track(bass).unwrap().inserts[0].sidechain, sc);
        h.undo(&mut p).unwrap();
        assert_eq!(p.mixer.track(bass).unwrap().inserts[0].sidechain, None);
    }

    #[test]
    fn reorder_inserts_and_undo() {
        let mut p = Project::new();
        let mut h = History::default();
        let t = add_track(&mut h, &mut p, "T").unwrap();
        let a = add_insert(&mut h, &mut p, t, Effect::new(EffectKind::ParametricEq)).unwrap();
        let b = add_insert(&mut h, &mut p, t, Effect::new(EffectKind::Reverb)).unwrap();
        h.execute(&mut p, Edit::MoveInsert(b, 0)).unwrap();
        let order: Vec<_> = p
            .mixer
            .track(t)
            .unwrap()
            .inserts
            .iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(order, [b, a]);
        h.undo(&mut p).unwrap();
        let order: Vec<_> = p
            .mixer
            .track(t)
            .unwrap()
            .inserts
            .iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(order, [a, b]);
        for _ in 0..6 {
            add_insert(&mut h, &mut p, t, comp()).unwrap();
        }
        assert!(
            add_insert(&mut h, &mut p, t, comp()).is_err(),
            "limit of 8 inserts"
        );
    }

    #[test]
    fn parameter_edits_validate_and_merge() {
        let mut p = Project::new();
        let mut h = History::default();
        let t = add_track(&mut h, &mut p, "T").unwrap();
        let s = add_send(&mut h, &mut p, t, TrackId::MASTER).unwrap();
        h.mark_saved();
        for v in [0.5, 0.7, 1.9] {
            h.execute_merging(&mut p, Edit::SetSendLevel(s, v)).unwrap();
        }
        h.end_gesture();
        assert!(h.execute(&mut p, Edit::SetSendLevel(s, 3.0)).is_err());
        h.undo(&mut p).unwrap();
        assert!(!h.is_dirty());
        let ins = add_insert(&mut h, &mut p, t, comp()).unwrap();
        assert!(
            h.execute(
                &mut p,
                Edit::SetInsertEffect(ins, comp().with("ratio", 0.5))
            )
            .is_err()
        );
        assert!(
            h.execute(&mut p, Edit::RemoveTrack(TrackId::MASTER))
                .is_err()
        );
        assert!(h.execute(&mut p, Edit::RemoveSend(SendId(999))).is_err());
    }
}
