// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-user audio settings, stored separately per backend/device (AUD-012).
//! These live in the user settings file, never in projects.

use crate::{BackendId, BufferRequest, DeviceId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceKey {
    pub backend: BackendId,
    pub device: DeviceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSettings {
    pub sample_rate: u32,
    pub buffer: BufferRequest,
    pub first_output_channel: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UserAudioSettings {
    /// Device used last; reopened on startup if present.
    pub last: Option<DeviceKey>,
    pub devices: Vec<(DeviceKey, DeviceSettings)>,
}

impl UserAudioSettings {
    pub fn get(&self, key: &DeviceKey) -> Option<DeviceSettings> {
        self.devices.iter().find(|(k, _)| k == key).map(|(_, s)| *s)
    }

    pub fn set(&mut self, key: DeviceKey, settings: DeviceSettings) {
        match self.devices.iter_mut().find(|(k, _)| *k == key) {
            Some((_, s)) => *s = settings,
            None => self.devices.push((key, settings)),
        }
    }
}
