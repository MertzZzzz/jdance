// SPDX-License-Identifier: GPL-3.0-or-later
//! Processing of whole samples, done in advance outside the audio thread
//! (spec 0.2.5 §4): the Tools panel, time stretching and pitch shifting.
//! Our own algorithms: plain sample arithmetic and an overlap-add
//! stretcher that aligns each grain with the previous one by correlation.

use crate::SampleBuffer;
use crate::resample_cubic;

/// The Tools panel's settings. Fractions are of the sample's length.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Tools {
    pub remove_dc: bool,
    pub reverse_polarity: bool,
    pub normalize: bool,
    pub fade_stereo: bool,
    pub reverse: bool,
    pub swap_stereo: bool,
    /// New start, `0..1`.
    pub start: f32,
    /// New length as a fraction of what follows `start`; 0 = all.
    pub length: f32,
    /// Fades, `0..=0.5` of the (cut) length.
    pub fade_in: f32,
    pub fade_out: f32,
    /// End blended into the start for a seamless loop, `0..=0.5`.
    pub crossfade: f32,
    /// Trailing audio below this level is cut (when `trim_on`).
    pub trim: f32,
    pub trim_on: bool,
}

/// Applies `t` to `buf`.
pub fn apply_tools(buf: &SampleBuffer, t: &Tools) -> SampleBuffer {
    let n = buf.len();
    let a = ((f64::from(t.start.clamp(0.0, 1.0)) * n as f64) as usize).min(n);
    let rest = n - a;
    let len = if t.length > 0.0 {
        ((f64::from(t.length.clamp(0.0, 1.0)) * rest as f64).round() as usize)
            .clamp(1.min(rest), rest)
    } else {
        rest
    };
    let mut l = buf.left[a..a + len].to_vec();
    let mut r = buf.right[a..a + len].to_vec();
    if t.reverse {
        l.reverse();
        r.reverse();
    }
    if t.remove_dc {
        for ch in [&mut l, &mut r] {
            let mean = ch.iter().map(|v| f64::from(*v)).sum::<f64>() / ch.len().max(1) as f64;
            for v in ch.iter_mut() {
                *v -= mean as f32;
            }
        }
    }
    if t.reverse_polarity {
        for v in l.iter_mut().chain(r.iter_mut()) {
            *v = -*v;
        }
    }
    if t.swap_stereo {
        std::mem::swap(&mut l, &mut r);
    }
    let m = l.len();
    if t.fade_stereo && m > 1 {
        // The sound moves from the left channel to the right one.
        for i in 0..m {
            let p = i as f32 / (m - 1) as f32;
            let mono = 0.5 * (l[i] + r[i]);
            l[i] = mono * (1.0 - p);
            r[i] = mono * p;
        }
    }
    let fade = |frac: f32| ((f64::from(frac.clamp(0.0, 0.5)) * m as f64) as usize).min(m);
    let fi = fade(t.fade_in);
    for i in 0..fi {
        let g = i as f32 / fi as f32;
        l[i] *= g;
        r[i] *= g;
    }
    let fo = fade(t.fade_out);
    for i in 0..fo {
        let g = i as f32 / fo as f32;
        l[m - 1 - i] *= g;
        r[m - 1 - i] *= g;
    }
    let xf = fade(t.crossfade);
    if xf > 0 && 2 * xf <= m {
        // The tail fades into the head: looping the result is seamless.
        for i in 0..xf {
            let g = i as f32 / xf as f32;
            let j = m - xf + i;
            l[j] = l[j] * (1.0 - g) + l[i] * g;
            r[j] = r[j] * (1.0 - g) + r[i] * g;
        }
    }
    if t.normalize {
        let peak = l.iter().chain(r.iter()).fold(0.0f32, |p, v| p.max(v.abs()));
        if peak > 1e-9 {
            let g = 1.0 / peak;
            for v in l.iter_mut().chain(r.iter_mut()) {
                *v *= g;
            }
        }
    }
    if t.trim_on {
        let floor = t.trim.max(0.0);
        let keep = (0..m)
            .rev()
            .find(|&i| l[i].abs() > floor || r[i].abs() > floor)
            .map_or(0, |i| i + 1)
            .max(1.min(m));
        l.truncate(keep);
        r.truncate(keep);
    }
    SampleBuffer {
        sample_rate: buf.sample_rate,
        left: l,
        right: r,
    }
}

/// Plays `buf` `speed` times faster (pitch and length change together):
/// the result is `len / speed` long at the same rate.
pub fn change_speed(buf: &SampleBuffer, speed: f64) -> SampleBuffer {
    if (speed - 1.0).abs() < 1e-9 || !speed.is_finite() || speed <= 0.0 {
        return buf.clone();
    }
    // Resampling from a rate `speed` times higher to the real one.
    const BASE: f64 = 100_000.0;
    let from = (BASE * speed).round().clamp(1.0, u32::MAX as f64) as u32;
    let to = BASE as u32;
    SampleBuffer {
        sample_rate: buf.sample_rate,
        left: resample_cubic(&buf.left, from, to),
        right: resample_cubic(&buf.right, from, to),
    }
}

/// Grain length of the stretcher, in samples at 48 kHz (scaled with the
/// rate).
const GRAIN_48K: usize = 2048;

/// Makes `buf` `factor` times longer without changing its pitch:
/// Hann-windowed grains are overlap-added at half a grain, each read
/// where it matches the continuation of the previous grain best (a
/// correlation search on every fourth sample).
pub fn time_stretch(buf: &SampleBuffer, factor: f64) -> SampleBuffer {
    let len = buf.len();
    if (factor - 1.0).abs() < 1e-9 || !factor.is_finite() || factor <= 0.0 || len == 0 {
        return buf.clone();
    }
    let grain = ((GRAIN_48K as u64 * u64::from(buf.sample_rate.max(1)) / 48_000) as usize)
        .clamp(256, 8192)
        & !1;
    let hop = grain / 2;
    let tol = grain / 8;
    let out_len = (len as f64 * factor).round() as usize;
    let window: Vec<f32> = (0..grain)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / grain as f32).cos())
        .collect();
    let mono: Vec<f32> = buf
        .left
        .iter()
        .zip(&buf.right)
        .map(|(a, b)| 0.5 * (a + b))
        .collect();
    let at = |v: &[f32], i: isize| -> f32 {
        if i >= 0 && (i as usize) < v.len() {
            v[i as usize]
        } else {
            0.0
        }
    };
    let mut out_l = vec![0.0f32; out_len + grain];
    let mut out_r = vec![0.0f32; out_len + grain];
    let mut norm = vec![0.0f32; out_len + grain];
    let mut prev: isize = 0;
    let mut k = 0usize;
    loop {
        let out_at = k * hop;
        if out_at >= out_len {
            break;
        }
        let nominal = (k as f64 * hop as f64 / factor).round() as isize;
        let pos = if k == 0 {
            0
        } else {
            // The previous grain would go on at `prev + hop`.
            let natural = prev + hop as isize;
            let mut best = (f32::MIN, nominal);
            let lo = (nominal - tol as isize).max(0);
            let hi = (nominal + tol as isize).min(len as isize - 1).max(lo);
            for cand in lo..=hi {
                let mut c = 0.0f32;
                let mut i = 0;
                while i < hop {
                    c += at(&mono, natural + i as isize) * at(&mono, cand + i as isize);
                    i += 4;
                }
                if c > best.0 {
                    best = (c, cand);
                }
            }
            best.1
        };
        for i in 0..grain {
            let w = window[i];
            out_l[out_at + i] += w * at(&buf.left, pos + i as isize);
            out_r[out_at + i] += w * at(&buf.right, pos + i as isize);
            norm[out_at + i] += w;
        }
        prev = pos;
        k += 1;
    }
    for i in 0..out_len {
        if norm[i] > 1e-3 {
            out_l[i] /= norm[i];
            out_r[i] /= norm[i];
        }
    }
    out_l.truncate(out_len);
    out_r.truncate(out_len);
    SampleBuffer {
        sample_rate: buf.sample_rate,
        left: out_l,
        right: out_r,
    }
}

/// Length `length_factor` times the original and pitch moved by
/// `semitones`: stretching keeps them apart, otherwise they change
/// together like tape (`stretch` false: the length factor slows the
/// sample down, the pitch speeds it up).
pub fn stretch_and_pitch(
    buf: &SampleBuffer,
    length_factor: f64,
    semitones: f64,
    stretch: bool,
) -> SampleBuffer {
    let ratio = 2f64.powf(semitones / 12.0);
    if stretch {
        // Longer by the pitch ratio, then sped up by it: the pitch moves,
        // the length is `length_factor`.
        let longer = time_stretch(buf, length_factor * ratio);
        change_speed(&longer, ratio)
    } else {
        change_speed(buf, ratio / length_factor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, secs: f32) -> SampleBuffer {
        let n = (48_000.0 * secs) as usize;
        let d: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        SampleBuffer::from_interleaved(48_000, 1, &d)
    }

    /// Frequency by counting upward zero crossings in the middle half.
    fn freq(b: &SampleBuffer) -> f32 {
        let n = b.len();
        let s = &b.left[n / 4..3 * n / 4];
        let ups = s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f32 * 48_000.0 / s.len() as f32
    }

    #[test]
    fn stretching_keeps_the_pitch_and_shifting_keeps_the_length() {
        let s = sine(440.0, 1.0);
        let long = time_stretch(&s, 2.0);
        assert_eq!(long.len(), 96_000);
        assert!((freq(&long) - 440.0).abs() < 8.0, "{}", freq(&long));
        let up = stretch_and_pitch(&s, 1.0, 12.0, true);
        assert!(up.len().abs_diff(48_000) < 64, "{}", up.len());
        assert!((freq(&up) - 880.0).abs() < 16.0, "{}", freq(&up));
        // Tape: an octave up is half as long.
        let tape = stretch_and_pitch(&s, 1.0, 12.0, false);
        assert!(tape.len().abs_diff(24_000) < 4, "{}", tape.len());
    }

    #[test]
    fn tools_do_what_they_say() {
        let b =
            SampleBuffer::from_interleaved(48_000, 2, &[0.1, 0.2, 0.3, 0.4, 0.5, 0.0, 0.0, 0.0]);
        let rev = apply_tools(
            &b,
            &Tools {
                reverse: true,
                ..Tools::default()
            },
        );
        assert_eq!(rev.left, [0.0, 0.5, 0.3, 0.1]);
        let norm = apply_tools(
            &b,
            &Tools {
                normalize: true,
                ..Tools::default()
            },
        );
        let peak = norm
            .left
            .iter()
            .chain(&norm.right)
            .fold(0.0f32, |p, v| p.max(v.abs()));
        assert!((peak - 1.0).abs() < 1e-6);
        let swapped = apply_tools(
            &b,
            &Tools {
                swap_stereo: true,
                ..Tools::default()
            },
        );
        assert_eq!(swapped.left, b.right);
        let trimmed = apply_tools(
            &b,
            &Tools {
                trim_on: true,
                ..Tools::default()
            },
        );
        assert_eq!(trimmed.len(), 3, "trailing digital silence cut");
        let dc = SampleBuffer::from_interleaved(48_000, 1, &[1.0, 1.2, 0.8, 1.0]);
        let no_dc = apply_tools(
            &dc,
            &Tools {
                remove_dc: true,
                ..Tools::default()
            },
        );
        assert!(no_dc.left.iter().sum::<f32>().abs() < 1e-6);
        let cut = apply_tools(
            &b,
            &Tools {
                start: 0.25,
                length: 0.5,
                ..Tools::default()
            },
        );
        // 4 frames: start 1, half of the 3 left (rounded to 2).
        assert_eq!(cut.left, [0.3, 0.5]);
    }
}
