// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared test helpers. Not used by shipping code.

use jdance_domain::{Channel, ChannelId, ChannelSource, Project, SampleRef};
use jdance_dsp::SampleBuffer;
use std::path::{Path, PathBuf};

/// Deterministic test "drum": a decaying click built only from `+`/`*` so
/// it is bit-identical on every platform (no libm calls).
pub fn click(sample_rate: u32, len: usize) -> SampleBuffer {
    let mut env = 1.0f32;
    let decay = 1.0 - 8.0 / sample_rate as f32;
    let mut sign = 1.0f32;
    let data: Vec<f32> = (0..len)
        .map(|i| {
            if i % 24 == 0 {
                sign = -sign;
            }
            env *= decay;
            sign * env * 0.5
        })
        .collect();
    SampleBuffer::from_interleaved(sample_rate, 1, &data)
}

/// Difference between two buffers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Diff {
    pub max_abs: f32,
    pub rms: f32,
    pub len_mismatch: bool,
}

pub fn diff(a: &[f32], b: &[f32]) -> Diff {
    let n = a.len().min(b.len());
    let mut max_abs = 0.0f32;
    let mut sum = 0.0f64;
    for i in 0..n {
        let d = (a[i] - b[i]).abs();
        max_abs = max_abs.max(d);
        sum += f64::from(d) * f64::from(d);
    }
    Diff {
        max_abs,
        rms: if n == 0 {
            0.0
        } else {
            (sum / n as f64).sqrt() as f32
        },
        len_mismatch: a.len() != b.len(),
    }
}

/// Builds a project with `channels` sampler channels, each referencing
/// `samples/<i>.wav`, and the given step pattern per channel.
pub fn step_project(tempo: f64, patterns: &[&[u32]]) -> Project {
    let mut p = Project::new();
    p.tempo_bpm = tempo;
    for (i, steps) in patterns.iter().enumerate() {
        let id = ChannelId(p.ids.next());
        let mut ch = Channel::new_sampler(id, format!("Ch {i}"));
        ch.source = ChannelSource::Sampler {
            sample: Some(SampleRef {
                path: format!("samples/{i}.wav"),
            }),
        };
        ch.pan = (i as f32 * 0.3) % 1.0 - 0.5;
        p.channels.push(ch);
        let row = p.patterns[0].row_mut(id);
        for &s in *steps {
            row.steps[s as usize].on = true;
            row.steps[s as usize].velocity = 0.5 + 0.1 * (s % 5) as f32;
        }
    }
    p
}

/// Directory with golden files: `crates/test-support/golden`.
pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("golden")
}

/// Compares stereo audio against `golden/<name>.wav` (32-bit float).
///
/// With `JDANCE_UPDATE_GOLDEN=1` the file is (re)written instead; golden
/// files are only updated deliberately (CI-003).
pub fn check_golden(name: &str, sample_rate: u32, left: &[f32], right: &[f32], tolerance: f32) {
    let path = golden_dir().join(format!("{name}.wav"));
    if std::env::var_os("JDANCE_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(golden_dir()).expect("create golden dir");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).expect("create golden");
        for (l, r) in left.iter().zip(right) {
            w.write_sample(*l).expect("write");
            w.write_sample(*r).expect("write");
        }
        w.finalize().expect("finalize golden");
        return;
    }
    let mut reader = hound::WavReader::open(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden file {}: {e}; run with JDANCE_UPDATE_GOLDEN=1 to create it",
            path.display()
        )
    });
    assert_eq!(reader.spec().sample_rate, sample_rate, "golden sample rate");
    let data: Vec<f32> = reader.samples::<f32>().map(|s| s.expect("read")).collect();
    let (gl, gr): (Vec<f32>, Vec<f32>) = data.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
    for (side, got, want) in [("left", left, &gl), ("right", right, &gr)] {
        let d = diff(got, want);
        assert!(
            !d.len_mismatch,
            "{name} {side}: length {} vs golden {}",
            got.len(),
            want.len()
        );
        assert!(
            d.max_abs <= tolerance,
            "{name} {side}: max abs diff {} > {tolerance} (rms {})",
            d.max_abs,
            d.rms
        );
    }
}
