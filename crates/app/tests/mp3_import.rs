// SPDX-License-Identifier: GPL-3.0-or-later
//! MP3 import (spec 0.2.5 §5), with files made by our own LAME export.

use jdance_mp3::{Mp3Settings, Mp3Writer};
use jdance_persistence::{SourceFormat, WavExportFormat, decode_mp3, read_audio, write_wav};
use std::io::Cursor;

/// One second of silence with a loud 1 kHz burst from frame `at`.
fn burst(rate: u32, at: usize) -> Vec<f32> {
    (0..rate as usize)
        .map(|i| {
            if (at..at + rate as usize / 10).contains(&i) {
                0.8 * (std::f32::consts::TAU * 1000.0 * (i - at) as f32 / rate as f32 + 0.5).sin()
            } else {
                0.0
            }
        })
        .collect()
}

fn encode(rate: u32, signal: &[f32], title: &str) -> Vec<u8> {
    let mut w = Mp3Writer::new(&Mp3Settings {
        kbps: 192,
        sample_rate: rate,
        title: title.into(),
        artist: String::new(),
    })
    .unwrap();
    let mut out = Cursor::new(Vec::new());
    for chunk in signal.chunks(4096) {
        w.write(chunk, chunk, &mut out).unwrap();
    }
    w.finish(&mut out).unwrap();
    out.into_inner()
}

/// First frame whose left sample is louder than a quarter of the peak.
fn onset(samples: &[f32], channels: usize) -> usize {
    samples
        .chunks(channels)
        .position(|f| f[0].abs() > 0.2)
        .unwrap()
}

#[test]
fn a_lame_file_starts_on_the_beat_and_keeps_its_length() {
    for rate in [44_100, 48_000] {
        let at = rate as usize / 10;
        // An ID3v2 tag (the title) in front is skipped.
        let data = encode(rate, &burst(rate, at), "Beat");
        let a = decode_mp3(&data).unwrap();
        assert_eq!(a.sample_rate, rate);
        assert_eq!(a.channels, 2);
        let frames = a.samples.len() / 2;
        assert!(
            frames.abs_diff(rate as usize) <= 1,
            "{rate}: {frames} frames"
        );
        let o = onset(&a.samples, 2);
        assert!(o.abs_diff(at) <= 2, "{rate}: onset {o}, expected {at}");
        assert!(
            matches!(a.format, SourceFormat::Mp3 { kbps: 192, .. }),
            "{:?}",
            a.format
        );
        assert!(a.warnings.is_empty());
    }
}

#[test]
fn without_the_info_frame_the_default_delay_is_cut() {
    let rate = 48_000;
    let at = 4_800;
    let data = encode(rate, &burst(rate, at), "");
    // Drop the Info frame (the first frame): no recorded delay.
    let first = nanomp3_frame_len(&data);
    let a = decode_mp3(&data[first..]).unwrap();
    let o = onset(&a.samples, 2);
    assert!(o.abs_diff(at) <= 2, "onset {o}, expected {at}");
}

/// Length of the first MPEG-1 Layer III frame (no ID3 in front).
fn nanomp3_frame_len(data: &[u8]) -> usize {
    const KBPS: [usize; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const RATES: [usize; 3] = [44_100, 48_000, 32_000];
    let h = &data[..4];
    assert_eq!(h[0], 0xFF);
    let kbps = KBPS[usize::from(h[2] >> 4)];
    let rate = RATES[usize::from((h[2] >> 2) & 3)];
    144 * kbps * 1000 / rate + usize::from((h[2] >> 1) & 1)
}

#[test]
fn a_cut_file_opens_up_to_the_cut_with_a_warning() {
    let data = encode(48_000, &burst(48_000, 4_800), "");
    let a = decode_mp3(&data[..data.len() * 6 / 10]).unwrap();
    assert!(a.samples.len() / 2 < 40_000);
    assert_eq!(a.warnings.len(), 1, "{:?}", a.warnings);
}

#[test]
fn the_format_comes_from_the_content_not_the_name() {
    let dir = tempfile::tempdir().unwrap();
    // A WAV file named .mp3 reads as WAV.
    let wav = dir.path().join("really_wav.mp3");
    write_wav(
        &wav,
        48_000,
        &[0.5; 100],
        &[0.5; 100],
        WavExportFormat::Pcm16,
    )
    .unwrap();
    let a = read_audio(&wav).unwrap();
    assert_eq!(a.format, SourceFormat::Wav { bits: 16 });
    // An MP3 named .WAV reads as MP3; upper-case extensions are fine.
    let mp3 = dir.path().join("SONG.WAV");
    std::fs::write(&mp3, encode(44_100, &burst(44_100, 100), "")).unwrap();
    assert!(matches!(
        read_audio(&mp3).unwrap().format,
        SourceFormat::Mp3 { .. }
    ));
    // Not audio at all: a clear error.
    let text = dir.path().join("notes.mp3");
    std::fs::write(&text, "these are notes, not audio ".repeat(1000)).unwrap();
    let e = read_audio(&text).unwrap_err().to_string();
    assert!(e.contains("not an audio file"), "{e}");
}

#[test]
fn an_mp3_imported_as_an_audio_clip_reports_its_decoded_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loop.mp3");
    std::fs::write(&path, encode(44_100, &burst(44_100, 4_410), "")).unwrap();
    let info = jdance_persistence::inspect_audio(&path).unwrap();
    assert_eq!((info.sample_rate, info.channels), (44_100, 2));
    assert!(info.frames.abs_diff(44_100) <= 1, "{}", info.frames);
}
