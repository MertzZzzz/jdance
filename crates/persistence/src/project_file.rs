// SPDX-License-Identifier: GPL-3.0-or-later
//! Project file format (PER-001): a single JSON file.
//!
//! ```json
//! { "format": "jdance-project", "format_version": 5, "project": { … } }
//! ```
//!
//! Any schema change increments `format_version` and adds a migration step
//! in [`migrate`] with a test.

use crate::{PersistError, io_err, write_atomically};
use jdance_domain::{ChannelSource, Project, SampleRef};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const FORMAT_TAG: &str = "jdance-project";
pub const CURRENT_FORMAT_VERSION: u32 = 14;
pub const PROJECT_EXTENSION: &str = "jdance";

/// Saves `project` to `path` atomically. Sample paths inside the project
/// directory are stored relative to it (PER-003); the in-memory project is
/// not modified.
pub fn save_project(path: &Path, project: &Project) -> Result<(), PersistError> {
    let dir = path.parent().unwrap_or(Path::new(""));
    backup_older_format(path)?;
    let mut doc = project.clone();
    for a in &mut doc.assets {
        if let Some(rel) = relative_to(Path::new(&a.path), dir) {
            a.path = rel;
        }
    }
    for ch in &mut doc.channels {
        let ChannelSource::Sampler { sample: Some(s) } = &mut ch.source else {
            continue;
        };
        if let Some(rel) = relative_to(Path::new(&s.path), dir) {
            s.path = rel;
        }
    }
    // Serialize directly (not through `serde_json::Value`) so `f32` values
    // keep their shortest representation (0.7, not 0.699999988…).
    #[derive(serde::Serialize)]
    struct File<'a> {
        format: &'static str,
        format_version: u32,
        project: &'a Project,
    }
    let mut bytes = serde_json::to_vec_pretty(&File {
        format: FORMAT_TAG,
        format_version: CURRENT_FORMAT_VERSION,
        project: &doc,
    })
    .map_err(|e| PersistError::Corrupt(e.to_string()))?;
    bytes.push(b'\n');
    write_atomically(path, &bytes)
}

/// Before a file written by an older format version is overwritten for the
/// first time, keeps a copy and a manifest next to it (spec 0.2.0 §13).
fn backup_older_format(path: &Path) -> Result<(), PersistError> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(());
    };
    let version = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("format_version").and_then(Value::as_u64));
    let Some(version) = version.filter(|v| *v < u64::from(CURRENT_FORMAT_VERSION)) else {
        return Ok(());
    };
    let name = path
        .file_name()
        .map_or_else(|| "project".into(), |n| n.to_string_lossy().into_owned());
    let backup = path.with_file_name(format!("{name}.v{version}.bak"));
    if backup.exists() {
        return Ok(());
    }
    std::fs::write(&backup, &bytes).map_err(io_err(&backup))?;
    let manifest = serde_json::json!({
        "format": "jdance-backup",
        "original": name,
        "original_format_version": version,
        "backup": backup.file_name().map(|n| n.to_string_lossy().into_owned()),
        "migrated_to_format_version": CURRENT_FORMAT_VERSION,
    });
    let manifest_path = path.with_file_name(format!("{name}.backup.json"));
    let mut text =
        serde_json::to_vec_pretty(&manifest).map_err(|e| PersistError::Corrupt(e.to_string()))?;
    text.push(b'\n');
    write_atomically(&manifest_path, &text)
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
    let mut project: Project =
        serde_json::from_value(project).map_err(|e| PersistError::Corrupt(e.to_string()))?;
    // Notes from before 0.2.0 have no IDs.
    for pat in &mut project.patterns {
        for row in &mut pat.rows {
            jdance_domain::assign_note_ids(&mut row.notes);
        }
    }
    // Parameters added in a newer schema of an effect take defaults.
    for t in std::iter::once(&mut project.mixer.master).chain(project.mixer.tracks.iter_mut()) {
        for i in &mut t.inserts {
            i.effect.complete();
        }
    }
    validate(&project)?;
    Ok(project)
}

/// Upgrades the `project` object from `version` to the current version.
fn migrate(version: u32, mut project: Value) -> Result<Value, PersistError> {
    let mut v = version;
    if v == 1 {
        project = v1_to_v2(project)?;
        v = 2;
    }
    if v == 2 {
        project = v2_to_v3(project)?;
        v = 3;
    }
    if v == 3 {
        project = v3_to_v4(project)?;
        v = 4;
    }
    if v == 4 {
        project = v4_to_v5(project)?;
        v = 5;
    }
    if v == 5 {
        // v6 added plugin inserts (`kind: "plugin"` with a `plugin` slot);
        // v5 files never contain them, so nothing changes.
        v = 6;
    }
    if v == 6 {
        // v7 added piano-roll notes to step rows (absent = none).
        v = 7;
    }
    if v == 7 {
        // v8 added instrument channels (`source.kind: "plugin"`).
        v = 8;
    }
    if v == 8 {
        // v9 added automation lanes (absent = none).
        v = 9;
    }
    if v == 9 {
        // v10 added optional channel and mixer track colors (absent =
        // palette by position).
        v = 10;
    }
    if v == 10 {
        project = v10_to_v11(project)?;
        v = 11;
    }
    if v == 11 {
        // The audio shape changes independently of automation: convert it
        // first, so the v12 step reads the current types.
        project = v12_to_v13(project)?;
        project = v11_to_v12(project)?;
        v = 13;
    }
    if v == 12 {
        project = v12_to_v13(project)?;
        v = 13;
    }
    if v == 13 {
        // v14 (0.2.3) added the time signature (absent = 4/4) and mixer
        // track inputs (absent = none).
        v = 14;
    }
    match v {
        CURRENT_FORMAT_VERSION => Ok(project),
        v => Err(PersistError::Corrupt(format!("unknown format_version {v}"))),
    }
}

/// v11 (0.2.0): pattern clips store `offset`, `duration` and `looped`
/// instead of a repeat count; audio assets, audio clips, lane audio routing
/// and the timebase are new (absent = none / defaults). Playback stays the
/// same: `duration = repeat × pattern length`, looped.
fn v10_to_v11(mut project: Value) -> Result<Value, PersistError> {
    let corrupt = |m: &str| PersistError::Corrupt(format!("v10 project: {m}"));
    let lengths: std::collections::BTreeMap<u64, u64> = project
        .get("patterns")
        .and_then(Value::as_array)
        .ok_or_else(|| corrupt("no patterns"))?
        .iter()
        .filter_map(|p| {
            let id = p.get("id")?.as_u64()?;
            let steps = p.get("length_steps")?.as_u64()?;
            Some((id, steps * u64::from(jdance_domain::TICKS_PER_STEP)))
        })
        .collect();
    let clips = project
        .get_mut("playlist")
        .and_then(|p| p.get_mut("clips"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| corrupt("no playlist clips"))?;
    for c in clips {
        let obj = c.as_object_mut().ok_or_else(|| corrupt("clip"))?;
        let repeat = obj
            .remove("repeat")
            .and_then(|r| r.as_u64())
            .ok_or_else(|| corrupt("clip without repeat"))?;
        let pattern = obj
            .get("pattern")
            .and_then(Value::as_u64)
            .ok_or_else(|| corrupt("clip without pattern"))?;
        let len = lengths
            .get(&pattern)
            .copied()
            .ok_or_else(|| corrupt("clip of a missing pattern"))?;
        obj.insert("offset".into(), 0.into());
        obj.insert("duration".into(), (repeat * len).into());
        obj.insert("looped".into(), true.into());
    }
    Ok(project)
}

/// v13 (0.2.2, spec ACH-061): audio clips become instances of audio
/// channels. One channel per (file, mixer track of the clip's lane), in
/// order of first appearance on the timeline, takes over the lane's route;
/// lanes lose `audio_track`. Unity volume and centered balance keep the
/// sound unchanged (ACH-063). Deterministic for a given file.
fn v12_to_v13(mut project: Value) -> Result<Value, PersistError> {
    use jdance_domain::{AssetId, Channel, ChannelId, TrackId};
    let corrupt = |m: &str| PersistError::Corrupt(format!("v12 project: {m}"));
    let u = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
    // Lane → mixer track of its audio clips (absent = master).
    let mut lane_track = std::collections::BTreeMap::new();
    if let Some(lanes) = project
        .get_mut("playlist")
        .and_then(|p| p.get_mut("lanes"))
        .and_then(Value::as_array_mut)
    {
        for l in lanes {
            let track = l
                .as_object_mut()
                .and_then(|o| o.remove("audio_track"))
                .and_then(|t| t.as_u64())
                .unwrap_or(TrackId::MASTER.0);
            if let Some(id) = u(l, "id") {
                lane_track.insert(id, track);
            }
        }
    }
    let asset_names: std::collections::BTreeMap<u64, String> = project
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| Some((u(a, "id")?, a.get("name")?.as_str()?.to_owned())))
        .collect();
    let track_names: std::collections::BTreeMap<u64, String> = project
        .get("mixer")
        .into_iter()
        .flat_map(|m| {
            m.get("master").into_iter().chain(
                m.get("tracks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
        })
        .filter_map(|t| Some((u(t, "id")?, t.get("name")?.as_str()?.to_owned())))
        .collect();
    let Some(clips) = project
        .get("playlist")
        .and_then(|p| p.get("audio_clips"))
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
        .cloned()
    else {
        return Ok(project);
    };
    // (asset, track) pairs in timeline order of their first clip.
    let mut order: Vec<(u64, u64, u64, usize)> = Vec::new(); // (start, id, asset, index)
    let mut keys = Vec::with_capacity(clips.len());
    for (i, c) in clips.iter().enumerate() {
        let asset = u(c, "asset").ok_or_else(|| corrupt("audio clip without a file"))?;
        let lane = u(c, "lane").ok_or_else(|| corrupt("audio clip without a lane"))?;
        let track = lane_track.get(&lane).copied().unwrap_or(TrackId::MASTER.0);
        keys.push((asset, track));
        order.push((
            u(c, "start").unwrap_or(0),
            u(c, "id").unwrap_or(0),
            asset,
            i,
        ));
    }
    order.sort_unstable();
    let mut pairs: Vec<(u64, u64)> = Vec::new();
    for (_, _, _, i) in &order {
        if !pairs.contains(&keys[*i]) {
            pairs.push(keys[*i]);
        }
    }
    let mut next = project
        .get("ids")
        .and_then(|i| u(i, "next"))
        .ok_or_else(|| corrupt("no ID allocator"))?;
    let mut channels = Vec::with_capacity(pairs.len());
    let mut channel_of = std::collections::BTreeMap::new();
    for &(asset, track) in &pairs {
        let base = asset_names
            .get(&asset)
            .cloned()
            .unwrap_or_else(|| format!("Audio {asset}"));
        let shared = pairs.iter().filter(|(a, _)| *a == asset).count() > 1;
        let name = if shared {
            let t = track_names.get(&track).map_or("?", String::as_str);
            format!("{base} → {t}")
        } else {
            base
        };
        let id = ChannelId(next);
        next += 1;
        let mut ch = Channel::new_audio(id, name, AssetId(asset));
        ch.track = TrackId(track);
        channels.push(serde_json::to_value(&ch).map_err(|e| corrupt(&e.to_string()))?);
        channel_of.insert((asset, track), id.0);
    }
    let list = project
        .get_mut("channels")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| corrupt("no channels"))?;
    list.extend(channels);
    let clips = project
        .get_mut("playlist")
        .and_then(|p| p.get_mut("audio_clips"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| corrupt("no audio clips"))?;
    for (c, key) in clips.iter_mut().zip(&keys) {
        let obj = c.as_object_mut().ok_or_else(|| corrupt("audio clip"))?;
        obj.remove("asset");
        obj.insert("channel".into(), channel_of[key].into());
    }
    project["ids"]["next"] = next.into();
    Ok(project)
}

/// v12 (0.2.0): automation lanes on the song timeline become sources
/// placed by automation clips (plan D1). The v11 shape reads as v12 with
/// sources of length 0, which the domain turns into a clip each.
fn v11_to_v12(project: Value) -> Result<Value, PersistError> {
    let mut p: Project = serde_json::from_value(project)
        .map_err(|e| PersistError::Corrupt(format!("v11 project: {e}")))?;
    p.adopt_timeline_automation();
    serde_json::to_value(&p).map_err(|e| PersistError::Corrupt(e.to_string()))
}

/// v5 added the playlist (lanes, pattern clips, loop). Older projects get
/// the default lanes, with IDs taken from the project's ID counter.
fn v4_to_v5(mut project: Value) -> Result<Value, PersistError> {
    let corrupt = |m: &str| PersistError::Corrupt(format!("v4 project: {m}"));
    let obj = project
        .as_object_mut()
        .ok_or_else(|| corrupt("not an object"))?;
    let next = obj
        .get("ids")
        .and_then(|i| i.get("next"))
        .and_then(Value::as_u64)
        .ok_or_else(|| corrupt("missing ID counter"))?;
    let lanes: Vec<Value> = (0..jdance_domain::DEFAULT_LANES)
        .map(|i| {
            let c = jdance_domain::LANE_COLORS[i % jdance_domain::LANE_COLORS.len()];
            serde_json::json!({
                "id": next + i as u64,
                "name": format!("Lane {}", i + 1),
                "color": c,
                "muted": false,
                "solo": false,
            })
        })
        .collect();
    obj.insert(
        "playlist".into(),
        serde_json::json!({"lanes": lanes, "clips": [], "loop_range": null, "loop_enabled": false}),
    );
    obj.insert(
        "ids".into(),
        serde_json::json!({"next": next + jdance_domain::DEFAULT_LANES as u64}),
    );
    Ok(project)
}

/// v4 stores effects as `{kind, schema, params{id: value}}` with named
/// parameters (spec "Built-in Effects") and adds the master limiter.
fn v3_to_v4(mut project: Value) -> Result<Value, PersistError> {
    let corrupt = |m: &str| PersistError::Corrupt(format!("v3 project: {m}"));
    let mixer = project
        .get_mut("mixer")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| corrupt("missing mixer"))?;
    mixer.insert(
        "limiter".into(),
        serde_json::json!({"enabled": false, "ceiling_db": -0.3, "release_ms": 80.0}),
    );
    let mut tracks: Vec<&mut Value> = Vec::new();
    let (master, rest): (Option<&mut Value>, Option<&mut Value>) = {
        let mut master = None;
        let mut rest = None;
        for (k, v) in mixer.iter_mut() {
            match k.as_str() {
                "master" => master = Some(v),
                "tracks" => rest = Some(v),
                _ => {}
            }
        }
        (master, rest)
    };
    tracks.extend(master);
    if let Some(Value::Array(list)) = rest {
        tracks.extend(list.iter_mut());
    }
    // v3 compressor fields → v4 parameter IDs.
    const RENAME: [(&str, &str); 6] = [
        ("threshold_db", "threshold"),
        ("ratio", "ratio"),
        ("attack_ms", "attack"),
        ("release_ms", "release"),
        ("makeup_db", "makeup"),
        ("lookahead_ms", "lookahead"),
    ];
    for t in tracks {
        let Some(inserts) = t.get_mut("inserts").and_then(Value::as_array_mut) else {
            continue;
        };
        for ins in inserts {
            let effect = ins
                .get_mut("effect")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| corrupt("bad insert"))?;
            if effect.get("kind").and_then(Value::as_str) != Some("compressor") {
                return Err(corrupt("unknown effect kind"));
            }
            let mut params = serde_json::Map::new();
            for (old, new) in RENAME {
                if let Some(v) = effect.remove(old) {
                    params.insert(new.into(), v);
                }
            }
            effect.insert("schema".into(), Value::from(1));
            effect.insert("params".into(), Value::Object(params));
        }
    }
    Ok(project)
}

/// v3 added solo on mixer tracks (MIX-012); nothing was soloed before.
fn v2_to_v3(mut project: Value) -> Result<Value, PersistError> {
    let corrupt = |m: &str| PersistError::Corrupt(format!("v2 project: {m}"));
    let mixer = project
        .get_mut("mixer")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| corrupt("missing mixer"))?;
    if let Some(master) = mixer.get_mut("master").and_then(Value::as_object_mut) {
        master.insert("solo".into(), Value::Bool(false));
    }
    if let Some(tracks) = mixer.get_mut("tracks").and_then(Value::as_array_mut) {
        for t in tracks {
            t.as_object_mut()
                .ok_or_else(|| corrupt("bad track"))?
                .insert("solo".into(), Value::Bool(false));
        }
    }
    Ok(project)
}

/// v2 added mixer tracks (MIX-001): every channel plays into a track, and
/// tracks have a main out, sends and inserts. v1 projects route all
/// channels to the master, which is what v1 played.
fn v1_to_v2(mut project: Value) -> Result<Value, PersistError> {
    let corrupt = |m: &str| PersistError::Corrupt(format!("v1 project: {m}"));
    let obj = project
        .as_object_mut()
        .ok_or_else(|| corrupt("not an object"))?;
    if let Some(channels) = obj.get_mut("channels").and_then(Value::as_array_mut) {
        for c in channels {
            let c = c.as_object_mut().ok_or_else(|| corrupt("bad channel"))?;
            c.insert("track".into(), Value::from(0));
        }
    }
    let mixer = obj
        .get_mut("mixer")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| corrupt("missing mixer"))?;
    mixer.insert("tracks".into(), Value::Array(Vec::new()));
    let master = mixer
        .get_mut("master")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| corrupt("missing master"))?;
    master.insert("main_out".into(), Value::from(0));
    master.insert("sends".into(), Value::Array(Vec::new()));
    master.insert("inserts".into(), Value::Array(Vec::new()));
    Ok(project)
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
    for t in &p.mixer.tracks {
        ids.push(t.id.0);
    }
    for t in p.mixer.all_tracks() {
        ids.extend(t.sends.iter().map(|s| s.id.0));
        ids.extend(t.inserts.iter().map(|i| i.id.0));
    }
    ids.extend(p.playlist.lanes.iter().map(|l| l.id.0));
    ids.extend(p.playlist.clips.iter().map(|c| c.id.0));
    ids.extend(p.automation.iter().map(|l| l.id.0));
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
        if c.pitch.abs() > jdance_domain::MAX_CHANNEL_PITCH
            || c.polyphony > jdance_domain::MAX_POLYPHONY
        {
            return bad(format!("channel {:?} has invalid pitch/polyphony", c.id));
        }
        c.sampler
            .validate()
            .map_err(|e| PersistError::Corrupt(format!("channel {:?}: {e}", c.id)))?;
    }
    if p.channels.len() > jdance_domain::MAX_CHANNELS {
        return bad("too many channels".into());
    }
    if let Err(e) = p.validate_routing() {
        return bad(format!("mixer routing: {e}"));
    }
    if let Err(e) = p.validate_automation() {
        return bad(format!("automation: {e}"));
    }
    if let Err(e) = p.validate_arrangement() {
        return bad(format!("arrangement: {e}"));
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
    fn automation_round_trips_in_the_spec_format() {
        use jdance_domain::{
            AutoPoint, AutomationId, AutomationLane, AutomationTarget, Curve, TrackId,
        };
        let mut p = sample_project("a.wav");
        let id = AutomationId(p.ids.next());
        p.automation.push(AutomationLane {
            id,
            name: "Master volume".into(),
            target: AutomationTarget::TrackVolume(TrackId::MASTER),
            length: 7680,
            points: vec![
                AutoPoint {
                    tick: 0,
                    value: 0.8,
                    curve: Curve::Curve,
                    tension: 0.5,
                },
                AutoPoint {
                    tick: 3840,
                    value: 0.25,
                    curve: Curve::Hold,
                    tension: 0.0,
                },
            ],
        });
        let lane = p.playlist.lanes[0].id;
        p.playlist
            .automation_clips
            .push(jdance_domain::AutomationClip {
                id: jdance_domain::ClipId(p.ids.next()),
                lane,
                source: id,
                start: 960,
                offset: 480,
                duration: 3840,
                muted: false,
            });
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jdance");
        save_project(&file, &p).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"kind\": \"track_volume\""), "{text}");
        assert!(text.contains("\"curve\": \"curve\""));
        assert_eq!(load_project(&file).unwrap(), p);
    }

    #[test]
    fn channel_settings_round_trip_and_are_validated() {
        let mut p = sample_project("a.wav");
        let ch = &mut p.channels[0];
        ch.color = Some([1, 2, 3]);
        ch.pitch = -7;
        ch.polyphony = 4;
        ch.mono = true;
        ch.sampler.start = 0.1;
        ch.sampler.mode = jdance_domain::PlaybackMode::Loop;
        ch.sampler.envelope = true;
        ch.sampler.root_key = 57;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jdance");
        save_project(&file, &p).unwrap();
        assert_eq!(load_project(&file).unwrap(), p);
        // Defaults are not written (older files stay small and readable).
        let plain = sample_project("a.wav");
        save_project(&file, &plain).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            !text.contains("\"sampler\":") && !text.contains("polyphony"),
            "{text}"
        );
        // Broken markers are rejected on load.
        let bad = text.replace(
            "\"muted\": false",
            "\"muted\": false, \"sampler\": {\"start\": 0.9, \"end\": 0.1}",
        );
        std::fs::write(&file, bad).unwrap();
        assert!(load_project(&file).is_err());
    }

    #[test]
    fn plugin_state_round_trips_unchanged() {
        use jdance_domain::{Effect, Insert, InsertId, PluginSlot};
        let mut p = sample_project("a.wav");
        let id = InsertId(p.ids.next());
        let state: Vec<u8> = (0..=255).cycle().take(1000).collect();
        p.mixer.master.inserts.push(Insert {
            id,
            bypass: true,
            effect: Effect::plugin(PluginSlot {
                format: "vst3".into(),
                class_id: "0123456789ABCDEF0123456789ABCDEF".into(),
                name: "Missing Plugin".into(),
                vendor: "Nobody".into(),
                version: "2.1".into(),
                category: "Fx".into(),
                component_state: state.clone(),
                controller_state: vec![7],
            }),
            sidechain: None,
        });
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.jdance");
        save_project(&file, &p).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"kind\": \"plugin\""), "{text}");
        let back = load_project(&file).unwrap();
        assert_eq!(back, p);
        let slot = back.mixer.master.inserts[0].effect.plugin.as_ref().unwrap();
        assert_eq!(slot.component_state, state);
        save_project(&file, &back).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            text,
            "byte-identical"
        );
    }

    #[test]
    fn v13_projects_are_in_four_four_and_signatures_round_trip() {
        let p = sample_project("a.wav");
        let mut v = serde_json::json!({"format": FORMAT_TAG, "format_version": 13, "project": p});
        v["project"]
            .as_object_mut()
            .unwrap()
            .remove("time_signature");
        let loaded = parse_project(v.to_string().as_bytes()).unwrap();
        assert_eq!(loaded.time_signature, jdance_domain::TimeSignature::COMMON);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sig.jdance");
        let mut p = sample_project("a.wav");
        p.time_signature = jdance_domain::TimeSignature::new(7, 8);
        save_project(&path, &p).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"format_version\": 14"), "{text}");
        assert_eq!(
            load_project(&path).unwrap().time_signature,
            p.time_signature
        );

        let mut v = serde_json::json!({"format": FORMAT_TAG, "format_version": 14, "project": p});
        v["project"]["time_signature"]["denominator"] = serde_json::json!(3);
        assert!(parse_project(v.to_string().as_bytes()).is_err(), "SIG-001");
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
        let mut v = serde_json::json!({"format": FORMAT_TAG, "format_version": 9, "project": p});
        v["project"]["channels"][0]["volume"] = serde_json::json!(50.0);
        assert!(matches!(
            parse_project(v.to_string().as_bytes()),
            Err(PersistError::Corrupt(_))
        ));
        let mut v = serde_json::json!({"format": FORMAT_TAG, "format_version": 9, "project": p});
        v["project"]["patterns"][0]["rows"][0]["steps"] = serde_json::json!([]);
        assert!(parse_project(v.to_string().as_bytes()).is_err());
        let mut v = serde_json::json!({"format": FORMAT_TAG, "format_version": 9, "project": p});
        v["project"]["ids"]["next"] = serde_json::json!(1);
        assert!(parse_project(v.to_string().as_bytes()).is_err());
    }

    #[test]
    fn v10_repeat_counts_become_looped_durations_and_old_files_are_backed_up() {
        let mut p = Project::new();
        let lane = p.playlist.lanes[0].id;
        let pat = p.patterns[0].clone();
        p.playlist.clips.push(jdance_domain::PatternClip::repeating(
            jdance_domain::ClipId(p.ids.next()),
            lane,
            &pat,
            3840,
            3,
        ));
        // The same project as format version 10 wrote it.
        let mut v = serde_json::to_value(&p).unwrap();
        let clip = &mut v["playlist"]["clips"][0];
        let obj = clip.as_object_mut().unwrap();
        for k in ["offset", "duration", "looped"] {
            obj.remove(k);
        }
        obj.insert("repeat".into(), 3.into());
        for l in v["playlist"]["lanes"].as_array_mut().unwrap() {
            l.as_object_mut().unwrap().remove("audio_track");
        }
        v.as_object_mut().unwrap().remove("timebase_rate");
        let file = serde_json::json!({"format": FORMAT_TAG, "format_version": 10, "project": v});
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.jdance");
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let loaded = load_project(&path).unwrap();
        assert_eq!(loaded, p, "same playback: 3 repetitions, looped");
        assert_eq!(loaded.timebase_rate, jdance_domain::DEFAULT_TIMEBASE);
        save_project(&path, &loaded).unwrap();
        let backup = dir.path().join("old.jdance.v10.bak");
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            serde_json::to_vec(&file).unwrap()
        );
        assert!(dir.path().join("old.jdance.backup.json").exists());
        // Saving again keeps the first backup.
        save_project(&path, &loaded).unwrap();
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            serde_json::to_vec(&file).unwrap()
        );
    }

    /// A v12 file: two lanes routed to different tracks, one file on
    /// both, another file on one (spec 0.2.2 ACH-061).
    fn v12_audio_file() -> Value {
        use jdance_domain::{AssetId, AudioAsset, MixerTrack, TrackId};
        let mut p = Project::new();
        let t = TrackId(p.ids.next());
        p.mixer.tracks.push(MixerTrack::new(t, "Drums"));
        let asset = |id: u64, name: &str| AudioAsset {
            id: AssetId(id),
            name: name.into(),
            path: format!("assets/{name}.wav"),
            hash: id,
            size: 10,
            sample_rate: 48_000,
            channels: 2,
            frames: 4_800,
        };
        let (a, b) = (p.ids.next(), p.ids.next());
        p.assets = vec![asset(a, "loop"), asset(b, "hit")];
        let (l1, l2) = (p.playlist.lanes[0].id.0, p.playlist.lanes[1].id.0);
        let mut v = serde_json::to_value(&p).unwrap();
        v["playlist"]["lanes"][0]["audio_track"] = t.0.into();
        v["playlist"]["lanes"][1]["audio_track"] = 0.into();
        let clip = |id: u64, lane: u64, asset: u64, start: u64| {
            serde_json::json!({"id": id, "lane": lane, "asset": asset, "start": start,
                "source_in": 0, "source_out": 4_800, "gain": 0.5, "muted": false})
        };
        let ids: Vec<u64> = (0..4).map(|_| p.ids.next()).collect();
        v["playlist"]["audio_clips"] = serde_json::json!([
            clip(ids[0], l2, a, 9_000),
            clip(ids[1], l1, a, 0),
            clip(ids[2], l1, b, 4_000),
            clip(ids[3], l1, a, 20_000),
        ]);
        v["ids"]["next"] = p.ids.next().into();
        serde_json::json!({"format": FORMAT_TAG, "format_version": 12, "project": v})
    }

    #[test]
    fn v12_audio_clips_get_channels_with_the_lane_routes() {
        let file = v12_audio_file();
        let bytes = serde_json::to_vec(&file).unwrap();
        let p = parse_project(&bytes).unwrap();
        let names: Vec<(&str, u64)> = p
            .channels
            .iter()
            .map(|c| (c.name.as_str(), c.track.0))
            .collect();
        let drums = p.mixer.tracks[0].id.0;
        assert_eq!(
            names,
            [
                ("loop → Drums", drums),
                ("hit", drums),
                ("loop → Master", 0)
            ],
            "timeline order of first appearance"
        );
        assert!(p.channels.iter().all(|c| c.volume == 1.0 && c.pan == 0.0));
        let clips = &p.playlist.audio_clips;
        let ch = |i: usize| p.channel(clips[i].channel).unwrap().name.clone();
        assert_eq!(ch(0), "loop → Master");
        assert_eq!(ch(1), "loop → Drums");
        assert_eq!(ch(3), "loop → Drums", "same file and route share a channel");
        assert_eq!(ch(2), "hit");
        assert!(clips.iter().all(|c| c.gain == 0.5));
        // New IDs do not collide with existing ones; the result is stable.
        let mut all: Vec<u64> = p.channels.iter().map(|c| c.id.0).collect();
        all.extend(clips.iter().map(|c| c.id.0));
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), p.channels.len() + clips.len());
        assert_eq!(parse_project(&bytes).unwrap(), p, "deterministic");
        // It saves and loads in the current format.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.jdance");
        save_project(&path, &p).unwrap();
        assert_eq!(load_project(&path).unwrap(), p);
    }

    #[test]
    fn v13_rejects_clips_without_an_audio_channel() {
        let file = v12_audio_file();
        let mut p: Value = file["project"].clone();
        p = migrate(12, p).unwrap();
        p["playlist"]["audio_clips"][0]["channel"] = 999.into();
        let bad = serde_json::json!({"format": FORMAT_TAG, "format_version": 13, "project": p});
        let err = parse_project(&serde_json::to_vec(&bad).unwrap()).unwrap_err();
        assert!(err.to_string().contains("no audio channel"), "{err}");
    }

    #[test]
    fn v11_timeline_automation_becomes_a_source_and_a_clip() {
        let mut p = Project::new();
        let lane = p.playlist.lanes[0].id;
        let pat = p.patterns[0].clone();
        p.playlist.clips.push(jdance_domain::PatternClip::repeating(
            jdance_domain::ClipId(p.ids.next()),
            lane,
            &pat,
            0,
            2,
        ));
        let id = jdance_domain::AutomationId(p.ids.next());
        let point = |tick, value| jdance_domain::AutoPoint {
            tick,
            value,
            curve: jdance_domain::Curve::Linear,
            tension: 0.0,
        };
        p.automation.push(jdance_domain::AutomationLane {
            id,
            name: String::new(),
            target: jdance_domain::AutomationTarget::TrackVolume(jdance_domain::TrackId::MASTER),
            length: 0,
            points: vec![point(0, 0.2), point(960, 0.8)],
        });
        let mut v = serde_json::to_value(&p).unwrap();
        for a in v["automation"].as_array_mut().unwrap() {
            let a = a.as_object_mut().unwrap();
            a.remove("name");
            a.remove("length");
        }
        let file = serde_json::json!({"format": FORMAT_TAG, "format_version": 11, "project": v});
        let loaded = parse_project(&serde_json::to_vec(&file).unwrap()).unwrap();
        let source = &loaded.automation[0];
        let bar = 4 * u64::from(jdance_domain::PPQ);
        assert_eq!(source.length, 2 * 3840 + bar, "arrangement end plus a bar");
        assert_eq!(source.points, p.automation[0].points, "points unchanged");
        assert_eq!(source.name, "Automation 1");
        let clip = &loaded.playlist.automation_clips[0];
        assert_eq!(
            (clip.source, clip.start, clip.offset, clip.duration),
            (id, 0, 0, source.length)
        );
        let new_lane = loaded.playlist.lane(clip.lane).unwrap();
        assert_eq!(new_lane.name, "Automation 1");
        assert_eq!(loaded.playlist.lanes.len(), p.playlist.lanes.len() + 1);
        assert_eq!(
            loaded.arrangement_end_ticks(),
            2 * 3840,
            "song length unchanged"
        );
    }

    #[test]
    fn migrates_v1_projects() {
        // A project as written by format version 1 (before mixer tracks).
        let v1 = br#"{
          "format": "jdance-project",
          "format_version": 1,
          "project": {
            "tempo_bpm": 128.0,
            "channels": [{"id": 2, "name": "Kick",
                          "source": {"kind": "sampler", "sample": {"path": "k.wav"}},
                          "volume": 0.8, "pan": 0.0, "muted": false}],
            "patterns": [{"id": 1, "name": "Pattern 1", "length_steps": 16, "rows": []}],
            "mixer": {"master": {"id": 0, "name": "Master", "volume": 1.0, "pan": 0.0, "muted": false}},
            "ids": {"next": 3}
          }
        }"#;
        let p = parse_project(v1).unwrap();
        assert_eq!(p.tempo_bpm, 128.0);
        assert_eq!(p.playlist.lanes.len(), jdance_domain::DEFAULT_LANES);
        assert!(
            p.playlist.lanes.iter().all(|l| l.id.0 >= 3),
            "lane IDs after existing IDs"
        );
        assert_eq!(p.channels[0].track, jdance_domain::TrackId::MASTER);
        assert!(p.mixer.tracks.is_empty());
        assert!(p.mixer.master.sends.is_empty());
    }

    #[test]
    fn migrates_v3_compressor_to_named_parameters() {
        let v3 = br#"{
          "format": "jdance-project",
          "format_version": 3,
          "project": {
            "tempo_bpm": 120.0, "channels": [],
            "patterns": [{"id": 1, "name": "P", "length_steps": 16, "rows": []}],
            "mixer": {
              "master": {"id": 0, "name": "Master", "volume": 1.0, "pan": 0.0, "muted": false,
                         "solo": false, "main_out": 0, "sends": [], "inserts": []},
              "tracks": [{"id": 2, "name": "T", "volume": 1.0, "pan": 0.0, "muted": false,
                          "solo": false, "main_out": 0, "sends": [],
                          "inserts": [{"id": 3, "bypass": true, "sidechain": null,
                                       "effect": {"kind": "compressor", "threshold_db": -30.0,
                                                  "ratio": 6.0, "attack_ms": 1.0, "release_ms": 150.0,
                                                  "makeup_db": 2.0, "lookahead_ms": 0.5}}]}]
            },
            "ids": {"next": 4}
          }
        }"#;
        let p = parse_project(v3).unwrap();
        let ins = &p.mixer.tracks[0].inserts[0];
        assert!(ins.bypass);
        assert_eq!(ins.effect.kind, jdance_domain::EffectKind::Compressor);
        assert_eq!(ins.effect.get("threshold"), -30.0);
        assert_eq!(ins.effect.get("lookahead"), 0.5);
        assert!(!p.mixer.limiter.enabled);
    }

    #[test]
    fn effect_params_are_saved_by_name() {
        use jdance_domain::{Effect, EffectKind, Insert, InsertId, MixerTrack, TrackId};
        let mut p = sample_project("a.wav");
        let mut t = MixerTrack::new(TrackId(p.ids.next()), "FX");
        t.inserts.push(Insert {
            id: InsertId(p.ids.next()),
            bypass: false,
            effect: Effect::new(EffectKind::TempoDelay).with("feedback", 0.7),
            sidechain: None,
        });
        p.mixer.tracks.push(t);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fx.jdance");
        save_project(&file, &p).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"feedback\": 0.7"));
        assert!(text.contains("\"kind\": \"tempo_delay\""));
        assert!(!text.contains("buf"), "no runtime buffers in the project");
        assert_eq!(load_project(&file).unwrap(), p);
    }

    #[test]
    fn rejects_cyclic_routing() {
        use jdance_domain::{MixerTrack, Send, SendId, TrackId};
        let mut p = sample_project("a.wav");
        let mut a = MixerTrack::new(TrackId(p.ids.next()), "A");
        let b_id = TrackId(p.ids.next());
        a.main_out = b_id;
        let mut b = MixerTrack::new(b_id, "B");
        b.sends.push(Send {
            id: SendId(p.ids.next()),
            dest: a.id,
            level: 1.0,
            pre_fader: false,
        });
        p.mixer.tracks = vec![a, b];
        let v = serde_json::json!({"format": FORMAT_TAG, "format_version": 9, "project": p});
        let err = parse_project(v.to_string().as_bytes()).unwrap_err();
        assert!(err.to_string().contains("feedback loop"), "{err}");
    }

    #[test]
    fn truncated_files_never_panic() {
        let p = sample_project("a.wav");
        let full = serde_json::to_vec(&serde_json::json!({
            "format": FORMAT_TAG, "format_version": 5, "project": p
        }))
        .unwrap();
        for n in 0..full.len() {
            let _ = parse_project(&full[..n]);
        }
    }
}
