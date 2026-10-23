// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio assets inside the project folder (spec 0.2.0 §2, plan D4).
//!
//! Imported files are copied into `<project>/assets/`; the original is
//! never changed or deleted. A copy with the same content is reused.

use crate::{PersistError, io_err};
use jdance_domain::{Project, content_hash};
use std::path::{Path, PathBuf};

/// Folder of copied assets, relative to the project file.
pub const ASSETS_DIR: &str = "assets";

/// What import learns about an audio file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFileInfo {
    pub hash: u64,
    pub size: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
}

/// Reads `path` once: content hash, size and the WAV header.
pub fn inspect_audio(path: &Path) -> Result<AudioFileInfo, PersistError> {
    let bytes = std::fs::read(path).map_err(io_err(path))?;
    let unsupported = |e: &dyn std::fmt::Display| {
        PersistError::UnsupportedAudio(format!("{}: {e}", path.display()))
    };
    let (sample_rate, channels, frames) = match hound::WavReader::new(std::io::Cursor::new(&bytes))
    {
        Ok(reader) => {
            let spec = reader.spec();
            (
                spec.sample_rate,
                spec.channels,
                u64::from(reader.duration()),
            )
        }
        // MP3: the length is what decodes, not the header's estimate
        // (spec 0.2.5 §5).
        Err(_) if nanomp3::detect(&bytes) => {
            let a = crate::decode_mp3(&bytes).map_err(|e| unsupported(&e))?;
            (a.sample_rate, a.channels, a.frames() as u64)
        }
        Err(e) => return Err(unsupported(&e)),
    };
    if frames == 0 || channels == 0 || sample_rate == 0 {
        return Err(PersistError::UnsupportedAudio(format!(
            "{}: the file holds no audio",
            path.display()
        )));
    }
    Ok(AudioFileInfo {
        hash: content_hash(&bytes),
        size: bytes.len() as u64,
        sample_rate,
        channels,
        frames,
    })
}

/// Copies `src` into `<project_dir>/assets/` and returns the relative path
/// (forward slashes). A file there with the same content is reused; a
/// different file with the same name gets a numbered name.
pub fn copy_into_assets(src: &Path, project_dir: &Path, hash: u64) -> Result<String, PersistError> {
    let dir = project_dir.join(ASSETS_DIR);
    std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    let stem = src
        .file_stem()
        .map_or_else(|| "audio".into(), |s| s.to_string_lossy().into_owned());
    let ext = src
        .extension()
        .map_or_else(|| "wav".into(), |s| s.to_string_lossy().into_owned());
    for n in 1..10_000 {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let target: PathBuf = dir.join(&name);
        let rel = format!("{ASSETS_DIR}/{name}");
        if target.exists() {
            let same = std::fs::read(&target).is_ok_and(|b| content_hash(&b) == hash);
            if same {
                return Ok(rel);
            }
            continue;
        }
        // Copy next to the target first: a failed copy leaves no file
        // under the final name.
        let part = dir.join(format!(".{name}.part"));
        std::fs::copy(src, &part).map_err(io_err(&part))?;
        std::fs::rename(&part, &target).map_err(io_err(&target))?;
        return Ok(rel);
    }
    Err(PersistError::Corrupt(
        "too many assets with one name".into(),
    ))
}

/// Copies assets that still point outside the project folder into its
/// `assets/` folder (first save of a new project) and stores relative
/// paths. Returns how many were copied; missing files are reported.
pub fn collect_assets(project: &mut Project, project_dir: &Path) -> Result<usize, PersistError> {
    let mut copied = 0;
    for a in &mut project.assets {
        let path = Path::new(&a.path);
        if !path.is_absolute() {
            continue;
        }
        if let Ok(dir) = project_dir.canonicalize()
            && let Ok(rel) = path.strip_prefix(&dir)
        {
            a.path = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            continue;
        }
        a.path = copy_into_assets(path, project_dir, a.hash)?;
        copied += 1;
    }
    sync_clip_samplers(project);
    Ok(copied)
}

/// A sampler with a clip file plays that file (spec 0.2.9 §1): its sample
/// follows the asset's path when the asset moves into the project.
pub fn sync_clip_samplers(project: &mut Project) {
    for i in 0..project.channels.len() {
        let Some(path) = project.channels[i]
            .clip_asset
            .and_then(|a| project.asset(a))
            .map(|a| a.path.clone())
        else {
            continue;
        };
        if let jdance_domain::ChannelSource::Sampler { sample } = &mut project.channels[i].source {
            *sample = Some(jdance_domain::SampleRef { path });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{AssetId, AudioAsset};

    fn wav(path: &Path, value: f32) {
        crate::write_wav(
            path,
            48_000,
            &[value; 100],
            &[value; 100],
            crate::WavExportFormat::Pcm16,
        )
        .unwrap();
    }

    #[test]
    fn inspect_copy_dedupe_and_collect() {
        let src = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        let a = src.path().join("hit.wav");
        wav(&a, 0.5);
        let info = inspect_audio(&a).unwrap();
        assert_eq!(
            (info.sample_rate, info.channels, info.frames),
            (48_000, 2, 100)
        );
        let rel = copy_into_assets(&a, proj.path(), info.hash).unwrap();
        assert_eq!(rel, "assets/hit.wav");
        assert_eq!(
            copy_into_assets(&a, proj.path(), info.hash).unwrap(),
            rel,
            "same content: reused"
        );
        // Another file with the same name gets a new name.
        let other = src.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let b = other.join("hit.wav");
        wav(&b, 0.25);
        let hb = inspect_audio(&b).unwrap().hash;
        assert_eq!(
            copy_into_assets(&b, proj.path(), hb).unwrap(),
            "assets/hit-2.wav"
        );
        assert!(a.exists() && b.exists(), "originals stay");

        let mut p = Project::new();
        p.assets.push(AudioAsset {
            id: AssetId(1),
            name: "hit".into(),
            path: b.to_string_lossy().into_owned(),
            hash: hb,
            size: info.size,
            sample_rate: 48_000,
            channels: 2,
            frames: 100,
        });
        assert_eq!(collect_assets(&mut p, proj.path()).unwrap(), 1);
        assert_eq!(
            p.assets[0].path, "assets/hit-2.wav",
            "reused the earlier copy"
        );
        assert_eq!(collect_assets(&mut p, proj.path()).unwrap(), 0);
    }
}
