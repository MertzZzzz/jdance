// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless offline render through the shipped binary (Amendment 01 §9).

use jdance_persistence::{WavExportFormat, read_wav, save_project, write_wav};
use jdance_test_support::{click, step_project};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jdance"))
}

#[test]
fn render_command_writes_expected_audio() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("samples")).unwrap();
    // Samples at 44.1 kHz exercise resampling to the 48 kHz render rate.
    for i in 0..2 {
        let s = click(44_100, 4_000);
        write_wav(
            &dir.path().join(format!("samples/{i}.wav")),
            44_100,
            &s.left,
            &s.right,
            WavExportFormat::Float32,
        )
        .unwrap();
    }
    let project = step_project(120.0, &[&[0, 4, 8, 12], &[2, 10]]);
    let project_path = dir.path().join("song.jdance");
    save_project(&project_path, &project).unwrap();
    let out = dir.path().join("out.wav");

    let status = bin()
        .args(["render"])
        .arg(&project_path)
        .arg(&out)
        .args([
            "--loops",
            "2",
            "--sample-rate",
            "48000",
            "--format",
            "float32",
            "--tail",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let audio = read_wav(&out).unwrap();
    assert_eq!((audio.channels, audio.sample_rate), (2, 48_000));
    // 120 BPM: one 16-step loop is 2 s = 96 000 frames.
    assert_eq!(audio.frames(), 2 * 96_000);
    let left: Vec<f32> = audio.samples.iter().step_by(2).copied().collect();
    let first = left.iter().position(|s| *s != 0.0).unwrap();
    assert_eq!(first, 0);
    // Step 2 of channel 1 starts at 12 000 frames; step 4 of channel 0 at 24 000.
    assert!(left[23_999] != left[24_000]);
    assert!(left.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
}

#[test]
fn render_reports_missing_samples_and_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().join("song.jdance");
    save_project(&project_path, &step_project(120.0, &[&[0]])).unwrap();
    let out = dir.path().join("out.wav");
    let r = bin()
        .arg("render")
        .arg(&project_path)
        .arg(&out)
        .output()
        .unwrap();
    assert!(r.status.success());
    assert!(String::from_utf8_lossy(&r.stderr).contains("missing sample"));

    let r = bin()
        .args(["render", "nope.jdance", "x.wav"])
        .output()
        .unwrap();
    assert!(!r.status.success());
    let r = bin()
        .arg("render")
        .arg(&project_path)
        .arg(&out)
        .args(["--format", "mp3"])
        .output()
        .unwrap();
    assert!(!r.status.success());
    let r = bin()
        .arg("render")
        .arg(&project_path)
        .arg(&out)
        .args(["--tail", "100"])
        .output()
        .unwrap();
    assert!(!r.status.success(), "tail above the maximum is rejected");
}
