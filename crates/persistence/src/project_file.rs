// SPDX-License-Identifier: GPL-3.0-or-later
//! Project file format (PER-001): a single JSON file.
//!
//! ```json
//! { "format": "jdance-project", "format_version": 1, "project": { … } }
//! ```
//!
//! Any schema change increments `format_version` and adds a migration step
//! in [`migrate`] with a test.

use crate::{PersistError, io_err, write_atomically};
use jdance_domain::{ChannelSource, Project, SampleRef};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const FORMAT_TAG: &str = "jdance-project";
pub const CURRENT_FORMAT_VERSION: u32 = 1;
pub const PROJECT_EXTENSION: &str = "jdance";

/// Saves `project` to `path` atomically. Sample paths inside the project
/// directory are stored relative to it (PER-003); the in-memory project is
/// not modified.
pub fn save_project(path: &Path, project: &Project) -> Result<(), PersistError> {
    let dir = path.parent().unwrap_or(Path::new(""));
    let mut doc = project.clone();
    for ch in &mut doc.channels {
        let ChannelSource::Sampler { sample: Some(s) } = &mut ch.source else {
            continue;
        };
        if let Some(rel) = relative_to(Path::new(&s.path), dir) {
            s.path = rel;
        }
    }
    let value = serde_json::json!({
        "format": FORMAT_TAG,
        "format_version": CURRENT_FORMAT_VERSION,
        "project": doc,
    });
    let mut bytes =
        serde_json::to_vec_pretty(&value).map_err(|e| PersistError::Corrupt(e.to_string()))?;
    bytes.push(b'\n');
    write_atomically(path, &bytes)
}

fn relative_to(sample: &Path, dir: &Path) -> Option<String> {
    if !sample.is_absolute() || dir.as_os_str().is_empty() {
        return None;
    }
    let dir = dir.canonicalize().ok()?;
    let rel = sample.strip_prefix(&dir).ok()?;
    // Forward slashes so files are portable between OSes.
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

pub fn load_project(path: &Path) -> Result<Project, PersistError> {
    let bytes = std::fs::read(path).map_err(io_err(path))?;
    parse_project(&bytes)
}

fn parse_project(bytes: &[u8]) -> Result<Project, PersistError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| PersistError::Corrupt(e.to_string()))?;
    if value.get("format").and_then(Value::as_str) != Some(FORMAT_TAG) {
        return Err(PersistError::NotAProject("missing format tag".into()));
    }
    let version = value
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| PersistError::Corrupt("missing format_version".into()))?;
    let version =
        u32::try_from(version).map_err(|_| PersistError::Corrupt("bad format_version".into()))?;
    if version > CURRENT_FORMAT_VERSION {
        return Err(PersistError::NewerFormat {
            found: version,
            supported: CURRENT_FORMAT_VERSION,
        });
    }
    let project = migrate(
        version,
        value.get("project").cloned().unwrap_or(Value::Null),
    )?;
    let project: Project =
        serde_json::from_value(project).map_err(|e| PersistError::Corrupt(e.to_string()))?;
    validate(&project)?;
    Ok(project)
}

/// Upgrades the `project` object from `version` to the current version.
fn migrate(version: u32, project: Value) -> Result<Value, PersistError> {
    match version {
        CURRENT_FORMAT_VERSION => Ok(project),
        v => Err(PersistError::Corrupt(format!("unknown format_version {v}"))),
    }
}

/// Semantic checks serde cannot express. Damaged files produce an error,
/// never a panic later on (PER-030).
fn validate(p: &Project) -> Result<(), PersistError> {
    let bad = |m: String| Err(PersistError::Corrupt(m));
    if !(jdance_domain::MIN_TEMPO_BPM..=jdance_domain::MAX_TEMPO_BPM).contains(&p.tempo_bpm) {
        return bad(format!("tempo {} out of range", p.tempo_bpm));
    }
    let mut ids: Vec<u64> = p.channels.iter().map(|c| c.id.0).collect();
    ids.extend(p.patterns.iter().map(|x| x.id.0));
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != n {
        return bad("duplicate object IDs".into());
    }
    let mut probe = p.ids.clone();
    if ids.last().is_some_and(|max| *max >= probe.next()) {
        return bad("ID counter is behind existing IDs".into());
    }
    for c in &p.channels {
        if !(c.volume.is_finite() && (0.0..=jdance_domain::MAX_VOLUME).contains(&c.volume))
            || !(c.pan.is_finite() && (-1.0..=1.0).contains(&c.pan))
        {
            return bad(format!("channel {:?} has invalid volume/pan", c.id));
        }
    }
    for pat in &p.patterns {
        if pat.length_steps == 0 || pat.length_steps > 1024 {
            return bad(format!("pattern {:?} has invalid length", pat.id));
        }
        for row in &pat.rows {
            if row.steps.len() != pat.length_steps as usize {
                return bad(format!("pattern {:?} row length mismatch", pat.id));
            }
        }
    }
    Ok(())
}

/// Resolves a sample reference against the project directory.
pub fn resolve_sample_path(project_dir: Option<&Path>, sample: &SampleRef) -> PathBuf {
    let p = PathBuf::from(&sample.path);
    match project_dir {
        Some(dir) if p.is_relative() => dir.join(p),
        _ => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{Channel, ChannelId};

    fn sample_project(sample_path: &str) -> Project {
        let mut p = Project::new();
        let id = ChannelId(p.ids.next());
        let mut ch = Channel::new_sampler(id, "Kick");
        ch.source = ChannelSource::Sampler {
            sample: Some(SampleRef {
                path: sample_path.into(),
            }),
        };
        p.channels.push(ch);
        p.patterns[0].row_mut(id).steps[0].on = true;
        p
    }

    #[test]
    fn round_trip_with_relative_samples() {
        let dir = tempfile::tempdir().unwrap();
        let canon = dir.path().canonicalize().unwrap();
        let abs = canon.join("samples").join("kick.wav");
        let p = sample_project(abs.to_str().unwrap());
        let file = canon.join("song.jdance");
        save_project(&file, &p).unwrap();
        let back = load_project(&file).unwrap();
        assert_eq!(back.channels[0].sample().unwrap().path, "samples/kick.wav");
        assert_eq!(
            resolve_sample_path(Some(&canon), back.channels[0].sample().unwrap()),
            canon.join("samples/kick.wav")
        );
        let mut expected = p.clone();
        expected.channels[0].source = back.channels[0].source.clone();
        assert_eq!(back, expected);
        // Save → load → save is byte-identical.
        let first = std::fs::read(&file).unwrap();
        save_project(&file, &back).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), first);
    }

    #[test]
    fn rejects_newer_and_foreign_files() {
        let newer = br#"{"format":"jdance-project","format_version":99,"project":{}}"#;
        assert!(matches!(
            parse_project(newer),
            Err(PersistError::NewerFormat { found: 99, .. })
        ));
        assert!(matches!(
            parse_project(br#"{"a":1}"#),
            Err(PersistError::NotAProject(_))
        ));
        assert!(matches!(
            parse_project(b"\x00\xff"),
            Err(PersistError::Corrupt(_))
        ));
    }

    #[test]
    fn rejects_semantically_broken_projects() {
        let p = sample_project("a.wav");
        let mut v = serde_json::json!({"format": FORMAT_TAG, "format_version": 1, "project": p});
        v["project"]["channels"][0]["volume"] = serde_json::json!(50.0);
        assert!(matches!(
            parse_project(v.to_string().as_bytes()),
            Err(PersistError::Corrupt(_))
        ));
        let mut v = serde_json::json!({"format": FORMAT_TAG, "format_version": 1, "project": p});
        v["project"]["patterns"][0]["rows"][0]["steps"] = serde_json::json!([]);
        assert!(parse_project(v.to_string().as_bytes()).is_err());
        let mut v = serde_json::json!({"format": FORMAT_TAG, "format_version": 1, "project": p});
        v["project"]["ids"]["next"] = serde_json::json!(1);
        assert!(parse_project(v.to_string().as_bytes()).is_err());
    }

    #[test]
    fn truncated_files_never_panic() {
        let p = sample_project("a.wav");
        let full = serde_json::to_vec(&serde_json::json!({
            "format": FORMAT_TAG, "format_version": 1, "project": p
        }))
        .unwrap();
        for n in 0..full.len() {
            let _ = parse_project(&full[..n]);
        }
    }
}
