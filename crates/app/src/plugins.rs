// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin catalog (scan cache and blocklist) and the live plugin
//! instances of the open project.
//!
//! Instances belong to the UI thread: the controller half stays here and
//! the processor half goes to the engine once, in the next snapshot
//! (see `jdance_engine::PluginRt`).

use crate::config;
use jdance_domain::{ChannelSource, EffectKind, PluginSlot, PluginTarget, Project};
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
/// Written while a plugin is being instantiated; found at start-up it
/// means the instantiation crashed the application (PLG-025).
const LOADING_MARKER: &str = "plugin-loading.json";

#[derive(Debug, Serialize, Deserialize)]
struct LoadingMarker {
    class_id: String,
    name: String,
}

fn marker_path() -> Option<PathBuf> {
    config::config_dir().map(|d| d.join(LOADING_MARKER))
}

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
    /// Classes whose instantiation crashed the application: not loaded
    /// automatically until the user asks (PLG-025). Class id → name.
    #[serde(default)]
    pub crashed: BTreeMap<String, String>,
    /// Version of jdance that made the scan: a newer one scans the
    /// blocklisted modules again, since it may load them now.
    #[serde(default)]
    pub scanned_by: String,
}

/// This build, as recorded in [`Catalog::scanned_by`].
const SCANNER_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    /// `progress` gets (done, total, module) before each module; when
    /// `cancel` is set, the remaining modules keep their cached results.
    pub fn rescan(
        &self,
        roots: &[PathBuf],
        scanner: &Path,
        full: bool,
        progress: &dyn Fn(usize, usize, &Path),
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Self {
        let mut out = Catalog {
            crashed: self.crashed.clone(),
            scanned_by: SCANNER_VERSION.to_owned(),
            ..Catalog::default()
        };
        let newer = self.scanned_by != SCANNER_VERSION;
        let modules = find_modules(roots);
        let total = modules.len();
        for (i, path) in modules.into_iter().enumerate() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(old) = self.modules.get(&path) {
                    out.modules.insert(path, old.clone());
                }
                continue;
            }
            progress(i, total, &path);
            let key = ModuleKey::of(&path);
            if !full
                && let Some(old) = self.modules.get(&path)
                && old.key.is_some()
                && old.key == key
                && !(newer && matches!(old.result, ModuleResult::Blocked(_)))
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
    /// The plugin asked for a restart (bus change or reload): recreate it
    /// with its current state.
    restart: bool,
    /// After a latency change notice, the latency is read again until
    /// then: some plugins announce it before their processor has the new
    /// value.
    latency_check: Option<std::time::Instant>,
}

/// Time the UI thread spends creating instances in one sync (spec 0.3.0
/// §8.2); what is left waits for the next frame.
const LOAD_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);
/// A plugin slower than this to create is reported (§8.4).
const SLOW_LOAD: std::time::Duration = std::time::Duration::from_secs(2);

/// Reads a plugin's binary through once, into the system's file cache.
/// Errors are left to the real load.
fn warm_up(bundle: &Path) {
    use std::io::Read;
    let Ok(binary) = jdance_plugin_vst3::binary_path(bundle) else {
        return;
    };
    let Ok(mut f) = std::fs::File::open(binary) else {
        return;
    };
    let mut buf = vec![0u8; 1 << 20];
    while matches!(f.read(&mut buf), Ok(n) if n > 0) {}
}

/// Live instances of the open project's plugin inserts.
pub struct PluginHost {
    pub catalog: Catalog,
    pub modules: ModuleCache,
    /// Module files loading on a worker thread (§8.1).
    preloading: Arc<Mutex<std::collections::HashSet<PathBuf>>>,
    /// A preload finished since the last poll.
    preloaded: Arc<std::sync::atomic::AtomicBool>,
    /// Instances the last sync left for later: (waiting, wanted).
    progress: (usize, usize),
    /// Some were left only because the frame's time was used up.
    over_budget: bool,
    /// Name of the plugin loading next, for the status line.
    loading_name: Option<String>,
    /// How long each class took to create, slow ones reported once.
    load_times: HashMap<String, std::time::Duration>,
    live: HashMap<PluginTarget, Live>,
    /// Inserts whose plugin could not be created (reported once), with
    /// the reason.
    failed: HashMap<(PluginTarget, String), String>,
    /// Fresh processors handed to the last compile.
    handed: Vec<PluginTarget>,
    setup: ProcessSetupInfo,
}

impl std::fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("live", &self.live.len())
            .finish_non_exhaustive()
    }
}

/// Plugin slots of a project: inserts and instrument channels.
pub fn plugin_slots(project: &Project) -> impl Iterator<Item = (PluginTarget, &PluginSlot)> {
    let inserts = std::iter::once(&project.mixer.master)
        .chain(&project.mixer.tracks)
        .flat_map(|t| &t.inserts)
        .filter(|i| i.effect.kind == EffectKind::Plugin)
        .filter_map(|i| {
            i.effect
                .plugin
                .as_ref()
                .map(|s| (PluginTarget::Insert(i.id), s))
        });
    let channels = project
        .channels
        .iter()
        .filter_map(|c| c.instrument().map(|s| (PluginTarget::Channel(c.id), s)));
    inserts.chain(channels)
}

impl PluginHost {
    /// Also checks for a plugin that crashed the previous run; its name is
    /// returned for a notice.
    pub fn new(mut catalog: Catalog) -> (Self, Option<String>) {
        let mut crashed = None;
        if let Some(path) = marker_path()
            && let Ok(bytes) = std::fs::read(&path)
        {
            if let Ok(m) = serde_json::from_slice::<LoadingMarker>(&bytes) {
                crashed = Some(m.name.clone());
                catalog.crashed.insert(m.class_id, m.name);
                let _ = catalog.save_cached();
            }
            let _ = std::fs::remove_file(&path);
        }
        (Self::with_catalog(catalog), crashed)
    }

    pub fn with_catalog(catalog: Catalog) -> Self {
        Self {
            catalog,
            modules: ModuleCache::default(),
            preloading: Arc::default(),
            preloaded: Arc::default(),
            progress: (0, 0),
            over_budget: false,
            loading_name: None,
            load_times: HashMap::new(),
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
        let wanted: HashMap<PluginTarget, &PluginSlot> = plugin_slots(project).collect();
        let started = std::time::Instant::now();
        let total = wanted.len();
        let mut waiting = 0;
        self.loading_name = None;
        self.over_budget = false;
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
            if self.catalog.crashed.contains_key(&slot.class_id) {
                let reason = "crashed the application when it was last loaded".to_owned();
                if self
                    .failed
                    .insert((id, slot.class_id.clone()), reason)
                    .is_none()
                {
                    messages.push(format!(
                        "Plugin {} was not loaded: it crashed the application last time. \
                         Use \"Load again\" in its slot to retry.",
                        slot.name
                    ));
                }
                continue;
            }
            // The module is still loading in the background, or this frame's
            // time is used up: next frame (spec 0.3.0 §8.1, §8.2).
            let path = self
                .catalog
                .find(&slot.class_id)
                .map(|d| PathBuf::from(&d.path));
            let preloading = path
                .as_ref()
                .is_some_and(|p| self.preloading.lock().is_ok_and(|set| set.contains(p)));
            if preloading || started.elapsed() > LOAD_BUDGET {
                self.over_budget |= !preloading;
                waiting += 1;
                self.loading_name.get_or_insert_with(|| slot.name.clone());
                continue;
            }
            let marker = marker_path();
            if let Some(path) = &marker
                && let Ok(bytes) = serde_json::to_vec(&LoadingMarker {
                    class_id: slot.class_id.clone(),
                    name: slot.name.clone(),
                })
            {
                let _ = jdance_persistence::write_atomically(path, &bytes);
            }
            let t = std::time::Instant::now();
            let created = create_instance(&self.catalog, &self.modules, &slot, &self.setup);
            if let Some(path) = &marker {
                let _ = std::fs::remove_file(path);
            }
            let took = t.elapsed();
            if took > SLOW_LOAD
                && self
                    .load_times
                    .insert(slot.class_id.clone(), took)
                    .is_none()
            {
                messages.push(format!(
                    "{} took {:.1} s to load (it reads its content from disk)",
                    slot.name,
                    took.as_secs_f64()
                ));
            }
            match created {
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
                            latency_check: None,
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
        self.progress = (waiting, total);
        messages
    }

    /// Reads the module files of `project`'s plugins on a worker thread,
    /// so loading them later on the UI thread finds them in the system's
    /// file cache instead of waiting for the disk (spec 0.3.0 §8.1).
    ///
    /// Only the file is read there. The module itself (`InitDll`,
    /// `ModuleEntry`, the factory) is loaded on the UI thread: plugins
    /// built with JUCE and others tie their window message loop to the
    /// thread that initialises them, and from a worker thread their
    /// editors stayed black and hung (0.3.1).
    pub fn preload(&self, project: &Project) {
        let paths: std::collections::BTreeSet<PathBuf> = plugin_slots(project)
            .filter_map(|(_, s)| self.catalog.find(&s.class_id))
            .map(|d| PathBuf::from(&d.path))
            .collect();
        for path in paths {
            let loaded = self.modules.lock().is_ok_and(|m| m.contains_key(&path));
            let Ok(mut set) = self.preloading.lock() else {
                return;
            };
            if loaded || !set.insert(path.clone()) {
                continue;
            }
            drop(set);
            let (preloading, done, file) = (
                Arc::clone(&self.preloading),
                Arc::clone(&self.preloaded),
                path.clone(),
            );
            let spawned = std::thread::Builder::new()
                .name("jdance-plugin-preload".into())
                .spawn(move || {
                    warm_up(&file);
                    if let Ok(mut set) = preloading.lock() {
                        set.remove(&file);
                    }
                    done.store(true, std::sync::atomic::Ordering::Release);
                });
            // No thread: the instance loads the module itself.
            if spawned.is_err()
                && let Ok(mut set) = self.preloading.lock()
            {
                set.remove(&path);
            }
        }
    }

    /// Instances still to load and all wanted (§8.3), with the next one's
    /// name; `None` when everything is loaded.
    pub fn loading(&self) -> Option<(usize, usize, String)> {
        let (waiting, total) = self.progress;
        (waiting > 0).then(|| {
            (
                total - waiting,
                total,
                self.loading_name.clone().unwrap_or_default(),
            )
        })
    }

    /// The engine slot for `id`, used while compiling. A waiting instance
    /// goes into this snapshot; a sent one is carried over.
    pub fn supply(&mut self, id: PluginTarget, generation: u64) -> PluginRt {
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
    pub fn latency_only(&self, id: PluginTarget) -> PluginRt {
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
        let inserts = tracks
            .flat_map(|t| &mut t.inserts)
            .filter_map(|i| Some((PluginTarget::Insert(i.id), i.effect.plugin.as_mut()?)));
        let channels = project
            .channels
            .iter_mut()
            .filter_map(|c| match &mut c.source {
                ChannelSource::Plugin { plugin } => Some((PluginTarget::Channel(c.id), plugin)),
                ChannelSource::Sampler { .. } | ChannelSource::Audio { .. } => None,
            });
        for (target, slot) in inserts.chain(channels) {
            let Some(live) = self.live.get_mut(&target) else {
                continue;
            };
            if let Ok(s) = live.controller.state() {
                slot.component_state = s.component;
                slot.controller_state = s.controller;
            }
        }
    }

    /// Forwards plugin events (GUI edits, output parameters). Returns
    /// whether a recompile is needed: a new latency goes into a new
    /// snapshot with the same instance (PLG-035, PLG-051); a reload
    /// recreates the instance.
    pub fn poll(&mut self) -> bool {
        use jdance_plugin_api::PluginEvent::{LatencyChanged, ReloadRequested};
        const LATENCY_CHECK: std::time::Duration = std::time::Duration::from_secs(2);
        // Instances left for later, or a module that finished loading.
        let preloaded = self
            .preloaded
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        let mut recompile = self.progress.0 > 0 && (self.over_budget || preloaded);
        let now = std::time::Instant::now();
        for l in self.live.values_mut() {
            for e in l.controller.poll() {
                match e {
                    // The same instance with its new latency: its editor
                    // stays open and its sound is not interrupted. Some
                    // plugins report a latency change on every edit.
                    LatencyChanged => l.latency_check = Some(now + LATENCY_CHECK),
                    ReloadRequested => {
                        l.restart = true;
                        recompile = true;
                    }
                    _ => {}
                }
            }
            if let Some(until) = l.latency_check {
                let latency = l.controller.latency();
                if latency != l.latency {
                    l.latency = latency;
                    recompile = true;
                }
                if now >= until {
                    l.latency_check = None;
                }
            }
        }
        recompile
    }

    /// Opens or closes the plugin's own editor window. `Err` explains
    /// why it cannot open (no editor, no window system, plugin not loaded).
    /// `scaling` is the user's choice per plugin class (absent:
    /// automatic).
    pub fn toggle_editor(
        &mut self,
        id: PluginTarget,
        title: &str,
        scaling: &BTreeMap<String, jdance_plugin_api::EditorScaling>,
    ) -> Result<(), String> {
        let live = self
            .live
            .get_mut(&id)
            .ok_or_else(|| "the plugin is not loaded".to_owned())?;
        if live.editor.take().is_some() {
            return Ok(());
        }
        let scaling = scaling.get(&live.class_id).copied().unwrap_or_default();
        match live.controller.open_editor(title, scaling) {
            Ok(Some(e)) => {
                live.editor = Some(e);
                Ok(())
            }
            Ok(None) => Err("this plugin has no editor; use Parameters".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Class id of the live plugin in `id`.
    pub fn class_id(&self, id: PluginTarget) -> Option<&str> {
        self.live.get(&id).map(|l| l.class_id.as_str())
    }

    pub fn editor_open(&self, id: PluginTarget) -> bool {
        self.live.get(&id).is_some_and(|l| l.editor.is_some())
    }

    pub fn any_editor_open(&self) -> bool {
        self.live.values().any(|l| l.editor.is_some())
    }

    /// Runs open editors; closes those the user closed. Plugin timers run
    /// too, with or without an editor (Windows).
    pub fn poll_editors(&mut self) {
        jdance_plugin_vst3::run_plugin_timers();
        for l in self.live.values_mut() {
            if let Some(e) = &mut l.editor
                && !e.poll()
            {
                l.editor = None;
            }
        }
    }

    pub fn controller(&mut self, id: PluginTarget) -> Option<&mut Vst3Controller> {
        self.live.get_mut(&id).map(|l| &mut l.controller)
    }

    /// Whether the plugin in `id` is held back after a crash (PLG-025).
    pub fn held_back(&self, id: PluginTarget) -> bool {
        self.failed
            .keys()
            .any(|(i, class)| *i == id && self.catalog.crashed.contains_key(class))
    }

    /// Allows a held-back plugin class to load again; the next sync
    /// tries it.
    pub fn retry(&mut self, id: PluginTarget) {
        let classes: Vec<String> = self
            .failed
            .keys()
            .filter(|(i, _)| *i == id)
            .map(|(_, c)| c.clone())
            .collect();
        for c in classes {
            self.catalog.crashed.remove(&c);
            self.failed.retain(|(_, fc), _| *fc != c);
        }
        let _ = self.catalog.save_cached();
    }

    pub fn problem(&self, id: PluginTarget) -> Option<String> {
        if self.live.contains_key(&id) {
            return None;
        }
        self.failed
            .iter()
            .find(|((i, _), _)| *i == id)
            .map(|(_, e)| e.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::{Effect, Insert};

    #[test]
    fn plugin_that_crashed_last_run_is_held_back_until_retried() {
        let dir = tempfile::tempdir().unwrap();
        // The only test in this crate that touches the config directory.
        config::TEST_DIR.set(Some(dir.path().to_path_buf()));
        let bundle = jdance_test_support::test_plugin_bundle(jdance_plugin_vst3::bundle_binary);
        let descriptors = jdance_plugin_vst3::scan_in_process(bundle).unwrap();
        let gain = descriptors
            .iter()
            .find(|d| d.plugin.name == "JD Test Gain")
            .unwrap()
            .clone();
        let mut catalog = Catalog::default();
        catalog.modules.insert(
            bundle.to_path_buf(),
            ModuleEntry {
                key: None,
                result: ModuleResult::Ok(descriptors),
            },
        );
        // A previous run died while loading the plugin.
        let marker = dir.path().join(LOADING_MARKER);
        let m = LoadingMarker {
            class_id: gain.plugin.class_id.clone(),
            name: gain.plugin.name.clone(),
        };
        std::fs::write(&marker, serde_json::to_vec(&m).unwrap()).unwrap();

        let (mut host, crashed) = PluginHost::new(catalog);
        assert_eq!(crashed.as_deref(), Some("JD Test Gain"));
        assert!(!marker.exists());
        assert!(
            Catalog::load_cached()
                .crashed
                .contains_key(&gain.plugin.class_id)
        );

        let mut p = Project::new();
        let insert = jdance_domain::InsertId(p.ids.next());
        let id = PluginTarget::Insert(insert);
        p.mixer.master.inserts.push(Insert {
            id: insert,
            bypass: false,
            wet: 1.0,
            effect: Effect::plugin(PluginSlot {
                format: "vst3".into(),
                class_id: gain.plugin.class_id.clone(),
                name: gain.plugin.name.clone(),
                vendor: gain.plugin.vendor.clone(),
                version: gain.plugin.version.clone(),
                category: gain.category.clone(),
                component_state: Vec::new(),
                controller_state: Vec::new(),
            }),
            sidechain: None,
        });
        let messages = host.sync(&p, 48_000, 1);
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(host.controller(id).is_none());
        assert!(host.held_back(id));

        host.retry(id);
        assert!(host.sync(&p, 48_000, 1).is_empty());
        assert!(host.controller(id).is_some());
        assert!(!host.held_back(id));
        assert!(!marker.exists(), "marker removed after a successful load");
        config::TEST_DIR.set(None);
    }
}

#[cfg(test)]
mod instrument_tests {
    use super::*;
    use jdance_domain::Channel;

    #[test]
    fn instrument_channels_get_live_instances() {
        let bundle = jdance_test_support::test_plugin_bundle(jdance_plugin_vst3::bundle_binary);
        let descriptors = jdance_plugin_vst3::scan_in_process(bundle).unwrap();
        let synth = descriptors
            .iter()
            .find(|d| d.plugin.name == "JD Test Synth")
            .unwrap()
            .clone();
        let mut catalog = Catalog::default();
        catalog.modules.insert(
            bundle.to_path_buf(),
            ModuleEntry {
                key: None,
                result: ModuleResult::Ok(descriptors),
            },
        );
        let mut host = PluginHost::with_catalog(catalog);
        let mut p = Project::new();
        let id = jdance_domain::ChannelId(p.ids.next());
        p.channels.push(Channel::new_instrument(
            id,
            "Synth",
            PluginSlot {
                format: "vst3".into(),
                class_id: synth.plugin.class_id.clone(),
                name: synth.plugin.name.clone(),
                vendor: synth.plugin.vendor.clone(),
                version: synth.plugin.version.clone(),
                category: synth.category.clone(),
                component_state: Vec::new(),
                controller_state: Vec::new(),
            },
        ));
        // Spec 0.3.0 §8.1: the module file is read on a worker thread;
        // until then the instance waits and the status says so. The module
        // itself loads on this (the UI) thread only (0.3.1: JUCE editors
        // stayed black when it was initialised elsewhere).
        host.preload(&p);
        while !host.preloading.lock().unwrap().is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            !host.modules.lock().unwrap().contains_key(bundle),
            "the worker only reads the file"
        );
        let messages = host.sync(&p, 48_000, 1);
        assert!(messages.is_empty(), "{messages:?}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while host.controller(PluginTarget::Channel(id)).is_none() {
            assert!(std::time::Instant::now() < deadline, "the preload ended");
            if let Some((done, total, name)) = host.loading() {
                assert_eq!((done, total, name.as_str()), (0, 1, "JD Test Synth"));
                assert!(host.poll() || host.preloading.lock().unwrap().contains(bundle));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            host.sync(&p, 48_000, 1);
        }
        assert!(host.loading().is_none());
        assert!(host.modules.lock().unwrap().contains_key(bundle));
        let rt = host.supply(PluginTarget::Channel(id), 1);
        assert!(rt.has_instance());
    }
}

#[cfg(test)]
mod compat_tests {
    use super::*;
    use jdance_domain::{Effect, Insert};

    fn bundle() -> &'static Path {
        jdance_test_support::test_plugin_bundle(jdance_plugin_vst3::bundle_binary)
    }

    #[test]
    fn an_upgrade_scans_blocklisted_modules_again() {
        let mut catalog = Catalog::default();
        catalog.modules.insert(
            bundle().to_path_buf(),
            ModuleEntry {
                key: ModuleKey::of(bundle()),
                result: ModuleResult::Blocked("old reason".into()),
            },
        );
        let roots = [bundle().parent().unwrap().to_path_buf()];
        // A scanner that cannot start: a rescan shows in the new reason.
        let scanner = Path::new("/nonexistent/jdance-scanner");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let reason = |c: &Catalog| match &c.modules[bundle()].result {
            ModuleResult::Blocked(r) => r.clone(),
            ModuleResult::Ok(_) => String::new(),
        };
        catalog.scanned_by = SCANNER_VERSION.to_owned();
        let same = catalog.rescan(&roots, scanner, false, &|_, _, _| {}, &cancel);
        assert_eq!(reason(&same), "old reason", "same version: cached");
        catalog.scanned_by = "0.2.7".into();
        let newer = catalog.rescan(&roots, scanner, false, &|_, _, _| {}, &cancel);
        assert_ne!(reason(&newer), "old reason", "newer version: scanned again");
        assert_eq!(newer.scanned_by, SCANNER_VERSION);
    }

    #[test]
    fn a_latency_change_keeps_the_instance() {
        let descriptors = jdance_plugin_vst3::scan_in_process(bundle()).unwrap();
        let strict = descriptors
            .iter()
            .find(|d| d.plugin.name == "JD Test Strict")
            .unwrap()
            .clone();
        let mut catalog = Catalog::default();
        catalog.modules.insert(
            bundle().to_path_buf(),
            ModuleEntry {
                key: None,
                result: ModuleResult::Ok(descriptors),
            },
        );
        let mut host = PluginHost::with_catalog(catalog);
        let mut p = Project::new();
        let insert = jdance_domain::InsertId(p.ids.next());
        let id = PluginTarget::Insert(insert);
        p.mixer.master.inserts.push(Insert {
            id: insert,
            bypass: false,
            wet: 1.0,
            effect: Effect::plugin(PluginSlot {
                format: "vst3".into(),
                class_id: strict.plugin.class_id.clone(),
                name: strict.plugin.name.clone(),
                vendor: strict.plugin.vendor.clone(),
                version: strict.plugin.version.clone(),
                category: strict.category.clone(),
                component_state: Vec::new(),
                controller_state: Vec::new(),
            }),
            sidechain: None,
        });
        assert!(host.sync(&p, 48_000, 1).is_empty());
        // The engine's half, run here.
        let mut processor = host.live.get_mut(&id).unwrap().processor.take().unwrap();
        host.live.get_mut(&id).unwrap().generation = 1;
        assert_eq!(host.latency_only(id).latency(), 64);
        // The plugin announces the change before its processor has it.
        host.controller(id).unwrap().set_param_normalized(0, 0.25);
        assert!(!host.poll());
        let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
        jdance_plugin_api::PluginProcessor::process(
            &mut processor,
            &mut l,
            &mut r,
            None,
            &jdance_plugin_api::Transport::default(),
        );
        assert!(host.poll(), "a recompile for the new latency");
        assert_eq!(host.latency_only(id).latency(), 32);
        // Not recreated: the next sync keeps the instance the engine has.
        assert!(host.sync(&p, 48_000, 1).is_empty());
        assert!(
            host.live.get(&id).is_some_and(|l| l.processor.is_none()),
            "the instance in the engine is kept"
        );
    }
}
