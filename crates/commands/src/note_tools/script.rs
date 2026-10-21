// SPDX-License-Identifier: GPL-3.0-or-later
//! Data shared by the script host and the editor (spec 0.2.0 §18.22):
//! a script's parameter schema, values and result. Running scripts is
//! the host's job (crate `jdance-scripts`).

use super::Outcome;

#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    Int { min: i64, max: i64 },
    Float { min: f64, max: f64 },
    Bool,
    Choice(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Index into the choices.
    Choice(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScriptParam {
    pub key: String,
    pub label: String,
    pub kind: ParamKind,
    pub default: ParamValue,
}

/// A script as the editor sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptInfo {
    /// Stable key: `builtin:<name>` or the file path.
    pub id: String,
    pub name: String,
    pub description: String,
    pub params: Vec<ScriptParam>,
    /// Content hash: "Run last script again" asks again when it changes.
    pub hash: u64,
    /// Changes the selection only.
    pub selects: bool,
}

/// What a script produced.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptOutput {
    Notes(Outcome),
    /// Indices into the row's notes.
    Selection(Vec<usize>),
}

/// A script run: which script, its parameter values and seed.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptRun {
    pub id: String,
    pub hash: u64,
    pub values: Vec<(String, ParamValue)>,
    pub seed: u64,
}
