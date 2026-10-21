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
pub const CURRENT_FORMAT_VERSION: u32 = 10;
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
    match v {
        CURRENT_FORMAT_VERSION => Ok(project),
        v => Err(PersistError::Corrupt(format!("unknown format_version {v}"))),
    }
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
            target: AutomationTarget::TrackVolume(TrackId::MASTER),
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
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jdance");
        save_project(&file, &p).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"kind\": \"track_volume\""), "{text}");
        assert!(text.contains("\"curve\": \"curve\""));
        assert_eq!(load_project(&file).unwrap(), p);
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
