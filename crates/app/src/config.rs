// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-user settings file (never stored in projects).

use jdance_audio_api::UserAudioSettings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserSettings {
    #[serde(default)]
    pub audio: UserAudioSettings,
    /// Extra VST3 folders (PLG-020); never stored in projects.
    #[serde(default)]
    pub plugin_paths: Vec<PathBuf>,
}

/// `JDANCE_CONFIG_DIR` overrides the platform location (used by tests/CI).
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("JDANCE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home().map(|h| h.join(".config")))
    }?;
    Some(base.join("jdance"))
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILE_NAME))
}

/// Missing or unreadable settings yield defaults plus a warning message.
pub fn load(path: &Path) -> (UserSettings, Option<String>) {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(s) => (s, None),
            Err(e) => (
                UserSettings::default(),
                Some(format!(
                    "Settings file {} is damaged ({e}); using defaults",
                    path.display()
                )),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (UserSettings::default(), None),
        Err(e) => (
            UserSettings::default(),
            Some(format!("Could not read settings {}: {e}", path.display())),
        ),
    }
}

pub fn save(path: &Path, settings: &UserSettings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    jdance_persistence::write_atomically(path, &bytes).map_err(|e| e.to_string())
}
