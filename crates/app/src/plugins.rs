// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin catalog (scan cache and blocklist) and the live plugin
//! instances of the open project.
//!
//! Instances belong to the UI thread: the controller half stays here and
//! the processor half goes to the engine once, in the next snapshot
//! (see `jdance_engine::PluginRt`).

use crate::config;
use jdance_domain::{EffectKind, InsertId, PluginSlot, Project};
use jdance_engine::PluginRt;
use jdance_plugin_api::{PluginController, PluginDescriptor, PluginState, ProcessSetupInfo};
use jdance_plugin_vst3::{
    SCAN_TIMEOUT, ScanOutcome, Vst3Controller, Vst3Module, Vst3Processor, find_modules,
    instantiate, run_scanner,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

const CACHE_FILE: &str = "plugins.json";

/// Cache key of a module (PLG-022 step 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleKey {
    pub size: u64,
    pub mtime: u64,
    pub arch: String,
}

impl ModuleKey {
    fn of(path: &Path) -> Option<Self> {
        let binary = jdance_plugin_vst3::binary_path(path).ok()?;
        let meta = std::fs::metadata(&binary).ok()?;
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(Self {
            size: meta.len(),
            mtime,
            arch: std::env::consts::ARCH.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleResult {
    Ok(Vec<PluginDescriptor>),
    /// Blocklisted with the reason (PLG-024).
    Blocked(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleEntry {
    pub key: Option<ModuleKey>,
    pub result: ModuleResult,
}

/// Scan results by module path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub modules: BTreeMap<PathBuf, ModuleEntry>,
}

impl Catalog {
    pub fn cache_path() -> Option<PathBuf> {
        config::config_dir().map(|d| d.join(CACHE_FILE))
    }

    /// The cached catalog; empty when missing or unreadable.
    pub fn load_cached() -> Self {
        Self::cache_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_cached(&self) -> Result<(), String> {
        let path = Self::cache_path().ok_or("no configuration directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        jdance_persistence::write_atomically(&path, &bytes).map_err(|e| e.to_string())
    }

    pub fn plugins(&self) -> impl Iterator<Item = &PluginDescriptor> {
        self.modules.values().flat_map(|m| match &m.result {
            ModuleResult::Ok(d) => d.as_slice(),
            ModuleResult::Blocked(_) => &[],
        })
    }

    pub fn find(&self, class_id: &str) -> Option<&PluginDescriptor> {
        self.plugins().find(|d| d.plugin.class_id == class_id)
    }

    pub fn blocked(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.modules.iter().filter_map(|(p, m)| match &m.result {
            ModuleResult::Blocked(r) => Some((p.as_path(), r.as_str())),
            ModuleResult::Ok(_) => None,
        })
    }

    /// Scans modules under `roots` with `scanner` (a separate process per
    /// module). Unchanged modules keep their cached result; with `full`,
    /// every module is scanned again, blocklisted ones included
    /// (PLG-023/024).
    pub fn rescan(&self, roots: &[PathBuf], scanner: &Path, full: bool) -> Self {
        let mut out = Catalog::default();
        for path in find_modules(roots) {
            let key = ModuleKey::of(&path);
            if !full
                && let Some(old) = self.modules.get(&path)
                && old.key.is_some()
                && old.key == key
            {
                out.modules.insert(path, old.clone());
                continue;
            }
            let mut cmd = std::process::Command::new(scanner);
            cmd.arg("scan-plugin").arg(&path);
            let result = match run_scanner(cmd, SCAN_TIMEOUT) {
                ScanOutcome::Ok(d) => ModuleResult::Ok(d),
                ScanOutcome::Failed(reason) => ModuleResult::Blocked(reason),
            };
            out.modules.insert(path, ModuleEntry { key, result });
        }
        out
    }
}

/// Modules shared by live and export instances; a module stays loaded
/// while any instance from it exists.
pub type ModuleCache = Arc<Mutex<HashMap<PathBuf, Arc<Vst3Module>>>>;

fn module_for(cache: &ModuleCache, path: &Path) -> Result<Arc<Vst3Module>, String> {
    let mut map = cache
        .lock()
        .map_err(|_| "module cache poisoned".to_owned())?;
    if let Some(m) = map.get(path) {
        return Ok(Arc::clone(m));
    }
    let m = Arc::new(Vst3Module::load(path).map_err(|e| e.to_string())?);
    map.insert(path.to_path_buf(), Arc::clone(&m));
    Ok(m)
}

/// Creates an instance of the plugin in `slot` with its saved state.
pub fn create_instance(
    catalog: &Catalog,
    modules: &ModuleCache,
    slot: &PluginSlot,
    setup: &ProcessSetupInfo,
) -> Result<(Vst3Processor, Vst3Controller), String> {
    if slot.format != "vst3" {
        return Err(format!("unsupported plugin format {}", slot.format));
    }
    let d = catalog
        .find(&slot.class_id)
        .ok_or_else(|| format!("{} ({}) is not installed", slot.name, slot.vendor))?;
    let module = module_for(modules, Path::new(&d.path))?;
    let state = PluginState {
        component: slot.component_state.clone(),
        controller: slot.controller_state.clone(),
    };
    let has_state = !state.component.is_empty();
    instantiate(&module, &slot.class_id, setup, has_state.then_some(&state))
        .map_err(|e| e.to_string())
}

struct Live {
    /// Declared first: the editor closes before the instance is released
    /// (PLG-063).
    editor: Option<jdance_plugin_vst3::Vst3Editor>,
    class_id: String,
    controller: Vst3Controller,
    latency: u32,
    /// Waiting to be sent to the engine.
    processor: Option<Vst3Processor>,
    /// Engine generation the processor was sent to.
    generation: u64,
    /// The plugin asked for a restart (latency or bus change): recreate
    /// it with its current state.
    restart: bool,
}

/// Live instances of the open project's plugin inserts.
pub struct PluginHost {
    pub catalog: Catalog,
    pub modules: ModuleCache,
    live: HashMap<InsertId, Live>,
    /// Inserts whose plugin could not be created (reported once), with
    /// the reason.
    failed: HashMap<(InsertId, String), String>,
    /// Fresh processors handed to the last compile.
    handed: Vec<InsertId>,
    setup: ProcessSetupInfo,
}

impl std::fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("live", &self.live.len())
            .finish_non_exhaustive()
    }
}

/// Plugin inserts of a project: (insert, slot).
fn plugin_inserts(project: &Project) -> impl Iterator<Item = (InsertId, &PluginSlot)> {
    std::iter::once(&project.mixer.master)
        .chain(&project.mixer.tracks)
        .flat_map(|t| &t.inserts)
        .filter(|i| i.effect.kind == EffectKind::Plugin)
        .filter_map(|i| i.effect.plugin.as_ref().map(|s| (i.id, s)))
}

impl PluginHost {
    pub fn new(catalog: Catalog) -> Self {
        Self {
            catalog,
            modules: ModuleCache::default(),
            live: HashMap::new(),
            failed: HashMap::new(),
            handed: Vec::new(),
            setup: ProcessSetupInfo {
                sample_rate: 48_000.0,
                max_block: jdance_engine::MAX_BLOCK,
                offline: false,
            },
        }
    }

    /// Makes the live instances match `project` for an engine of
    /// `generation` at `sample_rate`. Instances sent to an older engine
    /// are recreated with their current state (PLG-044). Returns messages
    /// for plugins that could not be created.
    pub fn sync(&mut self, project: &Project, sample_rate: u32, generation: u64) -> Vec<String> {
        let setup = ProcessSetupInfo {
            sample_rate: f64::from(sample_rate),
            ..self.setup
        };
        let wanted: HashMap<InsertId, &PluginSlot> = plugin_inserts(project).collect();
        // Drop removed or replaced instances.
        self.live
            .retain(|id, l| wanted.get(id).is_some_and(|s| s.class_id == l.class_id));
        let rebuild = setup != self.setup;
        self.setup = setup;
        let mut messages = Vec::new();
        for (id, slot) in wanted {
            let stale = self.live.get(&id).is_some_and(|l| {
                rebuild || l.restart || (l.processor.is_none() && l.generation != generation)
            });
            let mut slot = slot.clone();
            if stale
                && let Some(mut l) = self.live.remove(&id)
                && let Ok(s) = l.controller.state()
            {
                slot.component_state = s.component;
                slot.controller_state = s.controller;
            }
            if self.live.contains_key(&id) {
                continue;
            }
            match create_instance(&self.catalog, &self.modules, &slot, &self.setup) {
                Ok((processor, controller)) => {
                    self.failed.retain(|(i, _), _| *i != id);
                    self.live.insert(
                        id,
                        Live {
                            editor: None,
                            class_id: slot.class_id.clone(),
                            latency: jdance_plugin_api::PluginProcessor::latency_samples(
                                &processor,
                            ),
                            controller,
                            processor: Some(processor),
                            generation,
                            restart: false,
                        },
                    );
                }
                Err(e) => {
                    let msg = format!("Plugin {}: {e}", slot.name);
                    if self.failed.insert((id, slot.class_id.clone()), e).is_none() {
                        messages.push(msg);
                    }
                }
            }
        }
        messages
    }

    /// The engine slot for `id`, used while compiling. A waiting instance
    /// goes into this snapshot; a sent one is carried over.
    pub fn supply(&mut self, id: InsertId, generation: u64) -> PluginRt {
        match self.live.get_mut(&id) {
            Some(l) => match l.processor.take() {
                Some(p) => {
                    l.generation = generation;
                    self.handed.push(id);
                    PluginRt::fresh(Box::new(p))
                }
                None => PluginRt::carried(l.latency),
            },
            None => PluginRt::missing(),
        }
    }

    /// A slot with the instance's latency but without the instance, for
    /// snapshots that are only measured (no engine).
    pub fn latency_only(&self, id: InsertId) -> PluginRt {
        self.live
            .get(&id)
            .map_or_else(PluginRt::missing, |l| PluginRt::carried(l.latency))
    }

    /// The last compiled snapshot reached the engine.
    pub fn delivered(&mut self) {
        self.handed.clear();
    }

    /// The last compiled snapshot was dropped: its fresh instances are
    /// gone and are recreated on the next sync.
    pub fn undelivered(&mut self) {
        for id in self.handed.drain(..) {
            if let Some(l) = self.live.get_mut(&id) {
                l.generation = u64::MAX;
            }
        }
    }

    /// Writes the current plugin states into `project` (before saving or
    /// exporting, PLG-070).
    pub fn capture_states(&mut self, project: &mut Project) {
        let tracks = std::iter::once(&mut project.mixer.master).chain(&mut project.mixer.tracks);
        for ins in tracks.flat_map(|t| &mut t.inserts) {
            let (Some(live), Some(slot)) = (self.live.get_mut(&ins.id), &mut ins.effect.plugin)
            else {
                continue;
            };
            if let Ok(s) = live.controller.state() {
                slot.component_state = s.component;
                slot.controller_state = s.controller;
            }
        }
    }

    /// Forwards plugin events (GUI edits, output parameters). Returns
    /// whether a plugin needs a restart, which needs a recompile: a new
    /// latency is read after reactivation and applied through a new
    /// snapshot (PLG-035, PLG-051).
    pub fn poll(&mut self) -> bool {
        use jdance_plugin_api::PluginEvent::{LatencyChanged, ReloadRequested};
        let mut restart = false;
        for l in self.live.values_mut() {
            for e in l.controller.poll() {
                if matches!(e, LatencyChanged | ReloadRequested) {
                    l.restart = true;
                    restart = true;
                }
            }
        }
        restart
    }

    /// Opens or closes the plugin's own editor window. `Err` explains
    /// why it cannot open (no editor, no window system, plugin not loaded).
    pub fn toggle_editor(&mut self, id: InsertId, title: &str) -> Result<(), String> {
        let live = self
            .live
            .get_mut(&id)
            .ok_or_else(|| "the plugin is not loaded".to_owned())?;
        if live.editor.take().is_some() {
            return Ok(());
        }
        match live.controller.open_editor(title) {
            Ok(Some(e)) => {
                live.editor = Some(e);
                Ok(())
            }
            Ok(None) => Err("this plugin has no editor; use Parameters".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn editor_open(&self, id: InsertId) -> bool {
        self.live.get(&id).is_some_and(|l| l.editor.is_some())
    }

    pub fn any_editor_open(&self) -> bool {
        self.live.values().any(|l| l.editor.is_some())
    }

    /// Runs open editors; closes those the user closed.
    pub fn poll_editors(&mut self) {
        for l in self.live.values_mut() {
            if let Some(e) = &mut l.editor
                && !e.poll()
            {
                l.editor = None;
            }
        }
    }

    pub fn controller(&mut self, id: InsertId) -> Option<&mut Vst3Controller> {
        self.live.get_mut(&id).map(|l| &mut l.controller)
    }

    pub fn problem(&self, id: InsertId) -> Option<String> {
        if self.live.contains_key(&id) {
            return None;
        }
        self.failed
            .iter()
            .find(|((i, _), _)| *i == id)
            .map(|(_, e)| e.clone())
    }
}
