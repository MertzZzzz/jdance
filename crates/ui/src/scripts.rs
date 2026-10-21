// SPDX-License-Identifier: GPL-3.0-or-later
//! Tools > Scripts (spec 0.2.0 §18.22): the menu, and a dialog whose form
//! comes from the script's parameter schema. The host runs scripts on a
//! worker with a budget; results come back as notes or a selection and
//! apply as one undo step (a selection changes nothing in the project).

use crate::{Host, theme};
use egui::RichText;
use jdance_commands::note_tools::{
    Input, ParamKind, ParamValue, Range, ScriptInfo, ScriptOutput, ScriptRun,
};
use jdance_domain::Note;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ScriptRequest {
    Open(String),
    RunLast,
    OpenFolder,
}

fn last_id() -> egui::Id {
    egui::Id::unique("jdance_last_script_run")
}

/// The last successful run of this session.
pub(crate) fn last_run(ctx: &egui::Context) -> Option<ScriptRun> {
    ctx.data(|d| d.get_temp::<ScriptRun>(last_id()))
}

pub(crate) fn menu(
    ui: &mut egui::Ui,
    scripts: &[ScriptInfo],
    has_last: bool,
) -> Option<ScriptRequest> {
    let mut out = None;
    if ui
        .add_enabled(has_last, egui::Button::new("Run last script again"))
        .on_hover_text("Same script, settings and seed, on the current notes")
        .clicked()
    {
        out = Some(ScriptRequest::RunLast);
        ui.close();
    }
    if ui
        .button("Open script folder")
        .on_hover_text("Your own scripts (.rhai) go here; nothing runs until you choose it")
        .clicked()
    {
        out = Some(ScriptRequest::OpenFolder);
        ui.close();
    }
    ui.separator();
    for s in scripts {
        let r = ui.button(format!("{}…", s.name));
        let r = if s.description.is_empty() {
            r
        } else {
            r.on_hover_text(&s.description)
        };
        if r.clicked() {
            out = Some(ScriptRequest::Open(s.id.clone()));
            ui.close();
        }
    }
    out
}

/// A snapshot to run a script on (owned: the run happens elsewhere).
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptJob {
    pub notes: Vec<Note>,
    pub selected: Vec<usize>,
    pub pattern_len: u32,
    pub grid: u32,
    pub range: Option<Range>,
}

impl ScriptJob {
    pub fn of(input: &Input) -> Self {
        Self {
            notes: input.notes.to_vec(),
            selected: input.selected.to_vec(),
            pattern_len: input.pattern_len,
            grid: input.grid,
            range: input.range,
        }
    }
}

/// Parameter values by key.
type Values = Vec<(String, ParamValue)>;

#[derive(Debug, Clone)]
pub(crate) struct ScriptDialog {
    pub info: ScriptInfo,
    values: Vec<(String, ParamValue)>,
    seed: u64,
    /// Run in flight: ticket, settings and input it was started with.
    pending: Option<(u64, Values, u64, ScriptJob)>,
    /// Last result and what it was computed from.
    result: Option<(Values, u64, ScriptJob, Result<ScriptOutput, String>)>,
    /// Apply as soon as the result is in (Run last script again).
    pub auto_apply: bool,
    confirm: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ScriptDialogOutput {
    pub apply: Option<ScriptOutput>,
    pub close: bool,
}

impl ScriptDialog {
    pub fn new(info: ScriptInfo, last: Option<&ScriptRun>) -> Self {
        let mut values: Vec<(String, ParamValue)> = info
            .params
            .iter()
            .map(|p| (p.key.clone(), p.default.clone()))
            .collect();
        let mut seed = 1;
        if let Some(l) = last.filter(|l| l.id == info.id) {
            for (k, v) in &l.values {
                if let Some(slot) = values.iter_mut().find(|(key, _)| key == k) {
                    slot.1 = v.clone();
                }
            }
            seed = l.seed;
        }
        Self {
            info,
            values,
            seed,
            pending: None,
            result: None,
            auto_apply: false,
            confirm: false,
        }
    }

    /// Preview notes of the current result, if it is notes.
    pub fn preview(&self) -> Option<&[Note]> {
        match &self.result {
            Some((_, _, _, Ok(ScriptOutput::Notes(o)))) => Some(&o.notes),
            _ => None,
        }
    }

    /// Selection preview, if the script selects.
    pub fn preview_selection(&self) -> Option<&[usize]> {
        match &self.result {
            Some((_, _, _, Ok(ScriptOutput::Selection(s)))) => Some(s),
            _ => None,
        }
    }

    /// Starts a run when the settings or the notes changed; collects a
    /// finished one.
    pub fn poll(&mut self, host: &mut dyn Host, input: &Input) {
        if let Some((ticket, ..)) = &self.pending
            && let Some(r) = host.script_result(*ticket)
        {
            let (_, values, seed, job) = self.pending.take().expect("checked");
            self.result = Some((values, seed, job, r));
            self.confirm = false;
        }
        let job = ScriptJob::of(input);
        let current = |v: &Vec<(String, ParamValue)>, s: u64, j: &ScriptJob| {
            *v == self.values && s == self.seed && *j == job
        };
        let fresh = self
            .result
            .as_ref()
            .is_some_and(|(v, s, j, _)| current(v, *s, j))
            || self
                .pending
                .as_ref()
                .is_some_and(|(_, v, s, j)| current(v, *s, j));
        if !fresh {
            if self.pending.is_some() {
                host.cancel_script();
                self.pending = None;
            }
            let run = ScriptRun {
                id: self.info.id.clone(),
                hash: self.info.hash,
                values: self.values.clone(),
                seed: self.seed,
            };
            match host.start_script(run, job.clone()) {
                Some(t) => self.pending = Some((t, self.values.clone(), self.seed, job)),
                None => {
                    self.result = Some((
                        self.values.clone(),
                        self.seed,
                        job,
                        Err("scripts are not available".into()),
                    ));
                }
            }
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        host: &mut dyn Host,
        input: &Input,
    ) -> ScriptDialogOutput {
        let mut out = ScriptDialogOutput::default();
        self.poll(host, input);
        if self.pending.is_some() {
            ctx.request_repaint_after_secs(0.05);
        }
        if self.auto_apply {
            match &self.result {
                Some((_, _, _, Ok(ScriptOutput::Notes(o))))
                    if self.pending.is_none() && o.dropped == 0 =>
                {
                    out.apply = self.result.take().and_then(|r| r.3.ok());
                    out.close = true;
                    self.remember(ctx);
                    return out;
                }
                Some((_, _, _, Ok(ScriptOutput::Selection(_)))) if self.pending.is_none() => {
                    out.apply = self.result.take().and_then(|r| r.3.ok());
                    out.close = true;
                    self.remember(ctx);
                    return out;
                }
                _ if self.pending.is_some() => return out,
                // Errors and dropped notes: show the dialog.
                _ => self.auto_apply = false,
            }
        }
        let mut open = true;
        egui::Window::new(format!("Script · {}", self.info.name))
            .id(egui::Id::unique(("script_dialog", self.info.id.as_str())))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                if !self.info.description.is_empty() {
                    ui.label(RichText::new(&self.info.description).color(theme::MUTED));
                }
                ui.separator();
                egui::Grid::new("script_params")
                    .num_columns(2)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        for p in &self.info.params {
                            let Some((_, v)) = self.values.iter_mut().find(|(k, _)| *k == p.key)
                            else {
                                continue;
                            };
                            ui.label(&p.label);
                            match (&p.kind, v) {
                                (ParamKind::Int { min, max }, ParamValue::Int(x)) => {
                                    ui.add(egui::DragValue::new(x).range(*min..=*max));
                                }
                                (ParamKind::Float { min, max }, ParamValue::Float(x)) => {
                                    ui.add(egui::Slider::new(x, *min..=*max));
                                }
                                (ParamKind::Bool, ParamValue::Bool(b)) => {
                                    ui.checkbox(b, "");
                                }
                                (ParamKind::Choice(cs), ParamValue::Choice(i)) => {
                                    egui::ComboBox::from_id_salt(("script_choice", p.key.as_str()))
                                        .selected_text(cs.get(*i).map_or("?", String::as_str))
                                        .show_ui(ui, |ui| {
                                            for (k, c) in cs.iter().enumerate() {
                                                ui.selectable_value(i, k, c);
                                            }
                                        });
                                }
                                _ => {
                                    ui.label("?");
                                }
                            }
                            ui.end_row();
                        }
                        ui.label("Seed");
                        ui.add(egui::DragValue::new(&mut self.seed));
                        ui.end_row();
                    });
                ui.separator();
                if self.pending.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Running…");
                        if ui.button("Stop").clicked() {
                            host.cancel_script();
                        }
                    });
                }
                match &self.result {
                    Some((_, _, _, Err(e))) => {
                        ui.colored_label(theme::ERROR, e);
                    }
                    Some((_, _, _, Ok(ScriptOutput::Notes(o)))) => {
                        ui.label(format!(
                            "Preview: {} notes in the row, {} new or changed",
                            o.notes.len(),
                            o.select.len()
                        ));
                        for line in &o.info {
                            ui.colored_label(theme::WARN, line);
                        }
                    }
                    Some((_, _, _, Ok(ScriptOutput::Selection(s)))) => {
                        ui.label(format!(
                            "Selects {} note(s); the notes do not change",
                            s.len()
                        ));
                    }
                    None => {}
                }
                let ready =
                    self.pending.is_none() && matches!(&self.result, Some((_, _, _, Ok(_))));
                if self.confirm {
                    ui.colored_label(
                        theme::WARN,
                        "Some notes would leave the pattern. Apply without them?",
                    );
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready, egui::Button::new("Apply")).clicked()
                        && let Some((_, _, _, Ok(r))) = &self.result
                    {
                        let dropped = matches!(r, ScriptOutput::Notes(o) if o.dropped > 0);
                        if dropped && !self.confirm {
                            self.confirm = true;
                        } else {
                            out.apply = Some(r.clone());
                            out.close = true;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        out.close = true;
                    }
                    if ui.button("Reset").clicked() {
                        self.values = self
                            .info
                            .params
                            .iter()
                            .map(|p| (p.key.clone(), p.default.clone()))
                            .collect();
                        self.seed = 1;
                    }
                });
            });
        if !open {
            out.close = true;
        }
        if out.apply.is_some() {
            self.remember(ctx);
        }
        if out.close && self.pending.is_some() {
            host.cancel_script();
        }
        out
    }

    fn remember(&self, ctx: &egui::Context) {
        let run = ScriptRun {
            id: self.info.id.clone(),
            hash: self.info.hash,
            values: self.values.clone(),
            seed: self.seed,
        };
        ctx.data_mut(|d| d.insert_temp(last_id(), run));
    }
}
