// SPDX-License-Identifier: GPL-3.0-or-later
//! Automation in the UI (ADR-014): "Automate" in the context menu of
//! automatable controls, a mark on automated controls, lane labels and
//! value texts for the envelope editor in the playlist.

use crate::Host;
use egui::{Color32, Response, Stroke, pos2};
use jdance_domain::{
    AutomationId, AutomationLane, AutomationTarget, Insert, Mapping, MixerTrack, ParamUnit,
    PluginTarget, Project,
};

/// Orange used for automation marks and envelopes.
pub(crate) const AUTOMATION_COLOR: Color32 = crate::theme::TEAL;

/// Automation changes requested from a control.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AutomationRequest {
    /// New lane starting at the control's current normalized value.
    Create {
        target: AutomationTarget,
        initial: f32,
    },
    Remove(AutomationId),
}

/// Adds "Automate" / "Remove automation" to the control's context menu
/// and marks the control when a lane drives it.
pub(crate) fn attach(
    ui: &egui::Ui,
    resp: &Response,
    automation: &[AutomationLane],
    target: AutomationTarget,
    initial: f32,
    out: &mut Vec<AutomationRequest>,
) {
    let lane = automation.iter().find(|l| l.target == target).map(|l| l.id);
    if lane.is_some() {
        let r = resp.rect;
        let s = 6.0;
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                r.left_top(),
                pos2(r.left() + s, r.top()),
                pos2(r.left(), r.top() + s),
            ],
            AUTOMATION_COLOR,
            Stroke::NONE,
        ));
    }
    // Sliders sense drags only; the menu needs secondary clicks.
    let resp = resp.interact(egui::Sense::click());
    resp.context_menu(|ui| match lane {
        Some(id) => {
            ui.label("Automated: follows its lane during song playback");
            if ui.button("Remove automation").clicked() {
                out.push(AutomationRequest::Remove(id));
                ui.close();
            }
        }
        None => {
            if ui
                .button("Automate")
                .on_hover_text("Create an envelope lane in the playlist")
                .clicked()
            {
                out.push(AutomationRequest::Create {
                    target: target.clone(),
                    initial,
                });
                ui.close();
            }
        }
    });
}

/// Whether the secondary button is pressed or just released: egui
/// sliders follow any button, so a right-click for the menu must not
/// change the value.
pub(crate) fn secondary_press(ui: &egui::Ui) -> bool {
    ui.input(|i| {
        i.pointer.button_down(egui::PointerButton::Secondary)
            || i.pointer.button_released(egui::PointerButton::Secondary)
    })
}

/// Header of a lane in the playlist.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LaneInfo {
    pub id: AutomationId,
    /// What is automated, e.g. "Low gain".
    pub name: String,
    /// Where it lives, e.g. "Bass · Parametric EQ".
    pub context: String,
    /// The target no longer exists (AUT-050); the lane is kept.
    pub missing: bool,
}

fn insert_of(p: &Project, id: jdance_domain::InsertId) -> Option<(&MixerTrack, &Insert)> {
    p.mixer
        .all_tracks()
        .find_map(|t| t.inserts.iter().find(|i| i.id == id).map(|i| (t, i)))
}

fn plugin_param_name(host: &mut dyn Host, target: PluginTarget, param: u32) -> String {
    host.plugin_params(target)
        .and_then(|ps| ps.into_iter().find(|p| p.info.id == param))
        .map_or_else(|| format!("Param {param}"), |p| p.info.name)
}

pub(crate) fn lane_infos(p: &Project, host: &mut dyn Host) -> Vec<LaneInfo> {
    p.automation
        .iter()
        .map(|lane| {
            let missing = !lane.target.exists(p);
            let (name, context) = if missing {
                ("Target not found".to_owned(), String::new())
            } else {
                label(p, &lane.target, host)
            };
            LaneInfo {
                id: lane.id,
                name,
                context,
                missing,
            }
        })
        .collect()
}

/// (name, context) of an existing target.
fn label(p: &Project, target: &AutomationTarget, host: &mut dyn Host) -> (String, String) {
    let track = |id| {
        p.mixer
            .track(id)
            .map_or_else(|| "?".to_owned(), |t| t.name.clone())
    };
    let in_insert = |id, name: String| {
        let context = insert_of(p, id)
            .map(|(t, i)| format!("{} · {}", t.name, i.effect.name()))
            .unwrap_or_default();
        (name, context)
    };
    match target {
        AutomationTarget::TrackVolume(t) => ("Volume".into(), track(*t)),
        AutomationTarget::TrackPan(t) => ("Pan".into(), track(*t)),
        AutomationTarget::SendLevel(id) => {
            let context = p
                .mixer
                .all_tracks()
                .find_map(|t| {
                    t.sends
                        .iter()
                        .find(|s| s.id == *id)
                        .map(|s| format!("{} → {}", t.name, track(s.dest)))
                })
                .unwrap_or_default();
            ("Send level".into(), context)
        }
        AutomationTarget::EffectParam { insert, param } => {
            let name = insert_of(p, *insert)
                .and_then(|(_, i)| i.effect.kind.param(param))
                .map_or("?", |s| s.name);
            in_insert(*insert, name.to_owned())
        }
        AutomationTarget::InsertBypass(id) => in_insert(*id, "Bypass".into()),
        AutomationTarget::PluginParam {
            target: pt @ PluginTarget::Insert(id),
            param,
        } => in_insert(*id, plugin_param_name(host, *pt, *param)),
        AutomationTarget::PluginParam {
            target: pt @ PluginTarget::Channel(c),
            param,
        } => (
            plugin_param_name(host, *pt, *param),
            p.channel(*c).map_or("?", |c| c.name.as_str()).to_owned(),
        ),
    }
}

/// Text for normalized value `n` of `target`, in the target's units.
pub(crate) fn value_text(p: &Project, target: &AutomationTarget, n: f32) -> String {
    let Some(v) = target.plain(p, n) else {
        return format!("{:.0}%", n * 100.0);
    };
    match target {
        AutomationTarget::TrackVolume(_) | AutomationTarget::SendLevel(_) => {
            if v <= 0.0 {
                "-inf dB".into()
            } else {
                format!("{:+.1} dB", 20.0 * v.log10())
            }
        }
        AutomationTarget::TrackPan(_) => match (v * 100.0).round() as i32 {
            0 => "C".into(),
            x if x < 0 => format!("L{}", -x),
            x => format!("R{x}"),
        },
        AutomationTarget::InsertBypass(_) => if v >= 0.5 { "Bypassed" } else { "On" }.into(),
        AutomationTarget::EffectParam { insert, param } => {
            let Some(spec) = insert_of(p, *insert).and_then(|(_, i)| i.effect.kind.param(param))
            else {
                return format!("{v:.2}");
            };
            match spec.mapping {
                Mapping::Toggle => if v >= 0.5 { "On" } else { "Off" }.into(),
                Mapping::Choice(labels) => labels.get(v as usize).copied().unwrap_or("?").into(),
                _ if spec.unit == ParamUnit::Fraction => format!("{:.0}%", v * 100.0),
                _ => format!("{v:.2}{}", crate::mixer::unit_suffix(spec.unit)),
            }
        }
        AutomationTarget::PluginParam { .. } => format!("{:.0}%", n * 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{Effect, EffectKind, InsertId, TrackId};

    #[test]
    fn value_texts_use_target_units() {
        let mut p = Project::new();
        let mut t = MixerTrack::new(TrackId(7), "Bus");
        t.inserts.push(Insert {
            id: InsertId(8),
            bypass: false,
            effect: Effect::new(EffectKind::ParametricEq),
            sidechain: None,
        });
        p.mixer.tracks.push(t);
        let vol = AutomationTarget::TrackVolume(TrackId(7));
        assert_eq!(value_text(&p, &vol, 0.0), "-inf dB");
        assert_eq!(
            value_text(&p, &AutomationTarget::TrackPan(TrackId(7)), 0.5),
            "C"
        );
        assert_eq!(
            value_text(&p, &AutomationTarget::TrackPan(TrackId(7)), 0.0),
            "L100"
        );
        assert_eq!(
            value_text(&p, &AutomationTarget::InsertBypass(InsertId(8)), 1.0),
            "Bypassed"
        );
        let spec = &EffectKind::ParametricEq.params()[0];
        let param = AutomationTarget::EffectParam {
            insert: InsertId(8),
            param: spec.id.to_owned(),
        };
        assert!(
            value_text(&p, &param, 1.0).ends_with(crate::mixer::unit_suffix(spec.unit)),
            "{}",
            value_text(&p, &param, 1.0)
        );
        // A missing target falls back to percent.
        let gone = AutomationTarget::EffectParam {
            insert: InsertId(99),
            param: spec.id.to_owned(),
        };
        assert_eq!(value_text(&p, &gone, 0.5), "50%");
    }
}
