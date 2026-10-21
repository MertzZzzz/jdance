// SPDX-License-Identifier: GPL-3.0-or-later
//! Transforming tools (spec 0.2.0 §18.5–§18.12, §18.14–§18.18, §18.20):
//! they change the notes in scope and leave the rest alone.

use std::collections::BTreeMap;

use super::{
    Cand, Input, MAX_RESULT_NOTES, Mode, Outcome, Rng, in_scale, onset_groups, replace, snap,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Glue {
    /// Notes of one key this close (ticks) or closer join; 0 = touching
    /// or overlapping only.
    pub gap: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flip {
    pub horizontal: bool,
    pub vertical: bool,
    /// `None` = the centre of the scope's lowest and highest key, rounded
    /// down.
    pub pivot: Option<u8>,
}

impl Default for Flip {
    fn default() -> Self {
        Self {
            horizontal: true,
            vertical: false,
            pivot: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArticulateMode {
    #[default]
    Gate,
    Legato,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Articulate {
    pub mode: ArticulateMode,
    /// 1..=200 % of the original length (Gate).
    pub gate_percent: u32,
    /// Ticks taken off the end.
    pub gap: u32,
    /// 0..=100 % random length variation.
    pub variation_percent: u32,
    pub seed: u64,
    /// Ends stop at the next onset group.
    pub prevent_overlap: bool,
}

impl Default for Articulate {
    fn default() -> Self {
        Self {
            mode: ArticulateMode::Gate,
            gate_percent: 100,
            gap: 0,
            variation_percent: 0,
            seed: 1,
            prevent_overlap: false,
        }
    }
}

/// A custom groove: grid points within one cycle (own JSON schema, see
/// [`Groove::from_json`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Groove {
    /// Cycle length in ticks.
    pub length: u32,
    /// Sorted positions within the cycle, `< length`.
    pub points: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quantize {
    /// Grid step in ticks (triplets: e.g. 160).
    pub grid: u32,
    /// 0..=100 %.
    pub strength: u32,
    pub starts: bool,
    pub ends: bool,
    /// 0..=75 % of a grid step; odd grid points move later.
    pub swing: u32,
    /// Only notes this close (ticks) to a grid point move; 0 = any.
    pub window: u32,
    /// With starts only: keep lengths.
    pub preserve_length: bool,
    /// Used instead of grid and swing when set.
    pub groove: Option<Groove>,
}

impl Default for Quantize {
    fn default() -> Self {
        Self {
            grid: 240,
            strength: 100,
            starts: true,
            ends: false,
            swing: 0,
            window: 0,
            preserve_length: true,
            groove: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChopMode {
    /// Segments of this many ticks.
    Step(u32),
    /// This many equal parts.
    Parts(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chop {
    pub mode: ChopMode,
    /// 1..=100 % of each segment sounds.
    pub gate_percent: u32,
    /// Velocity factor of every second segment (1 = unchanged).
    pub alternate_velocity: f32,
    /// Run the per-segment velocity pattern from the last segment back
    /// (the alternating factor then counts from the end).
    pub reverse_velocity: bool,
}

impl Default for Chop {
    fn default() -> Self {
        Self {
            mode: ChopMode::Step(240),
            gate_percent: 100,
            alternate_velocity: 1.0,
            reverse_velocity: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strum {
    /// High to low instead of low to high.
    pub downward: bool,
    /// Delay per note in ticks.
    pub delay: u32,
    /// Notes starting within this many ticks of a group's first start
    /// belong to it.
    pub tolerance: u32,
    /// MIDI velocity added per note along the strum (may be negative).
    pub velocity_ramp: i32,
    /// Keep the original ends (lengths shrink) instead of the lengths.
    pub preserve_ends: bool,
}

impl Default for Strum {
    fn default() -> Self {
        Self {
            downward: false,
            delay: 30,
            tolerance: 10,
            velocity_ramp: 0,
            preserve_ends: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flam {
    /// Grace note start before the main note, ticks.
    pub offset: u32,
    pub grace_length: u32,
    /// Grace velocity as a factor of the main note's.
    pub grace_velocity: f32,
    /// A grace that would start before 0 moves the pair later instead of
    /// being skipped.
    pub shift_pair: bool,
}

impl Default for Flam {
    fn default() -> Self {
        Self {
            offset: 60,
            grace_length: 40,
            grace_velocity: 0.6,
            shift_pair: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub min: u8,
    pub max: u8,
    pub transpose: i32,
    /// Octave fold instead of clamping.
    pub fold: bool,
    /// Snap to this scale (root pitch class, mode) before limiting.
    pub scale: Option<(u8, Mode)>,
}

impl Default for Limit {
    fn default() -> Self {
        Self {
            min: 36,
            max: 84,
            transpose: 0,
            fold: true,
            scale: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleLevels {
    /// MIDI velocity 1..=127.
    pub center: u8,
    pub multiplier: f32,
    /// Normalized offset added after scaling.
    pub offset: f32,
    pub invert: bool,
    /// Power curve with `exponent` (> 0); off = linear.
    pub power: bool,
    pub exponent: f32,
}

impl Default for ScaleLevels {
    fn default() -> Self {
        Self {
            center: 64,
            multiplier: 1.0,
            offset: 0.0,
            invert: false,
            power: false,
            exponent: 1.0,
        }
    }
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// `x × percent / 100` rounded to the nearest tick (ties up), for `x ≥ 0`.
fn percent_of(x: i64, percent: u32) -> i64 {
    (2 * x * i64::from(percent) + 100).div_euclid(200)
}

/// MIDI velocity `v × factor`, rounded (clamped later by [`replace`]).
fn scale_velocity(v: i32, factor: f64) -> i32 {
    (f64::from(v) * factor).round().clamp(-1e6, 1e6) as i32
}

/// Each scope index with the start of the next onset group in the scope
/// (`None` for the last group). Notes outside the scope are no boundary.
fn with_next_onset(input: &Input, scope: &[usize]) -> Vec<(usize, Option<i64>)> {
    let groups = onset_groups(input, scope);
    let mut out = Vec::with_capacity(scope.len());
    for (gi, g) in groups.iter().enumerate() {
        let next = groups
            .get(gi + 1)
            .map(|h| i64::from(input.notes[h[0]].start));
        out.extend(g.iter().map(|&i| (i, next)));
    }
    out
}

/// Rejects a split that would make more notes than allowed before any
/// segment is allocated.
fn check_segments(input: &Input, segments: u64) -> Result<(), String> {
    let total = segments + (input.notes.len() as u64);
    if total > MAX_RESULT_NOTES as u64 {
        return Err(format!(
            "the result would have {total} notes; at most {MAX_RESULT_NOTES} are allowed"
        ));
    }
    Ok(())
}

/// `c` split at `bounds` (ascending, first = start, last = end); the
/// first part keeps the ID.
fn split_at(c: Cand, bounds: &[i64]) -> Vec<Cand> {
    bounds
        .windows(2)
        .enumerate()
        .map(|(j, w)| {
            let part = Cand {
                start: w[0],
                length: w[1] - w[0],
                ..c
            };
            if j == 0 { part } else { part.fresh() }
        })
        .collect()
}

/// Bounds of consecutive `step`-tick segments from the note's start; the
/// remainder is a last, shorter segment.
fn step_bounds(c: &Cand, step: i64) -> Vec<i64> {
    let mut b: Vec<i64> = (0..)
        .map(|k| c.start + k * step)
        .take_while(|&t| t < c.end())
        .collect();
    b.push(c.end());
    b
}

/// Number of `step`-tick segments of a note of `length` ticks.
fn step_count(length: i64, step: i64) -> u64 {
    (length.max(0) as u64).div_ceil(step.max(1) as u64)
}

// ---------------------------------------------------------------------
// Quick tools (§18.5, §18.7, §18.8, §18.10)
// ---------------------------------------------------------------------

/// §18.5: every note of an onset group ends where the next onset group of
/// the scope starts; the last group keeps its lengths.
pub fn quick_legato(input: &Input) -> Result<Outcome, String> {
    let scope = input.scope();
    let cands = with_next_onset(input, &scope)
        .into_iter()
        .map(|(i, next)| {
            let mut c = Cand::of(&input.notes[i]);
            if let Some(t) = next {
                c.length = t - c.start;
            }
            c
        })
        .collect();
    replace(input, &scope, cands)
}

/// §18.7 (`ends`): start and end snap to the grid independently, an empty
/// result becomes 1 tick. §18.8 (`!ends`): only the start snaps, the
/// length is kept.
pub fn quick_quantize(input: &Input, ends: bool) -> Result<Outcome, String> {
    let scope = input.scope();
    let g = input.grid();
    let cands = scope
        .iter()
        .map(|&i| {
            let mut c = Cand::of(&input.notes[i]);
            let start = snap(c.start, g);
            if ends {
                c.length = (snap(c.end(), g) - start).max(1);
            }
            c.start = start;
            c
        })
        .collect();
    replace(input, &scope, cands)
}

/// §18.10: each note is cut into grid-step segments from its start; the
/// remainder is a shorter last note.
pub fn quick_chop(input: &Input) -> Result<Outcome, String> {
    let scope = input.scope();
    let step = i64::from(input.grid());
    let count = scope
        .iter()
        .map(|&i| step_count(i64::from(input.notes[i].length), step))
        .sum();
    check_segments(input, count)?;
    let cands = scope
        .iter()
        .flat_map(|&i| {
            let c = Cand::of(&input.notes[i]);
            split_at(c, &step_bounds(&c, step))
        })
        .collect();
    replace(input, &scope, cands)
}

// ---------------------------------------------------------------------
// Glue (§18.12), Flip (§18.18)
// ---------------------------------------------------------------------

/// §18.12: notes of one key that touch, overlap or are at most `gap`
/// ticks apart become one note `min start..max end` with the velocity and
/// ID of the first one (by start, ID).
pub fn glue(input: &Input, s: &Glue) -> Result<Outcome, String> {
    let scope = input.scope();
    let mut by_key: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
    for &i in &scope {
        by_key.entry(input.notes[i].key).or_default().push(i);
    }
    let gap = i64::from(s.gap);
    let (mut cands, mut merged, mut mismatched) = (Vec::new(), 0usize, 0usize);
    for idx in by_key.values_mut() {
        idx.sort_by_key(|&i| (input.notes[i].start, input.notes[i].id, i));
        // The current merged note, its end and whether velocities differ.
        let mut cur: Option<(Cand, i64, bool)> = None;
        for &i in idx.iter() {
            let c = Cand::of(&input.notes[i]);
            match cur.as_mut() {
                Some((first, end, differ)) if c.start <= *end + gap => {
                    *end = (*end).max(c.end());
                    *differ |= c.velocity != first.velocity;
                    merged += 1;
                }
                _ => {
                    if let Some((mut first, end, differ)) = cur.take() {
                        first.length = end - first.start;
                        mismatched += usize::from(differ);
                        cands.push(first);
                    }
                    cur = Some((c, c.end(), false));
                }
            }
        }
        if let Some((mut first, end, differ)) = cur {
            first.length = end - first.start;
            mismatched += usize::from(differ);
            cands.push(first);
        }
    }
    let mut out = replace(input, &scope, cands)?;
    if merged > 0 {
        out.info.push(format!("{merged} note(s) glued into others"));
    }
    if mismatched > 0 {
        out.info.push(format!(
            "{mismatched} glued note(s) joined different velocities; the first note's velocity is kept"
        ));
    }
    Ok(out)
}

/// §18.18: horizontal mirrors time within `min start..max end` of the
/// scope (lengths kept), vertical mirrors keys around the pivot. Keys
/// leaving 0..=127 are clamped and counted.
pub fn flip(input: &Input, s: &Flip) -> Result<Outcome, String> {
    let scope = input.scope();
    let notes: Vec<Cand> = scope.iter().map(|&i| Cand::of(&input.notes[i])).collect();
    let range_start = notes.iter().map(|c| c.start).min().unwrap_or(0);
    let range_end = notes.iter().map(Cand::end).max().unwrap_or(0);
    let low = notes.iter().map(|c| c.key).min().unwrap_or(0);
    let high = notes.iter().map(|c| c.key).max().unwrap_or(0);
    // Keys are non-negative, so `/` rounds down.
    let pivot = s.pivot.map_or((low + high) / 2, i32::from);
    let cands = notes
        .into_iter()
        .map(|mut c| {
            if s.horizontal {
                c.start = range_start + range_end - c.end();
            }
            if s.vertical {
                c.key = 2 * pivot - c.key;
            }
            c
        })
        .collect();
    replace(input, &scope, cands)
}

// ---------------------------------------------------------------------
// Articulate (§18.6)
// ---------------------------------------------------------------------

/// §18.6: Gate (length × gate %) or Legato (up to the next onset group of
/// the scope; the last group keeps its length), then random variation and
/// the gap; at least 1 tick; optionally never past the next onset group.
pub fn articulate(input: &Input, s: &Articulate) -> Result<Outcome, String> {
    let scope = input.scope();
    let gate = s.gate_percent.clamp(1, 200);
    let variation = f64::from(s.variation_percent.min(100)) / 100.0;
    let gap = i64::from(s.gap);
    let mut rng = Rng::new(s.seed);
    let cands = with_next_onset(input, &scope)
        .into_iter()
        .map(|(i, next)| {
            let mut c = Cand::of(&input.notes[i]);
            let mut len = match s.mode {
                ArticulateMode::Gate => percent_of(c.length, gate),
                ArticulateMode::Legato => next.map_or(c.length, |t| t - c.start),
            };
            if variation > 0.0 {
                // Uniform factor in [1 - v, 1 + v), one draw per note.
                let f = 1.0 + variation * (2.0 * rng.unit() - 1.0);
                len = (len as f64 * f).round() as i64;
            }
            len = (len - gap).max(1);
            if s.prevent_overlap
                && let Some(t) = next
            {
                len = len.min(t - c.start);
            }
            c.length = len;
            c
        })
        .collect();
    replace(input, &scope, cands)
}

// ---------------------------------------------------------------------
// Quantize (§18.9) and grooves
// ---------------------------------------------------------------------

/// Where positions are pulled to.
enum Target<'a> {
    /// Grid points `k × step`, odd ones `swing` ticks later.
    Grid {
        step: i64,
        swing: i64,
    },
    Groove(&'a Groove),
}

impl Target<'_> {
    /// The nearest target point of `t` (ties toward the later one).
    fn nearest(&self, t: i64) -> i64 {
        let candidates: Vec<i64> = match self {
            Self::Grid { step, swing } => {
                let k0 = t.div_euclid(*step);
                (k0 - 1..=k0 + 2)
                    .map(|k| k * step + if k.rem_euclid(2) == 1 { *swing } else { 0 })
                    .collect()
            }
            Self::Groove(g) => {
                let len = i64::from(g.length);
                let c0 = t.div_euclid(len);
                (c0 - 1..=c0 + 1)
                    .flat_map(|c| g.points.iter().map(move |&p| c * len + i64::from(p)))
                    .collect()
            }
        };
        let mut best = t;
        let mut dist = i64::MAX;
        for p in candidates {
            let d = (p - t).abs();
            if d < dist || (d == dist && p > best) {
                best = p;
                dist = d;
            }
        }
        best
    }
}

/// §18.9: pulls starts and/or ends toward the nearest grid (with swing) or
/// groove point by `strength` %, rounded to a tick; with a window only
/// positions at most that far from their point move.
pub fn quantize(input: &Input, s: &Quantize) -> Result<Outcome, String> {
    let target = match &s.groove {
        Some(g) => {
            g.validate()?;
            Target::Groove(g)
        }
        None if s.grid == 0 => return Err("the grid step must be at least 1 tick".into()),
        None => Target::Grid {
            step: i64::from(s.grid),
            swing: percent_of(i64::from(s.grid), s.swing.min(75)),
        },
    };
    let strength = f64::from(s.strength.min(100)) / 100.0;
    let window = i64::from(s.window);
    let pull = |t: i64| -> i64 {
        let p = target.nearest(t);
        if window > 0 && (p - t).abs() > window {
            return t;
        }
        t + ((p - t) as f64 * strength).round() as i64
    };
    let scope = input.scope();
    let cands = scope
        .iter()
        .map(|&i| {
            let mut c = Cand::of(&input.notes[i]);
            let (start, end) = (c.start, c.end());
            match (s.starts, s.ends) {
                (true, false) => {
                    c.start = pull(start);
                    if !s.preserve_length {
                        c.length = end - c.start;
                    }
                }
                (false, true) => c.length = pull(end) - start,
                (true, true) => {
                    c.start = pull(start);
                    c.length = pull(end) - c.start;
                }
                (false, false) => {}
            }
            c.length = c.length.max(1);
            c
        })
        .collect();
    let mut out = replace(input, &scope, cands)?;
    if !s.starts && !s.ends {
        out.info
            .push("no target chosen: turn on starts and/or ends".into());
    }
    Ok(out)
}

/// Identifies the groove JSON schema.
pub const GROOVE_FORMAT: &str = "jdance-groove";
/// Version of the groove JSON schema.
pub const GROOVE_VERSION: u32 = 1;

impl Groove {
    /// Parses the own groove schema:
    /// `{"format":"jdance-groove","version":1,"length":960,"points":[0,250,480,730]}`.
    /// Unknown keys are ignored; points must be ascending and `< length`.
    pub fn from_json(text: &str) -> Result<Groove, String> {
        let value = json::parse(text).map_err(|e| format!("invalid groove JSON: {e}"))?;
        let json::Value::Object(fields) = value else {
            return Err("invalid groove: expected a JSON object".into());
        };
        let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v);
        match field("format") {
            Some(json::Value::String(f)) if f == GROOVE_FORMAT => {}
            Some(_) => {
                return Err(format!(
                    "invalid groove: format must be \"{GROOVE_FORMAT}\""
                ));
            }
            None => return Err("invalid groove: missing \"format\"".into()),
        }
        let version = field("version")
            .and_then(json::Value::as_u32)
            .ok_or("invalid groove: missing or bad \"version\"")?;
        if version != GROOVE_VERSION {
            return Err(format!(
                "unsupported groove version {version} (supported: {GROOVE_VERSION})"
            ));
        }
        let length = field("length")
            .and_then(json::Value::as_u32)
            .ok_or("invalid groove: \"length\" must be a whole number of ticks")?;
        let Some(json::Value::Array(items)) = field("points") else {
            return Err("invalid groove: \"points\" must be an array".into());
        };
        let points = items
            .iter()
            .map(|v| {
                v.as_u32()
                    .ok_or_else(|| "invalid groove: points must be whole ticks".to_string())
            })
            .collect::<Result<Vec<u32>, String>>()?;
        let g = Groove { length, points };
        g.validate()?;
        Ok(g)
    }

    /// The groove in the schema read by [`Groove::from_json`].
    pub fn to_json(&self) -> String {
        let points: Vec<String> = self.points.iter().map(u32::to_string).collect();
        format!(
            "{{\"format\":\"{GROOVE_FORMAT}\",\"version\":{GROOVE_VERSION},\"length\":{},\"points\":[{}]}}",
            self.length,
            points.join(",")
        )
    }

    /// Length ≥ 1, at least one point, points strictly ascending and
    /// `< length`.
    pub fn validate(&self) -> Result<(), String> {
        if self.length == 0 {
            return Err("invalid groove: length must be at least 1 tick".into());
        }
        if self.points.is_empty() {
            return Err("invalid groove: it needs at least one point".into());
        }
        if self.points.windows(2).any(|w| w[0] >= w[1]) {
            return Err("invalid groove: points must be strictly ascending".into());
        }
        if self.points.last().is_some_and(|&p| p >= self.length) {
            return Err("invalid groove: points must be less than the length".into());
        }
        Ok(())
    }
}

/// A tiny JSON reader for the groove schema (the crate has no JSON
/// dependency).
mod json {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        Number(f64),
        String(String),
        Array(Vec<Value>),
        Object(Vec<(String, Value)>),
    }

    impl Value {
        /// A whole number in `0..=u32::MAX`.
        pub fn as_u32(&self) -> Option<u32> {
            match *self {
                Self::Number(x) if x.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&x) => {
                    Some(x as u32)
                }
                _ => None,
            }
        }
    }

    const MAX_DEPTH: usize = 64;

    pub fn parse(text: &str) -> Result<Value, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            i: 0,
        };
        let v = p.value(0)?;
        p.ws();
        if p.i != p.s.len() {
            return Err(format!("unexpected text at byte {}", p.i));
        }
        Ok(v)
    }

    struct Parser<'a> {
        s: &'a [u8],
        i: usize,
    }

    impl Parser<'_> {
        fn ws(&mut self) {
            while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
                self.i += 1;
            }
        }

        fn peek(&self) -> Option<u8> {
            self.s.get(self.i).copied()
        }

        fn eat(&mut self, b: u8) -> Result<(), String> {
            self.ws();
            if self.peek() == Some(b) {
                self.i += 1;
                Ok(())
            } else {
                Err(format!("expected '{}' at byte {}", char::from(b), self.i))
            }
        }

        fn literal(&mut self, word: &str, v: Value) -> Result<Value, String> {
            if self.s[self.i..].starts_with(word.as_bytes()) {
                self.i += word.len();
                Ok(v)
            } else {
                Err(format!("unexpected text at byte {}", self.i))
            }
        }

        fn value(&mut self, depth: usize) -> Result<Value, String> {
            if depth > MAX_DEPTH {
                return Err("nesting too deep".into());
            }
            self.ws();
            match self.peek() {
                Some(b'{') => self.object(depth),
                Some(b'[') => self.array(depth),
                Some(b'"') => self.string().map(Value::String),
                Some(b't') => self.literal("true", Value::Bool(true)),
                Some(b'f') => self.literal("false", Value::Bool(false)),
                Some(b'n') => self.literal("null", Value::Null),
                Some(b'-' | b'0'..=b'9') => self.number(),
                Some(_) => Err(format!("unexpected character at byte {}", self.i)),
                None => Err("unexpected end of input".into()),
            }
        }

        fn object(&mut self, depth: usize) -> Result<Value, String> {
            self.eat(b'{')?;
            let mut fields = Vec::new();
            self.ws();
            if self.peek() == Some(b'}') {
                self.i += 1;
                return Ok(Value::Object(fields));
            }
            loop {
                self.ws();
                let key = self.string()?;
                self.eat(b':')?;
                fields.push((key, self.value(depth + 1)?));
                self.ws();
                match self.peek() {
                    Some(b',') => self.i += 1,
                    Some(b'}') => {
                        self.i += 1;
                        return Ok(Value::Object(fields));
                    }
                    _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
                }
            }
        }

        fn array(&mut self, depth: usize) -> Result<Value, String> {
            self.eat(b'[')?;
            let mut items = Vec::new();
            self.ws();
            if self.peek() == Some(b']') {
                self.i += 1;
                return Ok(Value::Array(items));
            }
            loop {
                items.push(self.value(depth + 1)?);
                self.ws();
                match self.peek() {
                    Some(b',') => self.i += 1,
                    Some(b']') => {
                        self.i += 1;
                        return Ok(Value::Array(items));
                    }
                    _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
                }
            }
        }

        fn string(&mut self) -> Result<String, String> {
            if self.peek() != Some(b'"') {
                return Err(format!("expected a string at byte {}", self.i));
            }
            self.i += 1;
            let mut out: Vec<u8> = Vec::new();
            loop {
                let b = self.peek().ok_or("unterminated string")?;
                self.i += 1;
                match b {
                    b'"' => break,
                    b'\\' => {
                        let e = self.peek().ok_or("unterminated string")?;
                        self.i += 1;
                        let c = match e {
                            b'"' => '"',
                            b'\\' => '\\',
                            b'/' => '/',
                            b'b' => '\u{8}',
                            b'f' => '\u{c}',
                            b'n' => '\n',
                            b'r' => '\r',
                            b't' => '\t',
                            b'u' => {
                                let hex = self
                                    .s
                                    .get(self.i..self.i + 4)
                                    .and_then(|h| std::str::from_utf8(h).ok())
                                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                                    .ok_or("bad \\u escape")?;
                                self.i += 4;
                                char::from_u32(hex).unwrap_or('\u{fffd}')
                            }
                            _ => return Err(format!("bad escape at byte {}", self.i)),
                        };
                        let mut buf = [0; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                    _ => out.push(b),
                }
            }
            String::from_utf8(out).map_err(|_| "string is not UTF-8".into())
        }

        fn number(&mut self) -> Result<Value, String> {
            let from = self.i;
            while self
                .peek()
                .is_some_and(|b| matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
            {
                self.i += 1;
            }
            std::str::from_utf8(&self.s[from..self.i])
                .ok()
                .and_then(|t| t.parse::<f64>().ok())
                .filter(|x| x.is_finite())
                .map(Value::Number)
                .ok_or_else(|| format!("bad number at byte {from}"))
        }
    }
}

// ---------------------------------------------------------------------
// Chop (§18.11)
// ---------------------------------------------------------------------

/// §18.11: fixed-step segments (the remainder is a shorter last one) or
/// N equal parts whose bounds come from the original range
/// (`start + k × length / N`, N at most the length). Each segment sounds
/// for `gate` % of itself from its start (≥ 1 tick); every second
/// segment's velocity is multiplied by `alternate_velocity`.
pub fn chop(input: &Input, s: &Chop) -> Result<Outcome, String> {
    if !s.alternate_velocity.is_finite() || s.alternate_velocity < 0.0 {
        return Err("the alternating velocity factor must be a number ≥ 0".into());
    }
    let scope = input.scope();
    let gate = s.gate_percent.clamp(1, 100);
    let alt = f64::from(s.alternate_velocity);
    // Parts used for a note: N, at most its length in ticks.
    let parts = |len: i64, n: u32| i64::from(n).min(len.max(1));
    let count: u64 = match s.mode {
        ChopMode::Step(0) => return Err("the chop step must be at least 1 tick".into()),
        ChopMode::Parts(0) => return Err("chop into at least 1 part".into()),
        ChopMode::Step(step) => scope
            .iter()
            .map(|&i| step_count(i64::from(input.notes[i].length), i64::from(step)))
            .sum(),
        ChopMode::Parts(n) => scope
            .iter()
            .map(|&i| parts(i64::from(input.notes[i].length), n) as u64)
            .sum(),
    };
    check_segments(input, count)?;
    let mut limited = 0usize;
    let mut cands = Vec::new();
    for &i in &scope {
        let c = Cand::of(&input.notes[i]);
        let bounds = match s.mode {
            ChopMode::Step(step) => step_bounds(&c, i64::from(step)),
            ChopMode::Parts(n) => {
                let k_max = parts(c.length, n);
                limited += usize::from(k_max < i64::from(n));
                (0..=k_max)
                    .map(|k| c.start + k * c.length / k_max)
                    .collect()
            }
        };
        let segments = split_at(c, &bounds);
        let last = segments.len().saturating_sub(1);
        for (j, mut seg) in segments.into_iter().enumerate() {
            let pos = if s.reverse_velocity { last - j } else { j };
            if pos % 2 == 1 {
                seg.velocity = scale_velocity(seg.velocity, alt);
            }
            seg.length = percent_of(seg.length, gate).max(1);
            cands.push(seg);
        }
    }
    let mut out = replace(input, &scope, cands)?;
    if limited > 0 {
        out.info.push(format!(
            "{limited} note(s) are shorter than the number of parts and were cut into 1-tick parts"
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// Strum (§18.14), Flam (§18.15)
// ---------------------------------------------------------------------

/// §18.14: notes starting within `tolerance` of a group's first start form
/// a chord; its i-th note (by key, low to high or high to low) starts
/// `i × delay` later, with `i × velocity_ramp` added. With preserve ends
/// the original end is kept and a note left without length is an error.
pub fn strum(input: &Input, s: &Strum) -> Result<Outcome, String> {
    let scope = input.scope();
    let tolerance = i64::from(s.tolerance);
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in &scope {
        let start = i64::from(input.notes[i].start);
        match groups.last_mut() {
            Some(g) if start - i64::from(input.notes[g[0]].start) <= tolerance => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    let (mut cands, mut invalid) = (Vec::with_capacity(scope.len()), 0usize);
    for mut g in groups {
        g.sort_by_key(|&i| {
            let n = &input.notes[i];
            let key = if s.downward {
                -i32::from(n.key)
            } else {
                i32::from(n.key)
            };
            (key, n.start, n.id, i)
        });
        for (k, &i) in g.iter().enumerate() {
            let mut c = Cand::of(&input.notes[i]);
            let end = c.end();
            let k = k as i64;
            c.start += k * i64::from(s.delay);
            c.velocity = c
                .velocity
                .saturating_add((k as i32).saturating_mul(s.velocity_ramp));
            if s.preserve_ends {
                c.length = end - c.start;
                invalid += usize::from(c.length < 1);
            }
            cands.push(c);
        }
    }
    if invalid > 0 {
        return Err(format!(
            "with preserve ends, {invalid} note(s) would end at or before their new start; \
             lower the delay or turn preserve ends off"
        ));
    }
    replace(input, &scope, cands)
}

/// §18.15: a grace note of the same key `offset` ticks before each note,
/// ending no later than the main start. A grace that would start before
/// tick 0 is skipped, or with `shift_pair` the main note moves later so
/// the grace starts at 0. The main note keeps its ID.
pub fn flam(input: &Input, s: &Flam) -> Result<Outcome, String> {
    if s.offset == 0 {
        return Err("the flam offset must be at least 1 tick".into());
    }
    if s.grace_length == 0 {
        return Err("the grace note length must be at least 1 tick".into());
    }
    if !s.grace_velocity.is_finite() || s.grace_velocity < 0.0 {
        return Err("the grace velocity factor must be a number ≥ 0".into());
    }
    let scope = input.scope();
    let offset = i64::from(s.offset);
    let grace_len = i64::from(s.grace_length).min(offset);
    let factor = f64::from(s.grace_velocity);
    let (mut skipped, mut shifted, mut max_shift) = (0usize, 0usize, 0i64);
    let mut cands = Vec::with_capacity(2 * scope.len());
    for &i in &scope {
        let mut main = Cand::of(&input.notes[i]);
        let mut grace_start = main.start - offset;
        if grace_start < 0 {
            if !s.shift_pair {
                skipped += 1;
                cands.push(main);
                continue;
            }
            main.start -= grace_start;
            max_shift = max_shift.max(-grace_start);
            shifted += 1;
            grace_start = 0;
        }
        cands.push(
            Cand {
                start: grace_start,
                length: grace_len,
                velocity: scale_velocity(main.velocity, factor),
                ..main
            }
            .fresh(),
        );
        cands.push(main);
    }
    let mut out = replace(input, &scope, cands)?;
    if skipped > 0 {
        out.info.push(format!(
            "{skipped} grace note(s) skipped: they would start before the pattern"
        ));
    }
    if shifted > 0 {
        out.info.push(format!(
            "{shifted} pair(s) moved later by up to {max_shift} ticks so the grace starts at 0"
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// Limit (§18.17), Scale Levels (§18.20)
// ---------------------------------------------------------------------

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The nearest key of the scale (ties toward the lower key).
fn snap_to_scale(key: i32, root: i32, mode: Mode) -> i32 {
    (0..12)
        .flat_map(|d| [key - d, key + d])
        .find(|&k| in_scale(k, root, mode))
        .unwrap_or(key)
}

/// §18.17: transpose, then optionally snap to the scale, then keep the key
/// in `min..=max` by clamping or by the nearest octave of the same pitch
/// class (an error when the range holds none).
pub fn limit(input: &Input, s: &Limit) -> Result<Outcome, String> {
    if s.min > s.max {
        return Err(format!(
            "the lowest key ({}) is above the highest ({})",
            s.min, s.max
        ));
    }
    let (min, max) = (i32::from(s.min), i32::from(s.max));
    let scope = input.scope();
    let mut unfoldable: Vec<usize> = Vec::new();
    let mut cands = Vec::with_capacity(scope.len());
    for &i in &scope {
        let mut c = Cand::of(&input.notes[i]);
        let mut key = c.key.saturating_add(s.transpose);
        if let Some((root, mode)) = s.scale {
            key = snap_to_scale(key, i32::from(root % 12), mode);
        }
        if !s.fold {
            key = key.clamp(min, max);
        } else if !(min..=max).contains(&key) {
            // The same pitch class closest to the range: the first octave
            // above `min` (below the range) or below `max` (above it).
            let folded = if key < min {
                min + (key - min).rem_euclid(12)
            } else {
                max - (max - key).rem_euclid(12)
            };
            if (min..=max).contains(&folded) {
                key = folded;
            } else {
                unfoldable.push(key.rem_euclid(12) as usize);
            }
        }
        c.key = key;
        cands.push(c);
    }
    if let Some(&pc) = unfoldable.first() {
        return Err(format!(
            "octave fold: {} note(s) have no key of their pitch class (e.g. {}) between {} and {}; \
             widen the range or use clamp",
            unfoldable.len(),
            NOTE_NAMES[pc],
            s.min,
            s.max
        ));
    }
    replace(input, &scope, cands)
}

/// §18.20: velocity `x = (v - 1) / 126` is scaled around the normalized
/// center, offset, clamped to 0..1, optionally inverted and raised to the
/// exponent, then mapped back with `round(1 + 126 x)`.
pub fn scale_levels(input: &Input, s: &ScaleLevels) -> Result<Outcome, String> {
    if !s.multiplier.is_finite() || !s.offset.is_finite() {
        return Err("multiplier and offset must be numbers".into());
    }
    if s.power && !(s.exponent.is_finite() && s.exponent > 0.0) {
        return Err("the exponent must be greater than 0".into());
    }
    let center = f64::from(i32::from(s.center.clamp(1, 127)) - 1) / 126.0;
    let (mul, off, exp) = (
        f64::from(s.multiplier),
        f64::from(s.offset),
        f64::from(s.exponent),
    );
    let scope = input.scope();
    let cands = scope
        .iter()
        .map(|&i| {
            let mut c = Cand::of(&input.notes[i]);
            let x = f64::from(c.velocity - 1) / 126.0;
            let mut x = ((x - center) * mul + center + off).clamp(0.0, 1.0);
            if s.invert {
                x = 1.0 - x;
            }
            if s.power {
                x = x.powf(exp);
            }
            c.velocity = (1.0 + 126.0 * x).round() as i32;
            c
        })
        .collect();
    replace(input, &scope, cands)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::super::{TICKS_PER_BAR, midi_velocity};
    use super::*;
    use jdance_domain::Note;

    fn vels(o: &Outcome) -> Vec<u8> {
        o.notes.iter().map(|n| midi_velocity(n.velocity)).collect()
    }

    fn ids(o: &Outcome) -> Vec<u32> {
        o.notes.iter().map(|n| n.id).collect()
    }

    fn unique_ids(o: &Outcome) -> bool {
        let mut v = ids(o);
        v.sort_unstable();
        v.windows(2).all(|w| w[0] != w[1]) && !v.contains(&0)
    }

    fn chord() -> Vec<Note> {
        vec![
            n(1, 0, 480, 60, 100),
            n(2, 0, 480, 64, 100),
            n(3, 0, 480, 67, 100),
            n(4, 960, 240, 72, 90),
        ]
    }

    /// Runs every transform with default settings on `notes`/`sel`.
    fn all_defaults(notes: &[Note], sel: &[usize]) -> Vec<Outcome> {
        let inp = input(notes, sel);
        vec![
            quick_legato(&inp).unwrap(),
            quick_quantize(&inp, true).unwrap(),
            quick_quantize(&inp, false).unwrap(),
            quick_chop(&inp).unwrap(),
            glue(&inp, &Glue::default()).unwrap(),
            flip(&inp, &Flip::default()).unwrap(),
            articulate(&inp, &Articulate::default()).unwrap(),
            quantize(&inp, &Quantize::default()).unwrap(),
            chop(&inp, &Chop::default()).unwrap(),
            strum(&inp, &Strum::default()).unwrap(),
            flam(&inp, &Flam::default()).unwrap(),
            limit(&inp, &Limit::default()).unwrap(),
            scale_levels(&inp, &ScaleLevels::default()).unwrap(),
        ]
    }

    #[test]
    fn empty_row_is_empty_result() {
        for o in all_defaults(&[], &[]) {
            assert!(o.notes.is_empty() && o.select.is_empty());
        }
    }

    #[test]
    fn notes_outside_the_selection_are_untouched() {
        let notes = [
            n(1, 10, 500, 60, 100),
            n(2, 970, 300, 30, 40),
            n(3, 1930, 500, 100, 70),
            n(4, 2000, 700, 61, 20),
        ];
        for o in all_defaults(&notes, &[0, 2]) {
            for keep in [notes[1], notes[3]] {
                assert!(o.notes.contains(&keep), "{keep:?} changed in {o:?}");
            }
            assert!(unique_ids(&o));
            assert!(o.notes.iter().all(|n| n.length >= 1));
            assert!(o.notes.iter().all(|n| n.start + n.length <= TICKS_PER_BAR));
        }
    }

    #[test]
    fn identity_settings_change_nothing() {
        let notes = chord();
        let inp = input(&notes, &[]);
        let same = |o: Outcome| assert_eq!(o.notes, notes.to_vec());
        same(scale_levels(&inp, &ScaleLevels::default()).unwrap());
        same(articulate(&inp, &Articulate::default()).unwrap());
        same(
            limit(
                &inp,
                &Limit {
                    min: 0,
                    max: 127,
                    ..Limit::default()
                },
            )
            .unwrap(),
        );
        same(glue(&inp, &Glue::default()).unwrap());
        same(
            flip(
                &inp,
                &Flip {
                    horizontal: false,
                    ..Flip::default()
                },
            )
            .unwrap(),
        );
        same(
            quantize(
                &inp,
                &Quantize {
                    strength: 0,
                    ..Quantize::default()
                },
            )
            .unwrap(),
        );
        same(
            chop(
                &inp,
                &Chop {
                    mode: ChopMode::Parts(1),
                    ..Chop::default()
                },
            )
            .unwrap(),
        );
        same(
            strum(
                &inp,
                &Strum {
                    delay: 0,
                    ..Strum::default()
                },
            )
            .unwrap(),
        );
    }

    #[test]
    fn legato_uses_only_scope_onsets() {
        let notes = [
            n(1, 0, 100, 60, 100),
            n(2, 0, 50, 64, 100),
            n(3, 240, 100, 62, 100), // outside the selection
            n(4, 480, 100, 65, 100),
        ];
        let o = quick_legato(&input(&notes, &[0, 1, 3])).unwrap();
        assert_eq!(
            shape(&o),
            [(0, 480, 60), (0, 480, 64), (240, 100, 62), (480, 100, 65)]
        );
        assert_eq!(ids(&o), [1, 2, 3, 4]);
    }

    #[test]
    fn quick_quantize_ends_and_starts() {
        let notes = [n(1, 130, 100, 60, 100), n(2, 300, 10, 62, 100)];
        let inp = input(&notes, &[]);
        let o = quick_quantize(&inp, true).unwrap();
        // 130..230 -> 240..240 -> 1 tick; 300..310 -> 240..240 -> 1 tick.
        assert_eq!(shape(&o), [(240, 1, 60), (240, 1, 62)]);
        let o = quick_quantize(&inp, false).unwrap();
        assert_eq!(shape(&o), [(240, 100, 60), (240, 10, 62)]);
        // A start snapped onto the pattern end is reported, not dropped silently.
        let late = [n(1, 3830, 10, 60, 100)];
        let o = quick_quantize(&input(&late, &[]), false).unwrap();
        assert_eq!(o.dropped, 1);
        assert!(!o.info.is_empty());
    }

    #[test]
    fn quick_chop_splits_with_remainder_and_keeps_first_id() {
        let notes = [n(5, 100, 600, 60, 90)];
        let o = quick_chop(&input(&notes, &[])).unwrap();
        assert_eq!(shape(&o), [(100, 240, 60), (340, 240, 60), (580, 120, 60)]);
        assert_eq!(ids(&o), [5, 6, 7]);
        assert_eq!(vels(&o), [90, 90, 90]);
    }

    #[test]
    fn glue_merges_by_key_and_reports_velocity() {
        let notes = [
            n(1, 0, 240, 60, 100),
            n(2, 240, 240, 60, 80),  // touching
            n(3, 400, 300, 60, 100), // overlapping
            n(4, 800, 100, 60, 100), // 100-tick gap
            n(5, 0, 900, 62, 100),   // other key
        ];
        let o = glue(&input(&notes, &[]), &Glue::default()).unwrap();
        assert_eq!(shape(&o), [(0, 700, 60), (0, 900, 62), (800, 100, 60)]);
        assert_eq!(ids(&o), [1, 5, 4]);
        assert_eq!(vels(&o)[0], 100);
        assert!(o.info.iter().any(|l| l.contains("velocit")));
        let o = glue(&input(&notes, &[]), &Glue { gap: 100 }).unwrap();
        assert_eq!(shape(&o), [(0, 900, 60), (0, 900, 62)]);
    }

    #[test]
    fn flip_horizontal_and_vertical() {
        let notes = [n(1, 0, 240, 60, 100), n(2, 480, 480, 67, 50)];
        let inp = input(&notes, &[]);
        let o = flip(&inp, &Flip::default()).unwrap();
        // Range 0..960.
        assert_eq!(shape(&o), [(0, 480, 67), (720, 240, 60)]);
        assert_eq!(vels(&o), [50, 100]);
        let v = Flip {
            horizontal: false,
            vertical: true,
            pivot: None,
        };
        // Pivot floor((60 + 67) / 2) = 63.
        assert_eq!(
            shape(&flip(&inp, &v).unwrap()),
            [(0, 240, 66), (480, 480, 59)]
        );
        let o = flip(
            &inp,
            &Flip {
                pivot: Some(120),
                ..v
            },
        )
        .unwrap();
        assert_eq!(o.clamped, 2, "pitch overflow is counted");
        assert!(o.notes.iter().all(|n| n.key == 127));
    }

    #[test]
    fn articulate_gate_legato_gap_and_overlap() {
        let notes = chord();
        let inp = input(&notes, &[]);
        let gate = |p, gap, prevent| Articulate {
            gate_percent: p,
            gap,
            prevent_overlap: prevent,
            ..Articulate::default()
        };
        let o = articulate(&inp, &gate(50, 0, false)).unwrap();
        assert_eq!(o.notes[0].length, 240);
        assert_eq!(o.notes[3].length, 120);
        let o = articulate(&inp, &gate(200, 0, true)).unwrap();
        assert_eq!(o.notes[0].length, 960, "stops at the next onset");
        assert_eq!(o.notes[3].length, 480);
        let o = articulate(&inp, &gate(100, 10_000, false)).unwrap();
        assert!(o.notes.iter().all(|n| n.length == 1));
        let legato = Articulate {
            mode: ArticulateMode::Legato,
            gap: 10,
            ..Articulate::default()
        };
        let o = articulate(&inp, &legato).unwrap();
        assert_eq!(o.notes[0].length, 950);
        assert_eq!(o.notes[3].length, 230, "last group keeps its length");
    }

    #[test]
    fn articulate_variation_is_seeded_and_stable() {
        let notes = chord();
        let inp = input(&notes, &[]);
        let s = Articulate {
            variation_percent: 50,
            seed: 99,
            ..Articulate::default()
        };
        let a = articulate(&inp, &s).unwrap();
        assert_eq!(a, articulate(&inp, &s).unwrap());
        assert!(
            a.notes
                .iter()
                .zip(&notes)
                .all(|(x, o)| x.length * 2 >= o.length && x.length <= o.length * 3 / 2)
        );
        let b = articulate(&inp, &Articulate { seed: 100, ..s }).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn quantize_strength_swing_window() {
        let notes = [n(1, 100, 100, 60, 100), n(2, 250, 100, 62, 100)];
        let inp = input(&notes, &[]);
        let q = |s: Quantize| shape(&quantize(&inp, &s).unwrap());
        assert_eq!(
            q(Quantize::default()),
            [(0, 100, 60), (240, 100, 62)],
            "100 is nearer to 0"
        );
        assert_eq!(
            q(Quantize {
                strength: 50,
                ..Quantize::default()
            }),
            [(50, 100, 60), (245, 100, 62)]
        );
        // 50 % swing: odd points at 240k + 120 -> 360, 840, …
        assert_eq!(
            q(Quantize {
                swing: 50,
                ..Quantize::default()
            }),
            [(0, 100, 60), (360, 100, 62)]
        );
        assert_eq!(
            q(Quantize {
                window: 20,
                ..Quantize::default()
            }),
            [(100, 100, 60), (240, 100, 62)],
            "only notes within the window move"
        );
        // Start only without preserve: end stays.
        assert_eq!(
            q(Quantize {
                preserve_length: false,
                ..Quantize::default()
            }),
            [(0, 200, 60), (240, 110, 62)]
        );
        // Start and end: 100..200 -> 0..240; 250..350 -> 240..240 -> 1.
        assert_eq!(
            q(Quantize {
                ends: true,
                ..Quantize::default()
            }),
            [(0, 240, 60), (240, 1, 62)]
        );
        assert!(
            quantize(
                &inp,
                &Quantize {
                    grid: 0,
                    ..Quantize::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn quantize_to_groove() {
        let g = Groove {
            length: 960,
            points: vec![0, 250, 480, 730],
        };
        let notes = [n(1, 200, 10, 60, 100), n(2, 900, 10, 60, 100)];
        let o = quantize(
            &input(&notes, &[]),
            &Quantize {
                groove: Some(g),
                swing: 75,
                ..Quantize::default()
            },
        )
        .unwrap();
        // 900 is nearer the next cycle's 0 (960) than 730.
        assert_eq!(shape(&o), [(250, 10, 60), (960, 10, 60)]);
    }

    #[test]
    fn groove_json_round_trip_and_rejection() {
        let text = r#" { "format" : "jdance-groove", "version": 1, "name": "xé",
            "length": 960, "points": [0, 250, 480, 730], "extra": {"a": [true, null]} } "#;
        let g = Groove::from_json(text).unwrap();
        assert_eq!(g.length, 960);
        assert_eq!(g.points, [0, 250, 480, 730]);
        assert_eq!(Groove::from_json(&g.to_json()).unwrap(), g);
        for bad in [
            "",
            "[]",
            r#"{"format":"other","version":1,"length":960,"points":[0]}"#,
            r#"{"format":"jdance-groove","version":2,"length":960,"points":[0]}"#,
            r#"{"format":"jdance-groove","version":1,"length":0,"points":[]}"#,
            r#"{"format":"jdance-groove","version":1,"length":960,"points":[480,240]}"#,
            r#"{"format":"jdance-groove","version":1,"length":960,"points":[0,960]}"#,
            r#"{"format":"jdance-groove","version":1,"length":960,"points":[0.5]}"#,
            r#"{"format":"jdance-groove","version":1,"length":960,"points":[]}"#,
            r#"{"format":"jdance-groove","version":1,"length":960,"points":[0]"#,
            r#"{"format":"jdance-groove","version":1,"length":960,"points":[0]} x"#,
        ] {
            assert!(Groove::from_json(bad).is_err(), "{bad}");
        }
        assert!(Groove::from_json(&"[".repeat(1000)).is_err());
    }

    #[test]
    fn chop_parts_are_exact() {
        let notes = [n(1, 0, 1000, 60, 100)];
        let s = Chop {
            mode: ChopMode::Parts(3),
            ..Chop::default()
        };
        let o = chop(&input(&notes, &[]), &s).unwrap();
        // 0, 333, 666, 1000 from the original range.
        assert_eq!(shape(&o), [(0, 333, 60), (333, 333, 60), (666, 334, 60)]);
        assert_eq!(ids(&o), [1, 2, 3]);
        // N above the length: 1-tick parts and an info line.
        let short = [n(1, 0, 2, 60, 100)];
        let o = chop(&input(&short, &[]), &s).unwrap();
        assert_eq!(shape(&o), [(0, 1, 60), (1, 1, 60)]);
        assert!(!o.info.is_empty());
    }

    #[test]
    fn chop_gate_and_velocity_patterns() {
        let notes = [n(1, 0, 960, 60, 100)];
        let inp = input(&notes, &[]);
        let s = Chop {
            mode: ChopMode::Step(300),
            gate_percent: 50,
            alternate_velocity: 0.5,
            reverse_velocity: false,
        };
        let o = chop(&inp, &s).unwrap();
        assert_eq!(
            shape(&o),
            [(0, 150, 60), (300, 150, 60), (600, 150, 60), (900, 30, 60)]
        );
        assert_eq!(vels(&o), [100, 50, 100, 50]);
        let o = chop(
            &inp,
            &Chop {
                mode: ChopMode::Parts(3),
                reverse_velocity: true,
                gate_percent: 1,
                ..s
            },
        )
        .unwrap();
        assert_eq!(vels(&o), [100, 50, 100]);
        assert!(o.notes.iter().all(|n| n.length == 3));
        let o = chop(
            &inp,
            &Chop {
                mode: ChopMode::Parts(4),
                reverse_velocity: true,
                ..s
            },
        )
        .unwrap();
        assert_eq!(vels(&o), [50, 100, 50, 100], "pattern from the end");
        for bad in [ChopMode::Step(0), ChopMode::Parts(0)] {
            assert!(chop(&inp, &Chop { mode: bad, ..s }).is_err());
        }
        let many = Chop {
            mode: ChopMode::Step(1),
            ..Chop::default()
        };
        let long = [n(1, 0, 3840, 60, 100)];
        let big: Vec<Note> = (0..30).map(|i| n(i + 1, 0, 3840, i as u8, 100)).collect();
        assert!(chop(&input(&long, &[]), &many).is_ok());
        assert!(chop(&input(&big, &[]), &many).is_err(), "result limit");
    }

    #[test]
    fn strum_groups_from_first_start_and_orders() {
        let notes = [
            n(1, 0, 480, 64, 100),
            n(2, 8, 480, 60, 100),
            n(3, 16, 480, 67, 100), // 16 > tolerance 10 from 0: own group
        ];
        let inp = input(&notes, &[]);
        let s = Strum {
            delay: 30,
            velocity_ramp: -10,
            ..Strum::default()
        };
        let o = strum(&inp, &s).unwrap();
        // Group {60@8, 64@0}: 60 stays, 64 +30; 67 alone.
        assert_eq!(shape(&o), [(8, 480, 60), (16, 480, 67), (30, 480, 64)]);
        assert_eq!(vels(&o), [100, 100, 90]);
        let o = strum(
            &inp,
            &Strum {
                downward: true,
                preserve_ends: true,
                ..s
            },
        )
        .unwrap();
        assert_eq!(shape(&o), [(0, 480, 64), (16, 480, 67), (38, 450, 60)]);
    }

    #[test]
    fn strum_preserve_ends_rejects_empty_notes() {
        let notes = [n(1, 0, 20, 60, 100), n(2, 0, 20, 64, 100)];
        let s = Strum {
            delay: 20,
            preserve_ends: true,
            ..Strum::default()
        };
        assert!(strum(&input(&notes, &[]), &s).is_err());
        let s = Strum { delay: 19, ..s };
        let o = strum(&input(&notes, &[]), &s).unwrap();
        assert_eq!(shape(&o), [(0, 20, 60), (19, 1, 64)]);
    }

    #[test]
    fn flam_adds_grace_notes_and_handles_the_start() {
        let notes = [n(1, 30, 240, 60, 100), n(2, 480, 240, 62, 100)];
        let inp = input(&notes, &[]);
        let o = flam(&inp, &Flam::default()).unwrap();
        assert_eq!(shape(&o), [(30, 240, 60), (420, 40, 62), (480, 240, 62)]);
        assert_eq!(vels(&o), [100, 60, 100]);
        assert_eq!(ids(&o), [1, 3, 2], "main notes keep IDs");
        assert!(o.info.iter().any(|l| l.contains("skipped")));
        let s = Flam {
            offset: 30,
            grace_length: 100,
            shift_pair: true,
            ..Flam::default()
        };
        let notes = [n(1, 10, 240, 60, 100)];
        let o = flam(&input(&notes, &[]), &s).unwrap();
        // Grace 0..30 (not past the main start), main moved to 30.
        assert_eq!(shape(&o), [(0, 30, 60), (30, 240, 60)]);
        assert_eq!(o.notes[1].id, 1);
        assert!(o.info.iter().any(|l| l.contains("moved")));
        assert!(
            flam(
                &inp,
                &Flam {
                    offset: 0,
                    ..Flam::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn limit_transpose_scale_clamp_and_fold() {
        let notes = [
            n(1, 0, 10, 30, 100),
            n(2, 0, 10, 61, 100),
            n(3, 0, 10, 100, 100),
        ];
        let inp = input(&notes, &[]);
        let keys = |s: Limit| -> Vec<u8> {
            limit(&inp, &s)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.key)
                .collect()
        };
        assert_eq!(keys(Limit::default()), [42, 61, 76], "fold into 36..=84");
        assert_eq!(
            keys(Limit {
                fold: false,
                ..Limit::default()
            }),
            [36, 61, 84]
        );
        // +1, then C major: 31 (G), 62 (D), 101 (F) are in the scale.
        assert_eq!(
            keys(Limit {
                min: 0,
                max: 127,
                transpose: 1,
                scale: Some((0, Mode::Major)),
                ..Limit::default()
            }),
            [31, 62, 101]
        );
        // C#: tie between C and D goes down.
        assert_eq!(
            keys(Limit {
                min: 0,
                max: 127,
                scale: Some((0, Mode::Major)),
                ..Limit::default()
            }),
            [29, 60, 100]
        );
        let narrow = Limit {
            min: 62,
            max: 66,
            ..Limit::default()
        };
        let e = limit(&inp, &narrow).unwrap_err();
        assert!(e.contains("octave fold"), "{e}");
        assert!(
            limit(
                &inp,
                &Limit {
                    min: 80,
                    max: 40,
                    ..Limit::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn scale_levels_math() {
        let notes = [
            n(1, 0, 10, 60, 1),
            n(2, 0, 10, 62, 64),
            n(3, 0, 10, 64, 127),
        ];
        let inp = input(&notes, &[]);
        let v = |s: ScaleLevels| vels(&scale_levels(&inp, &s).unwrap());
        assert_eq!(v(ScaleLevels::default()), [1, 64, 127]);
        assert_eq!(
            v(ScaleLevels {
                multiplier: 0.5,
                ..ScaleLevels::default()
            }),
            [33, 64, 96]
        );
        assert_eq!(
            v(ScaleLevels {
                offset: 1.0,
                ..ScaleLevels::default()
            }),
            [127, 127, 127]
        );
        assert_eq!(
            v(ScaleLevels {
                invert: true,
                ..ScaleLevels::default()
            }),
            [127, 64, 1]
        );
        assert_eq!(
            v(ScaleLevels {
                power: true,
                exponent: 2.0,
                ..ScaleLevels::default()
            }),
            [1, 33, 127]
        );
        assert!(
            scale_levels(
                &inp,
                &ScaleLevels {
                    power: true,
                    exponent: 0.0,
                    ..ScaleLevels::default()
                }
            )
            .is_err()
        );
    }
}
