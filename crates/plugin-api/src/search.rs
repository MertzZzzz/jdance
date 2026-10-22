// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin search and favorites (spec 0.2.5 §10): the query language,
//! ranking and the user's favorites, recent plugins and filters. Pure
//! functions; the UI draws the list.

use crate::{PluginDescriptor, PluginFormat, PluginKind};
use serde::{Deserialize, Serialize};

/// Recent plugins kept (§10 item 14).
pub const MAX_RECENT: usize = 10;

/// A favorite, by the plugin's stable identity: it survives rescans and
/// updates, and shows greyed out while the plugin is missing (§10 items
/// 9, 11). Name and vendor are kept to show a missing plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginFavorite {
    pub format: PluginFormat,
    pub class_id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    pub instrument: bool,
}

impl PluginFavorite {
    pub fn of(d: &PluginDescriptor) -> Self {
        Self {
            format: d.plugin.format,
            class_id: d.plugin.class_id.clone(),
            name: d.plugin.name.clone(),
            vendor: d.plugin.vendor.clone(),
            instrument: d.kind == PluginKind::Instrument,
        }
    }

    pub fn is(&self, d: &PluginDescriptor) -> bool {
        self.format == d.plugin.format && self.class_id == d.plugin.class_id
    }

    /// The same plugin (names may have changed with an update).
    pub fn is_same(&self, other: &Self) -> bool {
        self.format == other.format && self.class_id == other.class_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindFilter {
    #[default]
    All,
    Instruments,
    Effects,
}

/// Favorites, recent plugins and the list's filters: user settings, not
/// the project (§10 items 2, 7).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginPrefs {
    /// In the user's order.
    pub favorites: Vec<PluginFavorite>,
    /// Class ids, newest first.
    pub recent: Vec<String>,
    pub kind: KindFilter,
    /// Formats hidden by the format filter.
    pub hidden_formats: Vec<PluginFormat>,
    pub only_favorites: bool,
    /// Favorites sorted by name instead of the user's order.
    pub favorites_alphabetical: bool,
}

impl PluginPrefs {
    pub fn is_favorite(&self, d: &PluginDescriptor) -> bool {
        self.favorites.iter().any(|f| f.is(d))
    }

    /// Adds or removes `d` from the favorites.
    pub fn toggle_favorite(&mut self, d: &PluginDescriptor) {
        if self.is_favorite(d) {
            self.favorites.retain(|f| !f.is(d));
        } else {
            self.favorites.push(PluginFavorite::of(d));
        }
    }

    /// Remembers a loaded plugin (§10 item 14).
    pub fn note_used(&mut self, d: &PluginDescriptor) {
        self.recent.retain(|c| *c != d.plugin.class_id);
        self.recent.insert(0, d.plugin.class_id.clone());
        self.recent.truncate(MAX_RECENT);
    }

    /// Whether the filters let `d` through (the text aside).
    pub fn passes(&self, d: &PluginDescriptor) -> bool {
        let kind = match self.kind {
            KindFilter::All => true,
            KindFilter::Instruments => d.kind == PluginKind::Instrument,
            KindFilter::Effects => d.kind == PluginKind::Effect,
        };
        kind && !self.hidden_formats.contains(&d.plugin.format)
            && (!self.only_favorites || self.is_favorite(d))
    }

    /// Filters back to their defaults (the "Reset filters" button).
    pub fn reset_filters(&mut self) {
        self.kind = KindFilter::All;
        self.hidden_formats.clear();
        self.only_favorites = false;
    }
}

/// Lower case without diacritics (§10 item 5): "Sérum" finds "serum".
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => 'a',
            'ç' | 'ć' | 'č' => 'c',
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ě' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'ī' => 'i',
            'ñ' | 'ń' | 'ň' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => 'o',
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' => 'u',
            'ý' | 'ÿ' => 'y',
            'ž' | 'ź' | 'ż' => 'z',
            'š' | 'ś' => 's',
            'ř' => 'r',
            'ł' => 'l',
            'ё' => 'е',
            c => c,
        })
        .collect()
}

/// A parsed query: words that must all match and words that must not
/// (§10 item 5). A word may hold `*` for any part of a word.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Query {
    pub fn parse(text: &str) -> Self {
        let mut q = Self::default();
        for w in fold(text).split_whitespace() {
            match w.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => q.exclude.push(rest.to_owned()),
                Some(_) => {}
                None => q.include.push(w.to_owned()),
            }
        }
        q
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

/// Where `pattern` (with `*` wildcards) first matches in `text`, as a
/// char range. Both folded.
fn find(text: &str, pattern: &str) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let parts: Vec<Vec<char>> = pattern
        .split('*')
        .filter(|p| !p.is_empty())
        .map(|p| p.chars().collect())
        .collect();
    if parts.is_empty() {
        return Some((0, 0));
    }
    let at = |from: usize, part: &[char]| -> Option<usize> {
        (from..=chars.len().checked_sub(part.len())?).find(|&i| chars[i..i + part.len()] == *part)
    };
    // The first part fixes the start; the rest follow inside one word.
    let mut start = 0;
    while let Some(s) = at(start, &parts[0]) {
        let mut end = s + parts[0].len();
        let mut ok = true;
        for part in &parts[1..] {
            match at(end, part) {
                Some(i) if !chars[end..i].iter().any(|c| c.is_whitespace()) => {
                    end = i + part.len();
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some((s, end));
        }
        start = s + 1;
    }
    None
}

fn word_start(text: &str, at: usize) -> bool {
    at == 0
        || text
            .chars()
            .nth(at - 1)
            .is_some_and(|c| !c.is_alphanumeric())
}

/// How well a plugin matched, best first (§10 item 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    NameStart,
    WordStart,
    Contains,
    Other,
}

/// A plugin that matched: its index, rank and the matched chars of its
/// name, to highlight (§10 item 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub rank: Rank,
    pub highlight: Vec<(usize, usize)>,
}

/// Matches `d` against `q`: every word in the name, vendor, category
/// or format; no excluded word anywhere.
pub fn matches(d: &PluginDescriptor, q: &Query) -> Option<(Rank, Vec<(usize, usize)>)> {
    let name = fold(&d.plugin.name);
    let other = fold(&format!(
        "{} {} {}",
        d.plugin.vendor,
        d.category,
        d.plugin.format.label()
    ));
    if q.exclude
        .iter()
        .any(|w| find(&name, w).is_some() || find(&other, w).is_some())
    {
        return None;
    }
    let mut rank = Rank::NameStart;
    let mut highlight = Vec::new();
    for (i, w) in q.include.iter().enumerate() {
        let r = match find(&name, w) {
            Some((a, b)) => {
                highlight.push((a, b));
                if a == 0 && i == 0 {
                    Rank::NameStart
                } else if word_start(&name, a) {
                    Rank::WordStart
                } else {
                    Rank::Contains
                }
            }
            None => {
                find(&other, w)?;
                Rank::Other
            }
        };
        rank = rank.max(r);
    }
    Some((rank, highlight))
}

/// The plugins passing the filters and matching `text`, best first: by
/// rank, then favorites, then recently used, then by name (§10 item 6).
pub fn search(plugins: &[PluginDescriptor], prefs: &PluginPrefs, text: &str) -> Vec<Hit> {
    let q = Query::parse(text);
    let mut hits: Vec<(Hit, bool, usize, String)> = plugins
        .iter()
        .enumerate()
        .filter(|(_, d)| prefs.passes(d))
        .filter_map(|(index, d)| {
            let (rank, highlight) = matches(d, &q)?;
            let recent = prefs
                .recent
                .iter()
                .position(|c| *c == d.plugin.class_id)
                .unwrap_or(usize::MAX);
            Some((
                Hit {
                    index,
                    rank,
                    highlight,
                },
                !prefs.is_favorite(d),
                recent,
                fold(&d.plugin.name),
            ))
        })
        .collect();
    hits.sort_by(|a, b| {
        (a.0.rank, a.1, a.2, &a.3, a.0.index).cmp(&(b.0.rank, b.1, b.2, &b.3, b.0.index))
    });
    hits.into_iter().map(|h| h.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginRef;

    pub(crate) fn plugin(name: &str, vendor: &str, kind: PluginKind) -> PluginDescriptor {
        PluginDescriptor {
            plugin: PluginRef {
                format: PluginFormat::Vst3,
                class_id: format!("id-{name}"),
                name: name.into(),
                vendor: vendor.into(),
                version: "1".into(),
            },
            kind,
            category: match kind {
                PluginKind::Instrument => "Instrument|Synth".into(),
                PluginKind::Effect => "Fx|Reverb".into(),
            },
            path: format!("/p/{name}.vst3"),
            sdk_version: None,
        }
    }

    fn names(list: &[PluginDescriptor], prefs: &PluginPrefs, q: &str) -> Vec<String> {
        search(list, prefs, q)
            .iter()
            .map(|h| list[h.index].plugin.name.clone())
            .collect()
    }

    fn catalog() -> Vec<PluginDescriptor> {
        use PluginKind::*;
        vec![
            plugin("Laser", "Acme", Instrument),
            plugin("Serum", "Xfer", Instrument),
            plugin("Big Serpent", "Zoo", Instrument),
            plugin("Vintage Reverb", "Old Co", Effect),
            plugin("Plate Reverb", "Acme", Effect),
            plugin("Sérénade", "Ünique", Instrument),
        ]
    }

    #[test]
    fn name_start_ranks_above_word_start_and_contains() {
        let c = catalog();
        let p = PluginPrefs::default();
        assert_eq!(
            names(&c, &p, "ser"),
            ["Sérénade", "Serum", "Big Serpent", "Laser"]
        );
        let hit = &search(&c, &p, "seru")[0];
        assert_eq!(hit.highlight, [(0, 4)]);
        // Diacritics and case ignored both ways.
        assert_eq!(names(&c, &p, "SERE"), ["Sérénade"]);
        assert_eq!(names(&c, &p, "unique"), ["Sérénade"], "by vendor");
    }

    #[test]
    fn words_combine_and_minus_excludes() {
        let c = catalog();
        let p = PluginPrefs::default();
        assert_eq!(names(&c, &p, "reverb -vintage"), ["Plate Reverb"]);
        assert_eq!(names(&c, &p, "acme reverb"), ["Plate Reverb"]);
        assert_eq!(names(&c, &p, "se*m"), ["Serum"], "wildcard inside a word");
        assert_eq!(names(&c, &p, "").len(), c.len());
    }

    #[test]
    fn filters_favorites_and_recent_order_the_list() {
        let c = catalog();
        let mut p = PluginPrefs {
            kind: KindFilter::Effects,
            ..PluginPrefs::default()
        };
        assert_eq!(search(&c, &p, "").len(), 2, "effects only");
        p.kind = KindFilter::All;
        // Equal rank: favorites first, then recent, then by name.
        p.toggle_favorite(&c[4]);
        p.note_used(&c[3]);
        assert_eq!(names(&c, &p, "reverb"), ["Plate Reverb", "Vintage Reverb"]);
        p.toggle_favorite(&c[4]);
        p.toggle_favorite(&c[3]);
        assert_eq!(names(&c, &p, "")[0], "Vintage Reverb");
        p.only_favorites = true;
        assert_eq!(names(&c, &p, ""), ["Vintage Reverb"]);
        p.hidden_formats.push(PluginFormat::Vst3);
        assert!(search(&c, &p, "").is_empty());
        p.reset_filters();
        assert_eq!(search(&c, &p, "").len(), c.len());
        for i in 0..20 {
            let mut d = c[0].clone();
            d.plugin.class_id = format!("x{i}");
            p.note_used(&d);
        }
        assert_eq!(p.recent.len(), MAX_RECENT);
    }

    #[test]
    fn favorites_survive_a_rescan_by_identity() {
        let c = catalog();
        let mut p = PluginPrefs::default();
        p.toggle_favorite(&c[1]);
        // A new version at another path: still the favorite.
        let mut updated = c[1].clone();
        updated.path = "/new/Serum.vst3".into();
        updated.plugin.version = "2".into();
        assert!(p.is_favorite(&updated));
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<PluginPrefs>(&text).unwrap(), p);
    }

    #[test]
    fn five_thousand_plugins_answer_quickly() {
        let mut c = Vec::new();
        for i in 0..5000 {
            c.push(plugin(
                &format!("Plugin {i} Synth"),
                &format!("Vendor {}", i % 97),
                if i % 2 == 0 {
                    PluginKind::Instrument
                } else {
                    PluginKind::Effect
                },
            ));
        }
        let p = PluginPrefs::default();
        let t = std::time::Instant::now();
        let hits = search(&c, &p, "syn 12");
        let took = t.elapsed();
        assert!(!hits.is_empty());
        // 50 ms in release builds (§10); debug builds are slower.
        let limit = if cfg!(debug_assertions) { 500 } else { 50 };
        assert!(took.as_millis() < limit, "{took:?}");
    }
}
