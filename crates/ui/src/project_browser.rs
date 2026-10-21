// SPDX-License-Identifier: GPL-3.0-or-later
//! Project Browser left of the Playlist (spec 0.2.0 §10, UI-VIS §7.2):
//! the project's patterns, audio files and automations, not the disk. The selected item
//! is what Draw and Paint place; items drag into the Playlist;
//! double-click opens the editor.

use crate::playlist::Source;
use crate::{Host, theme};
use egui::{RichText, Sense, Stroke, StrokeKind, vec2};
use jdance_domain::{AssetId, AutomationId, PatternId, Project};

pub(crate) const WIDTH: f32 = 180.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Patterns,
    Samples,
    Automations,
}

/// Payload of a drag from the Project Browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectItem(pub Source);

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProjectAction {
    Select(Source),
    OpenPattern(PatternId),
    OpenAsset(AssetId),
    AddPattern,
    DuplicatePattern(PatternId),
    RenamePattern(PatternId),
    /// Ask before deleting when clips use it.
    DeletePattern(PatternId),
    ImportFiles,
    RenameAsset(AssetId),
    PreviewAsset(AssetId),
    LoadIntoSampler(AssetId),
    DeleteAsset(AssetId),
    OpenAutomation(AutomationId),
    RenameAutomation(AutomationId),
    DuplicateAutomation(AutomationId),
    /// Ask before deleting when clips use it.
    DeleteAutomation(AutomationId),
}

#[derive(Debug)]
pub(crate) struct ProjectBrowser {
    pub tab: Tab,
    query: String,
}

impl Default for ProjectBrowser {
    fn default() -> Self {
        Self {
            tab: Tab::Patterns,
            query: String::new(),
        }
    }
}

impl ProjectBrowser {
    /// `source` = what Draw and Paint place now (highlighted).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        source: Option<Source>,
        host: &mut dyn Host,
    ) -> Vec<ProjectAction> {
        let mut out = Vec::new();
        ui.set_width(WIDTH);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (tab, label) in [
                (Tab::Patterns, "Patterns"),
                (Tab::Samples, "Samples"),
                (Tab::Automations, "Auto"),
            ] {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("Search")
                .desired_width(WIDTH - 8.0),
        );
        let q = self.query.to_lowercase();
        let matches = |name: &str| q.is_empty() || name.to_lowercase().contains(&q);
        egui::ScrollArea::vertical()
            .id_salt("project_browser")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Patterns => {
                    for p in project.patterns.iter().filter(|p| matches(&p.name)) {
                        let uses = project.playlist.clips.iter().filter(|c| c.pattern == p.id).count();
                        let src = Source::Pattern(p.id);
                        let resp = item(ui, &p.name, &format!("{uses}"), source == Some(src));
                        let resp = resp.on_hover_text(format!(
                            "{} steps · used by {uses} clip(s)\nClick: place with Draw/Paint · drag into the Playlist · double-click: Piano Roll",
                            p.length_steps
                        ));
                        resp.dnd_set_drag_payload(ProjectItem(src));
                        if resp.clicked() {
                            out.push(ProjectAction::Select(src));
                        }
                        if resp.double_clicked() {
                            out.push(ProjectAction::OpenPattern(p.id));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Open in Piano Roll").clicked() {
                                out.push(ProjectAction::OpenPattern(p.id));
                                ui.close();
                            }
                            if ui.button("Rename…").clicked() {
                                out.push(ProjectAction::RenamePattern(p.id));
                                ui.close();
                            }
                            if ui.button("Duplicate").clicked() {
                                out.push(ProjectAction::DuplicatePattern(p.id));
                                ui.close();
                            }
                            if ui
                                .add_enabled(project.patterns.len() > 1, egui::Button::new("Delete…"))
                                .clicked()
                            {
                                out.push(ProjectAction::DeletePattern(p.id));
                                ui.close();
                            }
                        });
                    }
                    if ui.button("+ New pattern").clicked() {
                        out.push(ProjectAction::AddPattern);
                    }
                }
                Tab::Samples => {
                    if project.assets.is_empty() {
                        ui.label(
                            RichText::new("No audio files yet: drop one into the Playlist or import")
                                .size(11.0)
                                .color(theme::MUTED),
                        );
                    }
                    for a in project.assets.iter().filter(|a| matches(&a.name)) {
                        let uses = project.asset_usage(a.id);
                        let src = Source::Asset(a.id);
                        let resp = item(ui, &a.name, &format!("{uses}"), source == Some(src));
                        let resp = resp.on_hover_text(format!(
                            "{:.2} s · {} Hz · used by {uses} clip(s)\n{}\nClick: place with Draw/Paint · drag into the Playlist · double-click: Sample Editor",
                            a.seconds(),
                            a.sample_rate,
                            a.path
                        ));
                        resp.dnd_set_drag_payload(ProjectItem(src));
                        if resp.clicked() {
                            out.push(ProjectAction::Select(src));
                        }
                        if resp.double_clicked() {
                            out.push(ProjectAction::OpenAsset(a.id));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Open in Sample Editor").clicked() {
                                out.push(ProjectAction::OpenAsset(a.id));
                                ui.close();
                            }
                            if ui.button("Preview").clicked() {
                                out.push(ProjectAction::PreviewAsset(a.id));
                                ui.close();
                            }
                            if ui.button("Load into new sampler").clicked() {
                                out.push(ProjectAction::LoadIntoSampler(a.id));
                                ui.close();
                            }
                            if ui
                                .button("Rename…")
                                .on_hover_text("Changes the display name only; the file stays")
                                .clicked()
                            {
                                out.push(ProjectAction::RenameAsset(a.id));
                                ui.close();
                            }
                            if ui.button("Remove from project…").clicked() {
                                out.push(ProjectAction::DeleteAsset(a.id));
                                ui.close();
                            }
                        });
                    }
                    if ui
                        .button("+ Import…")
                        .on_hover_text("Copy audio files into the project")
                        .clicked()
                    {
                        out.push(ProjectAction::ImportFiles);
                    }
                }
                Tab::Automations => {
                    if project.automation.is_empty() {
                        ui.label(
                            RichText::new(
                                "No automation yet: right-click a mixer control, or open a clip's menu (the marker before its name, or Shift + right-click) and choose Automate",
                            )
                            .size(11.0)
                            .color(theme::MUTED),
                        );
                    }
                    for a in project.automation.iter().filter(|a| matches(&a.name)) {
                        let uses = project.automation_usage(a.id);
                        let src = Source::Automation(a.id);
                        let missing = !a.target.exists(project);
                        let target = if missing {
                            "target not found".to_owned()
                        } else {
                            let (name, context) = crate::automation::label(project, &a.target, host);
                            if context.is_empty() {
                                name
                            } else {
                                format!("{context} · {name}")
                            }
                        };
                        let count = if missing {
                            format!("! {uses}")
                        } else {
                            format!("{uses}")
                        };
                        let resp = item(ui, &a.name, &count, source == Some(src));
                        let resp = resp.on_hover_text(format!(
                            "{target} · {} bars · used by {uses} clip(s)\nClick: place with Draw/Paint · drag into the Playlist · double-click: Automation Editor",
                            a.length.div_ceil(3840)
                        ));
                        resp.dnd_set_drag_payload(ProjectItem(src));
                        if resp.clicked() {
                            out.push(ProjectAction::Select(src));
                        }
                        if resp.double_clicked() {
                            out.push(ProjectAction::OpenAutomation(a.id));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Open in Automation Editor").clicked() {
                                out.push(ProjectAction::OpenAutomation(a.id));
                                ui.close();
                            }
                            if ui.button("Rename…").clicked() {
                                out.push(ProjectAction::RenameAutomation(a.id));
                                ui.close();
                            }
                            if ui.button("Duplicate").clicked() {
                                out.push(ProjectAction::DuplicateAutomation(a.id));
                                ui.close();
                            }
                            if ui.button("Delete…").clicked() {
                                out.push(ProjectAction::DeleteAutomation(a.id));
                                ui.close();
                            }
                        });
                    }
                }
            });
        out
    }
}

/// A list row: name, usage count; highlighted when it is the source.
fn item(ui: &mut egui::Ui, name: &str, count: &str, on: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click_and_drag());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 5.0, theme::BLUE_BG);
        p.rect_stroke(rect, 5.0, Stroke::new(1.0, theme::BLUE), StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_filled(rect, 5.0, theme::RAISED);
    }
    p.text(
        rect.left_center() + vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(12.5),
        theme::TEXT,
    );
    p.text(
        rect.right_center() - vec2(8.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        count,
        theme::mono(11.0),
        theme::MUTED,
    );
    resp
}
