// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline rendering of a project to WAV or MP3, independent of audio
//! devices (ARCH-002). Used by `jdance render` and by File → Export.
//! Files are written under a temporary name and renamed when complete:
//! a cancelled or failed export leaves no file under the chosen name.

use crate::plugins::{Catalog, ModuleCache, create_instance};
use jdance_domain::{
    ClipId, ExportOptions, ExportRange, MAX_REPEAT, PatternClip, Playlist, Project, SampleRef,
    sample_processing::Processing,
};
use jdance_dsp::SampleBuffer;
use jdance_engine::{
    OfflineRender, PlayMode, PluginRt, compile_full, render_offline_with, tick_to_sample,
};
use jdance_persistence::{SourceFormat, read_audio, resolve_sample_path, write_wav};
use jdance_plugin_api::ProcessSetupInfo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Decodes a WAV or MP3 file and converts it to stereo at `sample_rate`.
pub fn load_sample(path: &Path, sample_rate: u32) -> Result<(SampleBuffer, SampleBuffer), String> {
    load_sample_info(path, sample_rate).map(|l| (l.original, l.converted))
}

/// A decoded sample with what the file was and what went wrong on the way.
#[derive(Debug)]
pub struct LoadedSample {
    pub original: SampleBuffer,
    pub converted: SampleBuffer,
    pub format: SourceFormat,
    pub warnings: Vec<String>,
}

/// [`load_sample`] with the source format and import warnings.
pub fn load_sample_info(path: &Path, sample_rate: u32) -> Result<LoadedSample, String> {
    let decoded = read_audio(path).map_err(|e| e.to_string())?;
    let original = SampleBuffer::from_interleaved(
        decoded.sample_rate,
        usize::from(decoded.channels),
        &decoded.samples,
    );
    let converted = original.resampled(sample_rate);
    Ok(LoadedSample {
        original,
        converted,
        format: decoded.format,
        warnings: decoded.warnings,
    })
}

/// A channel's sample with its precomputed processing (spec 0.2.5 §4):
/// the Tools panel, then the length and pitch. At the file's own rate.
pub fn process_sample(original: &SampleBuffer, p: &Processing) -> SampleBuffer {
    let c = &p.precompute;
    let tools = jdance_dsp::Tools {
        remove_dc: c.remove_dc,
        reverse_polarity: c.reverse_polarity,
        normalize: c.normalize,
        fade_stereo: c.fade_stereo,
        reverse: c.reverse,
        swap_stereo: c.swap_stereo,
        start: c.start,
        length: c.length,
        fade_in: c.fade_in,
        fade_out: c.fade_out,
        crossfade: c.crossfade,
        trim: c.trim,
        trim_on: c.trim_on,
    };
    let tooled = if c.is_identity() {
        original.clone()
    } else {
        jdance_dsp::apply_tools(original, &tools)
    };
    let s = &p.stretch;
    if s.is_identity() {
        return tooled;
    }
    let secs = tooled.len() as f64 / f64::from(tooled.sample_rate.max(1));
    let factor = s.length_factor(secs, p.tempo_bpm).clamp(0.05, 20.0);
    jdance_dsp::stretch_and_pitch(&tooled, factor, f64::from(s.pitch), s.stretches())
}

#[derive(Debug)]
pub struct RenderOutcome {
    pub audio: OfflineRender,
    /// Samples that could not be loaded (rendered as silence) and plugins
    /// that could not be created (rendered as bypass).
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
            if p.arrangement_end_ticks() == 0 {
                return Err("the playlist is empty; nothing to export".into());
            }
        }
        ExportRange::Pattern { pattern, loops } => {
            if p.pattern(pattern).is_none() {
                return Err("the pattern to export does not exist".into());
            }
            let mut playlist = Playlist::with_lanes(1, || u64::MAX - 1);
            let pat = p.pattern(pattern).expect("checked");
            playlist.clips.push(PatternClip::repeating(
                ClipId(u64::MAX),
                playlist.lanes[0].id,
                pat,
                0,
                loops.clamp(1, MAX_REPEAT),
            ));
            p.playlist = playlist;
        }
        ExportRange::Selection { start, end } => {
            if end <= start {
                return Err("the selection is empty; nothing to export".into());
            }
            p.playlist.loop_enabled = false;
            cut_at(&mut p, end);
        }
    }
    Ok(p)
}

/// Frames of the anti-click fade where a selection render cuts an audio
/// clip (EXP-005).
const CUT_FADE_FRAMES: u64 = 64;

/// Nothing sounds from tick `end` on (EXP-005): later clips go, clips
/// across it end there. Notes cut by a clip end get their note-off there;
/// audio clips end with a short fade.
fn cut_at(p: &mut Project, end: u64) {
    let pl = &mut p.playlist;
    pl.clips.retain(|c| c.start < end);
    for c in &mut pl.clips {
        c.duration = c.duration.min(end - c.start);
    }
    pl.automation_clips.retain(|c| c.start < end);
    for c in &mut pl.automation_clips {
        c.duration = c.duration.min(end - c.start);
    }
    let fpt = jdance_domain::frames_per_tick(p.tempo_bpm, p.timebase_rate);
    let end_frame = (end as f64 * fpt).round() as u64;
    let timebase = p.timebase_rate;
    let mut clips = std::mem::take(&mut p.playlist.audio_clips);
    clips.retain(|c| c.start < end_frame);
    for c in &mut clips {
        let Some(asset) = p.clip_asset(c) else {
            continue;
        };
        let len = c.length(asset, timebase);
        if c.start + len <= end_frame {
            continue;
        }
        let keep = end_frame - c.start;
        let src = (keep as f64 * f64::from(asset.sample_rate) / f64::from(timebase.max(1))).round()
            as u64;
        c.source_out = (c.source_in + src.max(1)).min(c.source_out);
        c.fade_out = c.fade_out.max(CUT_FADE_FRAMES.min(keep));
    }
    p.playlist.audio_clips = clips;
}

/// Estimated effect tail of the project, for the export dialog warning.
pub fn estimated_tail_seconds(project: &Project, sample_rate: u32) -> f32 {
    jdance_engine::compile_with(project, sample_rate, PlayMode::Song, |_| None).tail_seconds()
}

/// Renders `options.range` plus the tail through the realtime DSP code
/// with fresh processor instances (spec "Export"). The output is aligned
/// to the timeline: plugin delay compensation latency is trimmed.
/// Where an export finds its plugins. Exports create their own offline
/// instances from the saved states (PLG-045).
#[derive(Debug, Clone, Copy)]
pub struct ExportPlugins<'a> {
    pub catalog: &'a Catalog,
    pub modules: &'a ModuleCache,
}

#[cfg(test)]
pub fn render_project(
    project: &Project,
    project_dir: Option<&Path>,
    sample_rate: u32,
    options: &ExportOptions,
    plugins: Option<&ExportPlugins>,
) -> Result<RenderOutcome, String> {
    render_project_with(
        project,
        project_dir,
        sample_rate,
        options,
        plugins,
        &mut |_| true,
    )
}

/// Why an export stopped without a file.
pub const CANCELLED: &str = "export cancelled";

/// [`render_project`] reporting progress `0..=1`; `progress` returning
/// `false` cancels.
pub fn render_project_with(
    project: &Project,
    project_dir: Option<&Path>,
    sample_rate: u32,
    options: &ExportOptions,
    plugins: Option<&ExportPlugins>,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<RenderOutcome, String> {
    let project = export_project(project, options.range)?;
    let mut warnings = Vec::new();
    let setup = ProcessSetupInfo {
        sample_rate: f64::from(sample_rate),
        max_block: jdance_engine::MAX_BLOCK,
        offline: true,
    };
    let mut instances = HashMap::new();
    for (target, slot) in crate::plugins::plugin_slots(&project) {
        let created = match plugins {
            Some(p) => create_instance(p.catalog, p.modules, slot, &setup),
            None => Err("plugins are not available here".into()),
        };
        match created {
            // The controller half is not needed offline beyond creation.
            Ok((processor, controller)) => {
                instances.insert(target, (processor, controller));
            }
            Err(e) => warnings.push(format!(
                "plugin {} rendered as {}: {e}",
                slot.name,
                match target {
                    jdance_domain::PluginTarget::Insert(_) => "bypass",
                    jdance_domain::PluginTarget::Channel(_) => "silence",
                }
            )),
        }
    }
    let mut samples: HashMap<PathBuf, Arc<SampleBuffer>> = HashMap::new();
    let assets: Vec<SampleRef> = project
        .assets
        .iter()
        .map(|a| SampleRef {
            path: a.path.clone(),
        })
        .collect();
    for s in project
        .channels
        .iter()
        .filter_map(|c| c.sample())
        .chain(&assets)
    {
        let path = resolve_sample_path(project_dir, s);
        if samples.contains_key(&path) {
            continue;
        }
        match load_sample(&path, sample_rate) {
            Ok((_, converted)) => {
                samples.insert(path, Arc::new(converted));
            }
            Err(e) => warnings.push(format!("missing sample {}: {e}", path.display())),
        }
    }
    // Processed samples under their own keys (spec 0.2.5 §4).
    for c in &project.channels {
        let (Some(s), Some(p)) = (c.sample(), c.sampler.processing(project.tempo_bpm)) else {
            continue;
        };
        let Some(derived) = c.playback_sample(project.tempo_bpm) else {
            continue;
        };
        let path = resolve_sample_path(project_dir, &derived);
        if samples.contains_key(&path) {
            continue;
        }
        if let Ok((original, _)) = load_sample(&resolve_sample_path(project_dir, s), sample_rate) {
            let processed = process_sample(&original, &p).resampled(sample_rate);
            samples.insert(path, Arc::new(processed));
        }
    }
    let mut controllers = Vec::new();
    let snapshot = compile_full(
        &project,
        sample_rate,
        PlayMode::Song,
        |s| samples.get(&resolve_sample_path(project_dir, s)).cloned(),
        &mut |id, _| match instances.remove(&id) {
            Some((p, c)) => {
                controllers.push(c);
                PluginRt::fresh(Box::new(p))
            }
            None => PluginRt::missing(),
        },
    );
    let tail_warning = options.tail_warning(snapshot.tail_seconds());
    let latency = snapshot.latency as usize;
    let tail = (f64::from(options.tail()) * f64::from(sample_rate)).round() as u64;
    // A selection renders from the song start and drops what comes before
    // it, so it sounds as it does in the song (EXP-004/006).
    let (skip, body_end) = match options.range {
        ExportRange::Selection { start, end } => (
            tick_to_sample(start, project.tempo_bpm, sample_rate),
            tick_to_sample(end, project.tempo_bpm, sample_rate),
        ),
        _ => (0, snapshot.end),
    };
    let skip = usize::try_from(skip).map_err(|_| "render is too long".to_owned())?;
    let frames = usize::try_from(body_end + tail).map_err(|_| "render is too long".to_owned())?;
    let total = (frames + latency).max(1);
    let mut audio = render_offline_with(
        snapshot,
        frames + latency,
        || 1024,
        |done| progress(done as f32 / total as f32),
    )
    .ok_or_else(|| CANCELLED.to_owned())?;
    // Instances are released after the render, controllers last.
    drop(controllers);
    audio.left.drain(..latency + skip);
    audio.right.drain(..latency + skip);
    Ok(RenderOutcome {
        audio,
        warnings,
        tail_warning,
    })
}

/// Title and artist written into MP3 files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportTags {
    pub title: String,
    pub artist: String,
}

/// Renders and writes `out` (WAV, or MP3 at the MP3 rate): render
/// progress up to 90 %, writing the rest. The file appears under its
/// name only when complete.
#[allow(clippy::too_many_arguments)]
pub fn render_to_file(
    project: &Project,
    project_dir: Option<&Path>,
    out: &Path,
    device_rate: u32,
    options: &ExportOptions,
    tags: &ExportTags,
    plugins: Option<&ExportPlugins>,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<RenderOutcome, String> {
    let rate = options.mp3.map_or(device_rate, |m| m.sample_rate);
    let mut outcome =
        render_project_with(project, project_dir, rate, options, plugins, &mut |f| {
            progress(f * 0.9)
        })?;
    let peak = outcome
        .audio
        .left
        .iter()
        .chain(&outcome.audio.right)
        .fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 1.0 && options.mp3.is_some() {
        // No hidden normalization: say so instead.
        outcome.warnings.push(format!(
            "the mix peaks at {:+.1} dBFS and clips in the MP3; lower the master or turn on \
             the limiter",
            jdance_dsp::gain_to_db(peak)
        ));
    }
    let mut part = out.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let written = match options.mp3 {
        None => write_wav(
            &part,
            rate,
            &outcome.audio.left,
            &outcome.audio.right,
            options.format,
        )
        .map_err(|e| e.to_string()),
        Some(m) => write_mp3(&part, &outcome.audio, m, tags, progress),
    };
    let published = written.and_then(|()| std::fs::rename(&part, out).map_err(|e| e.to_string()));
    if let Err(e) = published {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    Ok(outcome)
}

fn write_mp3(
    path: &Path,
    audio: &OfflineRender,
    m: jdance_domain::Mp3Options,
    tags: &ExportTags,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<(), String> {
    let mut enc = jdance_mp3::Mp3Writer::new(&jdance_mp3::Mp3Settings {
        kbps: m.kbps,
        sample_rate: m.sample_rate,
        title: tags.title.clone(),
        artist: tags.artist.clone(),
    })?;
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut file = std::io::BufWriter::new(file);
    const BLOCK: usize = 16_384;
    let total = audio.left.len().max(1);
    for (k, (l, r)) in audio
        .left
        .chunks(BLOCK)
        .zip(audio.right.chunks(BLOCK))
        .enumerate()
    {
        enc.write(l, r, &mut file)?;
        let done = ((k + 1) * BLOCK).min(total);
        if !progress(0.9 + 0.1 * done as f32 / total as f32) {
            return Err(CANCELLED.into());
        }
    }
    enc.finish(&mut file)?;
    file.into_inner()
        .map_err(|e| e.to_string())?
        .sync_all()
        .map_err(|e| e.to_string())
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

    #[test]
    fn processing_refits_to_the_project_tempo_and_applies_tools() {
        use jdance_domain::sample_processing::{Precompute, Stretch, TimeFit};
        // One second of a ramp: reversed, it falls.
        let ramp: Vec<f32> = (0..RATE).map(|i| i as f32 / RATE as f32).collect();
        let buf = SampleBuffer::from_interleaved(RATE, 1, &ramp);
        let p = |tempo| Processing {
            precompute: Precompute {
                reverse: true,
                ..Precompute::default()
            },
            stretch: Stretch {
                time: TimeFit::Beats { beats: 4.0 },
                ..Stretch::default()
            },
            tempo_bpm: tempo,
        };
        // Four beats: 2 s at 120 BPM, 1 s at 240 BPM.
        let slow = process_sample(&buf, &p(120.0));
        let fast = process_sample(&buf, &p(240.0));
        assert!(
            slow.len().abs_diff(2 * RATE as usize) < 64,
            "{}",
            slow.len()
        );
        assert!(fast.len().abs_diff(RATE as usize) < 64, "{}", fast.len());
        assert!(fast.left[100] > fast.left[RATE as usize - 100], "reversed");
    }

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
        let id = ClipId(p.ids.next());
        let clip = PatternClip::repeating(id, lane, &p.patterns[0], start_bar * 3840, repeat);
        p.playlist.clips.push(clip);
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
            mp3: None,
        }
    }

    #[test]
    fn mp3_export_publishes_a_file_and_cancel_leaves_none() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 2);
        let out = dir.path().join("song.mp3");
        let options = ExportOptions {
            mp3: Some(jdance_domain::Mp3Options {
                kbps: 192,
                sample_rate: 44_100,
            }),
            ..opts(ExportRange::Song, 1.0)
        };
        let tags = ExportTags {
            title: "T".into(),
            artist: "A".into(),
        };
        let mut seen = Vec::new();
        let o = render_to_file(
            &p,
            Some(dir.path()),
            &out,
            RATE,
            &options,
            &tags,
            None,
            &mut |f| {
                seen.push(f);
                true
            },
        )
        .unwrap();
        assert_eq!(o.audio.sample_rate, 44_100, "rendered at the MP3 rate");
        assert!(seen.windows(2).all(|w| w[0] <= w[1]) && *seen.last().unwrap() >= 0.99);
        let data = std::fs::read(&out).unwrap();
        let (frames, rate) = jdance_mp3::count_frames(&data);
        assert_eq!(rate, 44_100);
        // 2 bars at 120 BPM + 1 s tail = 5 s; decoders trim delay/padding.
        let samples = (frames - 1) * 1152;
        let expected = o.audio.left.len();
        assert!(
            samples >= expected && samples < expected + 3 * 1152,
            "{samples} vs {expected}"
        );
        assert!(!dir.path().join("song.mp3.part").exists());

        // Cancel during encoding: no file under either name.
        let out2 = dir.path().join("cancel.mp3");
        let err = render_to_file(
            &p,
            Some(dir.path()),
            &out2,
            RATE,
            &options,
            &tags,
            None,
            &mut |f| f < 0.95,
        )
        .unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(!out2.exists() && !dir.path().join("cancel.mp3.part").exists());
        // Cancel during the render.
        let err = render_to_file(
            &p,
            Some(dir.path()),
            &out2,
            RATE,
            &opts(ExportRange::Song, 1.0),
            &tags,
            None,
            &mut |f| f < 0.3,
        )
        .unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(!out2.exists());
    }

    #[test]
    fn song_export_renders_arrangement_plus_tail_without_loop() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 2);
        p.playlist.loop_range = Some((0, 1920));
        p.playlist.loop_enabled = true;
        let o = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 1.0),
            None,
        )
        .unwrap();
        assert_eq!(o.audio.left.len(), 2 * BAR + RATE as usize);
        assert_eq!(onsets(&o.audio.left)[..2], [0, BAR]);
        assert!(o.warnings.is_empty() && o.tail_warning.is_none());
    }

    #[test]
    fn default_tail_is_five_seconds() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 1);
        let o = render_project(&p, Some(dir.path()), RATE, &ExportOptions::song(), None).unwrap();
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
        let o = render_project(&p, Some(dir.path()), RATE, &opts(range, 2.0), None).unwrap();
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
        assert!(render_project(&p, Some(dir.path()), RATE, &opts(missing, 0.0), None).is_err());
    }

    #[test]
    fn empty_song_is_an_error() {
        let (dir, p) = setup(1);
        assert!(render_project(&p, Some(dir.path()), RATE, &ExportOptions::song(), None).is_err());
    }

    #[test]
    fn latency_is_trimmed_so_audio_starts_on_time() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 1);
        master_insert(
            &mut p,
            Effect::new(EffectKind::Compressor).with("lookahead", 5.0),
        );
        let o = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 0.0),
            None,
        )
        .unwrap();
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
        let o = render_to_file(
            &p,
            Some(dir.path()),
            &out,
            RATE,
            &opts(ExportRange::Song, 0.0),
            &ExportTags::default(),
            None,
            &mut |_| true,
        )
        .unwrap();
        let peak = o.audio.left.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 1.0, "test needs an over, got {peak}");
        let back = jdance_persistence::read_wav(&out).unwrap();
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
        let short = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 1.0),
            None,
        );
        assert!(short.unwrap().tail_warning.is_some());
        let long = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 30.0),
            None,
        );
        assert!(long.unwrap().tail_warning.is_none());
    }

    fn test_plugins() -> (Catalog, ModuleCache) {
        use crate::plugins::{ModuleEntry, ModuleResult};
        let bundle = jdance_test_support::test_plugin_bundle(jdance_plugin_vst3::bundle_binary);
        let mut catalog = Catalog::default();
        catalog.modules.insert(
            bundle.to_path_buf(),
            ModuleEntry {
                key: None,
                result: ModuleResult::Ok(jdance_plugin_vst3::scan_in_process(bundle).unwrap()),
            },
        );
        (catalog, ModuleCache::default())
    }

    fn plugin_slot(catalog: &Catalog, name: &str) -> jdance_domain::PluginSlot {
        let d = catalog.plugins().find(|d| d.plugin.name == name).unwrap();
        jdance_domain::PluginSlot {
            format: "vst3".into(),
            class_id: d.plugin.class_id.clone(),
            name: d.plugin.name.clone(),
            vendor: d.plugin.vendor.clone(),
            version: d.plugin.version.clone(),
            category: d.category.clone(),
            component_state: Vec::new(),
            controller_state: Vec::new(),
        }
    }

    #[test]
    fn export_renders_plugins_with_compensated_latency() {
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 1);
        let (catalog, modules) = test_plugins();
        master_insert(
            &mut p,
            Effect::plugin(plugin_slot(&catalog, "JD Test Latency")),
        );
        let plugins = ExportPlugins {
            catalog: &catalog,
            modules: &modules,
        };
        let o = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 0.0),
            Some(&plugins),
        )
        .unwrap();
        assert!(o.warnings.is_empty(), "{:?}", o.warnings);
        let dry = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 0.0),
            None,
        )
        .unwrap();
        assert_eq!(dry.warnings.len(), 1, "missing plugin is reported");
        // The plugin delays by 64 samples; export trims the latency, so
        // the result lines up with the bypassed render.
        assert_eq!(onsets(&o.audio.left).first(), Some(&0));
        assert_eq!(o.audio.left, dry.audio.left);
    }

    #[test]
    fn export_restores_plugin_state() {
        use jdance_plugin_api::PluginController;
        let (dir, mut p) = setup(1);
        add_clip(&mut p, 0, 1);
        let (catalog, modules) = test_plugins();
        let mut slot = plugin_slot(&catalog, "JD Test Gain");
        // Gain normalized 0.25 → linear 0.5, saved in the plugin state.
        let setup_info = ProcessSetupInfo {
            sample_rate: f64::from(RATE),
            max_block: 512,
            offline: true,
        };
        let (mut proc_, mut ctrl) =
            create_instance(&catalog, &modules, &slot, &setup_info).unwrap();
        ctrl.set_param_normalized(0, 0.25);
        // The processor applies the change in its next block.
        let mut buf = [0.0f32; 16];
        let mut buf_r = [0.0f32; 16];
        jdance_plugin_api::PluginProcessor::process(
            &mut proc_,
            &mut buf,
            &mut buf_r,
            None,
            &jdance_plugin_api::Transport::default(),
        );
        let state = ctrl.state().unwrap();
        slot.component_state = state.component;
        slot.controller_state = state.controller;
        let unity = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 0.0),
            None,
        )
        .unwrap();
        master_insert(&mut p, Effect::plugin(slot));
        let plugins = ExportPlugins {
            catalog: &catalog,
            modules: &modules,
        };
        let o = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 0.0),
            Some(&plugins),
        )
        .unwrap();
        for (a, b) in o.audio.left.iter().zip(&unity.audio.left) {
            assert!((a - b * 0.5).abs() < 1e-7);
        }
    }

    #[test]
    fn export_plays_instrument_channels() {
        let (dir, mut p) = setup(0);
        let (catalog, modules) = test_plugins();
        let id = jdance_domain::ChannelId(p.ids.next());
        p.channels.push(jdance_domain::Channel::new_instrument(
            id,
            "Synth",
            plugin_slot(&catalog, "JD Test Synth"),
        ));
        p.patterns[0].row_mut(id).steps[4].on = true;
        add_clip(&mut p, 0, 1);
        let plugins = ExportPlugins {
            catalog: &catalog,
            modules: &modules,
        };
        let o = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 0.0),
            Some(&plugins),
        )
        .unwrap();
        assert!(o.warnings.is_empty(), "{:?}", o.warnings);
        // Step 4 at 120 BPM starts at 24 000 samples and lasts one step.
        let first = o.audio.left.iter().position(|v| v.abs() > 1e-6);
        assert_eq!(first, Some(4 * 6_000 + 1), "sine starts at phase 0");
        assert!(
            o.audio.left[5 * 6_000 + 10..].iter().all(|v| *v == 0.0),
            "gate ends"
        );
        let silent = render_project(
            &p,
            Some(dir.path()),
            RATE,
            &opts(ExportRange::Song, 0.0),
            None,
        )
        .unwrap();
        assert!(silent.warnings[0].contains("silence"));
        assert!(silent.audio.left.iter().all(|v| *v == 0.0));
    }

    /// Parameter points (id, offset, value) from the JD Test Logger state,
    /// split into process calls (each starts with the chase at offset 0).
    fn logger_blocks(ctrl: &mut jdance_plugin_vst3::Vst3Controller) -> Vec<Vec<(i32, f64)>> {
        use jdance_plugin_api::PluginController;
        let s = ctrl.state().unwrap().component;
        let n = u32::from_le_bytes(s[13..17].try_into().unwrap()) as usize;
        let mut blocks: Vec<Vec<(i32, f64)>> = Vec::new();
        for e in s[17..17 + n * 16].chunks_exact(16) {
            assert_eq!(
                u32::from_le_bytes(e[0..4].try_into().unwrap()),
                0,
                "param id"
            );
            let offset = i32::from_le_bytes(e[4..8].try_into().unwrap());
            let value = f64::from_le_bytes(e[8..16].try_into().unwrap());
            if offset == 0 {
                blocks.push(Vec::new());
            }
            blocks.last_mut().unwrap().push((offset, value));
        }
        blocks
    }

    #[test]
    fn vst3_automation_points_arrive_sample_accurately() {
        // AUT-031 and AUT-033 against the real test plugin.
        use jdance_domain::{AutoPoint, AutomationId, AutomationLane, AutomationTarget, Curve};
        let (_dir, mut p) = setup(1);
        add_clip(&mut p, 0, 2);
        let (catalog, modules) = test_plugins();
        let slot = plugin_slot(&catalog, "JD Test Logger");
        master_insert(&mut p, Effect::plugin(slot.clone()));
        let insert = p.mixer.master.inserts[0].id;
        let pt = |tick, value, curve| AutoPoint {
            tick,
            value,
            curve,
            tension: 0.0,
        };
        // 25 samples per tick: points at samples 0, 750 and 1500.
        let source = AutomationId(p.ids.next());
        p.automation.push(AutomationLane {
            id: source,
            name: "Param 0".into(),
            target: AutomationTarget::PluginParam {
                target: jdance_domain::PluginTarget::Insert(insert),
                param: 0,
            },
            length: 2 * 3840,
            points: vec![
                pt(0, 0.2, Curve::Hold),
                pt(30, 0.9, Curve::Linear),
                pt(60, 0.5, Curve::Linear),
            ],
        });
        p.playlist
            .automation_clips
            .push(jdance_domain::AutomationClip {
                id: jdance_domain::ClipId(p.ids.next()),
                lane: p.playlist.lanes[0].id,
                source,
                start: 0,
                offset: 0,
                duration: 2 * 3840,
                muted: false,
            });
        p.playlist.loop_range = Some((0, 3840));
        p.playlist.loop_enabled = true;
        let setup_info = ProcessSetupInfo {
            sample_rate: f64::from(RATE),
            max_block: 512,
            offline: false,
        };
        let (proc_, mut ctrl) = create_instance(&catalog, &modules, &slot, &setup_info).unwrap();
        let mut proc_ = Some(proc_);
        let snap = jdance_engine::compile_full(
            &p,
            RATE,
            jdance_engine::PlayMode::Song,
            |_| None,
            &mut |_, _| jdance_engine::PluginRt::fresh(Box::new(proc_.take().unwrap())),
        );
        let (mut h, mut e) = jdance_engine::create_engine(RATE);
        h.set_snapshot(snap).unwrap();
        h.play().unwrap();
        let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
        e.render(&mut l, &mut r);
        e.render(&mut l, &mut r);
        h.seek(1_100).unwrap();
        e.render(&mut l, &mut r);
        // The loop wraps after 256 samples of this block.
        h.seek(BAR as u64 - 256).unwrap();
        e.render(&mut l, &mut r);

        // Envelope values as the engine computes them (f32).
        let lin = |pos: u64| {
            let t = (pos - 750) as f64 / 750.0;
            f64::from(0.9f32 + (0.5f32 - 0.9f32) * t as f32)
        };
        let v = |x: f32| f64::from(x);
        let blocks = logger_blocks(&mut ctrl);
        assert_eq!(blocks.len(), 4, "{blocks:?}");
        let every_64 = |val: f64| (0..8).map(|k| (k * 64, val)).collect::<Vec<_>>();
        assert_eq!(blocks[0], every_64(v(0.2)), "hold before the first jump");
        let mut want = vec![
            (0, v(0.2)),
            (64, v(0.2)),
            (128, v(0.2)),
            (192, v(0.2)),
            (237, v(0.2)),
            (238, v(0.9)),
        ];
        want.extend((4..8).map(|k| (k * 64, lin(512 + k as u64 * 64))));
        assert_eq!(blocks[1], want, "jump at sample 750 is exact");
        assert_eq!(blocks[2][0], (0, lin(1_100)), "chase after seek");
        // The point at sample 1500 (offset 400) ends the ramp exactly.
        assert!(blocks[2].contains(&(399, lin(1_499))), "{:?}", blocks[2]);
        assert!(blocks[2].contains(&(400, v(0.5))), "{:?}", blocks[2]);
        assert_eq!(blocks[2].last(), Some(&(448, v(0.5))));
        let mut want = every_64(v(0.5));
        want.insert(4, (255, v(0.5)));
        for x in &mut want[5..] {
            x.1 = v(0.2);
        }
        assert_eq!(blocks[3], want, "loop wrap jumps back at its exact offset");
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use jdance_domain::{
        AssetId, AudioAsset, AudioClip, Channel, ChannelId, Effect, EffectKind, ExportFormat,
        Insert, InsertId,
    };
    use jdance_persistence::WavExportFormat;
    use jdance_test_support::{click, step_project};

    const RATE: u32 = 48_000;
    /// 120 BPM: 25 samples per tick.
    const SPT: u64 = 25;

    /// A click on every bar for 4 bars, optionally into a long reverb.
    fn song(reverb: bool) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("samples")).unwrap();
        let s = click(RATE, 4_000);
        write_wav(
            &dir.path().join("samples/0.wav"),
            RATE,
            &s.left,
            &s.right,
            WavExportFormat::Float32,
        )
        .unwrap();
        let mut p = step_project(120.0, &[&[0]]);
        p.channels[0].pan = 0.0;
        let lane = p.playlist.lanes[0].id;
        let clip = PatternClip::repeating(ClipId(p.ids.next()), lane, &p.patterns[0], 0, 4);
        p.playlist.clips.push(clip);
        if reverb {
            p.mixer.master.inserts.push(Insert {
                id: InsertId(p.ids.next()),
                bypass: false,
                effect: Effect::new(EffectKind::Reverb).with("decay", 6.0),
                sidechain: None,
            });
        }
        (dir, p)
    }

    fn opts(range: ExportRange) -> ExportOptions {
        ExportOptions {
            range,
            tail_seconds: 1.0,
            format: ExportFormat::Float32,
            mp3: None,
        }
    }

    #[test]
    fn a_selection_is_the_same_part_of_the_song_render() {
        // EXP-004/006: from bar 2 beat 2 to bar 4, with the reverb of the
        // first click and the second bar's click still sounding.
        let (dir, mut p) = song(true);
        p.playlist.loop_range = Some((0, 3840));
        p.playlist.loop_enabled = true;
        let (a, b) = (3840 + 960, 3 * 3840);
        let render = |range| {
            render_project(&p, Some(dir.path()), RATE, &opts(range), None)
                .unwrap()
                .audio
        };
        let full = render(ExportRange::Song);
        let sel = render(ExportRange::Selection { start: a, end: b });
        let (sa, sb) = ((a * SPT) as usize, (b * SPT) as usize);
        assert_eq!(
            sel.left.len(),
            sb - sa + RATE as usize,
            "selection + 1 s tail"
        );
        for i in 0..sb - sa {
            assert!(
                (sel.left[i] - full.left[sa + i]).abs() < 1e-6
                    && (sel.right[i] - full.right[sa + i]).abs() < 1e-6,
                "sample {i}"
            );
        }
        assert!(
            sel.left[..100].iter().any(|v| v.abs() > 1e-6),
            "the reverb from before"
        );
    }

    #[test]
    fn nothing_starts_after_the_selection_and_the_loop_is_ignored() {
        let (dir, mut p) = song(false);
        p.playlist.loop_range = Some((3840, 2 * 3840));
        p.playlist.loop_enabled = true;
        let range = ExportRange::Selection {
            start: 3840,
            end: 2 * 3840 + 960,
        };
        let sel = render_project(&p, Some(dir.path()), RATE, &opts(range), None)
            .unwrap()
            .audio;
        let onsets: Vec<usize> = (0..sel.left.len())
            .filter(|&i| sel.left[i] != 0.0 && (i == 0 || sel.left[i - 1] == 0.0))
            .collect();
        // Bar 2's click at 0 and bar 3's inside the selection; not bar 4's
        // (after the end), and no loop back.
        assert_eq!(onsets, [0, (3840 * SPT) as usize]);
        let empty = ExportRange::Selection { start: 10, end: 10 };
        assert!(render_project(&p, Some(dir.path()), RATE, &opts(empty), None).is_err());
    }

    #[test]
    fn an_audio_clip_across_the_end_is_cut_with_a_fade() {
        let mut p = Project::new();
        let asset = AssetId(p.ids.next());
        p.assets.push(AudioAsset {
            id: asset,
            name: "a".into(),
            path: "a.wav".into(),
            hash: 0,
            size: 0,
            sample_rate: p.timebase_rate,
            channels: 1,
            frames: 1_000_000,
        });
        let ch = ChannelId(p.ids.next());
        p.channels.push(Channel::new_audio(ch, "a", asset));
        let lane = p.playlist.lanes[0].id;
        let clip = |id, start| AudioClip {
            id: ClipId(id),
            lane,
            channel: ch,
            start,
            source_in: 100,
            source_out: 1_000_000,
            gain: 1.0,
            muted: false,
            fade_in: 0,
            fade_out: 0,
        };
        let fpt = jdance_domain::frames_per_tick(p.tempo_bpm, p.timebase_rate);
        let end = 3840;
        let end_frame = (end as f64 * fpt).round() as u64;
        p.playlist.audio_clips = vec![clip(1, 1000), clip(2, end_frame)];
        cut_at(&mut p, end);
        assert_eq!(
            p.playlist.audio_clips.len(),
            1,
            "the clip from the end on goes"
        );
        let c = &p.playlist.audio_clips[0];
        assert_eq!(c.source_out, 100 + end_frame - 1000);
        assert_eq!(c.fade_out, CUT_FADE_FRAMES);
    }
}
