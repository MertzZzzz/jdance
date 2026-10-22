// SPDX-License-Identifier: GPL-3.0-or-later
//! Tools that make new notes (spec 0.2.0 §18.3, §18.4, §18.13, §18.16,
//! §18.19).

use super::{
    Cand, Input, Insertion, MAX_RESULT_NOTES, Mode, Outcome, Range, Rng, degree_key, in_scale,
    onset_groups, replace, starting_in,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArpDirection {
    #[default]
    Up,
    Down,
    UpDown,
    DownUp,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arpeggiate {
    pub direction: ArpDirection,
    /// Ticks per arpeggio note.
    pub step: u32,
    /// 1..=100 % of a step sounds.
    pub gate_percent: u32,
    /// 1..=4 octaves of the chord's keys.
    pub octaves: u8,
    pub seed: u64,
}

impl Default for Arpeggiate {
    fn default() -> Self {
        Self {
            direction: ArpDirection::Up,
            step: 240,
            gate_percent: 90,
            octaves: 1,
            seed: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordProgression {
    /// Pitch class of the tonic, 0 = C.
    pub tonic: u8,
    pub mode: Mode,
    /// Scale degrees 1..=7 in order.
    pub degrees: Vec<u8>,
    pub seventh: bool,
    /// Ticks per chord.
    pub chord_length: u32,
    /// Octave of the first chord's root (MIDI octave, C4 = 60 → 4).
    pub octave: i8,
    /// Inversion of the first chord: 0 = root position.
    pub inversion: u8,
    /// Voice leading: later chords take the inversion closest to the one
    /// before; off = every chord in `inversion`.
    pub close_voicing: bool,
    pub velocity: u8,
    pub insertion: Insertion,
}

impl Default for ChordProgression {
    fn default() -> Self {
        Self {
            tonic: 0,
            mode: Mode::Major,
            degrees: vec![1, 5, 6, 4],
            seventh: false,
            chord_length: jdance_domain::WHOLE_NOTE,
            octave: 4,
            inversion: 0,
            close_voicing: true,
            velocity: 100,
            insertion: Insertion::ReplaceRange,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RandomMode {
    #[default]
    Transform,
    Generate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GenScale {
    #[default]
    Major,
    Minor,
    Chromatic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Randomize {
    pub mode: RandomMode,
    pub seed: u64,
    // Transform: maximum deviations (0 = unchanged).
    pub key_range: u8,
    pub start_range: u32,
    pub length_range: u32,
    pub velocity_range: u8,
    /// Keep transformed keys in this scale (root pitch class, mode).
    pub scale: Option<(u8, Mode)>,
    // Generate.
    pub grid: u32,
    /// Probability of a note in each grid cell, 0..=1.
    pub density: f32,
    pub key_min: u8,
    pub key_max: u8,
    pub gen_scale: GenScale,
    pub root: u8,
    /// Most notes sounding at once.
    pub polyphony: u8,
    pub length_min: u32,
    pub length_max: u32,
    pub velocity_min: u8,
    pub velocity_max: u8,
    pub insertion: Insertion,
}

impl Default for Randomize {
    fn default() -> Self {
        Self {
            mode: RandomMode::Transform,
            seed: 1,
            key_range: 0,
            start_range: 0,
            length_range: 0,
            velocity_range: 0,
            scale: None,
            grid: 240,
            density: 0.5,
            key_min: 60,
            key_max: 72,
            gen_scale: GenScale::Major,
            root: 0,
            polyphony: 1,
            length_min: 240,
            length_max: 240,
            velocity_min: 80,
            velocity_max: 110,
            insertion: Insertion::ReplaceRange,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClawMode {
    #[default]
    Remove,
    Gate,
    Shift,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClawMachine {
    /// Ticks per period.
    pub period: u32,
    pub slices: u32,
    /// Every Nth slice (counted from 1 after the phase) is marked.
    pub every: u32,
    pub phase: u32,
    pub mode: ClawMode,
    /// Shift of marked fragments, ticks.
    pub shift: i32,
}

impl Default for ClawMachine {
    fn default() -> Self {
        Self {
            period: 960,
            slices: 4,
            every: 2,
            phase: 0,
            mode: ClawMode::Remove,
            shift: 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiffMachine {
    pub root: u8,
    pub mode: Mode,
    pub key_min: u8,
    pub key_max: u8,
    /// Rhythm grid in ticks.
    pub division: u32,
    /// 0..=100 % of grid cells get an onset.
    pub density: u32,
    /// Chord degrees 1..=7 (one entry = a fixed chord).
    pub progression: Vec<u8>,
    /// Ticks per chord of the progression.
    pub chord_length: u32,
    pub arp: ArpDirection,
    pub octaves: u8,
    /// 1..=100 % of a cell sounds.
    pub gate_percent: u32,
    pub velocity: u8,
    pub velocity_variation: u8,
    /// Random timing offset, ±ticks.
    pub timing: u32,
    pub seed: u64,
    pub insertion: Insertion,
}

impl Default for RiffMachine {
    fn default() -> Self {
        Self {
            root: 0,
            mode: Mode::Minor,
            key_min: 48,
            key_max: 72,
            division: 240,
            density: 70,
            progression: vec![1, 6, 4, 5],
            chord_length: jdance_domain::WHOLE_NOTE,
            arp: ArpDirection::UpDown,
            octaves: 2,
            gate_percent: 80,
            velocity: 100,
            velocity_variation: 10,
            timing: 0,
            seed: 1,
            insertion: Insertion::ReplaceRange,
        }
    }
}

// ---- shared helpers ----

fn check_count(n: usize) -> Result<(), String> {
    if n > MAX_RESULT_NOTES {
        Err(format!(
            "the result would have more than {MAX_RESULT_NOTES} notes"
        ))
    } else {
        Ok(())
    }
}

/// Uniform in `-r..=r`; always draws one value so that each parameter
/// consumes the generator the same way whatever the other settings.
fn deviation(rng: &mut Rng, r: i64) -> i64 {
    let v = rng.next_u64();
    if r <= 0 {
        0
    } else {
        (v % (2 * r as u64 + 1)) as i64 - r
    }
}

/// Uniform in `lo..=hi` (`lo` when `hi <= lo`); always draws one value.
fn pick(rng: &mut Rng, lo: i64, hi: i64) -> i64 {
    let v = rng.next_u64();
    if hi <= lo {
        lo
    } else {
        lo + (v % ((hi - lo) as u64 + 1)) as i64
    }
}

/// Pool indices in arpeggio order for one cycle (not for `Random`).
/// Up-down and down-up do not repeat the extremes at the turns.
fn arp_order(n: usize, dir: ArpDirection) -> Vec<usize> {
    let inner = 1..n.saturating_sub(1);
    match dir {
        ArpDirection::Up | ArpDirection::Random => (0..n).collect(),
        ArpDirection::Down => (0..n).rev().collect(),
        ArpDirection::UpDown => (0..n).chain(inner.rev()).collect(),
        ArpDirection::DownUp => (0..n).rev().chain(inner).collect(),
    }
}

/// Index of the next arpeggio note: `step` counts the notes of the
/// current group; `Random` uses `draw`.
fn arp_index(order: &[usize], dir: ArpDirection, step: usize, draw: u64) -> usize {
    if dir == ArpDirection::Random {
        (draw % order.len() as u64) as usize
    } else {
        order[step % order.len()]
    }
}

fn removed_by(input: &Input, insertion: Insertion, r: Range) -> Vec<usize> {
    match insertion {
        Insertion::ReplaceRange => starting_in(input, r),
        Insertion::AppendLayer => Vec::new(),
    }
}

/// Validated 0-based degrees of 1..=7 entries.
fn degrees0(degrees: &[u8]) -> Result<Vec<i32>, String> {
    if degrees.is_empty() {
        return Err("the progression has no chords".into());
    }
    degrees
        .iter()
        .map(|&d| {
            if (1..=7).contains(&d) {
                Ok(i32::from(d) - 1)
            } else {
                Err(format!("degree {d} is not in I..VII"))
            }
        })
        .collect()
}

/// Intervals above the root of the diatonic triad or seventh on
/// `degree0` (stacked thirds of the scale).
fn chord_intervals(mode: Mode, degree0: i32, seventh: bool) -> Vec<i32> {
    let base = degree_key(0, mode, degree0);
    let n = if seventh { 4 } else { 3 };
    (0..n)
        .map(|i| degree_key(0, mode, degree0 + 2 * i) - base)
        .collect()
}

// ---- Arpeggiate (§18.13) ----

/// Each onset group of the scope becomes an arpeggio that lasts until
/// the next group starts (the last one: until the group's latest end),
/// never past the active range end. The source notes are replaced.
pub fn arpeggiate(input: &Input, s: &Arpeggiate) -> Result<Outcome, String> {
    let scope = input.scope();
    let groups = onset_groups(input, &scope);
    let step = i64::from(s.step.max(1));
    let sound = (step * i64::from(s.gate_percent.clamp(1, 100)) / 100).max(1);
    let octaves = i32::from(s.octaves.clamp(1, 4));
    let limit = i64::from(input.gen_range().end);
    let mut rng = Rng::new(s.seed);
    let mut new = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let start = i64::from(input.notes[g[0]].start);
        let end = match groups.get(gi + 1) {
            Some(next) => i64::from(input.notes[next[0]].start),
            None => g
                .iter()
                .map(|&i| Cand::of(&input.notes[i]).end())
                .max()
                .unwrap_or(start),
        }
        .min(limit);
        // Unique keys with the velocity of their first source note, then
        // octave copies; an original key wins over an octave copy.
        let mut base: Vec<(i32, i32)> = Vec::new();
        for &i in g {
            let c = Cand::of(&input.notes[i]);
            if !base.iter().any(|&(k, _)| k == c.key) {
                base.push((c.key, c.velocity));
            }
        }
        let mut pool: Vec<(i32, i32)> = (0..octaves)
            .flat_map(|o| base.iter().map(move |&(k, v)| (k + 12 * o, v)))
            .filter(|&(k, _)| k <= 127)
            .collect();
        pool.sort_by_key(|&(k, _)| k);
        pool.dedup_by_key(|p| p.0);
        let order = arp_order(pool.len(), s.direction);
        let mut t = start;
        let mut n = 0;
        while t < end {
            let draw = rng.next_u64();
            let (key, velocity) = pool[arp_index(&order, s.direction, n, draw)];
            new.push(Cand {
                id: 0,
                start: t,
                length: sound.min(end - t),
                key,
                velocity,
            });
            check_count(new.len())?;
            n += 1;
            t += step;
        }
    }
    replace(input, &scope, new)
}

// ---- Generate chord progression (§18.4) ----

/// Sorted keys of a chord: root key, intervals, the lowest `inversion`
/// tones raised an octave.
fn voicing(root: i32, intervals: &[i32], inversion: usize) -> Vec<i32> {
    let mut v: Vec<i32> = intervals
        .iter()
        .enumerate()
        .map(|(i, &iv)| root + iv + if i < inversion { 12 } else { 0 })
        .collect();
    v.sort_unstable();
    v
}

/// Total voice movement from `prev` to `next` (both sorted). Equal
/// counts: sum of `|a_i - b_i|`. Different counts (triad vs seventh) are
/// a documented fallback, not an optimal voice leading: the lowest voices
/// are paired up to the smaller count, and each extra voice of the larger
/// chord adds its distance to the nearest voice of the smaller one.
pub fn voice_movement(prev: &[i32], next: &[i32]) -> i64 {
    let m = prev.len().min(next.len());
    let paired: i64 = (0..m).map(|i| i64::from((prev[i] - next[i]).abs())).sum();
    let (short, long) = if prev.len() <= next.len() {
        (prev, next)
    } else {
        (next, prev)
    };
    let extra: i64 = long[m..]
        .iter()
        .map(|&x| {
            short
                .iter()
                .map(|&y| i64::from((x - y).abs()))
                .min()
                .unwrap_or(0)
        })
        .sum();
    paired + extra
}

/// The voicing of a chord closest to `prev`: every inversion within two
/// octaves of `root`, keys in 0..=127; ties go to the lexicographically
/// smaller sorted key set.
fn closest_voicing(prev: &[i32], root: i32, intervals: &[i32]) -> Option<Vec<i32>> {
    (-2..=2)
        .flat_map(|o| (0..intervals.len()).map(move |inv| voicing(root + 12 * o, intervals, inv)))
        .filter(|v| v.iter().all(|k| (0..=127).contains(k)))
        .min_by(|a, b| (voice_movement(prev, a), a).cmp(&(voice_movement(prev, b), b)))
}

/// Chords of `chord_length` over the generator range, the degrees
/// repeating until the range is filled; the last chord is cut at the
/// range end.
pub fn chord_progression(input: &Input, s: &ChordProgression) -> Result<Outcome, String> {
    let degrees = degrees0(&s.degrees)?;
    let r = input.gen_range();
    if r.is_empty() {
        let mut out = replace(input, &[], Vec::new())?;
        out.info.push("the range is empty".into());
        return Ok(out);
    }
    let len = i64::from(s.chord_length.max(1));
    let (start, end) = (i64::from(r.start), i64::from(r.end));
    let chords = (end - start + len - 1) / len;
    let voices = if s.seventh { 4 } else { 3 };
    check_count(
        usize::try_from(chords)
            .unwrap_or(usize::MAX)
            .saturating_mul(voices),
    )?;
    let tonic = i32::from(s.tonic % 12);
    let velocity = i32::from(s.velocity).clamp(1, 127);
    let mut prev: Option<Vec<i32>> = None;
    let mut new = Vec::new();
    for c in 0..chords {
        let d0 = degrees[(c as usize) % degrees.len()];
        let intervals = chord_intervals(s.mode, d0, s.seventh);
        let pc = degree_key(tonic, s.mode, d0).rem_euclid(12);
        let root = 12 * (i32::from(s.octave) + 1) + pc;
        let fixed = || voicing(root, &intervals, usize::from(s.inversion).min(voices - 1));
        let keys = match &prev {
            Some(p) if s.close_voicing => {
                closest_voicing(p, root, &intervals).unwrap_or_else(fixed)
            }
            _ => fixed(),
        };
        let t = start + c * len;
        for &key in &keys {
            new.push(Cand {
                id: 0,
                start: t,
                length: len.min(end - t),
                key,
                velocity,
            });
        }
        prev = Some(keys);
    }
    replace(input, &removed_by(input, s.insertion, r), new)
}

// ---- Randomize (§18.19) ----

/// The nearest key of the scale within 0..=127 (ties go down).
fn nearest_in_scale(key: i32, root: i32, mode: Mode) -> i32 {
    let k = key.clamp(0, 127);
    for d in 0..12 {
        if k - d >= 0 && in_scale(k - d, root, mode) {
            return k - d;
        }
        if k + d <= 127 && in_scale(k + d, root, mode) {
            return k + d;
        }
    }
    k
}

/// Transform: independent per-note deviations, in scope order, each
/// parameter drawing one value per note (key, start, length, velocity);
/// with a scale every scope key moves to the nearest key of the scale.
/// Unchanged notes stay as they are. Generate: see [`random_notes`].
pub fn randomize(input: &Input, s: &Randomize) -> Result<Outcome, String> {
    if s.mode == RandomMode::Generate {
        return random_notes(input, s);
    }
    let scope = input.scope();
    let mut rng = Rng::new(s.seed);
    let (mut removed, mut new) = (Vec::new(), Vec::new());
    for &i in &scope {
        let c = Cand::of(&input.notes[i]);
        let dk = deviation(&mut rng, i64::from(s.key_range));
        let ds = deviation(&mut rng, i64::from(s.start_range));
        let dl = deviation(&mut rng, i64::from(s.length_range));
        let dv = deviation(&mut rng, i64::from(s.velocity_range));
        let mut key = c.key + dk as i32;
        if let Some((root, mode)) = s.scale {
            key = nearest_in_scale(key, i32::from(root % 12), mode);
        }
        let next = Cand {
            start: c.start + ds,
            length: (c.length + dl).max(1),
            key,
            velocity: (c.velocity + dv as i32).clamp(1, 127),
            ..c
        };
        if next != c {
            removed.push(i);
            new.push(next);
        }
    }
    replace(input, &removed, new)
}

/// Generate: one candidate per grid cell of the generator range; it
/// becomes a note with probability `density` unless `polyphony` new notes
/// already sound there. Every cell draws chance, key, length and velocity
/// in this order whether or not it gets a note. Lengths are cut at the
/// range end.
fn random_notes(input: &Input, s: &Randomize) -> Result<Outcome, String> {
    let r = input.gen_range();
    let grid = i64::from(if s.grid == 0 { input.grid() } else { s.grid });
    let (lo, hi) = (s.key_min.min(s.key_max), s.key_min.max(s.key_max));
    let keys: Vec<i32> = (i32::from(lo)..=i32::from(hi.min(127)))
        .filter(|&k| match s.gen_scale {
            GenScale::Major => in_scale(k, i32::from(s.root % 12), Mode::Major),
            GenScale::Minor => in_scale(k, i32::from(s.root % 12), Mode::Minor),
            GenScale::Chromatic => true,
        })
        .collect();
    if keys.is_empty() {
        return Err("no key of the scale lies in the key range".into());
    }
    let lmin = i64::from(s.length_min.max(1));
    let lmax = i64::from(s.length_max).max(lmin);
    let vmin = i64::from(s.velocity_min.min(s.velocity_max));
    let vmax = i64::from(s.velocity_min.max(s.velocity_max));
    let density = f64::from(s.density.clamp(0.0, 1.0));
    let cap = usize::from(s.polyphony.max(1));
    let mut rng = Rng::new(s.seed);
    let (start, end) = (i64::from(r.start), i64::from(r.end));
    let mut ends: Vec<i64> = Vec::new();
    let mut new = Vec::new();
    let mut t = start;
    while t < end {
        let chance = rng.unit();
        let key = keys[pick(&mut rng, 0, keys.len() as i64 - 1) as usize];
        let length = pick(&mut rng, lmin, lmax).min(end - t);
        let velocity = pick(&mut rng, vmin, vmax).clamp(1, 127) as i32;
        ends.retain(|&e| e > t);
        if chance < density && ends.len() < cap {
            ends.push(t + length);
            new.push(Cand {
                id: 0,
                start: t,
                length,
                key,
                velocity,
            });
            check_count(new.len())?;
        }
        t += grid;
    }
    replace(input, &removed_by(input, s.insertion, r), new)
}

// ---- Claw machine (§18.16) ----

/// Slices of `period / slices` ticks (boundaries `floor(j * period /
/// slices)` within each period) are numbered from the generator range
/// start. After the phase, slice `k = (j - phase) mod slices` (0-based)
/// is marked when `(k + 1) % every == 0`, i.e. every Nth slice counted
/// from 1. Each scope note is cut at the slice boundaries; consecutive
/// fragments with the same mark stay joined. Remove drops marked
/// fragments, Gate keeps only them, Shift moves them by `shift`. The
/// first remaining part keeps the note's ID.
pub fn claw_machine(input: &Input, s: &ClawMachine) -> Result<Outcome, String> {
    if s.slices == 0 || s.period < s.slices {
        return Err("a period needs at least one tick per slice".into());
    }
    if s.every == 0 {
        return Err("every must be at least 1".into());
    }
    let origin = i64::from(input.gen_range().start);
    let (p, sl) = (i64::from(s.period), i64::from(s.slices));
    let phase = i64::from(s.phase) % sl;
    let every = i64::from(s.every);
    // End of the slice that contains `t`, and whether it is marked.
    let slice_at = |t: i64| -> (i64, bool) {
        let q = (t - origin).div_euclid(p);
        let off = (t - origin).rem_euclid(p);
        let mut j = off * sl / p;
        while (j + 1) * p / sl <= off {
            j += 1;
        }
        let k = (j - phase).rem_euclid(sl);
        (origin + q * p + (j + 1) * p / sl, (k + 1) % every == 0)
    };
    let scope = input.scope();
    let (mut removed, mut new) = (Vec::new(), Vec::new());
    let (mut split, mut deleted) = (0, 0);
    for &i in &scope {
        let c = Cand::of(&input.notes[i]);
        // Runs of fragments with the same mark: (start, end, marked).
        let mut runs: Vec<(i64, i64, bool)> = Vec::new();
        let mut t = c.start;
        while t < c.end() {
            let (slice_end, marked) = slice_at(t);
            let e = slice_end.min(c.end());
            match runs.last_mut() {
                Some(run) if run.2 == marked => run.1 = e,
                _ => runs.push((t, e, marked)),
            }
            t = e;
        }
        let parts: Vec<(i64, i64)> = runs
            .iter()
            .filter_map(|&(a, b, m)| match s.mode {
                ClawMode::Remove => (!m).then_some((a, b)),
                ClawMode::Gate => m.then_some((a, b)),
                ClawMode::Shift if m => Some((a + i64::from(s.shift), b + i64::from(s.shift))),
                ClawMode::Shift => Some((a, b)),
            })
            .collect();
        if let [(a, b)] = parts[..]
            && a == c.start
            && b == c.end()
        {
            continue;
        }
        removed.push(i);
        match parts.len() {
            0 => deleted += 1,
            1 => {}
            _ => split += 1,
        }
        for (pi, &(a, b)) in parts.iter().enumerate() {
            new.push(Cand {
                id: if pi == 0 { c.id } else { 0 },
                start: a,
                length: b - a,
                ..c
            });
        }
        check_count(new.len())?;
    }
    let mut out = replace(input, &removed, new)?;
    out.info
        .push(format!("{split} note(s) split, {deleted} note(s) deleted"));
    Ok(out)
}

// ---- Riff machine (§18.3) ----

/// Staged generator over the generator range: rhythm (an onset in each
/// `division` cell with `density` %) → harmony (the progression degree
/// sounding at the onset, `chord_length` each, repeating) → voicing (the
/// triad's keys in `key_min..=key_max`, from the lowest one up to
/// `octaves` octaves) → arpeggiation (`arp` over that pool, restarting at
/// each chord) → gate → velocity/timing variation → boundary validation
/// (starts kept in the range, lengths cut at its end). Every cell draws
/// onset, arp, velocity and timing values in this order.
pub fn riff_machine(input: &Input, s: &RiffMachine) -> Result<Outcome, String> {
    let degrees = degrees0(&s.progression)?;
    let r = input.gen_range();
    let root = i32::from(s.root % 12);
    let (lo, hi) = (
        i32::from(s.key_min.min(s.key_max)),
        i32::from(s.key_min.max(s.key_max).min(127)),
    );
    let octaves = i32::from(s.octaves.max(1));
    // Voicing pools per progression entry.
    let mut pools = Vec::with_capacity(degrees.len());
    for &d0 in &degrees {
        let pcs: Vec<i32> = chord_intervals(s.mode, d0, false)
            .iter()
            .map(|iv| (degree_key(root, s.mode, d0) + iv).rem_euclid(12))
            .collect();
        let tones: Vec<i32> = (lo..=hi)
            .filter(|k| pcs.contains(&k.rem_euclid(12)))
            .collect();
        let Some(&lowest) = tones.first() else {
            return Err(format!("no tone of chord {} lies in the key range", d0 + 1));
        };
        let pool: Vec<i32> = tones
            .into_iter()
            .filter(|&k| k < lowest + 12 * octaves)
            .collect();
        pools.push(pool);
    }
    let division = i64::from(s.division.max(1));
    let chord_len = i64::from(s.chord_length.max(1));
    let density = f64::from(s.density.min(100)) / 100.0;
    let sound = (division * i64::from(s.gate_percent.clamp(1, 100)) / 100).max(1);
    let base_velocity = i64::from(s.velocity);
    let mut rng = Rng::new(s.seed);
    let (start, end) = (i64::from(r.start), i64::from(r.end));
    let mut new = Vec::new();
    let (mut chord, mut n) = (-1, 0);
    let mut t = start;
    while t < end {
        let onset = rng.unit();
        let draw = rng.next_u64();
        let dv = deviation(&mut rng, i64::from(s.velocity_variation));
        let dt = deviation(&mut rng, i64::from(s.timing));
        let c = (t - start) / chord_len;
        if c != chord {
            chord = c;
            n = 0;
        }
        if onset < density {
            let pool = &pools[(c as usize) % pools.len()];
            let order = arp_order(pool.len(), s.arp);
            let key = pool[arp_index(&order, s.arp, n, draw)];
            n += 1;
            let at = (t + dt).clamp(start, end - 1);
            new.push(Cand {
                id: 0,
                start: at,
                length: sound.min(end - at),
                key,
                velocity: (base_velocity + dv).clamp(1, 127) as i32,
            });
            check_count(new.len())?;
        }
        t += division;
    }
    replace(input, &removed_by(input, s.insertion, r), new)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::super::{Note, TICKS_PER_BAR, midi_velocity};
    use super::*;

    fn keys(o: &Outcome) -> Vec<u8> {
        o.notes.iter().map(|n| n.key).collect()
    }

    fn chord(start: u32, length: u32, ks: &[u8]) -> Vec<Note> {
        ks.iter()
            .enumerate()
            .map(|(i, &k)| n(i as u32 + 1, start, length, k, 90 + i as u8))
            .collect()
    }

    fn arp(direction: ArpDirection, octaves: u8) -> Arpeggiate {
        Arpeggiate {
            direction,
            step: 240,
            gate_percent: 100,
            octaves,
            seed: 1,
        }
    }

    #[test]
    fn arpeggiate_directions() {
        let notes = chord(0, 960, &[64, 60, 67]);
        let inp = input(&notes, &[]);
        let up = arpeggiate(&inp, &arp(ArpDirection::Up, 1)).unwrap();
        assert_eq!(keys(&up), [60, 64, 67, 60]);
        assert_eq!(shape(&up)[3], (720, 240, 60));
        let down = arpeggiate(&inp, &arp(ArpDirection::Down, 1)).unwrap();
        assert_eq!(keys(&down), [67, 64, 60, 67]);
        // Velocity of the source note of each key.
        assert_eq!(midi_velocity(down.notes[0].velocity), 92);
        assert_eq!(midi_velocity(down.notes[2].velocity), 91);
        assert_eq!(down.select.len(), 4);
    }

    #[test]
    fn arpeggiate_turns_do_not_repeat_extremes() {
        let notes = chord(0, 2880, &[60, 64, 67]);
        let inp = input(&notes, &[]);
        let ud = arpeggiate(&inp, &arp(ArpDirection::UpDown, 2)).unwrap();
        assert_eq!(keys(&ud), [60, 64, 67, 72, 76, 79, 76, 72, 67, 64, 60, 64]);
        let du = arpeggiate(&inp, &arp(ArpDirection::DownUp, 2)).unwrap();
        assert_eq!(keys(&du), [79, 76, 72, 67, 64, 60, 64, 67, 72, 76, 79, 76]);
        assert!(du.notes.windows(2).all(|w| w[0].key != w[1].key));
    }

    #[test]
    fn arpeggiate_groups_gate_and_limits() {
        // Group at 0 lasts until 480; the last until its latest end.
        let mut notes = chord(0, 300, &[60, 64]);
        notes.push(n(9, 480, 200, 72, 100));
        notes.push(n(10, 480, 500, 120, 100));
        let inp = input(&notes, &[]);
        let s = Arpeggiate {
            gate_percent: 50,
            octaves: 2,
            ..arp(ArpDirection::Up, 2)
        };
        let o = arpeggiate(&inp, &s).unwrap();
        // 120 + 12 > 127 is left out of the pool; last note cut at 980.
        assert_eq!(
            shape(&o),
            [
                (0, 120, 60),
                (240, 120, 64),
                (480, 120, 72),
                (720, 120, 84),
                (960, 20, 120)
            ]
        );
        // Never past the active range.
        let ranged = Input {
            range: Some(Range { start: 0, end: 600 }),
            ..inp
        };
        let o = arpeggiate(&ranged, &s).unwrap();
        assert!(o.notes.iter().all(|n| n.start + n.length <= 600));
    }

    #[test]
    fn arpeggiate_random_is_seeded_and_empty_is_valid() {
        let notes = chord(0, 3840, &[60, 64, 67, 71]);
        let inp = input(&notes, &[]);
        let s = arp(ArpDirection::Random, 2);
        let a = arpeggiate(&inp, &s).unwrap();
        assert_eq!(a, arpeggiate(&inp, &s).unwrap());
        let b = arpeggiate(&inp, &Arpeggiate { seed: 2, ..s }).unwrap();
        assert_ne!(keys(&a), keys(&b));
        let empty = arpeggiate(&input(&[], &[]), &s).unwrap();
        assert!(empty.notes.is_empty() && empty.select.is_empty());
    }

    fn prog(degrees: &[u8]) -> ChordProgression {
        ChordProgression {
            degrees: degrees.to_vec(),
            chord_length: 960,
            close_voicing: false,
            ..ChordProgression::default()
        }
    }

    fn chords(o: &Outcome) -> Vec<(u32, Vec<u8>)> {
        let mut out: Vec<(u32, Vec<u8>)> = Vec::new();
        for n in &o.notes {
            match out.last_mut() {
                Some((s, ks)) if *s == n.start => ks.push(n.key),
                _ => out.push((n.start, vec![n.key])),
            }
        }
        out
    }

    #[test]
    fn chord_degrees_and_qualities() {
        let inp = input(&[], &[]);
        let o = chord_progression(&inp, &prog(&[1, 2, 5])).unwrap();
        assert_eq!(
            chords(&o),
            [
                (0, vec![60, 64, 67]),
                (960, vec![62, 65, 69]),
                (1920, vec![67, 71, 74]),
                (2880, vec![60, 64, 67])
            ]
        );
        let s7 = ChordProgression {
            seventh: true,
            ..prog(&[5])
        };
        let o = chord_progression(&inp, &s7).unwrap();
        assert_eq!(chords(&o)[0].1, [67, 71, 74, 77]);
        let minor = ChordProgression {
            tonic: 9,
            mode: Mode::Minor,
            inversion: 1,
            ..prog(&[1])
        };
        let o = chord_progression(&inp, &minor).unwrap();
        assert_eq!(chords(&o)[0].1, [72, 76, 81]);
        assert!(chord_progression(&inp, &prog(&[8])).is_err());
        assert!(chord_progression(&inp, &prog(&[])).is_err());
    }

    #[test]
    fn chord_voice_leading_is_deterministic() {
        let inp = input(&[], &[]);
        let s = ChordProgression {
            close_voicing: true,
            ..prog(&[1, 4, 5, 1])
        };
        let o = chord_progression(&inp, &s).unwrap();
        // F: second inversion moves 3; G: first inversion and second
        // inversion both move 6, the smaller key set wins.
        assert_eq!(
            chords(&o),
            [
                (0, vec![60, 64, 67]),
                (960, vec![60, 65, 69]),
                (1920, vec![59, 62, 67]),
                (2880, vec![60, 64, 67])
            ]
        );
        assert_eq!(o, chord_progression(&inp, &s).unwrap());
        // Different voice counts: paired lowest voices plus nearest.
        assert_eq!(voice_movement(&[60, 64, 67], &[59, 62, 65, 67]), 1 + 2 + 2);
    }

    #[test]
    fn chord_last_is_cut_and_insertion_modes() {
        let notes = [n(1, 100, 50, 40, 100), n(2, 2500, 50, 41, 100)];
        let inp = Input {
            range: Some(Range {
                start: 0,
                end: 2000,
            }),
            ..input(&notes, &[])
        };
        let o = chord_progression(&inp, &prog(&[1])).unwrap();
        let last: Vec<_> = o.notes.iter().filter(|n| n.start == 1920).collect();
        assert_eq!(last.len(), 3);
        assert!(last.iter().all(|n| n.length == 80));
        assert!(o.notes.iter().all(|n| n.key != 40), "replaced in range");
        assert!(o.notes.iter().any(|n| n.key == 41), "kept outside");
        let s = ChordProgression {
            insertion: Insertion::AppendLayer,
            ..prog(&[1])
        };
        let o = chord_progression(&inp, &s).unwrap();
        assert!(o.notes.iter().any(|n| n.key == 40));
        assert_eq!(o.select.len(), 9);
    }

    #[test]
    fn randomize_defaults_are_identity() {
        let notes = [n(1, 0, 240, 60, 100), n(2, 480, 240, 61, 77)];
        let inp = input(&notes, &[]);
        let o = randomize(&inp, &Randomize::default()).unwrap();
        assert_eq!(o.notes, notes);
        assert!(o.select.is_empty());
    }

    #[test]
    fn randomize_transform_is_seeded_and_in_scale() {
        let notes: Vec<Note> = (0..16).map(|i| n(i + 1, i * 240, 200, 60, 100)).collect();
        let s = Randomize {
            key_range: 7,
            start_range: 30,
            length_range: 300,
            velocity_range: 20,
            scale: Some((2, Mode::Minor)),
            ..Randomize::default()
        };
        let inp = input(&notes, &[]);
        let a = randomize(&inp, &s).unwrap();
        assert_eq!(a, randomize(&inp, &s).unwrap());
        assert_ne!(a, randomize(&inp, &Randomize { seed: 9, ..s }).unwrap());
        assert!(
            a.notes
                .iter()
                .all(|n| in_scale(i32::from(n.key), 2, Mode::Minor))
        );
        assert!(
            a.notes
                .iter()
                .all(|n| n.length >= 1 && (53..=67).contains(&n.key))
        );
        // Deviations of one parameter do not depend on the others.
        let only_vel = randomize(
            &inp,
            &Randomize {
                velocity_range: 20,
                ..Randomize::default()
            },
        )
        .unwrap();
        let both = randomize(
            &inp,
            &Randomize {
                velocity_range: 20,
                key_range: 5,
                ..Randomize::default()
            },
        )
        .unwrap();
        let vel = |o: &Outcome| -> Vec<u8> {
            let mut v: Vec<(u32, u8)> = o
                .notes
                .iter()
                .map(|n| (n.id, midi_velocity(n.velocity)))
                .collect();
            v.sort_unstable();
            v.into_iter().map(|p| p.1).collect()
        };
        assert_eq!(vel(&only_vel), vel(&both));
        assert_eq!(nearest_in_scale(61, 0, Mode::Major), 60, "ties go down");
    }

    fn gen_settings(density: f32, polyphony: u8, length: u32) -> Randomize {
        Randomize {
            mode: RandomMode::Generate,
            density,
            polyphony,
            length_min: length,
            length_max: length,
            ..Randomize::default()
        }
    }

    #[test]
    fn randomize_generate_density_and_scale() {
        let notes = [n(1, 0, 100, 30, 100)];
        let inp = input(&notes, &[]);
        let none = randomize(&inp, &gen_settings(0.0, 1, 240)).unwrap();
        assert!(none.notes.is_empty(), "density 0 and replace range");
        let all = randomize(&inp, &gen_settings(1.0, 1, 240)).unwrap();
        assert_eq!(all.notes.len(), 16);
        assert!(all.notes.iter().all(|n| {
            (60..=72).contains(&n.key)
                && in_scale(i32::from(n.key), 0, Mode::Major)
                && (80..=110).contains(&midi_velocity(n.velocity))
        }));
        let half = randomize(&inp, &gen_settings(0.5, 1, 240)).unwrap();
        assert!(half.notes.len() < 16 && !half.notes.is_empty());
        assert_eq!(half, randomize(&inp, &gen_settings(0.5, 1, 240)).unwrap());
        let other = randomize(
            &inp,
            &Randomize {
                seed: 5,
                ..gen_settings(0.5, 1, 240)
            },
        )
        .unwrap();
        assert_ne!(half, other);
        let chromatic = Randomize {
            gen_scale: GenScale::Chromatic,
            key_min: 61,
            key_max: 61,
            insertion: Insertion::AppendLayer,
            ..gen_settings(1.0, 1, 240)
        };
        let o = randomize(&inp, &chromatic).unwrap();
        assert_eq!(o.notes.len(), 17);
        let bad = Randomize {
            key_min: 61,
            key_max: 61,
            ..gen_settings(1.0, 1, 240)
        };
        assert!(randomize(&inp, &bad).is_err());
    }

    #[test]
    fn randomize_generate_polyphony_cap() {
        let inp = input(&[], &[]);
        let o = randomize(&inp, &gen_settings(1.0, 2, 960)).unwrap();
        let starts: Vec<u32> = o.notes.iter().map(|n| n.start).collect();
        assert_eq!(starts, [0, 240, 960, 1200, 1920, 2160, 2880, 3120]);
        for t in (0..TICKS_PER_BAR).step_by(60) {
            let sounding = o
                .notes
                .iter()
                .filter(|n| n.start <= t && t < n.start + n.length)
                .count();
            assert!(sounding <= 2);
        }
    }

    fn claw(mode: ClawMode) -> ClawMachine {
        ClawMachine {
            mode,
            ..ClawMachine::default()
        }
    }

    #[test]
    fn claw_remove_gate_shift() {
        let notes = [
            n(5, 0, 960, 60, 100),
            n(6, 0, 240, 62, 100),
            n(7, 240, 240, 64, 100),
        ];
        let inp = input(&notes, &[]);
        let o = claw_machine(&inp, &claw(ClawMode::Remove)).unwrap();
        assert_eq!(shape(&o), [(0, 240, 60), (0, 240, 62), (480, 240, 60)]);
        assert_eq!(o.notes[0].id, 5, "the first part keeps the ID");
        assert!(
            o.info
                .iter()
                .any(|l| l == "1 note(s) split, 1 note(s) deleted")
        );
        let o = claw_machine(&inp, &claw(ClawMode::Gate)).unwrap();
        assert_eq!(shape(&o), [(240, 240, 60), (240, 240, 64), (720, 240, 60)]);
        let o = claw_machine(&inp, &claw(ClawMode::Shift)).unwrap();
        assert_eq!(
            shape(&o),
            [
                (0, 240, 60),
                (0, 240, 62),
                (360, 240, 60),
                (360, 240, 64),
                (480, 240, 60),
                (840, 240, 60)
            ]
        );
        assert!(o.notes.iter().all(|n| n.length >= 1));
    }

    #[test]
    fn claw_phase_every_and_range_origin() {
        let notes = [n(1, 0, 960, 60, 100)];
        let inp = input(&notes, &[]);
        let s = ClawMachine {
            phase: 5,
            ..claw(ClawMode::Remove)
        };
        // Phase 5 mod 4 = 1: slices 0 and 2 are marked.
        let o = claw_machine(&inp, &s).unwrap();
        assert_eq!(shape(&o), [(240, 240, 60), (720, 240, 60)]);
        // Every 3rd of 4: slice 2 only; slices 0-1 stay joined.
        let s = ClawMachine {
            every: 3,
            ..claw(ClawMode::Remove)
        };
        let o = claw_machine(&inp, &s).unwrap();
        assert_eq!(shape(&o), [(0, 480, 60), (720, 240, 60)]);
        // Slices counted from the range start; 0..120 lies in the marked
        // last slice of the period before.
        let ranged = Input {
            range: Some(Range {
                start: 120,
                end: 3840,
            }),
            ..inp
        };
        let o = claw_machine(&ranged, &claw(ClawMode::Remove)).unwrap();
        assert_eq!(shape(&o), [(120, 240, 60), (600, 240, 60)]);
        assert!(claw_machine(&inp, &ClawMachine { every: 0, ..s }).is_err());
    }

    fn riff() -> RiffMachine {
        RiffMachine {
            timing: 30,
            ..RiffMachine::default()
        }
    }

    #[test]
    fn riff_is_seeded_in_scale_and_range() {
        let notes = [n(1, 0, 100, 30, 100)];
        let inp = Input {
            pattern_len: 4 * TICKS_PER_BAR,
            range: Some(Range {
                start: 960,
                end: 3 * TICKS_PER_BAR,
            }),
            ..input(&notes, &[])
        };
        let s = riff();
        let a = riff_machine(&inp, &s).unwrap();
        assert_eq!(a, riff_machine(&inp, &s).unwrap());
        assert_ne!(
            a,
            riff_machine(
                &inp,
                &RiffMachine {
                    seed: 2,
                    ..s.clone()
                }
            )
            .unwrap()
        );
        let new: Vec<&Note> = a.notes.iter().filter(|n| n.key != 30).collect();
        assert!(!new.is_empty());
        assert!(a.notes.iter().any(|n| n.key == 30), "before the range");
        for n in new {
            assert!(in_scale(i32::from(n.key), 0, Mode::Minor));
            assert!((48..=72).contains(&n.key));
            assert!(n.start >= 960 && n.start + n.length <= 3 * TICKS_PER_BAR);
        }
    }

    #[test]
    fn riff_density_and_chord_tones() {
        let inp = input(&[], &[]);
        let zero = riff_machine(
            &inp,
            &RiffMachine {
                density: 0,
                ..riff()
            },
        )
        .unwrap();
        assert!(zero.notes.is_empty());
        let s = RiffMachine {
            density: 100,
            progression: vec![1],
            arp: ArpDirection::Up,
            octaves: 1,
            timing: 0,
            ..riff()
        };
        let o = riff_machine(&inp, &s).unwrap();
        assert_eq!(o.notes.len(), 16);
        // C minor triad from 48 (C3) over one octave.
        assert_eq!(&keys(&o)[..4], [48, 51, 55, 48]);
        assert!(o.notes.iter().all(|n| n.length == 192));
        let narrow = RiffMachine {
            key_min: 49,
            key_max: 50,
            ..s
        };
        assert!(riff_machine(&inp, &narrow).is_err());
    }
}
