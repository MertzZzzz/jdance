// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio channels (spec 0.2.2 ACH-015…035): every Playlist audio clip is an
//! instance of an audio channel in the Channel Rack, which carries volume,
//! pan, mute and the mixer track for all of them. Whatever puts a sample on
//! the Playlist gets its channel here, so no path can leave a clip without
//! one (ACH-016). The helpers only build edits; the caller runs them as one
//! [`Edit::Batch`] (one undo step).

use crate::{Edit, EditError};
use jdance_domain::{AssetId, AudioAsset, AudioClip, Channel, ChannelId, ClipId, LaneId, Project};

/// Channels chosen for new clips during one user action. A file dropped
/// twice in one action gets one channel, as it would in two actions.
#[derive(Debug, Default)]
pub struct ChannelPlan {
    created: Vec<(AssetId, ChannelId)>,
}

impl ChannelPlan {
    pub fn new() -> Self {
        Self::default()
    }

    /// ACH-020: the channel for new clips of `asset` — its first audio
    /// channel in Channel Rack order, or one this plan already created, or
    /// a new channel named after the file (routed to the master). The edit
    /// adding a new channel is pushed to `edits`.
    pub fn channel_for(
        &mut self,
        p: &mut Project,
        asset: &AudioAsset,
        edits: &mut Vec<Edit>,
    ) -> ChannelId {
        if let Some(c) = p.audio_channels_of(asset.id).next() {
            return c.id;
        }
        if let Some((_, c)) = self.created.iter().find(|(a, _)| *a == asset.id) {
            return *c;
        }
        let index = p.channels.len() + self.created.len();
        let (id, edit) = new_audio_channel(p, asset.name.clone(), asset.id, index);
        edits.push(edit);
        self.created.push((asset.id, id));
        id
    }

    /// Channels created by this plan (ACH-018: shown to the user).
    pub fn created(&self) -> impl Iterator<Item = ChannelId> + '_ {
        self.created.iter().map(|(_, c)| *c)
    }
}

/// A new audio channel of `asset` at Channel Rack position `index`, as an
/// edit (not executed). Unity volume, centered, routed to the master.
pub fn new_audio_channel(
    p: &mut Project,
    name: String,
    asset: AssetId,
    index: usize,
) -> (ChannelId, Edit) {
    let id = ChannelId(p.ids.next());
    let edit = Edit::AddChannel {
        channel: Channel::new_audio(id, name, asset),
        index,
        rows: Vec::new(),
        automation: Vec::new(),
    };
    (id, edit)
}

/// A clip of `channel` playing all of `asset` from `start` (project
/// frames) on `lane`.
pub fn audio_clip(
    id: ClipId,
    lane: LaneId,
    channel: ChannelId,
    asset: &AudioAsset,
    start: u64,
) -> AudioClip {
    AudioClip {
        id,
        lane,
        channel,
        start,
        source_in: 0,
        source_out: asset.frames,
        gain: 1.0,
        muted: false,
        fade_in: 0,
        fade_out: 0,
    }
}

/// `base`, or `base 2`, `base 3`, … — the first name no channel has.
pub fn unique_channel_name(p: &Project, base: &str) -> String {
    let taken = |n: &str| p.channels.iter().any(|c| c.name == n);
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|k| format!("{base} {k}"))
        .find(|n| !taken(n))
        .expect("an unused name exists")
}

/// ACH-024/025 «Make unique» for audio clips of one channel: a new channel
/// with the same file and the first clip's channel settings, named
/// `<name> 2`, right after it in the Channel Rack; the clips move into it.
/// The file is not copied. Returns the new channel and the edit.
pub fn make_audio_unique(
    p: &mut Project,
    clips: &[ClipId],
) -> Result<(ChannelId, Edit), EditError> {
    let err = |m: String| EditError::Arrangement(m);
    let first = clips
        .first()
        .and_then(|id| p.playlist.audio_clip(*id))
        .ok_or_else(|| err("no audio clip to make unique".into()))?
        .channel;
    let source = p
        .channel(first)
        .filter(|c| c.is_audio())
        .cloned()
        .ok_or_else(|| err("the clip has no audio channel".into()))?;
    let mut moved = Vec::with_capacity(clips.len());
    for id in clips {
        let c = *p
            .playlist
            .audio_clip(*id)
            .ok_or_else(|| err(format!("clip {} does not exist", id.0)))?;
        if c.channel != first {
            return Err(err("the clips belong to different channels".into()));
        }
        moved.push(c);
    }
    let id = ChannelId(p.ids.next());
    let index = p.channel_index(first).map_or(p.channels.len(), |i| i + 1);
    let channel = Channel {
        id,
        name: unique_channel_name(p, &source.name),
        color: None,
        ..source
    };
    let mut edits = vec![Edit::AddChannel {
        channel,
        index,
        rows: Vec::new(),
        automation: Vec::new(),
    }];
    edits.extend(
        moved
            .into_iter()
            .map(|c| Edit::SetAudioClip(AudioClip { channel: id, ..c })),
    );
    Ok((id, Edit::Batch(edits)))
}

/// ACH-033: removing a channel takes its Playlist clips with it (one
/// batch; undo brings both back).
pub fn remove_channel(p: &Project, id: ChannelId) -> Edit {
    let mut edits: Vec<Edit> = p
        .channel_clips(id)
        .map(|c| Edit::RemoveAudioClip(c.id))
        .collect();
    edits.push(Edit::RemoveChannel(id));
    Edit::Batch(edits)
}

/// ACH-034: audio channels without clips, removed as one batch; `None`
/// when there are none. The files stay in the project.
pub fn remove_unused_audio_channels(p: &Project) -> Option<(usize, Edit)> {
    let unused: Vec<Edit> = p
        .channels
        .iter()
        .filter(|c| c.is_audio() && p.channel_clips(c.id).next().is_none())
        .map(|c| Edit::RemoveChannel(c.id))
        .collect();
    let n = unused.len();
    (n > 0).then_some((n, Edit::Batch(unused)))
}

/// REC-002: `<input> · <n>`, `n` one more than the highest take number of
/// that input in the project.
pub fn take_channel_name(p: &Project, input: &str) -> String {
    let prefix = format!("{input} · ");
    let last = p
        .channels
        .iter()
        .filter_map(|c| c.name.strip_prefix(&prefix)?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", last + 1)
}

/// REC-001/003: the channel of a recorded take — always a new audio
/// channel, at the end of the Channel Rack, routed as [`Project::record_to`]
/// says. With «New track» the track is created first (named after the
/// input) and the setting then points at it, so the next takes go there
/// too. Edits are appended to `edits`; the asset must be added before.
pub fn take_channel(
    p: &mut Project,
    asset: AssetId,
    input: &str,
    edits: &mut Vec<Edit>,
) -> ChannelId {
    use jdance_domain::{MixerTrack, RecordTo, TrackId};
    let track = match p.record_to {
        RecordTo::Master => TrackId::MASTER,
        RecordTo::Track { track } => track,
        RecordTo::NewTrack => {
            let t = TrackId(p.ids.next());
            edits.push(Edit::AddTrack {
                track: MixerTrack::new(t, input),
                index: p.mixer.tracks.len(),
            });
            edits.push(Edit::SetRecordTo(RecordTo::Track { track: t }));
            t
        }
    };
    let id = ChannelId(p.ids.next());
    let mut channel = Channel::new_audio(id, take_channel_name(p, input), asset);
    channel.track = track;
    edits.push(Edit::AddChannel {
        channel,
        index: p.channels.len(),
        rows: Vec::new(),
        automation: Vec::new(),
    });
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;
    use jdance_domain::TrackId;

    fn asset(p: &mut Project, name: &str) -> AudioAsset {
        let a = AudioAsset {
            id: AssetId(p.ids.next()),
            name: name.into(),
            path: format!("/x/{name}.wav"),
            hash: 1,
            size: 10,
            sample_rate: 48_000,
            channels: 2,
            frames: 4_800,
        };
        p.assets.push(a.clone());
        a
    }

    /// The invariant (ACH-015) after every step: each clip has an audio
    /// channel, and every channel is in the rack.
    fn assert_invariant(p: &Project) {
        p.validate_arrangement().unwrap();
        for c in &p.playlist.audio_clips {
            assert!(p.channel(c.channel).is_some_and(Channel::is_audio));
        }
    }

    fn place(p: &mut Project, h: &mut History, a: &AudioAsset, plan: &mut ChannelPlan) -> ClipId {
        let mut edits = Vec::new();
        let ch = plan.channel_for(p, a, &mut edits);
        let lane = p.playlist.lanes[0].id;
        let id = ClipId(p.ids.next());
        edits.push(Edit::AddAudioClip(audio_clip(id, lane, ch, a, 0)));
        h.execute(p, Edit::Batch(edits)).unwrap();
        id
    }

    #[test]
    fn the_same_file_reuses_its_channel_and_another_gets_one() {
        let mut p = Project::new();
        let mut h = History::default();
        let a = asset(&mut p, "kick");
        let b = asset(&mut p, "snare");
        let c1 = place(&mut p, &mut h, &a, &mut ChannelPlan::new());
        let c2 = place(&mut p, &mut h, &a, &mut ChannelPlan::new());
        place(&mut p, &mut h, &b, &mut ChannelPlan::new());
        assert_invariant(&p);
        assert_eq!(p.channels.len(), 2);
        let ch = p.playlist.audio_clip(c1).unwrap().channel;
        assert_eq!(p.playlist.audio_clip(c2).unwrap().channel, ch);
        let kick = p.channel(ch).unwrap();
        assert_eq!((kick.name.as_str(), kick.track), ("kick", TrackId::MASTER));
        assert_eq!(kick.volume, 1.0);
        // Undo of the first drop removes its channel too.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert!(p.channels.is_empty() && p.playlist.audio_clips.is_empty());
    }

    #[test]
    fn one_action_with_the_same_file_twice_makes_one_channel() {
        let mut p = Project::new();
        let a = asset(&mut p, "loop");
        let mut plan = ChannelPlan::new();
        let mut edits = Vec::new();
        let x = plan.channel_for(&mut p, &a, &mut edits);
        let y = plan.channel_for(&mut p, &a, &mut edits);
        assert_eq!(x, y);
        assert_eq!(edits.len(), 1);
        assert_eq!(plan.created().collect::<Vec<_>>(), [x]);
    }

    #[test]
    fn clips_need_an_audio_channel() {
        let mut p = Project::new();
        let mut h = History::default();
        let a = asset(&mut p, "a");
        let lane = p.playlist.lanes[0].id;
        let orphan = audio_clip(ClipId(p.ids.next()), lane, ChannelId(999), &a, 0);
        assert!(h.execute(&mut p, Edit::AddAudioClip(orphan)).is_err());
        // A sampler is not an audio channel.
        let s = ChannelId(p.ids.next());
        p.channels.push(Channel::new_sampler(s, "s"));
        let wrong = audio_clip(ClipId(p.ids.next()), lane, s, &a, 0);
        assert!(h.execute(&mut p, Edit::AddAudioClip(wrong)).is_err());
        assert!(p.playlist.audio_clips.is_empty());
    }

    #[test]
    fn a_channel_with_clips_goes_only_with_them() {
        let mut p = Project::new();
        let mut h = History::default();
        let a = asset(&mut p, "a");
        let clip = place(&mut p, &mut h, &a, &mut ChannelPlan::new());
        place(&mut p, &mut h, &a, &mut ChannelPlan::new());
        let ch = p.playlist.audio_clip(clip).unwrap().channel;
        assert!(h.execute(&mut p, Edit::RemoveChannel(ch)).is_err());
        assert!(h.execute(&mut p, Edit::RemoveAsset(a.id)).is_err());
        let edit = remove_channel(&p, ch);
        h.execute(&mut p, edit).unwrap();
        assert!(p.channels.is_empty() && p.playlist.audio_clips.is_empty());
        h.undo(&mut p).unwrap();
        assert_eq!(p.playlist.audio_clips.len(), 2);
        assert_invariant(&p);
    }

    #[test]
    fn make_unique_moves_clips_to_a_copy_of_the_channel() {
        let mut p = Project::new();
        let mut h = History::default();
        let a = asset(&mut p, "vox");
        let c1 = place(&mut p, &mut h, &a, &mut ChannelPlan::new());
        let c2 = place(&mut p, &mut h, &a, &mut ChannelPlan::new());
        let old = p.playlist.audio_clip(c1).unwrap().channel;
        let t = jdance_domain::TrackId(p.ids.next());
        h.execute(
            &mut p,
            Edit::AddTrack {
                track: jdance_domain::MixerTrack::new(t, "T"),
                index: 0,
            },
        )
        .unwrap();
        h.execute(&mut p, Edit::SetChannelTrack(old, t)).unwrap();
        let (new, edit) = make_audio_unique(&mut p, &[c2]).unwrap();
        h.execute(&mut p, edit).unwrap();
        assert_invariant(&p);
        let n = p.channel(new).unwrap();
        assert_eq!((n.name.as_str(), n.track), ("vox 2", t));
        assert_eq!(n.audio_asset(), Some(a.id));
        assert_eq!(p.playlist.audio_clip(c1).unwrap().channel, old);
        assert_eq!(p.playlist.audio_clip(c2).unwrap().channel, new);
        assert_eq!(p.channel_index(new), p.channel_index(old).map(|i| i + 1));
        h.undo(&mut p).unwrap();
        assert!(p.channel(new).is_none());
        assert_eq!(p.playlist.audio_clip(c2).unwrap().channel, old);
    }

    #[test]
    fn unused_audio_channels_are_removed_together() {
        let mut p = Project::new();
        let mut h = History::default();
        let a = asset(&mut p, "a");
        let clip = place(&mut p, &mut h, &a, &mut ChannelPlan::new());
        let (_, edit) = make_audio_unique(&mut p, &[clip]).unwrap();
        h.execute(&mut p, edit).unwrap();
        let (n, edit) = remove_unused_audio_channels(&p).unwrap();
        assert_eq!(n, 1);
        h.execute(&mut p, edit).unwrap();
        assert_eq!(p.channels.len(), 1);
        assert!(remove_unused_audio_channels(&p).is_none());
        assert!(p.asset(a.id).is_some(), "files stay");
    }

    #[test]
    fn takes_get_new_channels_routed_by_the_recording_setting() {
        use jdance_domain::RecordTo;
        let mut p = Project::new();
        let mut h = History::default();
        let take = |p: &mut Project, h: &mut History| {
            let a = AudioAsset {
                id: AssetId(p.ids.next()),
                name: "take".into(),
                path: "t.wav".into(),
                hash: 7,
                size: 1,
                sample_rate: 48_000,
                channels: 1,
                frames: 100,
            };
            let mut edits = vec![Edit::AddAsset(a.clone())];
            let ch = take_channel(p, a.id, "Input 1", &mut edits);
            let lane = p.playlist.lanes[0].id;
            let clip = ClipId(p.ids.next());
            edits.push(Edit::AddAudioClip(audio_clip(clip, lane, ch, &a, 0)));
            h.execute(p, Edit::Batch(edits)).unwrap();
            ch
        };
        let first = take(&mut p, &mut h);
        let c = p.channel(first).unwrap();
        assert_eq!((c.name.as_str(), c.track), ("Input 1 · 1", TrackId::MASTER));
        h.execute(&mut p, Edit::SetRecordTo(RecordTo::NewTrack))
            .unwrap();
        let second = take(&mut p, &mut h);
        let third = take(&mut p, &mut h);
        let t2 = p.channel(second).unwrap().track;
        assert_ne!(t2, TrackId::MASTER);
        assert_eq!(p.mixer.track(t2).unwrap().name, "Input 1");
        assert_eq!(p.channel(third).unwrap().track, t2, "same new track");
        assert_eq!(p.mixer.tracks.len(), 1);
        assert_eq!(p.channel(third).unwrap().name, "Input 1 · 3");
        assert_invariant(&p);
        // Removing that track sends the setting back to the master.
        h.execute(&mut p, Edit::RemoveTrack(t2)).unwrap();
        assert_eq!(p.record_to, RecordTo::Master);
        h.undo(&mut p).unwrap();
        assert_eq!(p.record_to, RecordTo::Track { track: t2 });
        // Undo of a take removes its channel, track and setting change.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert!(p.mixer.tracks.is_empty());
        assert_eq!(p.record_to, RecordTo::NewTrack);
    }

    #[test]
    fn audio_channels_take_no_steps() {
        let mut p = Project::new();
        let a = asset(&mut p, "a");
        let mut h = History::default();
        let pat = p.patterns[0].id;
        let (id, _) = new_audio_channel(&mut p, "a".into(), a.id, 0);
        let mut row = jdance_domain::StepRow::new(id, 16);
        row.steps[0].on = true;
        let bad = Edit::AddChannel {
            channel: Channel::new_audio(id, "a", a.id),
            index: 0,
            rows: vec![(pat, row)],
            automation: Vec::new(),
        };
        assert!(h.execute(&mut p, bad).is_err());
    }
}
