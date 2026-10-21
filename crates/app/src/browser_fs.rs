// SPDX-License-Identifier: GPL-3.0-or-later
//! Folder listings, the library index and file search for the Browser,
//! done on worker threads (BLD-022: no blocking file I/O on the UI
//! thread). The index walks each library folder once (recursively unless
//! the folder says otherwise), can be cancelled, reports progress and
//! never follows a symlink into a folder it has already seen.

use jdance_ui::{DirEntry, IndexStatus, LibraryRoot, Listing};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Extensions shown in the Browser; only WAV loads today.
const AUDIO: [&str; 7] = ["wav", "wave", "flac", "aif", "aiff", "mp3", "ogg"];
/// Search limits: results and folder depth.
const MAX_RESULTS: usize = 500;
const MAX_DEPTH: usize = 16;
/// Files kept by one index (memory bound).
const MAX_INDEX: usize = 200_000;

enum Job {
    List(PathBuf),
    Search(u64, Vec<PathBuf>, String),
}

enum Done {
    List(PathBuf, Listing),
    Search(u64, Listing),
}

/// The index of the searched folders and how far it got.
#[derive(Debug)]
struct Index {
    roots: Vec<PathBuf>,
    files: Arc<std::sync::Mutex<Option<Vec<DirEntry>>>>,
    count: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

#[derive(Debug)]
pub struct BrowserFs {
    jobs: Sender<Job>,
    done: Receiver<Done>,
    listings: HashMap<PathBuf, Listing>,
    /// Current search: (id, roots, query) and its result.
    search: Option<(u64, Vec<PathBuf>, String, Listing)>,
    next_search: u64,
    /// Library folders (their `recursive` flags apply to the index).
    library: Vec<LibraryRoot>,
    index: Option<Index>,
}

impl Default for BrowserFs {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserFs {
    pub fn new() -> Self {
        let (jobs, rx) = channel::<Job>();
        let (tx, done) = channel::<Done>();
        std::thread::Builder::new()
            .name("jdance-browser-fs".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let result = match job {
                        Job::List(path) => {
                            let l = list(&path);
                            Done::List(path, l)
                        }
                        Job::Search(id, roots, q) => Done::Search(id, search(&roots, &q)),
                    };
                    if tx.send(result).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn browser thread");
        Self {
            jobs,
            done,
            listings: HashMap::new(),
            search: None,
            next_search: 0,
            library: Vec::new(),
            index: None,
        }
    }

    /// Re-indexes `roots` (the library folders) in the background.
    pub fn index(&mut self, roots: Vec<LibraryRoot>) {
        self.library = roots;
        let paths: Vec<PathBuf> = self.library.iter().map(|r| r.path.clone()).collect();
        self.start_index(paths);
    }

    fn start_index(&mut self, roots: Vec<PathBuf>) {
        if let Some(old) = &self.index {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let index = Index {
            roots: roots.clone(),
            files: Arc::new(std::sync::Mutex::new(None)),
            count: Arc::new(AtomicUsize::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(true)),
        };
        let depth: Vec<(PathBuf, usize)> = roots
            .iter()
            .map(|p| {
                let flat = self.library.iter().any(|r| r.path == *p && !r.recursive);
                (p.clone(), if flat { 0 } else { MAX_DEPTH })
            })
            .collect();
        let (files, count, cancel, running) = (
            Arc::clone(&index.files),
            Arc::clone(&index.count),
            Arc::clone(&index.cancel),
            Arc::clone(&index.running),
        );
        let spawned = std::thread::Builder::new()
            .name("jdance-library-index".into())
            .spawn(move || {
                let found = walk(&depth, |_| true, &count, &cancel);
                if !cancel.load(Ordering::Relaxed)
                    && let Ok(mut f) = files.lock()
                {
                    *f = Some(found);
                }
                running.store(false, Ordering::Relaxed);
            });
        if spawned.is_err() {
            index.running.store(false, Ordering::Relaxed);
        }
        self.index = Some(index);
        self.search = None;
    }

    pub fn cancel_index(&mut self) {
        if let Some(i) = &self.index {
            i.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn index_status(&self) -> IndexStatus {
        self.index
            .as_ref()
            .map_or(IndexStatus::default(), |i| IndexStatus {
                files: i.count.load(Ordering::Relaxed),
                running: i.running.load(Ordering::Relaxed),
                cancelled: i.cancel.load(Ordering::Relaxed),
            })
    }

    /// Files of the finished index for exactly `roots`.
    fn indexed(&self, roots: &[PathBuf]) -> Option<Vec<DirEntry>> {
        let i = self.index.as_ref().filter(|i| i.roots == roots)?;
        i.files.lock().ok()?.clone()
    }

    /// Applies finished jobs; returns `true` if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(d) = self.done.try_recv() {
            changed = true;
            match d {
                Done::List(path, l) => {
                    self.listings.insert(path, l);
                }
                Done::Search(id, l) => {
                    if let Some(s) = &mut self.search
                        && s.0 == id
                    {
                        s.3 = l;
                    }
                }
            }
        }
        changed
    }

    pub fn list(&mut self, path: &Path) -> Listing {
        if let Some(l) = self.listings.get(path) {
            return l.clone();
        }
        self.listings.insert(path.to_path_buf(), Listing::Loading);
        let _ = self.jobs.send(Job::List(path.to_path_buf()));
        Listing::Loading
    }

    pub fn refresh(&mut self, path: &Path) {
        self.listings.remove(path);
    }

    pub fn search(&mut self, roots: &[PathBuf], query: &str) -> Listing {
        // Over the index when it covers these folders (no disk walk).
        if let Some(files) = self.indexed(roots) {
            let q = query.to_lowercase();
            let mut out: Vec<DirEntry> = files
                .into_iter()
                .filter(|e| e.name.to_lowercase().contains(&q))
                .take(MAX_RESULTS)
                .collect();
            out.sort_by_key(|e| e.name.to_lowercase());
            return Listing::Ready(out);
        }
        let indexing = self
            .index
            .as_ref()
            .is_some_and(|i| i.roots == roots && i.running.load(Ordering::Relaxed));
        if !indexing && self.index.as_ref().is_none_or(|i| i.roots != roots) {
            self.start_index(roots.to_vec());
        }
        if let Some((_, r, q, l)) = &self.search
            && r == roots
            && q == query
        {
            return l.clone();
        }
        self.next_search += 1;
        let id = self.next_search;
        self.search = Some((id, roots.to_vec(), query.to_owned(), Listing::Loading));
        let _ = self
            .jobs
            .send(Job::Search(id, roots.to_vec(), query.to_owned()));
        Listing::Loading
    }
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
}

fn entry(path: PathBuf, meta: &std::fs::Metadata) -> DirEntry {
    DirEntry {
        name: path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        path,
    }
}

/// Folders and audio files of `dir`, folders first, by name; hidden
/// entries are skipped.
fn list(dir: &Path) -> Listing {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            return Listing::Failed(match e.kind() {
                std::io::ErrorKind::NotFound => "folder not found".into(),
                std::io::ErrorKind::PermissionDenied => "no access".into(),
                _ => e.to_string(),
            });
        }
    };
    let mut out: Vec<DirEntry> = read
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let path = e.path();
            let meta = std::fs::metadata(&path).ok()?;
            (meta.is_dir() || is_audio(&path)).then(|| entry(path, &meta))
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Listing::Ready(out)
}

/// Audio files under `roots` whose name contains `query` (any case).
fn search(roots: &[PathBuf], query: &str) -> Listing {
    let q = query.to_lowercase();
    let depth: Vec<(PathBuf, usize)> = roots.iter().map(|r| (r.clone(), MAX_DEPTH)).collect();
    let never = AtomicBool::new(false);
    let mut out = walk(
        &depth,
        |e| e.name.to_lowercase().contains(&q),
        &AtomicUsize::new(0),
        &never,
    );
    out.truncate(MAX_RESULTS);
    out.sort_by_key(|e| e.name.to_lowercase());
    Listing::Ready(out)
}

/// Audio files under each `(root, depth)` accepted by `keep`, at most
/// [`MAX_INDEX`]. Each real folder is visited once, so symlink loops end.
fn walk(
    roots: &[(PathBuf, usize)],
    keep: impl Fn(&DirEntry) -> bool,
    count: &AtomicUsize,
    cancel: &AtomicBool,
) -> Vec<DirEntry> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = roots.to_vec();
    while let Some((dir, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) || out.len() >= MAX_INDEX {
            break;
        }
        let real = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if !seen.insert(real) {
            continue;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in read.filter_map(Result::ok) {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = e.path();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if depth > 0 {
                    stack.push((path, depth - 1));
                }
            } else if is_audio(&path) {
                let entry = entry(path, &meta);
                if keep(&entry) {
                    out.push(entry);
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    out
}

/// Opens the system file manager at `path` (its folder for a file).
pub fn reveal(path: &Path) -> std::io::Result<()> {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(fs: &mut BrowserFs, f: impl Fn(&mut BrowserFs) -> Listing) -> Listing {
        for _ in 0..500 {
            fs.poll();
            let l = f(fs);
            if !matches!(l, Listing::Loading) {
                return l;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("timed out");
    }

    #[test]
    fn lists_folders_first_and_searches_audio() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir(d.join("Kicks")).unwrap();
        std::fs::create_dir(d.join(".hidden")).unwrap();
        std::fs::write(d.join("b_snare.wav"), b"x").unwrap();
        std::fs::write(d.join("A_kick.WAV"), b"x").unwrap();
        std::fs::write(d.join("notes.txt"), b"x").unwrap();
        std::fs::write(d.join("Kicks").join("deep_kick.flac"), b"x").unwrap();
        let mut fs = BrowserFs::new();
        let Listing::Ready(items) = wait(&mut fs, |fs| fs.list(d)) else {
            panic!("listing failed");
        };
        let names: Vec<_> = items.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Kicks", "A_kick.WAV", "b_snare.wav"]);
        let Listing::Ready(found) = wait(&mut fs, |fs| fs.search(&[d.to_path_buf()], "KICK"))
        else {
            panic!("search failed");
        };
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A_kick.WAV", "deep_kick.flac"]);
        assert!(matches!(
            wait(&mut fs, |fs| fs.list(&d.join("missing"))),
            Listing::Failed(m) if m == "folder not found"
        ));
    }

    #[test]
    fn index_is_flat_when_asked_and_survives_symlink_loops() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("top.wav"), b"x").unwrap();
        std::fs::write(d.join("a/b/deep.wav"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d, d.join("a/b/loop")).unwrap();
        let mut fs = BrowserFs::new();
        let root = |recursive| LibraryRoot {
            path: d.to_path_buf(),
            name: Some("Mine".into()),
            recursive,
        };
        fs.index(vec![root(true)]);
        let Listing::Ready(all) = wait(&mut fs, |fs| fs.search(&[d.to_path_buf()], "")) else {
            panic!("search failed");
        };
        let names: Vec<_> = all.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["deep.wav", "top.wav"],
            "each file once, the loop ends"
        );
        assert_eq!(fs.index_status().files, 2);
        fs.index(vec![root(false)]);
        let Listing::Ready(flat) = wait(&mut fs, |fs| fs.search(&[d.to_path_buf()], "")) else {
            panic!("search failed");
        };
        assert_eq!(flat.len(), 1, "a flat folder: no subfolders");
    }
}
