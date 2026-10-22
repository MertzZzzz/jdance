// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano Roll script tools (spec 0.2.0 §18.22) on an embedded Rhai
//! runtime. This is jdance's own small API, not compatible with any other
//! application's scripting.
//!
//! Sandbox (a capability model, not OS isolation): scripts are plain
//! text; there is no module loading, `eval`, file, network or process
//! access, and output goes nowhere. Operations, call depth, expression
//! depth, string/array/map sizes and run time are bounded; a cancel flag
//! stops a run at the next operation check. A script never changes the
//! project: it returns notes or a selection, which the host validates and
//! applies as one undo step.
//!
//! A script defines `params()` (an array of
//! `#{key, label, kind: "int"|"float"|"bool"|"choice", min, max, init,
//! choices}`) and `run(ctx)`. `ctx` holds `notes` (every note of the row:
//! `#{id, start, length, key, velocity, selected}`, velocity 1..=127),
//! `scope` (the notes to work on), `ppq`, `beats_per_bar`, `beat_ticks`,
//! `pattern_len`, `grid`,
//! `range_start`, `range_end` and `params`. `run` returns `#{notes: [..]}`
//! (the scope's replacement; `mode: "range"` replaces the notes starting
//! in the range instead, `mode: "append"` keeps everything) or
//! `#{select: [ids]}`. Host functions: `random()` (0..1), `random_int(lo,
//! hi)`, `bjorklund(steps, pulses)`, `arpeggiate(direction, step, gate,
//! octaves)` (the built-in tool's notes for the scope).

use jdance_commands::note_tools::{
    self as nt, Cand, Input, ParamKind, ParamValue, Rng, ScriptInfo, ScriptOutput, ScriptParam,
    ScriptRun,
};
use rhai::{Array, Dynamic, Engine, Map, Scope};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Budget of one run.
pub const MAX_OPERATIONS: u64 = 200_000_000;
pub const MAX_RUN_TIME: Duration = Duration::from_secs(10);
const MAX_SCRIPT_BYTES: usize = 256 * 1024;

/// A loaded script.
#[derive(Debug, Clone)]
pub struct Script {
    pub info: ScriptInfo,
    source: String,
    pub path: Option<PathBuf>,
}

const BUILTIN: [(&str, &str); 5] = [
    ("Arpeggiator", include_str!("../builtin/arpeggiator.rhai")),
    ("Euclidean", include_str!("../builtin/euclidean.rhai")),
    ("Humanize", include_str!("../builtin/humanize.rhai")),
    (
        "Note repeater sprinkler",
        include_str!("../builtin/note_repeater_sprinkler.rhai"),
    ),
    (
        "Select by velocity and more",
        include_str!("../builtin/select_by_velocity.rhai"),
    ),
];

/// FNV-1a, for "did the script change".
fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// An engine with the sandbox limits; `rng` backs `random()`.
fn engine(cancel: Option<Arc<AtomicBool>>, rng: Rc<RefCell<Rng>>) -> Engine {
    let mut e = Engine::new();
    e.set_max_operations(MAX_OPERATIONS);
    e.set_max_call_levels(32);
    e.set_max_expr_depths(64, 32);
    e.set_max_string_size(64 * 1024);
    e.set_max_array_size(nt::MAX_RESULT_NOTES + 1024);
    e.set_max_map_size(1024);
    e.disable_symbol("eval");
    e.on_print(|_| {});
    e.on_debug(|_, _, _| {});
    let started = Instant::now();
    e.on_progress(move |ops| {
        let cancelled = cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
        let late = ops % 4096 == 0 && started.elapsed() > MAX_RUN_TIME;
        (cancelled || late).then(|| Dynamic::from("stopped"))
    });
    let r = Rc::clone(&rng);
    e.register_fn("random", move || r.borrow_mut().unit() as f32);
    let r = Rc::clone(&rng);
    e.register_fn("random_int", move |lo: i64, hi: i64| {
        r.borrow_mut().range(lo, hi)
    });
    e.register_fn("bjorklund", |steps: i64, pulses: i64| -> Array {
        bjorklund(steps.clamp(0, 1024) as usize, pulses.max(0) as usize)
            .into_iter()
            .map(Dynamic::from)
            .collect()
    });
    e
}

/// Euclidean rhythm by Bjorklund's algorithm, in normal form (starting
/// with a hit when `pulses > 0`): `pulses` hits spread over `steps`.
pub fn bjorklund(steps: usize, pulses: usize) -> Vec<bool> {
    let pulses = pulses.min(steps);
    if steps == 0 {
        return Vec::new();
    }
    if pulses == 0 {
        return vec![false; steps];
    }
    // Groups of sequences: pair the remainder with the front until at
    // most one remainder group is left.
    let mut a: Vec<Vec<bool>> = vec![vec![true]; pulses];
    let mut b: Vec<Vec<bool>> = vec![vec![false]; steps - pulses];
    while b.len() > 1 {
        let n = a.len().min(b.len());
        let mut na = Vec::with_capacity(n);
        for i in 0..n {
            let mut s = a[i].clone();
            s.extend_from_slice(&b[i]);
            na.push(s);
        }
        let rest: Vec<Vec<bool>> = if a.len() > n {
            a[n..].to_vec()
        } else {
            b[n..].to_vec()
        };
        a = na;
        b = rest;
    }
    let mut out: Vec<bool> = a.into_iter().chain(b).flatten().collect();
    // Normal form: start on a hit.
    if let Some(first) = out.iter().position(|h| *h) {
        out.rotate_left(first);
    }
    out
}

fn str_of(m: &Map, k: &str) -> Option<String> {
    m.get(k).and_then(|v| v.clone().into_string().ok())
}

fn num_of(m: &Map, k: &str) -> Option<f64> {
    let v = m.get(k)?;
    v.as_int()
        .map(|i| i as f64)
        .or_else(|_| v.as_float().map(f64::from))
        .ok()
}

fn parse_params(v: Dynamic) -> Result<Vec<ScriptParam>, String> {
    let arr = v
        .into_array()
        .map_err(|t| format!("params() must return an array, not {t}"))?;
    let mut out = Vec::new();
    for item in arr {
        let m = item
            .try_cast::<Map>()
            .ok_or("each parameter must be a map")?;
        let key = str_of(&m, "key").ok_or("a parameter has no key")?;
        let label = str_of(&m, "label").unwrap_or_else(|| key.clone());
        let kind = str_of(&m, "kind").unwrap_or_else(|| "int".into());
        let (kind, default) = match kind.as_str() {
            "int" => {
                let min = num_of(&m, "min").unwrap_or(0.0) as i64;
                let max = num_of(&m, "max").unwrap_or(100.0) as i64;
                let d = num_of(&m, "init").unwrap_or(min as f64) as i64;
                (
                    ParamKind::Int { min, max },
                    ParamValue::Int(d.clamp(min, max.max(min))),
                )
            }
            "float" => {
                let min = num_of(&m, "min").unwrap_or(0.0);
                let max = num_of(&m, "max").unwrap_or(1.0);
                let d = num_of(&m, "init").unwrap_or(min);
                (
                    ParamKind::Float { min, max },
                    ParamValue::Float(d.clamp(min, max.max(min))),
                )
            }
            "bool" => {
                let d = m
                    .get("init")
                    .and_then(|v| v.as_bool().ok())
                    .unwrap_or(false);
                (ParamKind::Bool, ParamValue::Bool(d))
            }
            "choice" => {
                let choices: Vec<String> = m
                    .get("choices")
                    .and_then(|v| v.clone().into_array().ok())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|c| c.into_string().ok())
                    .collect();
                if choices.is_empty() {
                    return Err(format!("choice parameter {key} has no choices"));
                }
                let d = num_of(&m, "init").unwrap_or(0.0).max(0.0) as usize;
                let d = d.min(choices.len() - 1);
                (ParamKind::Choice(choices), ParamValue::Choice(d))
            }
            k => return Err(format!("unknown parameter kind {k}")),
        };
        out.push(ScriptParam {
            key,
            label,
            kind,
            default,
        });
    }
    Ok(out)
}

/// `// name:` and `// description:` header lines.
fn header(source: &str, key: &str) -> Option<String> {
    source.lines().take(20).find_map(|l| {
        l.trim()
            .strip_prefix("//")
            .map(str::trim)
            .and_then(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_owned())
    })
}

impl Script {
    /// Compiles `source` and reads its parameters (no project access).
    pub fn parse(
        id: String,
        fallback_name: &str,
        source: &str,
        path: Option<PathBuf>,
    ) -> Result<Self, String> {
        if source.len() > MAX_SCRIPT_BYTES {
            return Err("the script is too large".into());
        }
        let e = engine(None, Rc::new(RefCell::new(Rng::new(0))));
        let ast = e
            .compile(source)
            .map_err(|err| format!("{fallback_name}: {err}"))?;
        let params = e
            .call_fn::<Dynamic>(&mut Scope::new(), &ast, "params", ())
            .map_err(|err| format!("{fallback_name}: params(): {err}"))
            .and_then(parse_params)?;
        let name = header(source, "name:").unwrap_or_else(|| fallback_name.to_owned());
        let description = header(source, "description:").unwrap_or_default();
        let selects = header(source, "result:").is_some_and(|r| r == "selection");
        Ok(Self {
            info: ScriptInfo {
                id,
                name,
                description,
                params,
                hash: hash(source),
                selects,
            },
            source: source.to_owned(),
            path,
        })
    }

    /// Runs with `run.values` (missing ones take defaults) on `input`.
    /// Errors, timeouts and cancellation return `Err`; nothing else is
    /// touched.
    pub fn run(
        &self,
        run: &ScriptRun,
        input: &Input,
        cancel: Arc<AtomicBool>,
    ) -> Result<ScriptOutput, String> {
        let rng = Rc::new(RefCell::new(Rng::new(run.seed)));
        let mut e = engine(Some(cancel), Rc::clone(&rng));
        // The built-in arpeggiator, shared with Tools > Arpeggiate.
        let snapshot: Vec<jdance_domain::Note> = input.notes.to_vec();
        let selected: Vec<usize> = input.selected.to_vec();
        let (len, grid, range, signature) =
            (input.pattern_len, input.grid, input.range, input.signature);
        let seed = run.seed;
        e.register_fn(
            "arpeggiate",
            move |direction: &str,
                  step: i64,
                  gate: i64,
                  octaves: i64|
                  -> Result<Array, Box<rhai::EvalAltResult>> {
                let direction = match direction {
                    "up" => nt::ArpDirection::Up,
                    "down" => nt::ArpDirection::Down,
                    "up-down" => nt::ArpDirection::UpDown,
                    "down-up" => nt::ArpDirection::DownUp,
                    "random" => nt::ArpDirection::Random,
                    d => return Err(format!("unknown direction {d}").into()),
                };
                let s = nt::Arpeggiate {
                    direction,
                    step: step.clamp(1, 1 << 20) as u32,
                    gate_percent: gate.clamp(1, 100) as u32,
                    octaves: octaves.clamp(1, 4) as u8,
                    seed,
                };
                let inp = Input {
                    notes: &snapshot,
                    selected: &selected,
                    pattern_len: len,
                    grid,
                    range,
                    signature,
                };
                let o = nt::arpeggiate(&inp, &s)
                    .map_err(|e| -> Box<rhai::EvalAltResult> { e.into() })?;
                Ok(o.select
                    .iter()
                    .map(|&i| note_map(&o.notes[i], false))
                    .collect())
            },
        );
        let ast = e.compile(&self.source).map_err(|err| err.to_string())?;
        let ctx = context(self, run, input);
        let result = e
            .call_fn::<Dynamic>(&mut Scope::new(), &ast, "run", (ctx,))
            .map_err(|err| match *err {
                rhai::EvalAltResult::ErrorTerminated(..) => {
                    "the script was stopped (cancelled or took too long)".to_owned()
                }
                rhai::EvalAltResult::ErrorTooManyOperations(..) => {
                    "the script used too many operations".to_owned()
                }
                other => other.to_string(),
            })?;
        output(result, input)
    }
}

fn note_map(n: &jdance_domain::Note, selected: bool) -> Dynamic {
    let mut m = Map::new();
    m.insert("id".into(), Dynamic::from(i64::from(n.id)));
    m.insert("start".into(), Dynamic::from(i64::from(n.start)));
    m.insert("length".into(), Dynamic::from(i64::from(n.length)));
    m.insert("key".into(), Dynamic::from(i64::from(n.key)));
    m.insert(
        "velocity".into(),
        Dynamic::from(i64::from(nt::midi_velocity(n.velocity))),
    );
    m.insert("selected".into(), Dynamic::from(selected));
    Dynamic::from_map(m)
}

fn context(script: &Script, run: &ScriptRun, input: &Input) -> Dynamic {
    let selected: std::collections::BTreeSet<usize> = input.selected.iter().copied().collect();
    let notes: Array = input
        .notes
        .iter()
        .enumerate()
        .map(|(i, n)| note_map(n, selected.contains(&i)))
        .collect();
    let scope: Array = input
        .scope()
        .into_iter()
        .map(|i| note_map(&input.notes[i], selected.contains(&i)))
        .collect();
    let mut params = Map::new();
    for p in &script.info.params {
        let v = run
            .values
            .iter()
            .find(|(k, _)| *k == p.key)
            .map_or(&p.default, |(_, v)| v);
        let d = match v {
            ParamValue::Int(i) => Dynamic::from(*i),
            ParamValue::Float(f) => Dynamic::from(*f as f32),
            ParamValue::Bool(b) => Dynamic::from(*b),
            ParamValue::Choice(c) => match &p.kind {
                ParamKind::Choice(cs) => Dynamic::from(cs.get(*c).cloned().unwrap_or_default()),
                _ => Dynamic::from(*c as i64),
            },
        };
        params.insert(p.key.as_str().into(), d);
    }
    let r = input.gen_range();
    let mut m = Map::new();
    m.insert("notes".into(), Dynamic::from_array(notes));
    m.insert("scope".into(), Dynamic::from_array(scope));
    m.insert("ppq".into(), Dynamic::from(i64::from(jdance_domain::PPQ)));
    // The project signature (spec 0.2.3 SIG-003).
    m.insert(
        "beats_per_bar".into(),
        Dynamic::from(i64::from(input.signature.numerator)),
    );
    m.insert(
        "beat_ticks".into(),
        Dynamic::from(i64::from(input.signature.beat_ticks())),
    );
    m.insert(
        "pattern_len".into(),
        Dynamic::from(i64::from(input.pattern_len)),
    );
    m.insert("grid".into(), Dynamic::from(i64::from(input.grid())));
    m.insert("range_start".into(), Dynamic::from(i64::from(r.start)));
    m.insert("range_end".into(), Dynamic::from(i64::from(r.end)));
    m.insert("params".into(), Dynamic::from_map(params));
    Dynamic::from_map(m)
}

/// Validates the script's result against the row.
fn output(result: Dynamic, input: &Input) -> Result<ScriptOutput, String> {
    let m = result
        .try_cast::<Map>()
        .ok_or("run() must return a map with notes or select")?;
    if let Some(sel) = m.get("select") {
        let ids = sel
            .clone()
            .into_array()
            .map_err(|_| "select must be an array of note IDs")?;
        let mut out = Vec::new();
        for id in ids {
            let id = id.as_int().map_err(|_| "select must hold note IDs")?;
            let i = input
                .notes
                .iter()
                .position(|n| i64::from(n.id) == id)
                .ok_or_else(|| format!("select names an unknown note ID {id}"))?;
            out.push(i);
        }
        out.sort_unstable();
        out.dedup();
        return Ok(ScriptOutput::Selection(out));
    }
    let notes = m
        .get("notes")
        .ok_or("run() returned neither notes nor select")?
        .clone()
        .into_array()
        .map_err(|_| "notes must be an array")?;
    if notes.len() > nt::MAX_RESULT_NOTES {
        return Err(format!(
            "the script returned {} notes; at most {} are allowed",
            notes.len(),
            nt::MAX_RESULT_NOTES
        ));
    }
    let scope = input.scope();
    let known: std::collections::BTreeSet<u32> = scope.iter().map(|&i| input.notes[i].id).collect();
    let mut cands = Vec::with_capacity(notes.len());
    for (k, n) in notes.into_iter().enumerate() {
        let n = n
            .try_cast::<Map>()
            .ok_or_else(|| format!("note {k} is not a map"))?;
        let get = |key: &str| -> Result<i64, String> {
            n.get(key)
                .and_then(|v| v.as_int().ok())
                .ok_or_else(|| format!("note {k} has no whole-number {key}"))
        };
        let id = n.get("id").and_then(|v| v.as_int().ok()).unwrap_or(0);
        // Only scope notes may keep their ID; anything else is new.
        let id = u32::try_from(id)
            .ok()
            .filter(|i| known.contains(i))
            .unwrap_or(0);
        let velocity = get("velocity")?;
        if !(1..=127).contains(&velocity) {
            return Err(format!("note {k}: velocity {velocity} is outside 1..127"));
        }
        let key = get("key")?;
        if !(0..=127).contains(&key) {
            return Err(format!("note {k}: key {key} is outside 0..127"));
        }
        cands.push(Cand {
            id,
            start: get("start")?,
            length: get("length")?,
            key: key as i32,
            velocity: velocity as i32,
        });
    }
    let removed = match m
        .get("mode")
        .and_then(|v| v.clone().into_string().ok())
        .as_deref()
    {
        None | Some("scope") => scope,
        Some("range") => nt::starting_in(input, input.gen_range()),
        Some("append") => Vec::new(),
        Some(other) => return Err(format!("unknown mode {other}")),
    };
    // Each kept ID only once (the first wins).
    let mut seen = std::collections::BTreeSet::new();
    for c in &mut cands {
        if c.id != 0 && !seen.insert(c.id) {
            c.id = 0;
        }
    }
    nt::replace(input, &removed, cands).map(ScriptOutput::Notes)
}

/// The scripts that ship with jdance.
pub fn builtins() -> Vec<Script> {
    BUILTIN
        .iter()
        .map(|(name, src)| {
            Script::parse(format!("builtin:{name}"), name, src, None)
                .unwrap_or_else(|e| panic!("built-in script {name}: {e}"))
        })
        .collect()
}

/// User scripts (`*.rhai`) in `dir`, sorted by name, and the files that
/// did not load. Nothing runs at load time except `params()`.
pub fn load_folder(dir: &Path) -> (Vec<Script>, Vec<String>) {
    let mut ok = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (ok, errors);
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "rhai"))
        .collect();
    paths.sort();
    for p in paths {
        let name = p
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        match std::fs::read_to_string(&p) {
            Ok(src) => match Script::parse(p.display().to_string(), &name, &src, Some(p.clone())) {
                Ok(s) => ok.push(s),
                Err(e) => errors.push(e),
            },
            Err(e) => errors.push(format!("{}: {e}", p.display())),
        }
    }
    (ok, errors)
}

/// Default range of a run for tests.
#[cfg(test)]
fn full(len: u32) -> Option<nt::Range> {
    Some(nt::Range { start: 0, end: len })
}

#[cfg(test)]
mod tests;
