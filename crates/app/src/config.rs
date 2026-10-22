// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-user settings file (never stored in projects).

use jdance_audio_api::UserAudioSettings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LibraryFolder {
    Full {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default = "yes")]
        recursive: bool,
    },
    /// Settings from 0.1.0.
    Path(PathBuf),
}

fn yes() -> bool {
    true
}

impl LibraryFolder {
    pub fn root(&self) -> jdance_ui::LibraryRoot {
        match self {
            Self::Full {
                path,
                name,
                recursive,
            } => jdance_ui::LibraryRoot {
                path: path.clone(),
                name: name.clone(),
                recursive: *recursive,
            },
            Self::Path(p) => jdance_ui::LibraryRoot::new(p.clone()),
        }
    }

    pub fn from_root(r: &jdance_ui::LibraryRoot) -> Self {
        Self::Full {
            path: r.path.clone(),
            name: r.name.clone(),
            recursive: r.recursive,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserSettings {
    #[serde(default)]
    pub audio: UserAudioSettings,
    /// Extra VST3 folders (PLG-020); never stored in projects.
    #[serde(default)]
    pub plugin_paths: Vec<PathBuf>,
    /// Plugin folders (standard or the user's) left out of scans.
    #[serde(default)]
    pub disabled_plugin_paths: Vec<PathBuf>,
    /// Sample library folders shown in the Browser (a plain path in
    /// settings written before 0.2.0).
    #[serde(default)]
    pub library_folders: Vec<LibraryFolder>,
    /// Favorite Browser folders.
    #[serde(default)]
    pub favorite_folders: Vec<PathBuf>,
    /// Pattern name presets and recent names (spec 0.2.5 §9); absent =
    /// the factory set, which an update never writes over the user's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_presets: Option<jdance_domain::name_presets::NamePresets>,
    /// Undocked panels and their windows (`dock` text format of the UI).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ui_layout: String,
    /// Recording input (monitoring is never stored: it starts off).
    #[serde(default)]
    pub recording: RecordingConfig,
    /// ASIO-025: close the ASIO driver while the window is inactive.
    #[serde(default)]
    pub release_asio_on_blur: bool,
    /// Metronome and count-in (spec 0.2.3 MET-002/020).
    #[serde(default)]
    pub metronome: MetronomeConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetronomeConfig {
    #[serde(default)]
    pub on: bool,
    #[serde(default = "default_click_db")]
    pub volume_db: f32,
    #[serde(default)]
    pub only_recording: bool,
    #[serde(default)]
    pub count_in: bool,
}

fn default_click_db() -> f32 {
    -6.0
}

impl Default for MetronomeConfig {
    fn default() -> Self {
        Self {
            on: false,
            volume_db: default_click_db(),
            only_recording: false,
            count_in: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingConfig {
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default = "first_input")]
    pub channels: jdance_audio_api::InputChannels,
    #[serde(default)]
    pub offset_ms: f32,
}

fn first_input() -> jdance_audio_api::InputChannels {
    jdance_audio_api::InputChannels::Mono(0)
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            device: None,
            channels: first_input(),
            offset_ms: 0.0,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Unit tests point the configuration directory here. Per thread, so
    /// tests running in parallel never see each other's directory.
    pub static TEST_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// `JDANCE_CONFIG_DIR` overrides the platform location (used by tests/CI).
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_DIR.with_borrow(Clone::clone) {
        return Some(dir);
    }
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

#[cfg(test)]
mod library_tests {
    use super::*;

    #[test]
    fn library_folders_from_0_1_settings_still_load() {
        let old = r#"{"library_folders": ["/samples"], "favorite_folders": []}"#;
        let s: UserSettings = serde_json::from_str(old).unwrap();
        let root = s.library_folders[0].root();
        assert_eq!(root.path, PathBuf::from("/samples"));
        assert!(root.recursive && root.name.is_none());
        let new = UserSettings {
            library_folders: vec![LibraryFolder::from_root(&jdance_ui::LibraryRoot {
                path: "/x".into(),
                name: Some("Drums".into()),
                recursive: false,
            })],
            ..s
        };
        let back: UserSettings =
            serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }
}
