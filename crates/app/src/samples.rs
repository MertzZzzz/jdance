// SPDX-License-Identifier: GPL-3.0-or-later
//! Sample cache with background loading (BLD-022: no blocking file I/O or
//! resampling on the UI thread).

use crate::render::load_sample;
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
    },
    Failed(String),
}

#[derive(Debug)]
enum Loaded {
    Decoded(
        PathBuf,
        Result<(SampleBuffer, SampleBuffer, Vec<f32>), String>,
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
}

impl SampleCache {
    pub fn new(rate: u32) -> Self {
        let (tx, rx) = channel();
        Self {
            entries: HashMap::new(),
            rate,
            tx,
            rx,
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
            let result = load_sample(&path, rate).map(|(o, c)| {
                let peaks = peaks(&o, PEAK_BUCKETS);
                (o, c, peaks)
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
                    let entry = match result {
                        Ok((original, converted, peaks)) => {
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
        changed
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
                ..
            } => match converted {
                Some(_) => SampleStatus::Ready {
                    frames: original.len(),
                    source_rate: original.sample_rate,
                },
                None => SampleStatus::Loading,
            },
            Entry::Failed(e) => SampleStatus::Failed(e.clone()),
        })
    }
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
