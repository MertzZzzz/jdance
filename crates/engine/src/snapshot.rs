// SPDX-License-Identifier: GPL-3.0-or-later
//! Compilation of the document into an immutable realtime snapshot.

use jdance_domain::{ChannelId, Project, SampleRef};
use jdance_dsp::{SampleBuffer, pan_gains};
use std::sync::Arc;

/// Everything the processor needs to play the project, precomputed in
/// samples at one sample rate. Built and dropped off the audio thread.
#[derive(Debug, Clone)]
pub struct PlaybackSnapshot {
    pub sample_rate: u32,
    /// Pattern loop length in samples; `0` means nothing to play.
    pub loop_len: u64,
    /// Step triggers sorted by `pos`, all `< loop_len`.
    pub events: Vec<StepEvent>,
    pub channels: Vec<ChannelRt>,
    pub master_gain: (f32, f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepEvent {
    /// Sample position inside the loop.
    pub pos: u64,
    /// Index into [`PlaybackSnapshot::channels`].
    pub channel: usize,
    pub velocity: f32,
}

#[derive(Debug, Clone)]
pub struct ChannelRt {
    pub id: ChannelId,
    /// `None` when the channel has no sample or it failed to load.
    pub sample: Option<Arc<SampleBuffer>>,
    /// Volume × pan × mute, excluding velocity.
    pub gain: (f32, f32),
}

impl PlaybackSnapshot {
    pub fn empty(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            loop_len: 0,
            events: Vec::new(),
            channels: Vec::new(),
            master_gain: (1.0, 1.0),
        }
    }
}

/// Sample position of a tick at a constant tempo. Uses absolute positions
/// so results never depend on block boundaries (RT-022).
pub fn tick_to_sample(tick: u64, tempo_bpm: f64, sample_rate: u32) -> u64 {
    let samples_per_tick =
        f64::from(sample_rate) * 60.0 / (tempo_bpm * f64::from(jdance_domain::PPQ));
    (tick as f64 * samples_per_tick).round() as u64
}

/// Compiles `project` for `sample_rate`. `samples` resolves sample
/// references to buffers already converted to `sample_rate`.
pub fn compile(
    project: &Project,
    sample_rate: u32,
    samples: impl Fn(&SampleRef) -> Option<Arc<SampleBuffer>>,
) -> PlaybackSnapshot {
    let channels: Vec<ChannelRt> = project
        .channels
        .iter()
        .map(|c| {
            let (l, r) = pan_gains(c.pan);
            let g = if c.muted { 0.0 } else { c.volume };
            ChannelRt {
                id: c.id,
                sample: c
                    .sample()
                    .and_then(&samples)
                    .filter(|s| s.sample_rate == sample_rate),
                gain: (l * g, r * g),
            }
        })
        .collect();

    let master = &project.mixer.master;
    let (ml, mr) = pan_gains(master.pan);
    let mg = if master.muted { 0.0 } else { master.volume };

    let mut snapshot = PlaybackSnapshot {
        sample_rate,
        loop_len: 0,
        events: Vec::new(),
        channels,
        master_gain: (ml * mg, mr * mg),
    };

    let Some(pattern) = project.active_pattern() else {
        return snapshot;
    };
    snapshot.loop_len = tick_to_sample(pattern.length_ticks(), project.tempo_bpm, sample_rate);
    for row in &pattern.rows {
        let Some(ch_index) = project.channel_index(row.channel) else {
            continue;
        };
        if project.channels[ch_index].muted {
            continue;
        }
        for (i, step) in row
            .steps
            .iter()
            .enumerate()
            .take(pattern.length_steps as usize)
        {
            if !step.on {
                continue;
            }
            let tick = i as u64 * u64::from(jdance_domain::TICKS_PER_STEP);
            let pos = tick_to_sample(tick, project.tempo_bpm, sample_rate);
            if pos < snapshot.loop_len {
                snapshot.events.push(StepEvent {
                    pos,
                    channel: ch_index,
                    velocity: step.velocity,
                });
            }
        }
    }
    snapshot.events.sort_by_key(|e| (e.pos, e.channel));
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{Channel, ChannelSource};

    #[test]
    fn step_positions_at_120_bpm() {
        // 120 BPM at 48 kHz: a sixteenth is 6000 samples.
        assert_eq!(tick_to_sample(240, 120.0, 48_000), 6000);
        assert_eq!(tick_to_sample(16 * 240, 120.0, 48_000), 96_000);
    }

    #[test]
    fn compile_orders_events_and_skips_muted() {
        let mut p = Project::new();
        p.tempo_bpm = 120.0;
        let a = ChannelId(p.ids.next());
        let b = ChannelId(p.ids.next());
        let mut cb = Channel::new_sampler(b, "B");
        cb.muted = true;
        p.channels.push(Channel::new_sampler(a, "A"));
        p.channels.push(cb);
        let pat = &mut p.patterns[0];
        pat.row_mut(a).steps[4].on = true;
        pat.row_mut(a).steps[0].on = true;
        pat.row_mut(b).steps[0].on = true;
        let s = compile(&p, 48_000, |_| None);
        assert_eq!(s.loop_len, 96_000);
        let pos: Vec<u64> = s.events.iter().map(|e| e.pos).collect();
        assert_eq!(pos, [0, 24_000]);
        assert_eq!(s.channels[1].gain, (0.0, 0.0));
    }

    #[test]
    fn sample_with_wrong_rate_is_ignored() {
        let mut p = Project::new();
        let a = ChannelId(p.ids.next());
        let mut c = Channel::new_sampler(a, "A");
        c.source = ChannelSource::Sampler {
            sample: Some(SampleRef {
                path: "x.wav".into(),
            }),
        };
        p.channels.push(c);
        let buf = Arc::new(SampleBuffer::from_interleaved(44_100, 1, &[1.0]));
        let s = compile(&p, 48_000, |_| Some(Arc::clone(&buf)));
        assert!(s.channels[0].sample.is_none());
    }
}
