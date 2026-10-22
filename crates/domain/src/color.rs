// SPDX-License-Identifier: GPL-3.0-or-later
//! Colors of lanes, mixer tracks, channels and patterns (spec 0.2.5 §6).
//!
//! Stored as 8-bit sRGB. Shown as RGB 0–255, HSL with hue 0–360 and
//! saturation and lightness 0–100, and `#RRGGBB`. The wheel's cells, the
//! "safe" range and the colour names are this project's own: the names
//! are hue words with tone words, generated below, not a copied table.

/// An 8-bit sRGB colour.
pub type Rgb = [u8; 3];

/// Colour of a pattern that has none of its own.
pub const DEFAULT_PATTERN_COLOR: Rgb = [0x5B, 0x8D, 0xD9];

/// Longest name of a lane, track, channel or pattern set by a drop or a
/// preset (spec 0.2.5 §7 item 10); longer names are cut.
pub const MAX_NAME_CHARS: usize = 64;

/// `name` cut to [`MAX_NAME_CHARS`] characters.
pub fn clip_name(name: &str) -> String {
    name.trim().chars().take(MAX_NAME_CHARS).collect()
}

/// `#RRGGBB` (upper case).
pub fn to_hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// `#RGB` or `#RRGGBB`, with or without `#`, any case.
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let t = text.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = |s: &str| u8::from_str_radix(s, 16).ok();
    match t.len() {
        3 => {
            let d: Vec<u8> = t.chars().filter_map(|c| v(&c.to_string())).collect();
            Some([d[0] * 17, d[1] * 17, d[2] * 17])
        }
        6 => Some([v(&t[0..2])?, v(&t[2..4])?, v(&t[4..6])?]),
        _ => None,
    }
}

/// Hue 0–360, saturation and lightness 0–100.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsl {
    pub h: f32,
    pub s: f32,
    pub l: f32,
}

pub fn to_hsl(c: Rgb) -> Hsl {
    let [r, g, b] = c.map(|v| f32::from(v) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return Hsl {
            h: 0.0,
            s: 0.0,
            l: l * 100.0,
        };
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    Hsl {
        h,
        s: (s * 100.0).min(100.0),
        l: l * 100.0,
    }
}

pub fn from_hsl(c: Hsl) -> Rgb {
    let (h, s, l) = (
        c.h.rem_euclid(360.0),
        (c.s / 100.0).clamp(0.0, 1.0),
        (c.l / 100.0).clamp(0.0, 1.0),
    );
    let ch = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = ch * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - ch / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (ch, x, 0.0),
        1 => (x, ch, 0.0),
        2 => (0.0, ch, x),
        3 => (0.0, x, ch),
        4 => (x, 0.0, ch),
        _ => (ch, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// CIE L*a*b* (D65) of an sRGB colour, for colour distances.
pub fn to_lab(c: Rgb) -> [f32; 3] {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = c.map(lin);
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| {
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_distance(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (to_lab(a), to_lab(b));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Hue words of the name table: (name, hue).
const HUES: [(&str, f32); 14] = [
    ("Red", 0.0),
    ("Vermilion", 15.0),
    ("Orange", 30.0),
    ("Amber", 45.0),
    ("Yellow", 58.0),
    ("Lime", 85.0),
    ("Green", 125.0),
    ("Jade", 155.0),
    ("Teal", 178.0),
    ("Sky", 200.0),
    ("Blue", 222.0),
    ("Indigo", 248.0),
    ("Violet", 275.0),
    ("Magenta", 310.0),
];

/// Tone words: (word, saturation, lightness).
const TONES: [(&str, f32, f32); 8] = [
    ("Pale", 55.0, 85.0),
    ("Light", 65.0, 70.0),
    ("", 75.0, 50.0),
    ("Vivid", 100.0, 50.0),
    ("Soft", 35.0, 60.0),
    ("Muted", 25.0, 42.0),
    ("Deep", 80.0, 32.0),
    ("Dark", 55.0, 20.0),
];

/// Greys of the name table.
const GREYS: [(&str, f32); 9] = [
    ("White", 100.0),
    ("Snow Gray", 92.0),
    ("Silver", 78.0),
    ("Light Gray", 66.0),
    ("Gray", 50.0),
    ("Slate Gray", 38.0),
    ("Charcoal", 24.0),
    ("Graphite", 13.0),
    ("Black", 0.0),
];

/// The colour name table: 14 hues × 8 tones plus 9 greys (121 names).
pub fn color_names() -> Vec<(String, Rgb)> {
    let mut out = Vec::with_capacity(HUES.len() * TONES.len() + GREYS.len());
    for (hue, h) in HUES {
        for (tone, s, l) in TONES {
            let name = if tone.is_empty() {
                hue.to_owned()
            } else {
                format!("{tone} {hue}")
            };
            out.push((name, from_hsl(Hsl { h, s, l })));
        }
    }
    for (name, l) in GREYS {
        out.push((name.to_owned(), from_hsl(Hsl { h: 0.0, s: 0.0, l })));
    }
    out
}

/// The nearest name in [`color_names`] by distance in Lab.
pub fn color_name(c: Rgb) -> String {
    color_names()
        .into_iter()
        .min_by(|a, b| lab_distance(a.1, c).total_cmp(&lab_distance(b.1, c)))
        .map(|(n, _)| n)
        .unwrap_or_default()
}

/// Saturation and lightness of "safe" colours: calm enough to sit behind
/// text in the interface (spec 0.2.5 §6, the lock).
pub const SAFE_SATURATION: (f32, f32) = (20.0, 70.0);
pub const SAFE_LIGHTNESS: (f32, f32) = (30.0, 65.0);

/// `c` moved into the safe range, hue kept.
pub fn safe(c: Rgb) -> Rgb {
    let h = to_hsl(c);
    if h.s < 1.0 {
        // Greys stay grey, within the lightness range.
        return from_hsl(Hsl {
            l: h.l.clamp(SAFE_LIGHTNESS.0, SAFE_LIGHTNESS.1),
            ..h
        });
    }
    from_hsl(Hsl {
        h: h.h,
        s: h.s.clamp(SAFE_SATURATION.0, SAFE_SATURATION.1),
        l: h.l.clamp(SAFE_LIGHTNESS.0, SAFE_LIGHTNESS.1),
    })
}

/// Hue sectors of the wheel.
pub const WHEEL_SECTORS: usize = 12;
/// Rings of the wheel from the centre out: saturation of each.
pub const WHEEL_RINGS: [f32; 4] = [25.0, 50.0, 75.0, 100.0];
/// Greys of the outer ring.
pub const WHEEL_GREYS: usize = 12;

/// The colour of wheel cell (`sector`, `ring`) at `lightness`.
pub fn wheel_cell(sector: usize, ring: usize, lightness: f32) -> Rgb {
    from_hsl(Hsl {
        h: sector as f32 * 360.0 / WHEEL_SECTORS as f32,
        s: WHEEL_RINGS[ring.min(WHEEL_RINGS.len() - 1)],
        l: lightness,
    })
}

/// A random safe colour from a seed (the dice button): a wheel cell at a
/// safe lightness.
pub fn random_safe(seed: u64) -> Rgb {
    // SplitMix64 step: well spread for consecutive seeds.
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let sector = (z % WHEEL_SECTORS as u64) as usize;
    let ring = 1 + ((z >> 8) % 2) as usize;
    let l = SAFE_LIGHTNESS.0 + ((z >> 16) % 30) as f32;
    safe(wheel_cell(sector, ring, l))
}

/// Up to ten recent colours, newest first; a repeat moves to the front.
pub fn push_history(history: &mut Vec<Rgb>, c: Rgb) {
    history.retain(|h| *h != c);
    history.insert(0, c);
    history.truncate(10);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_short_long_with_and_without_hash() {
        assert_eq!(parse_hex("#7E686A"), Some([126, 104, 106]));
        assert_eq!(parse_hex("7e686a"), Some([126, 104, 106]));
        assert_eq!(parse_hex("#fA0"), Some([255, 170, 0]));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#GG0000"), None);
        assert_eq!(to_hex([126, 104, 106]), "#7E686A");
    }

    #[test]
    fn hsl_round_trips() {
        for c in [
            [126, 104, 106],
            [255, 0, 0],
            [0, 128, 64],
            [12, 34, 200],
            [128, 128, 128],
        ] {
            let back = from_hsl(to_hsl(c));
            for i in 0..3 {
                assert!(back[i].abs_diff(c[i]) <= 1, "{c:?} → {back:?}");
            }
        }
        let h = to_hsl([255, 0, 0]);
        assert_eq!((h.h, h.s, h.l), (0.0, 100.0, 50.0));
    }

    #[test]
    fn names_are_many_unique_and_nearest() {
        let names = color_names();
        assert!(names.len() >= 100);
        let set: std::collections::BTreeSet<_> = names.iter().map(|n| &n.0).collect();
        assert_eq!(set.len(), names.len());
        assert_eq!(color_name([255, 0, 0]), "Vivid Red");
        assert_eq!(color_name([0, 0, 0]), "Black");
        // The same colour, the same name, whichever way it was entered.
        assert_eq!(
            color_name(parse_hex("#7E686A").unwrap()),
            color_name([126, 104, 106])
        );
    }

    #[test]
    fn safe_colours_stay_in_range_and_history_keeps_ten() {
        for seed in 0..200 {
            let h = to_hsl(random_safe(seed));
            assert!(
                h.l >= SAFE_LIGHTNESS.0 - 1.0 && h.l <= SAFE_LIGHTNESS.1 + 1.0,
                "{h:?}"
            );
            assert!(h.s <= SAFE_SATURATION.1 + 1.0, "{h:?}");
        }
        let mut hist = Vec::new();
        for i in 0..12u8 {
            push_history(&mut hist, [i, 0, 0]);
        }
        assert_eq!(hist.len(), 10);
        assert_eq!(hist[0], [11, 0, 0]);
        push_history(&mut hist, [5, 0, 0]);
        assert_eq!(hist[0], [5, 0, 0], "a repeat moves to the front");
        assert_eq!(hist.len(), 10);
    }
}
