// SPDX-License-Identifier: GPL-3.0-or-later
//! Folder listings and file search for the Browser, done on a worker
//! thread (BLD-022: no blocking file I/O on the UI thread).

use jdance_ui::{DirEntry, Listing};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Extensions shown in the Browser; only WAV loads today.
const AUDIO: [&str; 7] = ["wav", "wave", "flac", "aif", "aiff", "mp3", "ogg"];
/// Search limits: results and folder depth.
const MAX_RESULTS: usize = 500;
const MAX_DEPTH: usize = 8;

enum Job {
    List(PathBuf),
    Search(u64, Vec<PathBuf>, String),
}

enum Done {
    List(PathBuf, Listing),
    Search(u64, Listing),
}

#[derive(Debug)]
pub struct BrowserFs {
    jobs: Sender<Job>,
    done: Receiver<Done>,
    listings: HashMap<PathBuf, Listing>,
    /// Current search: (id, roots, query) and its result.
    search: Option<(u64, Vec<PathBuf>, String, Listing)>,
    next_search: u64,
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
        }
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
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
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
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if is_audio(&path)
                && path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&q))
            {
                out.push(entry(path, &meta));
                if out.len() >= MAX_RESULTS {
                    return Listing::Ready(out);
                }
            }
        }
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    Listing::Ready(out)
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
}
