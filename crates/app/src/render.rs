// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline rendering of a project to WAV, independent of audio devices
//! (ARCH-002). Used by `jdance render` and by File → Export WAV.

use jdance_domain::Project;
use jdance_dsp::SampleBuffer;
use jdance_engine::{OfflineRender, compile, render_offline};
use jdance_persistence::{WavExportFormat, read_wav, resolve_sample_path, write_wav};
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
}

pub fn render_project(
    project: &Project,
    project_dir: Option<&Path>,
    sample_rate: u32,
    loops: u32,
) -> Result<RenderOutcome, String> {
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
    let snapshot = compile(project, sample_rate, |s| {
        samples.get(&resolve_sample_path(project_dir, s)).cloned()
    });
    if snapshot.loop_len == 0 {
        return Err("the project has no pattern to render".into());
    }
    let frames = usize::try_from(snapshot.loop_len * u64::from(loops.max(1)))
        .map_err(|_| "render is too long".to_owned())?;
    let audio = render_offline(snapshot, frames, || 1024);
    Ok(RenderOutcome { audio, warnings })
}

pub fn render_to_wav(
    project: &Project,
    project_dir: Option<&Path>,
    out: &Path,
    sample_rate: u32,
    loops: u32,
    format: WavExportFormat,
) -> Result<RenderOutcome, String> {
    let outcome = render_project(project, project_dir, sample_rate, loops)?;
    write_wav(
        out,
        sample_rate,
        &outcome.audio.left,
        &outcome.audio.right,
        format,
    )
    .map_err(|e| e.to_string())?;
    Ok(outcome)
}
