// SPDX-License-Identifier: GPL-3.0-or-later
//! VST3 hosting (Amendment 02, section 11) through the `vst3` bindings
//! (ADR-008). VST3 types stay inside this crate (PLG-010).
//!
//! Unsafe code is unavoidable here: every plugin call goes through COM
//! vtables. It is confined to this crate and the test plugin.

#![allow(unsafe_code)]
// VST3 enum types differ between Windows and other platforms, so casts
// that are no-ops on one OS are needed on another.
#![allow(clippy::unnecessary_cast)]

mod com;
mod editor;
mod instance;
mod module;
mod scan;
mod uid;

pub use editor::{Vst3Editor, take_space_presses};
pub use instance::{Vst3Controller, Vst3Processor, instantiate};
pub use module::{Vst3Module, binary_path, bundle_binary};
pub use scan::{
    SCAN_TIMEOUT, ScanOutcome, default_search_paths, find_modules, run_scanner, scan_in_process,
    scan_out_of_process,
};
pub use uid::{from_hex as class_id_from_hex, to_hex as class_id_to_hex};
