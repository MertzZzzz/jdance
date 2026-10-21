// SPDX-License-Identifier: GPL-3.0-or-later
//! Project files, safe save and audio file import/export.

mod assets;
mod audio_file;
mod project_file;
mod safe_write;

pub use assets::{ASSETS_DIR, AudioFileInfo, collect_assets, copy_into_assets, inspect_audio};
pub use audio_file::{DecodedAudio, WavExportFormat, read_wav, write_wav};
pub use project_file::{
    CURRENT_FORMAT_VERSION, FORMAT_TAG, PROJECT_EXTENSION, load_project, resolve_sample_path,
    save_project,
};
pub use safe_write::write_atomically;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("not a jdance project: {0}")]
    NotAProject(String),
    #[error(
        "project was saved by a newer version (format {found}, this build reads up to {supported})"
    )]
    NewerFormat { found: u32, supported: u32 },
    #[error("project file is damaged: {0}")]
    Corrupt(String),
    #[error("unsupported audio file: {0}")]
    UnsupportedAudio(String),
    #[error("audio file error: {0}")]
    Audio(String),
}

pub(crate) fn io_err(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> PersistError + '_ {
    move |source| PersistError::Io {
        path: path.display().to_string(),
        source,
    }
}
