// SPDX-License-Identifier: GPL-3.0-or-later
//! Name presets: ready names with colours offered when a pattern is
//! created or renamed (spec 0.2.5 §9). User settings, not the project.
//! The factory set is this project's own.

use crate::color::{Rgb, parse_hex, to_hex};
use serde::{Deserialize, Serialize};

/// What a set of presets is for; only patterns use one in 0.2.5.
pub const KIND_PATTERN: &str = "pattern";
pub const PRESETS_VERSION: u32 = 1;
/// Recent names kept below the presets.
pub const MAX_RECENT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PresetItem {
    Preset {
        name: String,
        color: String,
    },
    /// A heading grouping the presets after it; not selectable.
    Category {
        name: String,
    },
}

impl PresetItem {
    pub fn name(&self) -> &str {
        match self {
            Self::Preset { name, .. } | Self::Category { name } => name,
        }
    }

    /// The colour of a preset; `None` for a category or a bad colour.
    pub fn color(&self) -> Option<Rgb> {
        match self {
            Self::Preset { color, .. } => parse_hex(color),
            Self::Category { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamePresets {
    pub version: u32,
    pub kind: String,
    pub items: Vec<PresetItem>,
    /// Names typed recently, newest first.
    #[serde(default)]
    pub recent: Vec<String>,
}

/// A preset that matches the text typed so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetMatch {
    /// Index into `items`.
    pub index: usize,
    /// Byte range of the match in the name, to highlight.
    pub range: Option<(usize, usize)>,
}

impl Default for NamePresets {
    fn default() -> Self {
        Self::factory_patterns()
    }
}

impl NamePresets {
    /// The factory set for patterns: drums warm, basses dark blue and
    /// violet, melodic parts green and teal, voice and effects pink and
    /// gold, song parts grey.
    pub fn factory_patterns() -> Self {
        let cat = |n: &str| PresetItem::Category { name: n.into() };
        let p = |n: &str, c: Rgb| PresetItem::Preset {
            name: n.into(),
            color: to_hex(c),
        };
        Self {
            version: PRESETS_VERSION,
            kind: KIND_PATTERN.into(),
            items: vec![
                cat("Drums"),
                p("Kick", [0xC8, 0x4B, 0x3C]),
                p("Snare", [0xD9, 0x6A, 0x3B]),
                p("Clap", [0xD8, 0x84, 0x45]),
                p("Hat", [0xC9, 0x9A, 0x3E]),
                p("Open Hat", [0xB8, 0x8A, 0x3A]),
                p("Perc", [0xB5, 0x5D, 0x4A]),
                p("Shaker", [0xC2, 0x78, 0x55]),
                p("Tom", [0xA8, 0x4E, 0x3F]),
                p("Cymbal", [0xBF, 0xA0, 0x5A]),
                cat("Bass"),
                p("808", [0x3E, 0x4F, 0x9E]),
                p("Bass", [0x47, 0x5C, 0xB0]),
                p("Sub", [0x5B, 0x46, 0x9C]),
                cat("Melody"),
                p("Lead", [0x3F, 0xA0, 0x6A]),
                p("Pluck", [0x4E, 0xAE, 0x86]),
                p("Arp", [0x3D, 0x9C, 0x9A]),
                p("Keys", [0x58, 0xA8, 0x5E]),
                p("Pad", [0x46, 0x93, 0xA6]),
                p("Chords", [0x5A, 0x9E, 0x7C]),
                cat("Voice and effects"),
                p("Vocal", [0xC0, 0x5C, 0x93]),
                p("FX", [0xB8, 0x9B, 0x3C]),
                p("Riser", [0xC9, 0x7A, 0xA8]),
                p("Impact", [0xA9, 0x8B, 0x2E]),
                cat("Structure"),
                p("Intro", [0x7A, 0x7F, 0x88]),
                p("Verse", [0x6B, 0x70, 0x79]),
                p("Chorus", [0x8A, 0x8F, 0x98]),
                p("Bridge", [0x5E, 0x63, 0x6C]),
                p("Outro", [0x72, 0x77, 0x80]),
            ],
            recent: Vec::new(),
        }
    }

    /// The colour of the preset named `name`, ignoring case (§9 item 5).
    pub fn color_of(&self, name: &str) -> Option<Rgb> {
        let name = name.trim();
        self.items
            .iter()
            .find(|i| matches!(i, PresetItem::Preset { .. }) && i.name().eq_ignore_ascii_case(name))
            .and_then(PresetItem::color)
    }

    /// Presets (and, with nothing typed, the categories) matching
    /// `query`: names whose word starts with it first, then names that
    /// contain it; case is ignored (§9 item 3).
    pub fn filter(&self, query: &str) -> Vec<PresetMatch> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return (0..self.items.len())
                .map(|index| PresetMatch { index, range: None })
                .collect();
        }
        let mut starts = Vec::new();
        let mut contains = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            if !matches!(item, PresetItem::Preset { .. }) {
                continue;
            }
            let lower = item.name().to_lowercase();
            let Some(at) = lower.find(&q) else {
                continue;
            };
            let word_start = at == 0
                || !lower[..at]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric);
            let m = PresetMatch {
                index,
                range: Some((at, at + q.len())),
            };
            if word_start {
                starts.push(m);
            } else {
                contains.push(m);
            }
        }
        starts.extend(contains);
        starts
    }

    /// Remembers a typed name (§9 item 7): numbers and "Pattern N" are
    /// left out; repeats move to the front; five at most.
    pub fn note_recent(&mut self, name: &str) {
        let name = name.trim();
        let is_number = name.parse::<f64>().is_ok();
        let is_default = name
            .strip_prefix("Pattern ")
            .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
        if name.is_empty() || is_number || is_default || self.color_of(name).is_some() {
            return;
        }
        self.recent.retain(|r| !r.eq_ignore_ascii_case(name));
        self.recent.insert(0, name.to_owned());
        self.recent.truncate(MAX_RECENT);
    }
}

/// "Pattern N" with the first N no pattern name uses (§9 item 1).
pub fn first_free_pattern_name<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let used: std::collections::BTreeSet<u64> = names
        .filter_map(|n| n.strip_prefix("Pattern ")?.parse().ok())
        .collect();
    let n = (1..).find(|n| !used.contains(n)).unwrap_or(1);
    format!("Pattern {n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_filters_by_word_start_then_contains() {
        let p = NamePresets::factory_patterns();
        let names = |q: &str| -> Vec<String> {
            p.filter(q)
                .iter()
                .map(|m| p.items[m.index].name().to_owned())
                .collect()
        };
        assert_eq!(names("ki"), ["Kick"]);
        let m = &p.filter("ki")[0];
        assert_eq!(m.range, Some((0, 2)), "the matched part");
        // "hat": Hat and Open Hat start a word; nothing merely contains it.
        assert_eq!(names("hat"), ["Hat", "Open Hat"]);
        // "ar": Arp starts with it; Snare, Chords?, … contain it.
        let ar = names("ar");
        assert_eq!(ar[0], "Arp");
        assert!(ar.contains(&"Snare".to_owned()));
        // Nothing typed: everything, categories included.
        assert_eq!(p.filter("").len(), p.items.len());
        assert_eq!(p.color_of("kick"), p.items[1].color());
    }

    #[test]
    fn recent_names_skip_numbers_defaults_and_presets() {
        let mut p = NamePresets::factory_patterns();
        for n in ["Drop 1", "120", "Pattern 7", "Kick", "Break", "drop 1"] {
            p.note_recent(n);
        }
        assert_eq!(p.recent, ["drop 1", "Break"]);
        for i in 0..10 {
            p.note_recent(&format!("Idea {i}"));
        }
        assert_eq!(p.recent.len(), MAX_RECENT);
        assert_eq!(p.recent[0], "Idea 9");
    }

    #[test]
    fn default_names_take_the_first_free_number_and_json_round_trips() {
        assert_eq!(
            first_free_pattern_name(["Pattern 1", "Kick", "Pattern 3"].into_iter()),
            "Pattern 2"
        );
        assert_eq!(first_free_pattern_name([].into_iter()), "Pattern 1");
        let p = NamePresets::factory_patterns();
        let text = serde_json::to_string(&p).unwrap();
        assert!(text.contains(r#""type":"category""#), "{text}");
        assert_eq!(serde_json::from_str::<NamePresets>(&text).unwrap(), p);
    }
}
