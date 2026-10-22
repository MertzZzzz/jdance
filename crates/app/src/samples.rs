// SPDX-License-Identifier: GPL-3.0-or-later
//! Sample cache with background loading (BLD-022: no blocking file I/O or
//! resampling on the UI thread).

use crate::render::load_sample_info;
use jdance_domain::sample_processing::Processing;
use jdance_dsp::SampleBuffer;
use jdance_ui::SampleStatus;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

#[derive(Debug)]
enum Entry {
    Loading,
    Ready {
        original: Arc<SampleBuffer>,
        /// Peak level per bucket of the original, for waveform displays.
        peaks: Arc<Vec<f32>>,
        /// Converted to the current engine rate; `None` while converting.
        converted: Option<Arc<SampleBuffer>>,
        /// "16", "MP3 192 kbit/s joint stereo" (spec 0.2.5 §5).
        format: String,
    },
    Failed(String),
}

#[derive(Debug)]
enum Loaded {
    Decoded(
        PathBuf,
        Result<(crate::render::LoadedSample, Vec<f32>), String>,
        u32,
    ),
    Converted(PathBuf, SampleBuffer),
}

#[derive(Debug)]
pub struct SampleCache {
    entries: HashMap<PathBuf, Entry>,
    rate: u32,
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
    /// Import warnings not yet shown.
    warnings: Vec<String>,
    /// Processed samples waiting for their file: derived path → file
    /// path and processing (spec 0.2.5 §4).
    derived: HashMap<PathBuf, (PathBuf, Processing)>,
}

impl SampleCache {
    pub fn new(rate: u32) -> Self {
        let (tx, rx) = channel();
        Self {
            entries: HashMap::new(),
            rate,
            tx,
            rx,
            warnings: Vec::new(),
            derived: HashMap::new(),
        }
    }

    /// Starts computing `processing` of the file at `base` into a sample
    /// cached under `derived`, once the file is loaded.
    pub fn request_processed(&mut self, derived: &Path, base: &Path, processing: Processing) {
        if self.entries.contains_key(derived) {
            return;
        }
        self.request(base);
        self.entries.insert(derived.to_path_buf(), Entry::Loading);
        self.derived
            .insert(derived.to_path_buf(), (base.to_path_buf(), processing));
        self.start_derived();
    }

    /// Forgets processed samples not in `keep`: every knob position makes
    /// a new one.
    pub fn retain_processed(&mut self, keep: &std::collections::HashSet<PathBuf>) {
        let stale: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|p| is_derived(p) && !keep.contains(*p))
            .cloned()
            .collect();
        for p in stale {
            self.entries.remove(&p);
            self.derived.remove(&p);
        }
    }

    /// Processes the waiting samples whose file is loaded.
    fn start_derived(&mut self) {
        let mut done = Vec::new();
        for (derived, (base, processing)) in &self.derived {
            match self.entries.get(base) {
                Some(Entry::Ready { original, .. }) => {
                    done.push(derived.clone());
                    let (tx, path, original, p, rate) = (
                        self.tx.clone(),
                        derived.clone(),
                        Arc::clone(original),
                        *processing,
                        self.rate,
                    );
                    std::thread::spawn(move || {
                        let processed = crate::render::process_sample(&original, &p);
                        let converted = processed.resampled(rate);
                        let peaks = peaks(&processed, PEAK_BUCKETS);
                        let loaded = crate::render::LoadedSample {
                            original: processed,
                            converted,
                            format: jdance_persistence::SourceFormat::Unknown,
                            warnings: Vec::new(),
                        };
                        let _ = tx.send(Loaded::Decoded(path, Ok((loaded, peaks)), rate));
                    });
                }
                Some(Entry::Failed(e)) => {
                    done.push(derived.clone());
                    let e = e.clone();
                    self.entries.insert(derived.clone(), Entry::Failed(e));
                }
                _ => {}
            }
        }
        for d in done {
            self.derived.remove(&d);
        }
    }

    /// Starts loading `path` unless it is cached or in flight.
    pub fn request(&mut self, path: &Path) {
        if self.entries.contains_key(path) {
            return;
        }
        self.entries.insert(path.to_path_buf(), Entry::Loading);
        let (tx, path, rate) = (self.tx.clone(), path.to_path_buf(), self.rate);
        std::thread::spawn(move || {
            let result = load_sample_info(&path, rate).map(|l| {
                let peaks = peaks(&l.original, PEAK_BUCKETS);
                (l, peaks)
            });
            let _ = tx.send(Loaded::Decoded(path, result, rate));
        });
    }

    /// Changes the engine rate; cached samples are reconverted.
    pub fn set_rate(&mut self, rate: u32) {
        if rate == self.rate {
            return;
        }
        self.rate = rate;
        for (path, entry) in &mut self.entries {
            if let Entry::Ready {
                original,
                converted,
                ..
            } = entry
            {
                *converted = None;
                let (tx, path, original) = (self.tx.clone(), path.clone(), Arc::clone(original));
                std::thread::spawn(move || {
                    let _ = tx.send(Loaded::Converted(path, original.resampled(rate)));
                });
            }
        }
    }

    /// Applies finished loads. Returns `true` if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            match msg {
                Loaded::Decoded(path, result, rate) => {
                    if !self.entries.contains_key(&path) {
                        continue; // a processed sample no longer wanted
                    }
                    let entry = match result {
                        Ok((loaded, peaks)) => {
                            let name = path.file_name().map_or_else(
                                || path.display().to_string(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            self.warnings
                                .extend(loaded.warnings.iter().map(|w| format!("{name}: {w}")));
                            let format = loaded.format.to_string();
                            let (original, converted) = (loaded.original, loaded.converted);
                            let original = Arc::new(original);
                            let peaks = Arc::new(peaks);
                            let converted = (rate == self.rate).then(|| Arc::new(converted));
                            if converted.is_none() {
                                let (tx, p, o, r) = (
                                    self.tx.clone(),
                                    path.clone(),
                                    Arc::clone(&original),
                                    self.rate,
                                );
                                std::thread::spawn(move || {
                                    let _ = tx.send(Loaded::Converted(p, o.resampled(r)));
                                });
                            }
                            Entry::Ready {
                                original,
                                peaks,
                                converted,
                                format,
                            }
                        }
                        Err(e) => Entry::Failed(e),
                    };
                    self.entries.insert(path, entry);
                }
                Loaded::Converted(path, buf) => {
                    if buf.sample_rate != self.rate {
                        continue; // stale conversion
                    }
                    if let Some(Entry::Ready { converted, .. }) = self.entries.get_mut(&path) {
                        *converted = Some(Arc::new(buf));
                    }
                }
            }
        }
        if changed {
            self.start_derived();
        }
        changed
    }

    /// Import warnings since the last call.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// Waveform peaks of a loaded sample.
    pub fn peaks(&self, path: &Path) -> Option<Arc<Vec<f32>>> {
        match self.entries.get(path)? {
            Entry::Ready { peaks, .. } => Some(Arc::clone(peaks)),
            _ => None,
        }
    }

    pub fn converted(&self, path: &Path) -> Option<Arc<SampleBuffer>> {
        match self.entries.get(path)? {
            Entry::Ready { converted, .. } => converted.clone(),
            _ => None,
        }
    }

    pub fn status(&self, path: &Path) -> Option<SampleStatus> {
        Some(match self.entries.get(path)? {
            Entry::Loading => SampleStatus::Loading,
            Entry::Ready {
                original,
                converted,
                format,
                ..
            } => match converted {
                Some(_) => SampleStatus::Ready {
                    frames: original.len(),
                    source_rate: original.sample_rate,
                    format: format.clone(),
                },
                None => SampleStatus::Loading,
            },
            Entry::Failed(e) => SampleStatus::Failed(e.clone()),
        })
    }
}

/// A processed sample's path ([`jdance_domain::sample_processing::processed_ref`]).
fn is_derived(p: &Path) -> bool {
    p.to_string_lossy()
        .contains(jdance_domain::sample_processing::PROCESSED_SEPARATOR)
}

/// Resolution of waveform peaks.
const PEAK_BUCKETS: usize = 2_048;

/// Largest absolute value of either channel per bucket.
fn peaks(s: &SampleBuffer, buckets: usize) -> Vec<f32> {
    let n = s.len();
    if n == 0 {
        return Vec::new();
    }
    let buckets = buckets.min(n);
    (0..buckets)
        .map(|b| {
            let (a, z) = (
                b * n / buckets,
                ((b + 1) * n / buckets).max(b * n / buckets + 1),
            );
            s.left[a..z]
                .iter()
                .chain(&s.right[a..z])
                .fold(0.0f32, |m, v| m.max(v.abs()))
        })
        .collect()
}
