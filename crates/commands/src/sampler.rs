// SPDX-License-Identifier: GPL-3.0-or-later
//! Replacing the sample of a sampler channel (spec 0.2.5 §3).

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
    edits.push(Edit::SetChannelSample(ch, Some(sample)));
    Ok(Some(Edit::Batch(edits)))
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
}
