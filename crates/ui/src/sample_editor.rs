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

const WAVE_H: f32 = 220.0;
const MARKER_GRAB: f32 = 6.0;
const MAX_ZOOM: f32 = 256.0;
const OVERVIEW_H: f32 = 20.0;

/// What a drag on the overview moves (UI-RD §10.9).
#[derive(Debug, Clone, Copy, PartialEq)]
enum OverviewDrag {
    /// The window, grabbed at this fraction from its start.
    Window(f64),
    /// Its left or right edge: zooms.
    Left,
    Right,
}

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
    /// Magnification: 1 = the whole file fits, up to [`MAX_ZOOM`].
    zoom: f32,
    /// First visible point, a fraction of the file (UI-VIS-039).
    scroll: f64,
    /// Dragging the overview's window: the pointer's offset in it, or
    /// an edge being moved.
    overview: Option<OverviewDrag>,
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
    /// The clip being edited, if the editor is open for one.
    pub fn clip(&self) -> Option<ClipId> {
        match self.target {
            Target::Clip(id) => Some(id),
            Target::Asset(_) | Target::File(_) => None,
        }
    }

    pub fn for_clip(c: &AudioClip) -> Self {
        Self::new(Target::Clip(c.id))
    }

    pub fn new(target: Target) -> Self {
        Self {
            target,
            zoom: 1.0,
            scroll: 0.0,
            overview: None,
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
                let asset = project.clip_asset(&clip)?.clone();
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
                        ..
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
                use crate::icons::Glyph as G;
                let range = self.play_range(&v);
                // UI-RD §6 (board R4 · Sample Editor): transport, the
                // file in one line, the selection chip, the actions.
                ui.horizontal(|ui| {
                    crate::widgets::segmented(ui, |ui| {
                        if crate::widgets::button(
                            ui,
                            Some(G::Play),
                            "",
                            false,
                            crate::widgets::Look::Amber,
                            "Play the selection, the clip range or the whole file",
                        )
                        .clicked()
                            && v.frames > 0
                        {
                            let f = |x: u64| x as f64 / v.frames as f64;
                            host.preview_range(&abs, (f(range.0), f(range.1)));
                        }
                        if crate::widgets::button(
                            ui,
                            Some(G::Stop),
                            "",
                            false,
                            crate::widgets::Look::Raised,
                            "Stop",
                        )
                        .clicked()
                        {
                            host.preview_file(None);
                        }
                    });
                    if v.frames > 0 && v.rate > 0 {
                        let secs = v.frames as f64 / f64::from(v.rate);
                        let ch = v.channels.map_or_else(String::new, |c| match c {
                            1 => " · mono".into(),
                            2 => " · stereo".into(),
                            c => format!(" · {c} ch"),
                        });
                        info_chip(
                            ui,
                            &format!("{secs:.3} s · {:.1} kHz{ch}", f64::from(v.rate) / 1000.0),
                            theme::MUTED,
                        );
                    } else {
                        ui.label(RichText::new("Loading…").color(theme::MUTED));
                        host.inspect_file(&abs);
                    }
                    if let Some((a, b)) = self.selection {
                        let secs = |f: u64| f as f64 / f64::from(v.rate.max(1));
                        let chip = crate::widgets::chip(
                            ui,
                            Some(G::Select),
                            &format!("{:.3} – {:.3} s", secs(a), secs(b)),
                            true,
                        )
                        .on_hover_text("Click to clear the selection");
                        if chip.clicked() {
                            self.selection = None;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                            .map_or_else(|| "New".to_owned(), |c| c.name.clone());
                        egui::ComboBox::from_id_salt("load_target")
                            .width(90.0)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.sampler, None, "New sampler channel");
                                for c in &samplers {
                                    ui.selectable_value(&mut self.sampler, Some(c.id), &c.name);
                                }
                            })
                            .response
                            .on_hover_text("The sampler channel to load into");
                        if crate::widgets::button(
                            ui,
                            Some(G::Sample),
                            "Load into Sampler",
                            true,
                            crate::widgets::Look::Amber,
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
                        if crate::widgets::button(
                            ui,
                            Some(G::Playlist),
                            "Place in Playlist",
                            false,
                            crate::widgets::Look::Raised,
                            "New audio clip of this range at the playhead",
                        )
                        .clicked()
                        {
                            match &v.asset {
                                Some(a) => out.place = Some((a.id, range.0, range.1)),
                                None => out.import = Some(abs.clone()),
                            }
                        }
                        if let (Some(c), Some((a, b))) = (&v.clip, self.selection)
                            && crate::widgets::button(
                                ui,
                                Some(G::Slice),
                                "Use as clip range",
                                false,
                                crate::widgets::Look::Raised,
                                "Trims this clip only; other clips and the file stay",
                            )
                            .clicked()
                        {
                            let asset = v.asset.as_ref().expect("clip has an asset");
                            out.edits
                                .push(Edit::SetAudioClip(trimmed(c, asset, project, a, b)));
                            out.end_gesture = true;
                        }
                    });
                });
                ui.add_space(4.0);

                // The overview: the whole file, the visible part framed
                // (UI-RD §10.9).
                let peaks = host.sample_peaks(&v.path, dir);
                let peaks = peaks.as_deref().map(Vec::as_slice);
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    crate::icons::paint(ui, G::Zoom, r, theme::DIM);
                    let w = ui.available_width() - 44.0;
                    let (rect, resp) = ui.allocate_exact_size(
                        vec2(w.max(40.0), OVERVIEW_H),
                        Sense::click_and_drag(),
                    );
                    self.overview(ui, rect, &resp, peaks);
                    ui.label(
                        RichText::new(format!("×{:.1}", self.zoom))
                            .font(theme::mono(10.0))
                            .color(theme::DIM),
                    );
                });
                ui.add_space(2.0);

                // Ruler in seconds and the waveform.
                let w = ui.available_width();
                let (ruler, _) = ui.allocate_exact_size(vec2(w, 16.0), Sense::hover());
                self.ruler(ui, ruler, &v);
                let (rect, resp) = ui.allocate_exact_size(vec2(w, WAVE_H), Sense::click_and_drag());
                self.wheel(ui, rect, &resp);
                self.waveform(ui, &v, peaks, rect, &resp, out);

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let text = match &v.clip {
                        Some(c) => {
                            let secs = |f: u64| f as f64 / f64::from(v.rate.max(1));
                            format!(
                                "clip {:.3} – {:.3} s · {} frames",
                                secs(c.source_in),
                                secs(c.source_out),
                                c.source_out - c.source_in
                            )
                        }
                        None => format!("{} frames", v.frames),
                    };
                    ui.label(
                        RichText::new(text)
                            .font(theme::mono(10.5))
                            .color(theme::MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let hint = |ui: &mut egui::Ui, t: &str| {
                            ui.label(RichText::new(t).size(10.5).color(theme::DIM));
                        };
                        if matches!(self.target, Target::File(_)) {
                            hint(
                                ui,
                                "Library file: nothing is added until you place or load it",
                            );
                        } else {
                            hint(ui, "Nothing here changes the file on disk");
                        }
                        hint(ui, "Ctrl+wheel — zoom");
                        if v.clip.is_some() {
                            hint(ui, "Drag IN / OUT — clip range");
                        }
                        hint(ui, "Drag — select");
                    });
                });
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

    /// The visible part as fractions of the file.
    fn window(&self) -> (f64, f64) {
        let w = 1.0 / f64::from(self.zoom.clamp(1.0, MAX_ZOOM));
        let start = self.scroll.clamp(0.0, 1.0 - w);
        (start, start + w)
    }

    /// Zooms to `zoom` keeping the point at fraction `at` of the file
    /// under the same place.
    fn zoom_to(&mut self, zoom: f32, at: f64) {
        let (t0, t1) = self.window();
        let rel = ((at - t0) / (t1 - t0)).clamp(0.0, 1.0);
        self.zoom = zoom.clamp(1.0, MAX_ZOOM);
        let w = 1.0 / f64::from(self.zoom);
        self.scroll = (at - rel * w).clamp(0.0, 1.0 - w);
    }

    /// Ctrl+wheel zooms around the pointer; the wheel scrolls.
    fn wheel(&mut self, ui: &egui::Ui, rect: Rect, resp: &egui::Response) {
        if !resp.hovered() {
            return;
        }
        let (scroll, zoom, ctrl, pos) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.zoom_delta(),
                i.modifiers.command,
                i.pointer.hover_pos(),
            )
        });
        let (t0, t1) = self.window();
        let at = pos.map_or((t0 + t1) / 2.0, |p| {
            t0 + f64::from(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * (t1 - t0)
        });
        if zoom != 1.0 {
            self.zoom_to(self.zoom * zoom, at);
        } else if ctrl && scroll.y != 0.0 {
            self.zoom_to(self.zoom * (scroll.y / 200.0).exp(), at);
        } else {
            let d = if scroll.x != 0.0 { scroll.x } else { scroll.y };
            if d != 0.0 {
                let w = t1 - t0;
                self.scroll = (t0 - f64::from(d / rect.width()) * w).clamp(0.0, 1.0 - w);
            }
        }
    }

    /// The overview strip: the whole file, the visible part framed; drag
    /// the frame to scroll, its edges to zoom, click elsewhere to jump.
    fn overview(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        resp: &egui::Response,
        peaks: Option<&[f32]>,
    ) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 5.0, Color32::from_rgb(0x10, 0x12, 0x15));
        if let Some(peaks) = peaks.filter(|p| !p.is_empty()) {
            let mid = rect.center().y;
            let half = rect.height() / 2.0 - 3.0;
            let cols = rect.width().max(1.0) as usize;
            for c in 0..cols {
                let a = c * peaks.len() / cols;
                let b = ((c + 1) * peaks.len() / cols).max(a + 1).min(peaks.len());
                let v = peaks[a..b].iter().fold(0.0f32, |m, p| m.max(*p)).min(1.0);
                let x = rect.left() + c as f32 + 0.5;
                p.line_segment(
                    [pos2(x, mid - v * half), pos2(x, mid + v * half)],
                    Stroke::new(1.0, theme::MUTED.gamma_multiply(0.6)),
                );
            }
        }
        let (t0, t1) = self.window();
        let x = |t: f64| rect.left() + t as f32 * rect.width();
        let frame = Rect::from_min_max(
            pos2(x(t0), rect.top() + 1.0),
            pos2(x(t1), rect.bottom() - 1.0),
        );
        p.rect_stroke(
            frame,
            4.0,
            Stroke::new(1.5, theme::TEXT),
            StrokeKind::Inside,
        );
        let at = |px: f32| f64::from(((px - rect.left()) / rect.width()).clamp(0.0, 1.0));
        let grab = 5.0;
        if resp.drag_started()
            && let Some(o) = ui.input(|i| i.pointer.press_origin())
        {
            self.overview = Some(if (o.x - frame.left()).abs() < grab {
                OverviewDrag::Left
            } else if (o.x - frame.right()).abs() < grab {
                OverviewDrag::Right
            } else if frame.x_range().contains(o.x) {
                OverviewDrag::Window(at(o.x) - t0)
            } else {
                // Elsewhere: centre the window there, then drag it.
                let w = t1 - t0;
                OverviewDrag::Window(w / 2.0)
            });
        }
        if let (Some(d), Some(pos)) = (self.overview, resp.interact_pointer_pos()) {
            let t = at(pos.x);
            let min_w = 1.0 / f64::from(MAX_ZOOM);
            match d {
                OverviewDrag::Window(off) => {
                    let w = t1 - t0;
                    self.scroll = (t - off).clamp(0.0, 1.0 - w);
                }
                OverviewDrag::Left => {
                    let start = t.min(t1 - min_w);
                    self.zoom = (1.0 / (t1 - start)) as f32;
                    self.scroll = start;
                }
                OverviewDrag::Right => {
                    let end = t.max(t0 + min_w);
                    self.zoom = (1.0 / (end - t0)) as f32;
                    self.scroll = t0;
                }
            }
            self.zoom = self.zoom.clamp(1.0, MAX_ZOOM);
        }
        if resp.drag_stopped() {
            self.overview = None;
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let w = t1 - t0;
            self.scroll = (at(pos.x) - w / 2.0).clamp(0.0, 1.0 - w);
        }
        if resp.double_clicked() {
            self.zoom = 1.0;
            self.scroll = 0.0;
        }
        if let Some(h) = resp.hover_pos()
            && ((h.x - frame.left()).abs() < grab || (h.x - frame.right()).abs() < grab)
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        crate::widgets::hint_on_hover(
            resp,
            "Overview: drag the frame to scroll, its edges to zoom; double-click shows all",
        );
    }

    /// Seconds over the visible part.
    fn ruler(&self, ui: &egui::Ui, rect: Rect, v: &View) {
        if v.frames == 0 || v.rate == 0 {
            return;
        }
        let p = ui.painter_at(rect);
        let (t0, t1) = self.window();
        let secs = v.frames as f64 / f64::from(v.rate);
        let (s0, s1) = (t0 * secs, t1 * secs);
        // A step of 1, 2 or 5 × 10^n seconds, about 70 px apart.
        let raw = (s1 - s0) * 70.0 / f64::from(rect.width().max(1.0));
        let mag = 10f64.powf(raw.log10().floor());
        let step = [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .map(|m| m * mag)
            .find(|s| *s >= raw)
            .unwrap_or(10.0 * mag);
        let decimals = (-step.log10().floor()).max(0.0) as usize;
        let mut s = (s0 / step).ceil() * step;
        while s <= s1 {
            let x = rect.left() + ((s - s0) / (s1 - s0)) as f32 * rect.width();
            p.line_segment(
                [pos2(x, rect.bottom() - 5.0), pos2(x, rect.bottom())],
                Stroke::new(1.0, theme::LINE_STRONG),
            );
            p.text(
                pos2(x + 3.0, rect.top()),
                Align2::LEFT_TOP,
                format!("{s:.decimals$}s"),
                theme::mono(9.0),
                theme::DIM,
            );
            s += step;
        }
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
        p.rect_filled(rect, 6.0, Color32::from_rgb(0x10, 0x12, 0x15));
        if v.frames == 0 {
            return;
        }
        let frames = v.frames as f64;
        // The visible part of the file (UI-RD §10.9).
        let (t0, t1) = self.window();
        let (f0, span) = (t0 * frames, (t1 - t0) * frames);
        let x_of = |f: u64| rect.left() + ((f as f64 - f0) / span) as f32 * rect.width();
        let frame_at = |x: f32| {
            let t = f64::from(((x - rect.left()) / rect.width()).clamp(0.0, 1.0));
            (f0 + t * span).clamp(0.0, frames) as u64
        };
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
            let mut x = rect.left();
            let n = peaks.len() as f64;
            while x < rect.right() {
                let t = |x: f32| (t0 + f64::from((x - rect.left()) / rect.width()) * (t1 - t0)) * n;
                let b0 = t(x) as usize;
                let b1 = (t(x + 1.0).ceil() as usize).max(b0 + 1);
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
            p.line_segment(
                [pos2(rect.left(), mid), pos2(rect.right(), mid)],
                Stroke::new(1.0, theme::BLUE.gamma_multiply(0.5)),
            );
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
                if !(rect.left() - 1.0..=rect.right() + 1.0).contains(&x) {
                    continue;
                }
                p.line_segment(
                    [pos2(x, rect.top()), pos2(x, rect.bottom())],
                    Stroke::new(2.0, theme::LED_ON),
                );
                // A tag beside the line: IN to the right, OUT to the left.
                let galley =
                    p.layout_no_wrap(label.to_owned(), theme::strong(9.5), theme::ON_AMBER);
                let size = galley.size() + vec2(8.0, 4.0);
                let left = if label == "OUT" {
                    x - 3.0 - size.x
                } else {
                    x + 3.0
                };
                let tag = Rect::from_min_size(pos2(left, rect.top() + 6.0), size);
                p.rect_filled(tag, 3.0, theme::LED_ON);
                p.galley(tag.center() - galley.size() / 2.0, galley, theme::ON_AMBER);
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

/// A rounded field with mono text (the file details).
fn info_chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(theme::FIELD)
        .stroke(Stroke::new(1.0, theme::LINE))
        .corner_radius(7)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(theme::mono(10.5)).color(color));
        });
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

pub(crate) fn resolve(dir: Option<&Path>, s: &SampleRef) -> PathBuf {
    let p = Path::new(&s.path);
    match dir {
        Some(d) if p.is_relative() => d.join(p),
        _ => p.to_path_buf(),
    }
}
