// SPDX-License-Identifier: GPL-3.0-or-later
//! The interface the UI needs from the rest of the application.
//!
//! The UI never touches the engine or audio backends directly (ARCH-001);
//! `jdance-app` implements [`Host`] and does the wiring.

use jdance_audio_api::{
    ActiveStreamInfo, AudioError, BackendId, DeviceCapabilities, DeviceInfo, DeviceKey,
    DeviceSettings, StatsReading, StreamState,
};
use jdance_domain::{Project, SampleRef};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransportStatus {
    pub playing: bool,
    /// Playhead inside the pattern loop, `0.0..1.0`; `None` if unknown.
    pub loop_pos: Option<f32>,
    pub peak_load: f32,
    pub voices: u32,
    pub dropped_commands: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamStatus {
    pub info: ActiveStreamInfo,
    pub stats: StatsReading,
    pub state: StreamState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleStatus {
    Loading,
    Ready { frames: usize, source_rate: u32 },
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
}

impl Notice {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Info,
            text: text.into(),
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Warning,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Error,
            text: text.into(),
        }
    }
}

pub trait Host {
    /// Housekeeping, called once per UI frame.
    fn poll(&mut self);
    /// The document changed; recompile what the engine plays.
    fn set_project(&mut self, project: &Project, project_dir: Option<&Path>);
    fn play(&mut self);
    fn stop(&mut self);
    fn transport(&self) -> TransportStatus;
    fn stream(&self) -> Option<StreamStatus>;
    fn sample_status(&self, sample: &SampleRef, project_dir: Option<&Path>)
    -> Option<SampleStatus>;

    fn backends(&self) -> Vec<(BackendId, String)>;
    fn devices(&self, backend: &BackendId) -> Result<Vec<DeviceInfo>, AudioError>;
    fn capabilities(&self, key: &DeviceKey) -> Result<DeviceCapabilities, AudioError>;
    fn saved_settings(&self, key: &DeviceKey) -> Option<DeviceSettings>;
    fn current_device(&self) -> Option<DeviceKey>;
    fn open_device(&mut self, key: DeviceKey, settings: DeviceSettings) -> Result<(), AudioError>;
    fn close_device(&mut self);
    fn open_control_panel(&mut self, key: &DeviceKey) -> Result<(), AudioError>;

    /// Starts an offline render of `loops` pattern loops to `out`; the
    /// result arrives as a notice.
    fn export_wav(
        &mut self,
        project: &Project,
        project_dir: Option<&Path>,
        out: PathBuf,
        loops: u32,
    );
    fn take_notices(&mut self) -> Vec<Notice>;
}
