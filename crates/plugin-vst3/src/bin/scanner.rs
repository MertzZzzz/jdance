// SPDX-License-Identifier: GPL-3.0-or-later
//! `jdance-plugin-scanner MODULE`: loads one VST3 module in its own
//! process and prints its classes as JSON (PLG-012, PLG-022). A crash or
//! hang here only blocklists the module.

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: jdance-plugin-scanner MODULE.vst3");
        return ExitCode::from(2);
    };
    match jdance_plugin_vst3::scan_in_process(std::path::Path::new(&path)) {
        Ok(d) => match serde_json::to_string(&d) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
