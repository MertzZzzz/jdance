// SPDX-License-Identifier: GPL-3.0-or-later
//! Replacing the sample of a sampler channel (spec 0.2.5 §3) and
//! putting an audio channel's file into a new sampler.

use crate::Edit;
use jdance_domain::{ChannelId, Project, SampleRef, SamplerSettings};

/// File name without folders and extension.
fn stem(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// Why a sample cannot replace the channel's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceRefusal {
    NoChannel,
    /// An instrument or an audio channel: it does not take samples.
    NotASampler,
    /// The sampler's file plays its Playlist clips (spec 0.2.9 §1.8).
    HasClips,
}

/// One edit that loads `sample` into sampler channel `ch`: the markers
/// that depend on the sample's length (start, end, loop) go back to the
/// whole sample; volume, pan, pitch, routing, envelope and colour stay.
/// A channel named after its old file takes the new file's name; a name
/// the user gave stays. `None` when it is the same sample.
pub fn replace_sample(
    p: &Project,
    ch: ChannelId,
    sample: SampleRef,
) -> Result<Option<Edit>, ReplaceRefusal> {
    let c = p.channel(ch).ok_or(ReplaceRefusal::NoChannel)?;
    if c.instrument().is_some() || c.is_audio() {
        return Err(ReplaceRefusal::NotASampler);
    }
    let old = c.sample().cloned();
    if old.as_ref() == Some(&sample) {
        return Ok(None);
    }
    if p.channel_clips(ch).next().is_some() {
        return Err(ReplaceRefusal::HasClips);
    }
    let auto_named = match &old {
        Some(o) => c.name == stem(&o.path),
        None => true,
    };
    let mut edits = Vec::new();
    if auto_named {
        let name = stem(&sample.path).to_owned();
        if !name.is_empty() && name != c.name {
            edits.push(Edit::RenameChannel(ch, name));
        }
    }
    let defaults = SamplerSettings::default();
    let markers = SamplerSettings {
        start: defaults.start,
        end: defaults.end,
        loop_start: defaults.loop_start,
        loop_end: defaults.loop_end,
        ..c.sampler
    };
    if markers != c.sampler {
        edits.push(Edit::SetSampler(ch, markers));
    }
    // Another file: the sampler no longer has a clip file.
    if c.clip_asset.is_some() {
        edits.push(Edit::SetChannelClipAsset(ch, None));
    }
    edits.push(Edit::SetChannelSample(ch, Some(sample)));
    Ok(Some(Edit::Batch(edits)))
}

/// A new sampler channel with the file of audio channel `ch`, right after
/// it: same name, colour, volume, pan and mixer track. One undo step; the
/// audio channel and its clips stay.
pub fn sampler_from_audio(
    history: &mut crate::History,
    p: &mut Project,
    ch: ChannelId,
) -> Result<ChannelId, crate::EditError> {
    let index = p
        .channel_index(ch)
        .ok_or(crate::EditError::NoSuchChannel(ch))?;
    let c = p.channels[index].clone();
    let path = c
        .audio_asset()
        .and_then(|a| p.asset(a))
        .map(|a| a.path.clone())
        .ok_or(crate::EditError::NoSuchChannel(ch))?;
    let id = ChannelId(p.ids.next());
    let mut channel = jdance_domain::Channel::new_sampler(id, c.name);
    channel.source = jdance_domain::ChannelSource::Sampler {
        sample: Some(SampleRef { path }),
    };
    channel.color = c.color;
    channel.volume = c.volume;
    channel.pan = c.pan;
    channel.track = c.track;
    history.execute(
        p,
        Edit::AddChannel {
            channel,
            index: index + 1,
            rows: Vec::new(),
            automation: Vec::new(),
        },
    )?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;
    use jdance_domain::{Channel, ChannelSource, PlaybackMode};

    fn project() -> (Project, ChannelId) {
        let mut p = Project::new();
        let ch = ChannelId(p.ids.next());
        let mut c = Channel::new_sampler(ch, "kick_01");
        c.source = ChannelSource::Sampler {
            sample: Some(SampleRef {
                path: "samples/kick_01.wav".into(),
            }),
        };
        c.volume = 0.5;
        c.pitch = 3;
        c.sampler.start = 0.2;
        c.sampler.mode = PlaybackMode::Loop;
        c.sampler.loop_start = 0.3;
        c.sampler.root_key = 48;
        p.channels.push(c);
        (p, ch)
    }

    #[test]
    fn the_sample_changes_markers_reset_settings_stay_one_undo() {
        let (mut p, ch) = project();
        let mut h = History::default();
        let new = SampleRef {
            path: "/lib/kick_07.mp3".into(),
        };
        let e = replace_sample(&p, ch, new.clone()).unwrap().unwrap();
        h.execute(&mut p, e).unwrap();
        let c = p.channel(ch).unwrap();
        assert_eq!(c.sample(), Some(&new));
        assert_eq!(c.name, "kick_07", "auto-named: follows the file");
        assert_eq!((c.volume, c.pitch, c.sampler.root_key), (0.5, 3, 48));
        assert_eq!((c.sampler.start, c.sampler.loop_start), (0.0, 0.0));
        assert_eq!(c.sampler.mode, PlaybackMode::Loop);
        h.undo(&mut p).unwrap();
        let c = p.channel(ch).unwrap();
        assert_eq!(c.name, "kick_01");
        assert_eq!(c.sampler.start, 0.2);
        // Same sample: nothing.
        let old = c.sample().unwrap().clone();
        assert_eq!(replace_sample(&p, ch, old), Ok(None));
    }

    #[test]
    fn a_given_name_stays_and_instruments_refuse() {
        let (mut p, ch) = project();
        p.channels[0].name = "Main kick".into();
        let Some(Edit::Batch(e)) = replace_sample(
            &p,
            ch,
            SampleRef {
                path: "k2.wav".into(),
            },
        )
        .unwrap() else {
            panic!()
        };
        assert!(!e.iter().any(|e| matches!(e, Edit::RenameChannel(..))));
        assert_eq!(
            replace_sample(&p, ChannelId(999), SampleRef { path: "x".into() }),
            Err(ReplaceRefusal::NoChannel)
        );
    }

    #[test]
    fn an_audio_channel_goes_into_a_new_sampler_in_one_step() {
        let mut p = Project::new();
        let mut h = History::default();
        let track = crate::add_track(&mut h, &mut p, "Vox").unwrap();
        let asset = jdance_domain::AudioAsset {
            id: jdance_domain::AssetId(p.ids.next()),
            name: "vox".into(),
            path: "assets/vox.wav".into(),
            hash: 1,
            size: 10,
            sample_rate: 48_000,
            channels: 2,
            frames: 48_000,
        };
        let a = ChannelId(p.ids.next());
        let mut audio = Channel::new_audio(a, "Vox take", asset.id);
        audio.track = track;
        audio.color = Some([1, 2, 3]);
        audio.volume = 0.4;
        p.assets.push(asset);
        p.channels.push(audio);
        let n = p.channels.len();
        let s = sampler_from_audio(&mut h, &mut p, a).unwrap();
        assert_eq!(p.channels.len(), n + 1);
        assert_eq!(p.channel_index(s), Some(p.channel_index(a).unwrap() + 1));
        let c = p.channel(s).unwrap();
        assert_eq!(c.sample().map(|r| r.path.as_str()), Some("assets/vox.wav"));
        assert_eq!(
            (c.name.as_str(), c.color, c.volume, c.track),
            ("Vox take", Some([1, 2, 3]), 0.4, track)
        );
        assert!(p.channel(a).is_some(), "the audio channel stays");
        h.undo(&mut p).unwrap();
        assert_eq!(p.channels.len(), n);
        // A sampler is not an audio channel.
        assert!(sampler_from_audio(&mut h, &mut p, ChannelId(999)).is_err());
    }
}
