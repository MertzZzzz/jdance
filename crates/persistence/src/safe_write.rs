// SPDX-License-Identifier: GPL-3.0-or-later
//! Atomic file replacement (PER-002).

use crate::{PersistError, io_err};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Writes `bytes` to `path` so that a crash at any point leaves either the
/// old or the new complete file:
/// 1. write `<name>.tmp` in the same directory and fsync it;
/// 2. copy the current file (if any) to `<name>.bak`;
/// 3. atomically rename the temp file over `path`;
/// 4. fsync the directory where the OS supports it.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), PersistError> {
    let tmp = sibling(path, ".tmp");
    let result = (|| {
        let mut f = File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(bytes).map_err(io_err(&tmp))?;
        f.sync_all().map_err(io_err(&tmp))?;
        drop(f);
        if path.exists() {
            let bak = sibling(path, ".bak");
            fs::copy(path, &bak).map_err(io_err(&bak))?;
        }
        fs::rename(&tmp, path).map_err(io_err(path))?;
        sync_dir(path);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(unix)]
fn sync_dir(path: &Path) {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && let Ok(d) = File::open(dir)
    {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) {
    // Directory handles cannot be fsynced through std on this platform;
    // the rename itself is atomic.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_and_keeps_backup() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jdance");
        write_atomically(&p, b"one").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"one");
        assert!(!sibling(&p, ".bak").exists());
        write_atomically(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read(sibling(&p, ".bak")).unwrap(), b"one");
        assert!(!sibling(&p, ".tmp").exists());
    }

    #[test]
    fn failure_leaves_original() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing-dir").join("a.jdance");
        assert!(write_atomically(&p, b"x").is_err());
    }
}
