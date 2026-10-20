// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline rendering of a project to WAV, independent of audio devices
//! (ARCH-002). Used by `jdance render` and by File → Export WAV.

use jdance_domain::{
    ClipId, ExportOptions, ExportRange, MAX_REPEAT, PatternClip, Playlist, Project,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{OfflineRender, PlayMode, compile_with, render_offline};
use jdance_persistence::{read_wav, resolve_sample_path, write_wav};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Decodes a WAV file and converts it to stereo at `sample_rate`.
pub fn load_sample(path: &Path, sample_rate: u32) -> Result<(SampleBuffer, SampleBuffer), String> {
    let decoded = read_wav(path).map_err(|e| e.to_string())?;
    let original = SampleBuffer::from_interleaved(
        decoded.sample_rate,
        usize::from(decoded.channels),
        &decoded.samples,
    );
    let converted = original.resampled(sample_rate);
    Ok((original, converted))
}

#[derive(Debug)]
pub struct RenderOutcome {
    pub audio: OfflineRender,
    /// Samples that could not be loaded (rendered as silence).
    pub warnings: Vec<String>,
    /// Set when effects are expected to ring longer than the chosen tail.
    pub tail_warning: Option<String>,
}

/// The project as it is rendered for `range`: the arrangement without
/// its loop, or a single clip repeating the pattern.
fn export_project(project: &Project, range: ExportRange) -> Result<Project, String> {
    let mut p = project.clone();
    match range {
        ExportRange::Song => {
            p.playlist.loop_enabled = false;
            if p.playlist.arrangement_end(&p.patterns) == 0 {
                return Err("the playlist is empty; nothing to export".into());
            }
        }
        ExportRange::Pattern { pattern, loops } => {
            if p.pattern(pattern).is_none() {
                return Err("the pattern to export does not exist".into());
            }
            let mut playlist = Playlist::with_lanes(1, || u64::MAX - 1);
            playlist.clips.push(PatternClip {
                id: ClipId(u64::MAX),
                lane: playlist.lanes[0].id,
                pattern,
                start: 0,
                repeat: loops.clamp(1, MAX_REPEAT),
                muted: false,
            });
            p.playlist = playlist;
        }
    }
    Ok(p)
}

/// Estimated effect tail of the project, for the export dialog warning.
pub fn estimated_tail_seconds(project: &Project, sample_rate: u32) -> f32 {
    compile_with(project, sample_rate, PlayMode::Song, |_| None).tail_seconds()
}

/// Renders `options.range` plus the tail through the realtime DSP code
/// with fresh processor instances (spec "Export"). The output is aligned
/// to the timeline: plugin delay compensation latency is trimmed.
pub fn render_project(
    project: &Project,
    project_dir: Option<&Path>,
    sample_rate: u32,
    options: &ExportOptions,
) -> Result<RenderOutcome, String> {
    let project = export_project(project, options.range)?;
    let mut warnings = Vec::new();
    let mut samples: HashMap<PathBuf, Arc<SampleBuffer>> = HashMap::new();
    for ch in &project.channels {
        let Some(s) = ch.sample() else { continue };
        let path = resolve_sample_path(project_dir, s);
        if samples.contains_key(&path) {
            continue;
        }
        match load_sample(&path, sample_rate) {
            Ok((_, converted)) => {
                samples.insert(path, Arc::new(converted));
            }
            Err(e) => warnings.push(format!("{}: {e}", path.display())),
        }
    }
    let snapshot = compile_with(&project, sample_rate, PlayMode::Song, |s| {
        samples.get(&resolve_sample_path(project_dir, s)).cloned()
    });
    let tail_warning = options.tail_warning(snapshot.tail_seconds());
    let latency = snapshot.latency as usize;
    let tail = (f64::from(options.tail()) * f64::from(sample_rate)).round() as u64;
    let frames =
        usize::try_from(snapshot.end + tail).map_err(|_| "render is too long".to_owned())?;
    let mut audio = render_offline(snapshot, frames + latency, || 1024);
    audio.left.drain(..latency);
    audio.right.drain(..latency);
    Ok(RenderOutcome {
        audio,
        warnings,
        tail_warning,
    })
}

pub fn render_to_wav(
    project: &Project,
    project_dir: Option<&Path>,
    out: &Path,
    sample_rate: u32,
    options: &ExportOptions,
) -> Result<RenderOutcome, String> {
    let outcome = render_project(project, project_dir, sample_rate, options)?;
    write_wav(
        out,
        sample_rate,
        &outcome.audio.left,
        &outcome.audio.right,
        options.format,
    )
    .map_err(|e| e.to_string())?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{
        DEFAULT_EXPORT_TAIL_SECONDS, Effect, EffectKind, ExportFormat, Insert, InsertId, PatternId,
    };
    use jdance_persistence::WavExportFormat;
    use jdance_test_support::{click, step_project};

    const RATE: u32 = 48_000;
    /// One 16-step bar at 120 BPM.
    const BAR: usize = 96_000;

    fn setup(channels: usize) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("samples")).unwrap();
        let s = click(RATE, 4_000);
        for i in 0..channels {
            write_wav(
                &dir.path().join(format!("samples/{i}.wav")),
                RATE,
                &s.left,
                &s.right,
                WavExportFormat::Float32,
            )
            .unwrap();
        }
        let steps: Vec<&[u32]> = vec![&[0]; channels];
        let mut p = step_project(120.0, &steps);
        for ch in &mut p.channels {
            ch.pan = 0.0;
        }
        (dir, p)
    }

    fn add_clip(p: &mut Project, start_bar: u64, repeat: u32) {
        let lane = p.playlist.lanes[0].id;
        p.playlist.clips.push(PatternClip {
            id: ClipId(p.ids.next()),
            lane,
            pattern: p.patterns[0].id,
            start: start_bar * 3840,
            repeat,
            muted: false,
        });
    }

    fn master_insert(p: &mut Project, effect: Effect) {
        let id = InsertId(p.ids.next());
        p.mixer.master.inserts.push(Insert {
            id,
            bypass: false,
            effect,
            sidechain: None,
        });
    }

    fn onsets(left: &[f32]) -> Vec<usize> {
        (0..left.len())
            .filter(|&i| left[i] != 0.0 && (i == 0 || left[i - 1] == 0.0))
            .collect()
    }

    fn opts(range: ExportRange, tail: f32) -> ExportOptions {
        ExportOptions {
            range,
            tail_seconds: tail,
            format: ExportFormat::Float32,
        }
    }

    #[test]
    fn song_export_renders_arrangement_plus_tail_without_loop() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 2);
        p.playlist.loop_range = Some((0, 1920));
        p.playlist.loop_enabled = true;
        let o = render_project(&p, Some(dir.path()), RATE, &opts(ExportRange::Song, 1.0)).unwrap();
        assert_eq!(o.audio.left.len(), 2 * BAR + RATE as usize);
        assert_eq!(onsets(&o.audio.left)[..2], [0, BAR]);
        assert!(o.warnings.is_empty() && o.tail_warning.is_none());
    }

    #[test]
    fn default_tail_is_five_seconds() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 1);
        let o = render_project(&p, Some(dir.path()), RATE, &ExportOptions::song()).unwrap();
        assert_eq!(DEFAULT_EXPORT_TAIL_SECONDS, 5.0);
        assert_eq!(o.audio.left.len(), BAR + 5 * RATE as usize);
    }

    #[test]
    fn pattern_export_repeats_then_lets_effects_ring() {
        let (dir, mut p) = setup(1);
        // The last note is a bar (2 s) before the tail; a 6 s decay is
        // still audible there.
        master_insert(&mut p, Effect::new(EffectKind::Reverb).with("decay", 6.0));
        let range = ExportRange::Pattern {
            pattern: p.patterns[0].id,
            loops: 3,
        };
        let o = render_project(&p, Some(dir.path()), RATE, &opts(range, 2.0)).unwrap();
        assert_eq!(o.audio.left.len(), 3 * BAR + 2 * RATE as usize);
        let tail = &o.audio.left[3 * BAR..];
        let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak(&tail[..RATE as usize / 2]) > 1e-4, "reverb rings");
        assert!(
            peak(tail) < peak(&o.audio.left[..BAR]),
            "no new notes in the tail"
        );
        let missing = ExportRange::Pattern {
            pattern: PatternId(999),
            loops: 1,
        };
        assert!(render_project(&p, Some(dir.path()), RATE, &opts(missing, 0.0)).is_err());
    }

    #[test]
    fn empty_song_is_an_error() {
        let (dir, p) = setup(1);
        assert!(render_project(&p, Some(dir.path()), RATE, &ExportOptions::song()).is_err());
    }

    #[test]
    fn latency_is_trimmed_so_audio_starts_on_time() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 1);
        master_insert(
            &mut p,
            Effect::new(EffectKind::Compressor).with("lookahead", 5.0),
        );
        let o = render_project(&p, Some(dir.path()), RATE, &opts(ExportRange::Song, 0.0)).unwrap();
        assert_eq!(o.audio.left.len(), BAR);
        assert_eq!(onsets(&o.audio.left).first(), Some(&0));
    }

    #[test]
    fn float32_export_keeps_overs() {
        let (dir, mut p) = setup(4);
        let id = p.patterns[0].id;
        for ch in p.channels.clone() {
            p.pattern_mut(id).unwrap().row_mut(ch.id).steps[0].velocity = 1.0;
        }
        add_clip(&mut p, 0, 1);
        let out = dir.path().join("out.wav");
        let o = render_to_wav(
            &p,
            Some(dir.path()),
            &out,
            RATE,
            &opts(ExportRange::Song, 0.0),
        )
        .unwrap();
        let peak = o.audio.left.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 1.0, "test needs an over, got {peak}");
        let back = read_wav(&out).unwrap();
        let back_peak = back.samples.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert_eq!(back_peak, peak);
    }

    #[test]
    fn long_effect_tail_warns() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 1);
        master_insert(&mut p, Effect::new(EffectKind::Reverb).with("decay", 10.0));
        let est = estimated_tail_seconds(&p, RATE);
        assert!(est > 5.0, "estimate {est}");
        let short = render_project(&p, Some(dir.path()), RATE, &opts(ExportRange::Song, 1.0));
        assert!(short.unwrap().tail_warning.is_some());
        let long = render_project(&p, Some(dir.path()), RATE, &opts(ExportRange::Song, 30.0));
        assert!(long.unwrap().tail_warning.is_none());
    }
}
