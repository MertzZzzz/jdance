// SPDX-License-Identifier: GPL-3.0-or-later
//! Sample Editor (spec 0.2.0 §3): waveform with zoom and scroll, preview,
//! selection, file details; for an audio clip its source range with
//! draggable in/out markers. "Place in Playlist" and "Load into Sampler"
//! use the selection (or the clip range, or the whole file). Nothing here
//! changes the file on disk: edits change only the clip.

use crate::{Host, SampleStatus, theme};
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use jdance_commands::Edit;
use jdance_domain::{
    AssetId, AudioAsset, AudioClip, ChannelId, ChannelSource, ClipId, Project, SampleRef,
};
use std::path::{Path, PathBuf};

const WAVE_H: f32 = 180.0;
const MARKER_GRAB: f32 = 6.0;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    /// An audio clip: edits change its source range.
    Clip(ClipId),
    /// A project asset as a whole.
    Asset(AssetId),
    /// A file in a library folder: preview only, nothing is added.
    File(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Marker {
    In,
    Out,
}

#[derive(Debug, Clone)]
pub(crate) struct SampleEditor {
    pub target: Target,
    /// Pixels per source frame at zoom 1 = the whole file fits.
    zoom: f32,
    /// Selection in source frames.
    selection: Option<(u64, u64)>,
    selecting: Option<u64>,
    marker: Option<Marker>,
    /// Sampler channel for "Load into Sampler" (`None` = a new channel).
    sampler: Option<ChannelId>,
}

/// What the editor asks the app to do.
#[derive(Debug, Default)]
pub(crate) struct SampleEditorOutput {
    pub edits: Vec<Edit>,
    pub end_gesture: bool,
    /// Place `[in, out)` source frames of the asset in the playlist.
    pub place: Option<(AssetId, u64, u64)>,
    /// Load the asset into a sampler (`None` = new channel), with the
    /// sampler's start/end markers set to the range (fractions).
    pub load: Option<(AssetId, Option<ChannelId>, (f32, f32))>,
    /// Import a library file first (for Place / Load from a file).
    pub import: Option<PathBuf>,
}

/// What the editor shows: the file and, for a clip, its range.
struct View {
    name: String,
    path: SampleRef,
    asset: Option<AudioAsset>,
    clip: Option<AudioClip>,
    frames: u64,
    rate: u32,
    channels: Option<u16>,
    /// Project timebase (clip positions).
    timebase: u32,
}

impl SampleEditor {
    pub fn for_clip(c: &AudioClip) -> Self {
        Self::new(Target::Clip(c.id))
    }

    pub fn new(target: Target) -> Self {
        Self {
            target,
            zoom: 1.0,
            selection: None,
            selecting: None,
            marker: None,
            sampler: None,
        }
    }

    fn view(&self, project: &Project, host: &dyn Host, dir: Option<&Path>) -> Option<View> {
        match &self.target {
            Target::Clip(id) => {
                let clip = *project.playlist.audio_clip(*id)?;
                let asset = project.asset(clip.asset)?.clone();
                Some(View {
                    name: asset.name.clone(),
                    path: SampleRef {
                        path: asset.path.clone(),
                    },
                    frames: asset.frames,
                    rate: asset.sample_rate,
                    channels: Some(asset.channels),
                    asset: Some(asset),
                    clip: Some(clip),
                    timebase: project.timebase_rate,
                })
            }
            Target::Asset(id) => {
                let asset = project.asset(*id)?.clone();
                Some(View {
                    name: asset.name.clone(),
                    path: SampleRef {
                        path: asset.path.clone(),
                    },
                    frames: asset.frames,
                    rate: asset.sample_rate,
                    channels: Some(asset.channels),
                    asset: Some(asset),
                    clip: None,
                    timebase: project.timebase_rate,
                })
            }
            Target::File(path) => {
                let r = SampleRef {
                    path: path.to_string_lossy().into_owned(),
                };
                let (frames, rate) = match host.sample_status(&r, dir) {
                    Some(SampleStatus::Ready {
                        frames,
                        source_rate,
                    }) => (frames as u64, source_rate),
                    _ => (0, 0),
                };
                Some(View {
                    name: path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                    path: r,
                    asset: None,
                    clip: None,
                    frames,
                    rate,
                    channels: None,
                    timebase: project.timebase_rate,
                })
            }
        }
    }

    /// Shows the window; returns `false` when it was closed.
    #[allow(clippy::too_many_lines)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        host: &mut dyn Host,
        dir: Option<&Path>,
        out: &mut SampleEditorOutput,
        layout: &mut crate::dock::Layout,
    ) -> bool {
        let Some(v) = self.view(project, host, dir) else {
            return false;
        };
        let abs = resolve(dir, &v.path);
        let title = format!("Sample Editor · {}", v.name);
        let open = crate::dock::editor_window(
            ctx,
            layout,
            crate::dock::Panel::SampleEditor,
            0,
            &title,
            &mut |ui| {
                // Details.
                ui.horizontal(|ui| {
                    if v.frames > 0 && v.rate > 0 {
                        let secs = v.frames as f64 / f64::from(v.rate);
                        let ch = v.channels.map_or_else(String::new, |c| {
                            if c == 1 {
                                " · mono".into()
                            } else {
                                format!(" · {c} ch")
                            }
                        });
                        ui.label(
                            RichText::new(format!(
                                "{secs:.3} s · {} Hz{ch} · {} frames",
                                v.rate, v.frames
                            ))
                            .font(theme::mono(11.5))
                            .color(theme::MUTED),
                        );
                    } else {
                        ui.label(RichText::new("Loading…").color(theme::MUTED));
                        host.inspect_file(&abs);
                    }
                    if let Some(c) = &v.clip {
                        ui.separator();
                        ui.label(
                            RichText::new(format!(
                                "clip range {} – {} ({} frames)",
                                c.source_in,
                                c.source_out,
                                c.source_out - c.source_in
                            ))
                            .font(theme::mono(11.5)),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Slider::new(&mut self.zoom, 1.0..=64.0)
                            .logarithmic(true)
                            .text("Zoom"),
                    );
                    let range = self.play_range(&v);
                    if ui
                        .button("Play")
                        .on_hover_text("Play the selection, the clip range or the whole file")
                        .clicked()
                        && v.frames > 0
                    {
                        let f = |x: u64| x as f64 / v.frames as f64;
                        host.preview_range(&abs, (f(range.0), f(range.1)));
                    }
                    if ui.button("Stop").clicked() {
                        host.preview_file(None);
                    }
                    if let Some((a, b)) = self.selection {
                        ui.label(
                            RichText::new(format!("selection {a} – {b}"))
                                .font(theme::mono(11.5))
                                .color(theme::BLUE),
                        );
                        if ui.small_button("Clear").clicked() {
                            self.selection = None;
                        }
                    }
                });

                // Waveform.
                let peaks = host.sample_peaks(&v.path, dir);
                let avail = ui.available_width();
                let total_w = avail * self.zoom;
                egui::ScrollArea::horizontal()
                    .id_salt("sample_editor_scroll")
                    .show(ui, |ui| {
                        let (rect, resp) =
                            ui.allocate_exact_size(vec2(total_w, WAVE_H), Sense::click_and_drag());
                        self.waveform(
                            ui,
                            &v,
                            peaks.as_deref().map(Vec::as_slice),
                            rect,
                            &resp,
                            out,
                        );
                    });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let range = self.play_range(&v);
                    if let (Some(c), Some((a, b))) = (&v.clip, self.selection)
                        && ui
                            .button("Use selection as clip range")
                            .on_hover_text("Trims this clip only; other clips and the file stay")
                            .clicked()
                    {
                        let asset = v.asset.as_ref().expect("clip has an asset");
                        out.edits
                            .push(Edit::SetAudioClip(trimmed(c, asset, project, a, b)));
                        out.end_gesture = true;
                    }
                    if ui
                        .button("Place in Playlist")
                        .on_hover_text("New audio clip of this range at the playhead")
                        .clicked()
                    {
                        match &v.asset {
                            Some(a) => out.place = Some((a.id, range.0, range.1)),
                            None => out.import = Some(abs.clone()),
                        }
                    }
                    ui.separator();
                    let samplers: Vec<_> = project
                        .channels
                        .iter()
                        .filter(|c| matches!(c.source, ChannelSource::Sampler { .. }))
                        .collect();
                    if self
                        .sampler
                        .is_some_and(|id| !samplers.iter().any(|c| c.id == id))
                    {
                        self.sampler = None;
                    }
                    let current = self
                        .sampler
                        .and_then(|id| project.channel(id))
                        .map_or_else(|| "New sampler channel".to_owned(), |c| c.name.clone());
                    egui::ComboBox::from_id_salt("load_target")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.sampler, None, "New sampler channel");
                            for c in &samplers {
                                ui.selectable_value(&mut self.sampler, Some(c.id), &c.name);
                            }
                        });
                    if ui
                        .button("Load into Sampler")
                        .on_hover_text(
                            "The sampler plays this file; its start/end markers follow the range",
                        )
                        .clicked()
                        && v.frames > 0
                    {
                        let f = |x: u64| (x as f64 / v.frames as f64) as f32;
                        match &v.asset {
                            Some(a) => {
                                out.load = Some((a.id, self.sampler, (f(range.0), f(range.1))));
                            }
                            None => out.import = Some(abs.clone()),
                        }
                    }
                });
                if matches!(self.target, Target::File(_)) {
                    ui.label(
                        RichText::new("Library file: nothing is added to the project until you place or load it")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                }
            },
        );
        if !open {
            host.preview_file(None);
        }
        open
    }

    /// Selection, else the clip range, else the whole file.
    fn play_range(&self, v: &View) -> (u64, u64) {
        self.selection
            .or(v.clip.map(|c| (c.source_in, c.source_out)))
            .unwrap_or((0, v.frames))
    }

    fn waveform(
        &mut self,
        ui: &egui::Ui,
        v: &View,
        peaks: Option<&[f32]>,
        rect: Rect,
        resp: &egui::Response,
        out: &mut SampleEditorOutput,
    ) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 6.0, theme::FIELD);
        if v.frames == 0 {
            return;
        }
        let frames = v.frames as f64;
        let x_of = |f: u64| rect.left() + (f as f64 / frames) as f32 * rect.width();
        let frame_at =
            |x: f32| (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * frames) as u64;
        // Clip range shaded, the rest dimmed.
        if let Some(c) = &v.clip {
            let (a, b) = (x_of(c.source_in), x_of(c.source_out));
            p.rect_filled(
                Rect::from_min_max(rect.min, pos2(a, rect.bottom())),
                0.0,
                Color32::from_black_alpha(110),
            );
            p.rect_filled(
                Rect::from_min_max(pos2(b, rect.top()), rect.max),
                0.0,
                Color32::from_black_alpha(110),
            );
        }
        if let Some((a, b)) = self.selection {
            p.rect_filled(
                Rect::from_min_max(pos2(x_of(a), rect.top()), pos2(x_of(b), rect.bottom())),
                0.0,
                theme::BLUE.gamma_multiply(0.18),
            );
        }
        if let Some(peaks) = peaks.filter(|p| !p.is_empty()) {
            let mid = rect.center().y;
            let half = rect.height() / 2.0 - 4.0;
            let visible = ui.clip_rect().intersect(rect);
            let mut x = visible.left().floor();
            while x < visible.right() {
                let t0 = ((x - rect.left()) / rect.width()).max(0.0);
                let t1 = ((x + 1.0 - rect.left()) / rect.width()).max(t0);
                let b0 = (t0 * peaks.len() as f32) as usize;
                let b1 = ((t1 * peaks.len() as f32).ceil() as usize).max(b0 + 1);
                let peak = peaks[b0.min(peaks.len() - 1)..b1.min(peaks.len())]
                    .iter()
                    .fold(0.0f32, |m, v| m.max(*v));
                let h = (peak.min(1.0) * half).max(0.5);
                p.line_segment(
                    [pos2(x + 0.5, mid - h), pos2(x + 0.5, mid + h)],
                    Stroke::new(1.0, theme::BLUE),
                );
                x += 1.0;
            }
        } else {
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Loading waveform…",
                FontId::proportional(12.0),
                theme::MUTED,
            );
        }

        // Clip in/out markers.
        let press = ui.input(|i| i.pointer.press_origin());
        let near = |x: f32, f: u64| (x - x_of(f)).abs() < MARKER_GRAB;
        if let Some(c) = &v.clip {
            for (f, label) in [(c.source_in, "IN"), (c.source_out, "OUT")] {
                let x = x_of(f);
                p.line_segment(
                    [pos2(x, rect.top()), pos2(x, rect.bottom())],
                    Stroke::new(2.0, theme::AMBER),
                );
                p.text(
                    pos2(x + 3.0, rect.top() + 2.0),
                    Align2::LEFT_TOP,
                    label,
                    theme::mono(10.0),
                    theme::AMBER,
                );
            }
            if resp.drag_started()
                && let Some(o) = press
            {
                self.marker = if near(o.x, c.source_in) {
                    Some(Marker::In)
                } else if near(o.x, c.source_out) {
                    Some(Marker::Out)
                } else {
                    None
                };
            }
            if let Some(h) = resp.hover_pos()
                && (near(h.x, c.source_in) || near(h.x, c.source_out))
            {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        if resp.drag_started() && self.marker.is_none() {
            self.selecting = press.map(|o| frame_at(o.x));
        }
        if let Some(pos) = resp.interact_pointer_pos()
            && resp.dragged()
        {
            let f = frame_at(pos.x);
            match (self.marker, &v.clip, &v.asset) {
                (Some(m), Some(c), Some(a)) => {
                    let (lo, hi) = match m {
                        Marker::In => (f.min(c.source_out - 1), c.source_out),
                        Marker::Out => (c.source_in, f.max(c.source_in + 1).min(a.frames)),
                    };
                    let mut new = *c;
                    new.source_in = lo;
                    new.source_out = hi;
                    if m == Marker::In {
                        // Like the left clip edge: the end stays.
                        let tb = f64::from(v.rate.max(1));
                        let shift = ((lo as f64 - c.source_in as f64) / tb * f64::from(v.timebase))
                            .round() as i64;
                        new.start = (c.start as i64 + shift).max(0) as u64;
                    }
                    if new != *c {
                        out.edits.push(Edit::SetAudioClip(new));
                    }
                }
                _ => {
                    if let Some(s) = self.selecting {
                        self.selection = Some((s.min(f), s.max(f).max(s.min(f) + 1)));
                    }
                }
            }
        }
        if resp.drag_stopped() {
            if self.marker.take().is_some() {
                out.end_gesture = true;
            }
            self.selecting = None;
        }
        if resp.clicked() {
            self.selection = None;
        }
        p.rect_stroke(rect, 6.0, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
    }
}

/// `c` trimmed to source frames `[a, b)`, keeping its timeline position of
/// the material: the start moves with `a`.
fn trimmed(c: &AudioClip, asset: &AudioAsset, project: &Project, a: u64, b: u64) -> AudioClip {
    let ratio = f64::from(project.timebase_rate) / f64::from(asset.sample_rate.max(1));
    let shift = ((a as f64 - c.source_in as f64) * ratio).round() as i64;
    AudioClip {
        source_in: a.min(asset.frames - 1),
        source_out: b.clamp(a + 1, asset.frames),
        start: (c.start as i64 + shift).max(0) as u64,
        ..*c
    }
}

fn resolve(dir: Option<&Path>, s: &SampleRef) -> PathBuf {
    let p = Path::new(&s.path);
    match dir {
        Some(d) if p.is_relative() => d.join(p),
        _ => p.to_path_buf(),
    }
}
