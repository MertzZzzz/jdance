// SPDX-License-Identifier: GPL-3.0-or-later
//! Format-neutral plugin hosting types (PLG-010).
//!
//! Only the data types are defined so far; the processor/controller/editor
//! traits are added together with the VST3 implementation (Milestone P1).
//! VST3-specific types must never appear here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginFormat {
    Vst3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Instrument,
    Effect,
}

/// Identity of a plugin class, stored in projects (PLG-070). Lookup is by
/// `class_id`, never by file path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginRef {
    pub format: PluginFormat,
    /// Format-specific class identifier, hex-encoded (VST3: 16-byte FUID).
    pub class_id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
}

/// Result of a plugin scan (PLG-022).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    pub plugin: PluginRef,
    pub kind: PluginKind,
    pub category: String,
    /// Bundle or module path on this machine; not stored in projects.
    pub path: String,
    pub sdk_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ParamFlags {
    pub automatable: bool,
    pub read_only: bool,
    pub is_bypass: bool,
}

/// One plugin parameter, values normalized to `0.0..=1.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub units: String,
    pub default_normalized: f64,
    /// `0` = continuous; otherwise number of discrete steps.
    pub step_count: u32,
    pub flags: ParamFlags,
}
