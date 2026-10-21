// SPDX-License-Identifier: GPL-3.0-or-later
//! Automation lanes on the song timeline (ADR-014, AUT-001..060).

use crate::{
    AutomationId, ChannelId, EffectKind, InsertId, MAX_SEND_LEVEL, MAX_VOLUME, PluginTarget,
    Project, SendId, TrackId,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Points per lane.
pub const MAX_AUTOMATION_POINTS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    /// Keeps the value until the next point.
    Hold,
    #[default]
    Linear,
    /// `t^(4^tension)` between this point and the next.
    Curve,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutoPoint {
    /// Song time in ticks.
    pub tick: u64,
    /// Normalized `0.0..=1.0`.
    pub value: f32,
    /// Shape from this point to the next.
    pub curve: Curve,
    /// `-1.0..=1.0`, used by [`Curve::Curve`].
    #[serde(default)]
    pub tension: f32,
}

/// What a lane controls (AUT-002).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AutomationTarget {
    TrackVolume(TrackId),
    TrackPan(TrackId),
    SendLevel(SendId),
    /// A built-in effect parameter by schema ID.
    EffectParam {
        insert: InsertId,
        param: String,
    },
    InsertBypass(InsertId),
    /// A VST3 parameter (`ParamID`) of an insert or instrument.
    PluginParam {
        target: PluginTarget,
        param: u32,
    },
}

/// AUT-060 form: `{ kind, object_id, param_id }`.
#[derive(Serialize, Deserialize)]
struct TargetRepr {
    kind: String,
    object_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    param_id: Option<String>,
}

impl Serialize for AutomationTarget {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let (kind, object_id, param_id) = match self {
            Self::TrackVolume(t) => ("track_volume", t.0, None),
            Self::TrackPan(t) => ("track_pan", t.0, None),
            Self::SendLevel(id) => ("send_level", id.0, None),
            Self::EffectParam { insert, param } => ("builtin_param", insert.0, Some(param.clone())),
            Self::InsertBypass(i) => ("insert_bypass", i.0, None),
            Self::PluginParam {
                target: PluginTarget::Insert(i),
                param,
            } => ("vst3_param", i.0, Some(param.to_string())),
            Self::PluginParam {
                target: PluginTarget::Channel(c),
                param,
            } => ("vst3_instrument_param", c.0, Some(param.to_string())),
        };
        TargetRepr {
            kind: kind.into(),
            object_id,
            param_id,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for AutomationTarget {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let r = TargetRepr::deserialize(d)?;
        let param = || {
            r.param_id
                .clone()
                .ok_or_else(|| D::Error::custom(format!("{} needs param_id", r.kind)))
        };
        let plugin_param = || {
            param()?
                .parse::<u32>()
                .map_err(|_| D::Error::custom("VST3 param_id must be a number"))
        };
        Ok(match r.kind.as_str() {
            "track_volume" => Self::TrackVolume(TrackId(r.object_id)),
            "track_pan" => Self::TrackPan(TrackId(r.object_id)),
            "send_level" => Self::SendLevel(SendId(r.object_id)),
            "builtin_param" => Self::EffectParam {
                insert: InsertId(r.object_id),
                param: param()?,
            },
            "insert_bypass" => Self::InsertBypass(InsertId(r.object_id)),
            "vst3_param" => Self::PluginParam {
                target: PluginTarget::Insert(InsertId(r.object_id)),
                param: plugin_param()?,
            },
            "vst3_instrument_param" => Self::PluginParam {
                target: PluginTarget::Channel(ChannelId(r.object_id)),
                param: plugin_param()?,
            },
            k => return Err(D::Error::custom(format!("unknown automation target {k}"))),
        })
    }
}

impl AutomationTarget {
    /// Whether the target exists in `p` (AUT-050). Plugin parameters are
    /// only checked for their plugin.
    pub fn exists(&self, p: &Project) -> bool {
        let insert = |id: InsertId| {
            p.mixer
                .all_tracks()
                .flat_map(|t| &t.inserts)
                .find(|i| i.id == id)
        };
        match self {
            Self::TrackVolume(t) | Self::TrackPan(t) => p.mixer.has_track(*t),
            Self::SendLevel(id) => p
                .mixer
                .all_tracks()
                .any(|t| t.sends.iter().any(|s| s.id == *id)),
            Self::EffectParam { insert: id, param } => {
                insert(*id).is_some_and(|i| i.effect.kind.param(param).is_some())
            }
            Self::InsertBypass(id) => insert(*id).is_some(),
            Self::PluginParam {
                target: PluginTarget::Insert(id),
                ..
            } => insert(*id).is_some_and(|i| i.effect.kind == EffectKind::Plugin),
            Self::PluginParam {
                target: PluginTarget::Channel(c),
                ..
            } => p.channel(*c).is_some_and(|c| c.instrument().is_some()),
        }
    }

    /// Plain value for normalized `n` (ADR-014); `None` if the target is
    /// gone.
    pub fn plain(&self, p: &Project, n: f32) -> Option<f32> {
        let n = n.clamp(0.0, 1.0);
        Some(match self {
            Self::TrackVolume(_) => n * MAX_VOLUME,
            Self::TrackPan(_) => 2.0 * n - 1.0,
            Self::SendLevel(_) => n * MAX_SEND_LEVEL,
            Self::EffectParam { insert, param } => {
                let ins = p
                    .mixer
                    .all_tracks()
                    .flat_map(|t| &t.inserts)
                    .find(|i| i.id == *insert)?;
                ins.effect.kind.param(param)?.from_normalized(n)
            }
            Self::InsertBypass(_) => {
                if n >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Self::PluginParam { .. } => n,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    pub id: AutomationId,
    pub target: AutomationTarget,
    /// Sorted by tick.
    pub points: Vec<AutoPoint>,
}

/// Shaped interpolation weight for `t` in `0..=1`.
pub fn curve_weight(curve: Curve, tension: f32, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        Curve::Hold => 0.0,
        Curve::Linear => t,
        Curve::Curve => t.powf(4f64.powf(f64::from(tension.clamp(-1.0, 1.0)))),
    }
}

impl AutomationLane {
    /// Envelope value at `tick` (AUT-011): first value before the first
    /// point, last value after the last.
    pub fn value_at(&self, tick: f64) -> Option<f32> {
        let pts = &self.points;
        let first = pts.first()?;
        if tick < first.tick as f64 {
            return Some(first.value);
        }
        let i = pts.partition_point(|p| p.tick as f64 <= tick);
        let a = pts[i - 1];
        let Some(b) = pts.get(i) else {
            return Some(a.value);
        };
        let span = (b.tick - a.tick) as f64;
        let t = (tick - a.tick as f64) / span;
        let w = curve_weight(a.curve, a.tension, t);
        Some(a.value + (b.value - a.value) * w as f32)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.points.len() > MAX_AUTOMATION_POINTS {
            return Err("too many automation points".into());
        }
        for w in self.points.windows(2) {
            if w[1].tick < w[0].tick {
                return Err("automation points are not sorted".into());
            }
        }
        for p in &self.points {
            if !(0.0..=1.0).contains(&p.value) || !(-1.0..=1.0).contains(&p.tension) {
                return Err("automation point out of range".into());
            }
        }
        Ok(())
    }
}

/// Sorts points by tick, keeping the order of equal ticks.
pub fn sort_points(points: &mut [AutoPoint]) {
    points.sort_by_key(|p| p.tick);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(tick: u64, value: f32, curve: Curve) -> AutoPoint {
        AutoPoint {
            tick,
            value,
            curve,
            tension: 0.0,
        }
    }

    fn lane(points: Vec<AutoPoint>) -> AutomationLane {
        AutomationLane {
            id: AutomationId(1),
            target: AutomationTarget::TrackVolume(TrackId(2)),
            points,
        }
    }

    #[test]
    fn curves_and_edges() {
        let l = lane(vec![
            pt(100, 0.2, Curve::Linear),
            pt(200, 0.6, Curve::Hold),
            pt(300, 1.0, Curve::Linear),
        ]);
        assert_eq!(l.value_at(0.0), Some(0.2), "before the first point");
        assert!((l.value_at(150.0).unwrap() - 0.4).abs() < 1e-6);
        assert_eq!(l.value_at(250.0), Some(0.6), "hold");
        assert_eq!(l.value_at(300.0), Some(1.0));
        assert_eq!(l.value_at(1e9), Some(1.0), "after the last point");
        assert_eq!(lane(vec![]).value_at(5.0), None);
        let single = lane(vec![pt(50, 0.3, Curve::Linear)]);
        assert_eq!(single.value_at(0.0), Some(0.3));
        assert_eq!(single.value_at(99.0), Some(0.3));
        // Equal ticks: the later point wins from that tick on.
        let jump = lane(vec![
            pt(0, 0.0, Curve::Linear),
            pt(10, 0.2, Curve::Linear),
            pt(10, 0.8, Curve::Linear),
        ]);
        assert_eq!(jump.value_at(10.0), Some(0.8));
        assert!((jump.value_at(5.0).unwrap() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn tension_shapes_the_curve() {
        assert_eq!(curve_weight(Curve::Curve, 0.0, 0.5), 0.5);
        assert!(curve_weight(Curve::Curve, 0.5, 0.5) < 0.5);
        assert!(curve_weight(Curve::Curve, -0.5, 0.5) > 0.5);
        assert_eq!(curve_weight(Curve::Curve, 1.0, 1.0), 1.0);
    }

    #[test]
    fn targets_serialize_in_the_spec_format() {
        let t = AutomationTarget::PluginParam {
            target: PluginTarget::Channel(ChannelId(7)),
            param: 42,
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "vst3_instrument_param", "object_id": 7, "param_id": "42"})
        );
        assert_eq!(serde_json::from_value::<AutomationTarget>(json).unwrap(), t);
        let e = AutomationTarget::EffectParam {
            insert: InsertId(3),
            param: "threshold".into(),
        };
        let back: AutomationTarget =
            serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        assert!(
            serde_json::from_value::<AutomationTarget>(
                serde_json::json!({"kind": "tempo", "object_id": 0})
            )
            .is_err()
        );
    }
}
