// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline sample-rate conversion used when loading samples.
//!
//! Interim implementation: 4-point Catmull-Rom (cubic Hermite)
//! interpolation. It has no anti-aliasing filter, so downsampling material
//! with content above the new Nyquist frequency aliases. The production
//! algorithm and its quality target are decided in ADR-012 (BLD-041).

/// Resamples `input` from `from_rate` to `to_rate`. Not realtime-safe.
pub fn resample_cubic(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    assert!(
        from_rate > 0 && to_rate > 0,
        "sample rates must be positive"
    );
    if input.is_empty() || from_rate == to_rate {
        return input.to_vec();
    }
    let out_len =
        ((input.len() as u64 * u64::from(to_rate)).div_ceil(u64::from(from_rate))) as usize;
    let ratio = f64::from(from_rate) / f64::from(to_rate);
    let at = |i: isize| -> f32 {
        let i = i.clamp(0, input.len() as isize - 1);
        input[i as usize]
    };
    (0..out_len)
        .map(|n| {
            let x = n as f64 * ratio;
            let i = x.floor() as isize;
            let t = (x - i as f64) as f32;
            let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
            let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
            let c = -0.5 * p0 + 0.5 * p2;
            ((a * t + b) * t + c) * t + p1
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_lengths() {
        let x = [0.0, 1.0, 0.0, -1.0];
        assert_eq!(resample_cubic(&x, 48_000, 48_000), x);
        assert_eq!(resample_cubic(&x, 24_000, 48_000).len(), 8);
        assert_eq!(resample_cubic(&[0.0; 441], 44_100, 48_000).len(), 480);
        assert!(resample_cubic(&[], 44_100, 48_000).is_empty());
    }

    #[test]
    fn upsampling_passes_through_original_points() {
        let x: Vec<f32> = (0..32).map(|i| (i as f32 * 0.3).sin()).collect();
        let y = resample_cubic(&x, 1, 2);
        for (i, v) in x.iter().enumerate() {
            assert!((y[2 * i] - v).abs() < 1e-6);
        }
    }

    #[test]
    fn low_frequency_sine_is_preserved() {
        let f = 440.0f64;
        let src: Vec<f32> = (0..4410)
            .map(|n| (2.0 * std::f64::consts::PI * f * n as f64 / 44_100.0).sin() as f32)
            .collect();
        let dst = resample_cubic(&src, 44_100, 48_000);
        for (n, v) in dst.iter().enumerate().take(4700).skip(10) {
            let expected = (2.0 * std::f64::consts::PI * f * n as f64 / 48_000.0).sin() as f32;
            assert!((v - expected).abs() < 1e-3, "n={n}");
        }
    }
}
