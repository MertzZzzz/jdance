// SPDX-License-Identifier: GPL-3.0-or-later
//! Our own glyphs (UI-VIS-041, UI-RD §1.4): 1.5 px lines on a 16×16 grid,
//! drawn as egui shapes (the interface fonts have no such symbols).
//!
//! Each glyph is a small SVG path (the board "R2 · Icons"), parsed once
//! into polylines and scaled to the target rectangle. Transport glyphs
//! (Play, Stop, Record) are filled shapes.

use crate::theme;
use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Glyph {
    // Transport and project.
    Play,
    Pause,
    Stop,
    Record,
    Pattern,
    Song,
    Tap,
    Metronome,
    CountIn,
    Panic,
    Save,
    Open,
    Export,
    Undo,
    Redo,
    // Windows.
    Browser,
    Rack,
    PianoRoll,
    Playlist,
    Mixer,
    Undock,
    Dock,
    Close,
    Menu,
    // Editor tools.
    Draw,
    Paint,
    Delete,
    Mute,
    Slice,
    Select,
    Zoom,
    Playback,
    Speed,
    // Browser and sources.
    Files,
    Plugins,
    History,
    Sample,
    Automation,
    Notes,
    Favorite,
    Search,
    Folder,
    // Mixer and effects.
    FxSlots,
    Arm,
    Pdc,
    Send,
    Eq,
    Compressor,
    Delay,
    Ghost,
    Shield,
    // Pattern and channels.
    New,
    Rename,
    Clone,
    Loop,
    Snap,
    StepsView,
    Filter,
    CleanUp,
    // Piano Roll and channel windows.
    Stamp,
    Slide,
    Scale,
    Lfo,
    Tools,
    Channel,
    Instrument,
    Envelope,
    Color,
    Keyboard,
    Midi,
    Speaker,
    Mic,
    Settings,
    Help,
    Prev,
    Next,
    ChevronDown,
    Check,
    Warning,
    Script,
    Quantize,
    Transpose,
    Listen,
}

impl Glyph {
    /// The path on the 16×16 grid; filled glyphs start with `F`.
    fn path(self) -> &'static str {
        use Glyph as G;
        match self {
            G::Play => "FM4.5 2.5v11L13 8z",
            G::Pause => "FM4 3h3v10H4zM9 3h3v10H9z",
            G::Stop => "FM3.5 3.5h9v9h-9z",
            G::Record => "FM8 3a5 5 0 1 1 0 10A5 5 0 0 1 8 3z",
            G::Pattern => "M2 2h5v5H2zM9 2h5v5H9zM2 9h5v5H2zM9 9h5v5H9z",
            G::Song | G::Playlist => "M2 4h6M10 4h4M2 8h3M7 8h7M2 12h8M12 12h2",
            G::Tap => {
                "M8 7a3.5 3.5 0 1 1 0 7 3.5 3.5 0 1 1 0-7zM8 1.5v2M3 3.5l1.4 1.4M13 3.5l-1.4 1.4"
            }
            G::Metronome => "M5 14h6L9 2H7zM8 10l4-5",
            G::CountIn => "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM8 5v3l2 2",
            G::Panic => "M2 6h3l4-3v10l-4-3H2zM11 6l4 4M15 6l-4 4",
            G::Save => "M3 2h8l2 2v10H3zM5 2v4h5V2M5 14v-4h6v4",
            G::Open => "M2 13V3h4l1.5 1.5H13V7M2 13l2.5-6H15l-2.5 6z",
            G::Export => "M8 2v8M5 7l3 3 3-3M3 11v3h10v-3",
            G::Undo => "M5 3L2 6l3 3M2 6h7.5a4 4 0 0 1 0 8H6",
            G::Redo => "M11 3l3 3-3 3M14 6H6.5a4 4 0 0 0 0 8H10",
            G::Browser | G::Folder => "M2 4h4l1.5 1.5H14V13H2z",
            G::Rack => "M2 3h12M2 6.5h12M2 10h12M2 13.5h12",
            G::PianoRoll | G::Instrument => "M2 2h12v12H2zM5.5 2v7M8 2v7M10.5 2v7M5.5 9v5M10.5 9v5",
            G::Mixer => "M4 2v12M8 2v12M12 2v12M2.5 10h3M6.5 5h3M10.5 8h3",
            G::Undock => "M9 2h5v5M14 2L8.5 7.5M12 10v4H2V4h4",
            G::Dock => "M6 2H2v12h12v-4M14 2L8 8M8 4v4h4",
            G::Close => "M4 4l8 8M12 4l-8 8",
            G::Menu => "M2.5 4h11M2.5 8h11M2.5 12h11",
            G::Draw => "M2 14l1-3.5 7.5-7.5 2.5 2.5L5.5 13zM9 4.5l2.5 2.5",
            G::Paint => "M2 2.5h9v3.5H2zM11 4h2v4H7v2M6 10h2v4.5H6z",
            G::Delete => "M6 13.5l-3.5-3.5 7-7 4 4-6.5 6.5zM6 13.5h8M5 7.5l4 4",
            G::Mute => "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM3.8 3.8l8.4 8.4",
            G::Slice => {
                "M5 2a2 2 0 1 1 0 4 2 2 0 0 1 0-4zM5 10a2 2 0 1 1 0 4 2 2 0 0 1 0-4zM6.5 5.5L14 12M6.5 10.5L14 4"
            }
            G::Select => "M2 2h3M7 2h2M11 2h3v3M14 7v2M14 11v3h-3M9 14H7M5 14H2v-3M2 9V7M2 5V2",
            G::Zoom => "M7 2.5a4.5 4.5 0 1 1 0 9 4.5 4.5 0 1 1 0-9zM10.5 10.5L14 14M5 7h4M7 5v4",
            G::Playback => "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM6.5 5.5v5l4-2.5z",
            G::Speed => "M2 12.5a6 6 0 1 1 12 0M8 12.5l3.2-4.2M4.2 8.6l.9.6M8 6.5v1M11.8 8.6l-.9.6",
            G::Files => "M4 2h5l3 3v9H4zM9 2v3h3",
            G::Plugins => "M6 2v3M10 2v3M4 5h8v2.5a4 4 0 0 1-8 0zM8 11.5V14",
            G::History => "M2.5 8a5.5 5.5 0 1 0 1.6-3.9M2.5 2.5v3h3M8 5v3l2 1.5",
            G::Sample => "M1 8h2l1.5-4 2 8 2-10 2 9 1.5-3H15",
            G::Automation => "M1.5 12c3 0 3.5-8 6.5-8s3.5 6 6.5 6",
            G::Notes => "M2 11h4M7 7h4M10 4h4M4 13.5h5",
            G::Favorite => "M8 2l1.8 3.8 4.2.5-3.1 2.9.8 4.1L8 11.3 4.3 13.3l.8-4.1L2 6.3l4.2-.5z",
            G::Search => "M7 2.5a4.5 4.5 0 1 1 0 9 4.5 4.5 0 1 1 0-9zM10.5 10.5L14 14",
            G::FxSlots => "M3 4h10M3 8h10M3 12h10",
            G::Arm => {
                "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM8 6.5a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3z"
            }
            G::Pdc => "M4 2h8M4 14h8M5 2c0 4 6 4 6 6s-6 2-6 6M11 2c0 4-6 4-6 6",
            G::Send => "M2 8h10M9 4.5L12.5 8 9 11.5M14 3v10",
            G::Eq => "M1.5 10c2 0 2-5 4-5s2 7 4 7 2-4 5-4",
            G::Compressor => "M2 14L8 8h6M2 2v12h12",
            G::Delay => "M2 12V4M6 12V6M10 12V8M14 12v-2",
            G::Ghost => "M3 14V7a5 5 0 0 1 10 0v7l-2-1.5L9.5 14 8 12.5 6.5 14 5 12.5z",
            G::Shield => "M8 2l5 2v4c0 3-2 5-5 6-3-1-5-3-5-6V4z",
            G::New => "M8 3v10M3 8h10",
            G::Rename => "M2 14l1-3 7-7 2 2-7 7zM9 5l2 2",
            G::Clone => "M5 5h8v8H5zM3 11V3h8",
            G::Loop => "M3 9V7a3 3 0 0 1 3-3h7M11 2l2 2-2 2M13 7v2a3 3 0 0 1-3 3H3M5 14l-2-2 2-2",
            G::Snap => {
                "M3.5 2.5v5.5a4.5 4.5 0 0 0 9 0V2.5h-3v5.5a1.5 1.5 0 0 1-3 0V2.5zM3.5 5.5h3M9.5 5.5h3"
            }
            G::StepsView => "M2 5h3v6H2zM6.5 5h3v6h-3zM11 5h3v6h-3z",
            G::Filter => "M2 3h12l-4.5 5.5V13l-3-1.5v-3z",
            G::CleanUp => "M10 2L7 8M4 8h6l1 6H3zM6 11v3M8.5 11v3",
            G::Stamp => "M6 2h4v4l2 2v2H4V8l2-2zM3 13h10",
            G::Slide => "M2 12h4l4-8h4M11 2l3 2-3 2",
            G::Scale => "M2 13h2.5V10H7V7h2.5V4H14",
            G::Lfo => "M1.5 8c1.5-5 3.5-5 5 0s3.5 5 5 0 2-3 3-3",
            G::Tools => {
                "M10 2.5a3.5 3.5 0 0 0-3.3 4.6L2.5 11.3 4.7 13.5l4.2-4.2A3.5 3.5 0 0 0 13.5 6l-2 2-2-2 2-2z"
            }
            G::Channel | G::Settings => {
                "M8 5.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5zM8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"
            }
            G::Envelope => "M1.5 13.5L4 3l3 5h4l3.5 5.5",
            G::Color => {
                "M8 2a6 6 0 1 0 0 12c1 0 1.5-.7 1.2-1.6-.4-1 .3-1.9 1.3-1.9H12a2 2 0 0 0 2-2A6 6 0 0 0 8 2zM5 7.5h.5M7 5h.5M10 5.5h.5"
            }
            G::Keyboard => "M1.5 4h13v8h-13zM4 6.5h1M7.5 6.5h1M11 6.5h1M4.5 9.5h7",
            G::Midi => {
                "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM5 8h.5M10.5 8h.5M6 5.5h.5M9.5 5.5h.5M8 4.5v.5"
            }
            G::Speaker => "M2 6h3l4-3v10l-4-3H2zM11 5.5a3.5 3.5 0 0 1 0 5M12.5 3.5a6 6 0 0 1 0 9",
            G::Mic => "M6 2h4v7a2 2 0 0 1-4 0zM3.5 8a4.5 4.5 0 0 0 9 0M8 12.5V14",
            G::Help => {
                "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM6.2 6.2A1.9 1.9 0 0 1 9.8 7c0 1.2-1.8 1.4-1.8 2.6M8 11.5v.5"
            }
            G::Prev => "M10 3L5 8l5 5",
            G::Next => "M6 3l5 5-5 5",
            G::ChevronDown => "M4 6l4 4 4-4",
            G::Check => "M3 8.5l3 3 7-7",
            G::Warning => "M8 2l6.5 11.5h-13zM8 6.5v3M8 11.5v.3",
            G::Script => "M5 4L1.5 8 5 12M11 4l3.5 4-3.5 4M9.5 2.5l-3 11",
            G::Quantize => "M2 2v12M6 2v12M10 2v12M14 2v12M3.5 5h1M7.5 9h1M11.5 6h1",
            G::Transpose => "M5 13V3M2 6l3-3 3 3M11 3v10M8 10l3 3 3-3",
            G::Listen => "M3 10V8a5 5 0 0 1 10 0v2M3 10h2v4H3zM11 10h2v4h-2z",
        }
    }
}

/// A parsed glyph: stroked polylines (closed or open) and filled shapes,
/// in grid units.
#[derive(Debug, Default)]
struct Parsed {
    lines: Vec<(Vec<Pos2>, bool)>,
    filled: bool,
}

fn parsed(glyph: Glyph) -> Arc<Parsed> {
    static CACHE: OnceLock<Mutex<HashMap<Glyph, Arc<Parsed>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(glyph)
        .or_insert_with(|| {
            let src = glyph.path();
            let (filled, d) = match src.strip_prefix('F') {
                Some(rest) => (true, rest),
                None => (false, src),
            };
            Arc::new(Parsed {
                lines: parse_path(d),
                filled,
            })
        })
        .clone()
}

/// Splits path data into commands and numbers ("1-3.5", ".9.6" included).
fn tokens(d: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let b = d.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() {
            out.push(Tok::Cmd(c));
            i += 1;
        } else if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut dot = c == '.';
            while i < b.len() {
                let ch = b[i] as char;
                if ch.is_ascii_digit() {
                    i += 1;
                } else if ch == '.' && !dot {
                    dot = true;
                    i += 1;
                } else if (ch == 'e' || ch == 'E') && i + 1 < b.len() {
                    i += 1;
                    if b[i] == b'-' || b[i] == b'+' {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            if let Ok(v) = d[start..i].parse::<f32>() {
                out.push(Tok::Num(v));
            }
        } else {
            i += 1;
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
enum Tok {
    Cmd(char),
    Num(f32),
}

/// Parses SVG path data (M L H V C S Q A Z, absolute and relative) into
/// polylines; curves are flattened.
fn parse_path(d: &str) -> Vec<(Vec<Pos2>, bool)> {
    let toks = tokens(d);
    let mut out: Vec<(Vec<Pos2>, bool)> = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    let mut pos = Pos2::ZERO;
    let mut start = Pos2::ZERO;
    let mut last_ctrl: Option<Pos2> = None;
    let mut cmd = 'M';
    let mut i = 0;
    let num = |i: &mut usize| -> Option<f32> {
        match toks.get(*i) {
            Some(Tok::Num(v)) => {
                *i += 1;
                Some(*v)
            }
            _ => None,
        }
    };
    let flush = |cur: &mut Vec<Pos2>, out: &mut Vec<(Vec<Pos2>, bool)>, closed: bool| {
        if cur.len() > 1 {
            out.push((std::mem::take(cur), closed));
        } else {
            cur.clear();
        }
    };
    while i < toks.len() {
        if let Tok::Cmd(c) = toks[i] {
            cmd = c;
            i += 1;
            if c == 'Z' || c == 'z' {
                flush(&mut cur, &mut out, true);
                pos = start;
                last_ctrl = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { pos.to_vec2() } else { egui::Vec2::ZERO };
        match cmd.to_ascii_uppercase() {
            'M' => {
                let (Some(x), Some(y)) = (num(&mut i), num(&mut i)) else {
                    break;
                };
                flush(&mut cur, &mut out, false);
                pos = pos2(x, y) + base;
                start = pos;
                cur.push(pos);
                // Further pairs are line-tos.
                cmd = if rel { 'l' } else { 'L' };
                last_ctrl = None;
            }
            'L' => {
                let (Some(x), Some(y)) = (num(&mut i), num(&mut i)) else {
                    break;
                };
                if cur.is_empty() {
                    cur.push(pos);
                }
                pos = pos2(x, y) + base;
                cur.push(pos);
                last_ctrl = None;
            }
            'H' => {
                let Some(x) = num(&mut i) else { break };
                if cur.is_empty() {
                    cur.push(pos);
                }
                pos.x = if rel { pos.x + x } else { x };
                cur.push(pos);
                last_ctrl = None;
            }
            'V' => {
                let Some(y) = num(&mut i) else { break };
                if cur.is_empty() {
                    cur.push(pos);
                }
                pos.y = if rel { pos.y + y } else { y };
                cur.push(pos);
                last_ctrl = None;
            }
            'C' | 'S' => {
                let smooth = cmd.eq_ignore_ascii_case(&'S');
                let c1 = if smooth {
                    last_ctrl.map_or(pos, |c| pos + (pos - c))
                } else {
                    let (Some(x), Some(y)) = (num(&mut i), num(&mut i)) else {
                        break;
                    };
                    pos2(x, y) + base
                };
                let (Some(x2), Some(y2), Some(x), Some(y)) =
                    (num(&mut i), num(&mut i), num(&mut i), num(&mut i))
                else {
                    break;
                };
                let c2 = pos2(x2, y2) + base;
                let end = pos2(x, y) + base;
                if cur.is_empty() {
                    cur.push(pos);
                }
                for k in 1..=12 {
                    let t = k as f32 / 12.0;
                    let u = 1.0 - t;
                    let p = pos.to_vec2() * (u * u * u)
                        + c1.to_vec2() * (3.0 * u * u * t)
                        + c2.to_vec2() * (3.0 * u * t * t)
                        + end.to_vec2() * (t * t * t);
                    cur.push(p.to_pos2());
                }
                last_ctrl = Some(c2);
                pos = end;
            }
            'Q' => {
                let (Some(x1), Some(y1), Some(x), Some(y)) =
                    (num(&mut i), num(&mut i), num(&mut i), num(&mut i))
                else {
                    break;
                };
                let c = pos2(x1, y1) + base;
                let end = pos2(x, y) + base;
                if cur.is_empty() {
                    cur.push(pos);
                }
                for k in 1..=10 {
                    let t = k as f32 / 10.0;
                    let u = 1.0 - t;
                    let p = pos.to_vec2() * (u * u)
                        + c.to_vec2() * (2.0 * u * t)
                        + end.to_vec2() * (t * t);
                    cur.push(p.to_pos2());
                }
                last_ctrl = Some(c);
                pos = end;
            }
            'A' => {
                let vals: Vec<f32> = (0..7).filter_map(|_| num(&mut i)).collect();
                if vals.len() < 7 {
                    break;
                }
                let end = pos2(vals[5], vals[6]) + base;
                if cur.is_empty() {
                    cur.push(pos);
                }
                arc(
                    &mut cur,
                    pos,
                    end,
                    vals[0],
                    vals[1],
                    vals[3] != 0.0,
                    vals[4] != 0.0,
                );
                pos = end;
                last_ctrl = None;
            }
            _ => {
                // Unknown command: skip a token.
                i += 1;
            }
        }
    }
    flush(&mut cur, &mut out, false);
    out
}

/// Flattens an SVG elliptical arc (no rotation) from `p0` to `p1`.
fn arc(out: &mut Vec<Pos2>, p0: Pos2, p1: Pos2, rx: f32, ry: f32, large: bool, sweep: bool) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 || p0 == p1 {
        out.push(p1);
        return;
    }
    let dx = (p0.x - p1.x) / 2.0;
    let dy = (p0.y - p1.y) / 2.0;
    let lambda = (dx * dx) / (rx * rx) + (dy * dy) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = (rx * rx * ry * ry - rx * rx * dy * dy - ry * ry * dx * dx).max(0.0);
    let den = rx * rx * dy * dy + ry * ry * dx * dx;
    let mut coef = if den > 0.0 { (num / den).sqrt() } else { 0.0 };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * dy / ry;
    let cyp = -coef * ry * dx / rx;
    let cx = cxp + (p0.x + p1.x) / 2.0;
    let cy = cyp + (p0.y + p1.y) / 2.0;
    let angle = |ux: f32, uy: f32| uy.atan2(ux);
    let t0 = angle((dx - cxp) / rx, (dy - cyp) / ry);
    let t1 = angle((-dx - cxp) / rx, (-dy - cyp) / ry);
    let mut dt = t1 - t0;
    let tau = std::f32::consts::TAU;
    if sweep && dt < 0.0 {
        dt += tau;
    } else if !sweep && dt > 0.0 {
        dt -= tau;
    }
    let n = ((dt.abs() / tau) * 32.0).ceil().max(2.0) as usize;
    for k in 1..=n {
        let t = t0 + dt * k as f32 / n as f32;
        out.push(pos2(cx + rx * t.cos(), cy + ry * t.sin()));
    }
}

/// Draws `glyph` into `r` (any square; the grid is 16 units).
pub(crate) fn paint(ui: &Ui, glyph: Glyph, r: Rect, color: Color32) {
    paint_with(ui.painter(), glyph, r, color);
}

/// [`paint`] on a painter.
pub(crate) fn paint_with(p: &egui::Painter, glyph: Glyph, r: Rect, color: Color32) {
    let g = parsed(glyph);
    let scale = r.width().min(r.height()) / 16.0;
    let origin = r.center() - vec2(8.0, 8.0) * scale;
    let map = |q: &Pos2| origin + q.to_vec2() * scale;
    let width = (1.5 * scale).clamp(1.1, 2.0);
    for (pts, closed) in &g.lines {
        let pts: Vec<Pos2> = pts.iter().map(map).collect();
        if g.filled {
            p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        } else if *closed {
            p.add(Shape::closed_line(pts, Stroke::new(width, color)));
        } else {
            p.add(Shape::line(pts, Stroke::new(width, color)));
        }
    }
}

/// Fill and border of a small button by state (UI-RD §1.5).
pub(crate) fn button_frame(resp: &Response, on: bool) -> (Color32, Stroke) {
    if on {
        (theme::BLUE_BG, Stroke::new(1.0, theme::BLUE))
    } else if resp.is_pointer_button_down_on() {
        (theme::RAISED, Stroke::new(1.0, theme::LINE_STRONG))
    } else if resp.hovered() {
        (theme::RAISED, Stroke::NONE)
    } else {
        (Color32::TRANSPARENT, Stroke::NONE)
    }
}

/// A 24×24 tool button; `on` = the active tool.
pub(crate) fn tool_button(ui: &mut Ui, glyph: Glyph, on: bool, tip: &str) -> Response {
    icon_button_sized(ui, glyph, on, tip, vec2(26.0, 24.0), 15.0)
}

/// A square icon button labelled `tip` (tooltip, status hint and
/// accessible name).
pub(crate) fn icon_button(ui: &mut Ui, glyph: Glyph, on: bool, tip: &str) -> Response {
    icon_button_sized(ui, glyph, on, tip, vec2(28.0, 26.0), 15.0)
}

pub(crate) fn icon_button_sized(
    ui: &mut Ui,
    glyph: Glyph,
    on: bool,
    tip: &str,
    size: egui::Vec2,
    icon: f32,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let (fill, stroke) = button_frame(&resp, on);
    let p = ui.painter();
    p.rect_filled(rect, 5.0, fill);
    p.rect_stroke(rect, 5.0, stroke, StrokeKind::Inside);
    let color = if !ui.is_enabled() {
        theme::DIM.gamma_multiply(0.6)
    } else if on || resp.hovered() {
        theme::TEXT
    } else {
        theme::MUTED
    };
    paint(
        ui,
        glyph,
        Rect::from_center_size(rect.center(), vec2(icon, icon)),
        color,
    );
    let tip_owned = tip.to_owned();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), &tip_owned));
    crate::widgets::hint_on_hover(&resp, tip);
    resp.on_hover_text(tip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_parse_into_lines() {
        let squares = parse_path("M2 2h5v5H2zM9 2h5v5H9z");
        assert_eq!(squares.len(), 2);
        assert!(squares.iter().all(|(l, closed)| *closed && l.len() == 4));
        assert_eq!(squares[1].0[1], pos2(14.0, 2.0));
        // Packed numbers: "1-3.5" and ".9.6".
        let t = tokens("l1-3.5.9.6");
        assert_eq!(t.len(), 5);
        // A full circle from two arcs stays on the circle.
        let c = parse_path("M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2z");
        for p in &c[0].0 {
            let r = (*p - pos2(8.0, 8.0)).length();
            assert!((r - 6.0).abs() < 0.05, "{p:?}");
        }
    }

    #[test]
    fn every_glyph_parses() {
        use Glyph as G;
        for g in [
            G::Play,
            G::Stop,
            G::Record,
            G::Pattern,
            G::Tap,
            G::Speed,
            G::Snap,
            G::Loop,
            G::Color,
            G::Help,
            G::Tools,
            G::Ghost,
            G::Stamp,
            G::Listen,
        ] {
            let p = parsed(g);
            assert!(!p.lines.is_empty(), "{g:?}");
            for (l, _) in &p.lines {
                for q in l {
                    assert!(
                        (-1.0..=17.0).contains(&q.x) && (-1.0..=17.0).contains(&q.y),
                        "{g:?} {q:?}"
                    );
                }
            }
        }
    }
}
