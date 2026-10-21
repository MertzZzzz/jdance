// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in effect types and their parameter schemas.
//!
//! Every parameter has a stable string ID, unit, range, default, mapping
//! and smoothing policy. Projects store parameters by ID together with
//! the schema version, never by position.

use crate::PluginSlot;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    ParametricEq,
    TempoDelay,
    Reverb,
    Compressor,
    /// A hosted plugin; see [`Effect::plugin`]. Its parameters live in the
    /// plugin state, not in [`Effect::params`].
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamUnit {
    Decibels,
    Hertz,
    Milliseconds,
    Seconds,
    /// `x:1`.
    Ratio,
    /// 0..1 shown as percent.
    Fraction,
    /// Filter quality factor.
    Q,
    Toggle,
    Choice,
}

/// How a parameter is presented and interpolated in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapping {
    Linear,
    /// Logarithmic (frequencies, times); `min` must be positive.
    Log,
    /// `0.0` or `1.0`.
    Toggle,
    /// Integer index into the labels.
    Choice(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Smoothing {
    /// Applied at the next block boundary (toggles, choices, structure).
    Step,
    /// Ramped over the given time to avoid zipper noise.
    Ramp { ms: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub unit: ParamUnit,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub mapping: Mapping,
    pub smoothing: Smoothing,
}

const RAMP: Smoothing = Smoothing::Ramp { ms: 20 };

#[allow(clippy::too_many_arguments)] // a table constructor; one field per argument
const fn p(
    id: &'static str,
    name: &'static str,
    unit: ParamUnit,
    min: f32,
    max: f32,
    default: f32,
    mapping: Mapping,
    smoothing: Smoothing,
) -> ParamSpec {
    ParamSpec {
        id,
        name,
        unit,
        min,
        max,
        default,
        mapping,
        smoothing,
    }
}

const fn toggle(id: &'static str, name: &'static str, default: bool) -> ParamSpec {
    p(
        id,
        name,
        ParamUnit::Toggle,
        0.0,
        1.0,
        if default { 1.0 } else { 0.0 },
        Mapping::Toggle,
        Smoothing::Step,
    )
}

use Mapping::{Linear, Log};
use ParamUnit::*;

/// EQ frequency upper bound before the sample-rate clamp
/// (`min(20 kHz, 0.45 · sample_rate)` is applied by the processor).
pub const EQ_MAX_FREQ: f32 = 20_000.0;

const EQ_PARAMS: &[ParamSpec] = &[
    toggle("low_on", "Low shelf", true),
    p(
        "low_freq",
        "Low freq",
        Hertz,
        20.0,
        EQ_MAX_FREQ,
        100.0,
        Log,
        RAMP,
    ),
    p(
        "low_gain", "Low gain", Decibels, -24.0, 24.0, 0.0, Linear, RAMP,
    ),
    toggle("mid1_on", "Bell 1", true),
    p(
        "mid1_freq",
        "Bell 1 freq",
        Hertz,
        20.0,
        EQ_MAX_FREQ,
        500.0,
        Log,
        RAMP,
    ),
    p(
        "mid1_gain",
        "Bell 1 gain",
        Decibels,
        -24.0,
        24.0,
        0.0,
        Linear,
        RAMP,
    ),
    p("mid1_q", "Bell 1 Q", Q, 0.1, 18.0, 1.0, Log, RAMP),
    toggle("mid2_on", "Bell 2", true),
    p(
        "mid2_freq",
        "Bell 2 freq",
        Hertz,
        20.0,
        EQ_MAX_FREQ,
        2_500.0,
        Log,
        RAMP,
    ),
    p(
        "mid2_gain",
        "Bell 2 gain",
        Decibels,
        -24.0,
        24.0,
        0.0,
        Linear,
        RAMP,
    ),
    p("mid2_q", "Bell 2 Q", Q, 0.1, 18.0, 1.0, Log, RAMP),
    toggle("high_on", "High shelf", true),
    p(
        "high_freq",
        "High freq",
        Hertz,
        20.0,
        EQ_MAX_FREQ,
        8_000.0,
        Log,
        RAMP,
    ),
    p(
        "high_gain",
        "High gain",
        Decibels,
        -24.0,
        24.0,
        0.0,
        Linear,
        RAMP,
    ),
];

pub const DELAY_DIVISIONS: &[&str] = &["1/16", "1/8", "1/4", "1/2", "1/8 dotted", "1/8 triplet"];

const DELAY_PARAMS: &[ParamSpec] = &[
    toggle("sync", "Tempo sync", true),
    p(
        "time_ms",
        "Time",
        Milliseconds,
        1.0,
        2_000.0,
        375.0,
        Log,
        Smoothing::Step,
    ),
    p(
        "division",
        "Division",
        Choice,
        0.0,
        5.0,
        2.0,
        Mapping::Choice(DELAY_DIVISIONS),
        Smoothing::Step,
    ),
    p(
        "feedback", "Feedback", Fraction, 0.0, 0.95, 0.4, Linear, RAMP,
    ),
    p("wet", "Wet", Fraction, 0.0, 1.0, 0.3, Linear, RAMP),
    p(
        "lowpass",
        "Feedback low-pass",
        Hertz,
        200.0,
        20_000.0,
        8_000.0,
        Log,
        RAMP,
    ),
    toggle("ping_pong", "Ping-pong", false),
];

const REVERB_PARAMS: &[ParamSpec] = &[
    p("decay", "Decay (RT60)", Seconds, 0.2, 10.0, 1.8, Log, RAMP),
    p("damping", "Damping", Fraction, 0.0, 1.0, 0.4, Linear, RAMP),
    p(
        "predelay",
        "Pre-delay",
        Milliseconds,
        0.0,
        100.0,
        10.0,
        Linear,
        Smoothing::Step,
    ),
    p(
        "width",
        "Stereo width",
        Fraction,
        0.0,
        1.0,
        1.0,
        Linear,
        RAMP,
    ),
    p("wet", "Wet", Fraction, 0.0, 1.0, 0.25, Linear, RAMP),
];

const COMPRESSOR_PARAMS: &[ParamSpec] = &[
    p(
        "threshold",
        "Threshold",
        Decibels,
        -60.0,
        0.0,
        -18.0,
        Linear,
        RAMP,
    ),
    p("ratio", "Ratio", Ratio, 1.0, 20.0, 4.0, Log, RAMP),
    p(
        "attack",
        "Attack",
        Milliseconds,
        0.0,
        200.0,
        5.0,
        Linear,
        RAMP,
    ),
    p(
        "release",
        "Release",
        Milliseconds,
        1.0,
        2_000.0,
        120.0,
        Log,
        RAMP,
    ),
    p("makeup", "Makeup", Decibels, 0.0, 24.0, 0.0, Linear, RAMP),
    // Changes latency: applied structurally (recompiled with PDC).
    p(
        "lookahead",
        "Lookahead",
        Milliseconds,
        0.0,
        20.0,
        0.0,
        Linear,
        Smoothing::Step,
    ),
];

impl EffectKind {
    /// Built-in effects (plugins are added from the plugin browser).
    pub const ALL: [EffectKind; 4] = [
        EffectKind::ParametricEq,
        EffectKind::TempoDelay,
        EffectKind::Reverb,
        EffectKind::Compressor,
    ];

    pub fn name(self) -> &'static str {
        match self {
            EffectKind::ParametricEq => "Parametric EQ",
            EffectKind::TempoDelay => "Tempo Delay",
            EffectKind::Reverb => "Reverb",
            EffectKind::Compressor => "Compressor",
            EffectKind::Plugin => "Plugin",
        }
    }

    /// Current parameter schema version of this effect type.
    pub fn schema_version(self) -> u32 {
        1
    }

    pub fn params(self) -> &'static [ParamSpec] {
        match self {
            EffectKind::ParametricEq => EQ_PARAMS,
            EffectKind::TempoDelay => DELAY_PARAMS,
            EffectKind::Reverb => REVERB_PARAMS,
            EffectKind::Compressor => COMPRESSOR_PARAMS,
            EffectKind::Plugin => &[],
        }
    }

    pub fn param(self, id: &str) -> Option<&'static ParamSpec> {
        self.params().iter().find(|p| p.id == id)
    }

    /// Plugins may have a sidechain input; the host feeds it only if the
    /// plugin has one (PLG-031).
    pub fn accepts_sidechain(self) -> bool {
        matches!(self, EffectKind::Compressor | EffectKind::Plugin)
    }
}

impl ParamSpec {
    /// Value for a normalized automation value `n` in `0..=1`
    /// (ADR-014): linear, logarithmic, toggle at 0.5, or the nearest
    /// choice index.
    pub fn from_normalized(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        match self.mapping {
            Mapping::Linear => self.min + (self.max - self.min) * n,
            Mapping::Log => self.min * (self.max / self.min).powf(n),
            Mapping::Toggle => {
                if n >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Mapping::Choice(_) => (self.min + (self.max - self.min) * n).round(),
        }
    }

    /// Inverse of [`ParamSpec::from_normalized`].
    pub fn to_normalized(&self, v: f32) -> f32 {
        let v = v.clamp(self.min, self.max);
        let n = match self.mapping {
            Mapping::Log => (v / self.min).ln() / (self.max / self.min).ln(),
            _ if self.max > self.min => (v - self.min) / (self.max - self.min),
            _ => 0.0,
        };
        n.clamp(0.0, 1.0)
    }

    /// Checks a value for this parameter.
    pub fn check(&self, v: f32) -> Result<(), String> {
        if !v.is_finite() || v < self.min || v > self.max {
            return Err(format!(
                "{} = {v} outside {}..={}",
                self.id, self.min, self.max
            ));
        }
        match self.mapping {
            Mapping::Toggle if v != 0.0 && v != 1.0 => Err(format!("{} must be 0 or 1", self.id)),
            Mapping::Choice(_) if v.fract() != 0.0 => Err(format!("{} must be an index", self.id)),
            _ => Ok(()),
        }
    }
}

/// An effect instance's settings as stored in the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub kind: EffectKind,
    pub schema: u32,
    /// Values by parameter ID. Missing IDs take their defaults.
    pub params: BTreeMap<String, f32>,
    /// Set exactly when `kind` is [`EffectKind::Plugin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginSlot>,
}

impl Effect {
    pub fn new(kind: EffectKind) -> Self {
        Self {
            kind,
            schema: kind.schema_version(),
            params: kind
                .params()
                .iter()
                .map(|p| (p.id.to_owned(), p.default))
                .collect(),
            plugin: None,
        }
    }

    pub fn plugin(slot: PluginSlot) -> Self {
        Self {
            plugin: Some(slot),
            ..Self::new(EffectKind::Plugin)
        }
    }

    pub fn name(&self) -> &str {
        match &self.plugin {
            Some(p) => &p.name,
            None => self.kind.name(),
        }
    }

    pub fn accepts_sidechain(&self) -> bool {
        self.kind.accepts_sidechain()
    }

    /// Value of `id`, or its default if absent. Panics on an unknown ID
    /// (a programming error, not a data error).
    pub fn get(&self, id: &str) -> f32 {
        let spec = self
            .kind
            .param(id)
            .unwrap_or_else(|| panic!("{} has no parameter {id}", self.kind.name()));
        self.params.get(id).copied().unwrap_or(spec.default)
    }

    pub fn get_bool(&self, id: &str) -> bool {
        self.get(id) >= 0.5
    }

    /// Returns a copy with `id` set to `v`.
    pub fn with(&self, id: &str, v: f32) -> Self {
        let mut e = self.clone();
        e.params.insert(id.to_owned(), v);
        e
    }

    pub fn validate(&self) -> Result<(), String> {
        if (self.kind == EffectKind::Plugin) != self.plugin.is_some() {
            return Err(format!("{} has inconsistent plugin data", self.name()));
        }
        if self.schema != self.kind.schema_version() {
            return Err(format!(
                "{} parameter schema {} is not supported (expected {})",
                self.kind.name(),
                self.schema,
                self.kind.schema_version()
            ));
        }
        for (id, v) in &self.params {
            let spec = self
                .kind
                .param(id)
                .ok_or_else(|| format!("{} has no parameter {id}", self.kind.name()))?;
            spec.check(*v)?;
        }
        Ok(())
    }

    /// Fills in defaults for parameters missing from an older file.
    pub fn complete(&mut self) {
        for p in self.kind.params() {
            self.params.entry(p.id.to_owned()).or_insert(p.default);
        }
    }

    /// Whether changing from `self` to `other` alters latency or
    /// structure, which needs a recompile with delay compensation rather
    /// than a smoothed parameter change.
    ///
    /// For plugins only a different class counts: a state change comes
    /// from the plugin instance itself and needs no engine update.
    pub fn changes_latency(&self, other: &Effect) -> bool {
        fn class(e: &Effect) -> Option<(&str, &str)> {
            e.plugin
                .as_ref()
                .map(|p| (p.format.as_str(), p.class_id.as_str()))
        }
        self.kind != other.kind
            || class(self) != class(other)
            || (self.kind == EffectKind::Compressor
                && self.get("lookahead") != other.get("lookahead"))
    }
}

/// Master safety limiter stage (not an insert; optional).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LimiterSettings {
    pub enabled: bool,
    /// dBFS, `-12.0..=0.0`.
    pub ceiling_db: f32,
    /// `10.0..=500.0` ms.
    pub release_ms: f32,
}

impl Default for LimiterSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            ceiling_db: -0.3,
            release_ms: 80.0,
        }
    }
}

impl LimiterSettings {
    pub const CEILING_DB: (f32, f32) = (-12.0, 0.0);
    pub const RELEASE_MS: (f32, f32) = (10.0, 500.0);

    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: f32, (lo, hi): (f32, f32)| v.is_finite() && (lo..=hi).contains(&v);
        if !ok(self.ceiling_db, Self::CEILING_DB) || !ok(self.release_ms, Self::RELEASE_MS) {
            return Err("limiter settings out of range".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_complete() {
        for kind in EffectKind::ALL {
            let e = Effect::new(kind);
            assert!(e.validate().is_ok(), "{kind:?}");
            assert_eq!(e.params.len(), kind.params().len());
            for p in kind.params() {
                assert!(p.check(p.default).is_ok(), "{kind:?} {}", p.id);
                if matches!(p.mapping, Mapping::Log) {
                    assert!(p.min > 0.0);
                }
            }
            let mut ids: Vec<_> = kind.params().iter().map(|p| p.id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), kind.params().len(), "unique IDs in {kind:?}");
        }
    }

    #[test]
    fn validation_rejects_bad_values() {
        let e = Effect::new(EffectKind::TempoDelay);
        assert!(e.with("feedback", 0.99).validate().is_err());
        assert!(e.with("sync", 0.5).validate().is_err());
        assert!(e.with("division", 1.5).validate().is_err());
        assert!(e.with("nope", 1.0).validate().is_err());
        assert!(e.with("wet", f32::NAN).validate().is_err());
        let mut partial = e.clone();
        partial.params.remove("wet");
        assert!(partial.validate().is_ok());
        assert_eq!(partial.get("wet"), 0.3);
        partial.complete();
        assert_eq!(partial.params.len(), DELAY_PARAMS.len());
    }

    #[test]
    fn serializes_parameters_by_name() {
        let e = Effect::new(EffectKind::Reverb).with("decay", 3.0);
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["kind"], "reverb");
        assert_eq!(json["schema"], 1);
        assert_eq!(json["params"]["decay"], 3.0);
    }
}
