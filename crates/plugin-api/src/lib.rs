// SPDX-License-Identifier: GPL-3.0-or-later
//! Format-neutral plugin hosting types (PLG-010).
//!
//! The realtime part ([`PluginProcessor`]) lives in the engine; the
//! non-realtime part ([`PluginController`]) lives on the UI thread
//! (PLG-011). VST3-specific types must never appear here.

use serde::{Deserialize, Serialize};

pub mod search;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginFormat {
    Vst3,
}

impl PluginFormat {
    pub const ALL: [Self; 1] = [Self::Vst3];

    pub fn label(self) -> &'static str {
        match self {
            Self::Vst3 => "VST3",
        }
    }
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

/// Processing configuration fixed at activation (PLG-044).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessSetupInfo {
    pub sample_rate: f64,
    /// Largest block passed to [`PluginProcessor::process`]; longer
    /// blocks are split by the host.
    pub max_block: usize,
    /// Offline render (PLG-045).
    pub offline: bool,
}

/// Transport state for one block (PLG-034).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transport {
    pub playing: bool,
    pub tempo_bpm: f64,
    /// Project time signature (spec 0.2.3 SIG-030).
    pub time_sig: (u8, u8),
    /// Position of the block start on the timeline.
    pub project_time_samples: i64,
    /// The same position in quarter notes.
    pub project_time_quarters: f64,
    /// Loop range in quarter notes when cycling.
    pub cycle: Option<(f64, f64)>,
}

impl Default for Transport {
    fn default() -> Self {
        Self {
            playing: false,
            tempo_bpm: 120.0,
            time_sig: (4, 4),
            project_time_samples: 0,
            project_time_quarters: 0.0,
            cycle: None,
        }
    }
}

impl Transport {
    /// Start of the bar holding `quarters`, in quarter notes.
    pub fn bar_start_quarters(&self, quarters: f64) -> f64 {
        let (n, d) = self.time_sig;
        let bar = f64::from(n.max(1)) * 4.0 / f64::from(d.max(1));
        (quarters / bar).floor() * bar
    }
}

/// Saved plugin state blobs (PLG-070).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PluginState {
    pub component: Vec<u8>,
    pub controller: Vec<u8>,
}

/// Notifications from a plugin to the host, delivered on the UI thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PluginEvent {
    /// The plugin's own GUI or the processor changed a parameter.
    ParamChanged { id: u32, value: f64 },
    /// The latency changed; the host must re-read it (PLG-035, PLG-051).
    LatencyChanged,
    /// Parameter values or titles changed; refresh the generic editor.
    ParamsChanged,
    /// The bus configuration changed or the plugin asks to be reloaded.
    ReloadRequested,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    Load(String),
    NotFound(String),
    Unsupported(String),
    Failed(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::Load(e) => write!(f, "could not load plugin: {e}"),
            PluginError::NotFound(e) => write!(f, "plugin not found: {e}"),
            PluginError::Unsupported(e) => write!(f, "unsupported plugin: {e}"),
            PluginError::Failed(e) => write!(f, "plugin call failed: {e}"),
        }
    }
}

impl std::error::Error for PluginError {}

/// A note event for an instrument, at a sample offset inside the block
/// (PLG-033).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteEvent {
    pub offset: u32,
    /// Pairs a note-on with its note-off.
    pub note_id: i32,
    pub key: u8,
    pub kind: NoteKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoteKind {
    /// Velocity `0.0..=1.0`.
    On {
        velocity: f32,
    },
    Off,
}

/// Realtime part of a plugin instance, owned by the audio thread.
///
/// Implementations do not allocate in [`PluginProcessor::process`]; the
/// plugin's own code may (PLG-040).
pub trait PluginProcessor: Send {
    /// Processes `left`/`right` in place. `sidechain` feeds the plugin's
    /// sidechain input if it has one; otherwise it is ignored.
    fn process(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        sidechain: Option<(&[f32], &[f32])>,
        transport: &Transport,
    );
    /// Renders an instrument block into `left`/`right` (overwritten) with
    /// `events` sorted by offset. Effects ignore the events and output
    /// silence here.
    fn process_instrument(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        events: &[NoteEvent],
        transport: &Transport,
    ) {
        let _ = (events, transport);
        left.fill(0.0);
        right.fill(0.0);
    }
    /// Queues an automation point for the next `process` or
    /// `process_instrument` call, at `offset` samples into that block
    /// (AUT-030). Realtime-safe; extra points are dropped when full.
    fn queue_param(&mut self, id: u32, offset: u32, value: f64) {
        let _ = (id, offset, value);
    }
    /// Latency reported at activation, in samples.
    fn latency_samples(&self) -> u32;
    fn has_sidechain_input(&self) -> bool;
}

/// Non-realtime part of a plugin instance, owned by the UI thread.
pub trait PluginController {
    fn params(&self) -> &[ParamInfo];
    fn param_normalized(&self, id: u32) -> f64;
    /// Sets the value in the controller and sends it to the processor
    /// (applied at the start of the next block, PLG-042).
    fn set_param_normalized(&mut self, id: u32, value: f64);
    fn param_text(&self, id: u32, value: f64) -> String;
    fn param_from_text(&self, id: u32, text: &str) -> Option<f64>;
    fn state(&mut self) -> Result<PluginState, PluginError>;
    /// Events since the last call: GUI edits, output parameter changes
    /// from the processor, restart requests.
    fn poll(&mut self) -> Vec<PluginEvent>;
}

#[cfg(test)]
mod transport_tests {
    use super::Transport;

    #[test]
    fn bar_start_follows_the_signature() {
        let mut t = Transport::default();
        assert_eq!(t.bar_start_quarters(5.5), 4.0);
        t.time_sig = (3, 4);
        assert_eq!(t.bar_start_quarters(5.5), 3.0);
        t.time_sig = (7, 8);
        assert_eq!(t.bar_start_quarters(7.0), 7.0);
        assert_eq!(t.bar_start_quarters(6.9), 3.5);
    }
}
