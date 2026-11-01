// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin list with instant search and favorites (spec 0.2.5 §10):
//! one list for the Browser's Plugins section, the plugin picker (F8),
//! the Channel Rack's instrument menu and the mixer's effect menu.

use crate::{Host, theme};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Key, RichText, Sense, vec2};
use jdance_plugin_api::search::{KindFilter, PluginFavorite, PluginPrefs, search};
use jdance_plugin_api::{PluginDescriptor, PluginFormat, PluginKind};

/// Which plugins a list offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Both kinds, with the kind filter.
    All,
    Instruments,
    Effects,
}

impl Scope {
    fn allows(self, kind: PluginKind) -> bool {
        match self {
            Self::All => true,
            Self::Instruments => kind == PluginKind::Instrument,
            Self::Effects => kind == PluginKind::Effect,
        }
    }
}

/// The list's state: the text typed and the highlighted row.
#[derive(Debug, Clone, Default)]
pub(crate) struct PluginList {
    pub text: String,
    /// Highlighted row of the results (index into them).
    pub selected: Option<usize>,
    /// Put the focus into the search field on the next frame.
    pub focus: bool,
    /// A single click loads (the picker window); otherwise it highlights
    /// and a double click loads (the Browser).
    pub click_loads: bool,
    /// The key badge in the search field ("F8" in the picker).
    pub badge: &'static str,
}

#[derive(Debug, Default)]
pub(crate) struct ListOut {
    /// Load this plugin (double click or Enter).
    pub load: Option<PluginDescriptor>,
    /// Esc with nothing typed: close the window or menu.
    pub close: bool,
}

/// One row of the list.
enum Row<'a> {
    Heading(&'a str),
    Plugin {
        d: &'a PluginDescriptor,
        highlight: &'a [(usize, usize)],
        /// Index into the results, for the keyboard.
        result: Option<usize>,
    },
    /// A favorite whose plugin was not found (§10 item 11).
    Missing(&'a PluginFavorite),
}

impl PluginList {
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        host: &mut dyn Host,
        scope: Scope,
        max_height: f32,
    ) -> ListOut {
        let mut out = ListOut::default();
        let mut prefs = host.plugin_prefs();
        let before = prefs.clone();
        let mut plugins = host.plugin_instruments();
        plugins.extend(host.plugin_effects());
        plugins.retain(|d| scope.allows(d.kind));
        if scope != Scope::All {
            prefs.kind = KindFilter::All;
        }

        // UI-RD §7 (board R5 · Plugins, picker): a framed search field
        // with its key badge; Ctrl+F puts the focus into it (§10 item 4).
        let field = egui::Frame::new()
            .fill(theme::FIELD)
            .stroke(egui::Stroke::new(1.0, theme::BLUE.gamma_multiply(0.7)))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    crate::icons::paint(ui, crate::icons::Glyph::Search, r, theme::MUTED);
                    let badge = if self.badge.is_empty() {
                        "Ctrl+F"
                    } else {
                        self.badge
                    };
                    let room = ui.available_width() - 14.0 - 9.0 * badge.len() as f32;
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.text)
                            .id_salt((id, "search"))
                            .hint_text("Search plugins")
                            .frame(egui::Frame::NONE)
                            .desired_width(room.max(60.0)),
                    );
                    crate::widgets::kbd(ui, badge);
                    field
                })
                .inner
            })
            .inner;
        let ctrl_f = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::F));
        // Asked until the field has it (a new window's first frame only
        // sizes it).
        if field.has_focus() {
            self.focus = false;
        } else if self.focus || ctrl_f {
            field.request_focus();
        }
        if field.changed() {
            self.selected = None;
        }

        // Filters as chips.
        ui.horizontal_wrapped(|ui| {
            use crate::icons::Glyph as G;
            ui.spacing_mut().item_spacing.x = 5.0;
            if scope == Scope::All {
                for (k, g, label) in [
                    (KindFilter::All, G::Menu, "All"),
                    (KindFilter::Instruments, G::Instrument, "Instruments"),
                    (KindFilter::Effects, G::Eq, "Effects"),
                ] {
                    if crate::widgets::chip(ui, Some(g), label, prefs.kind == k).clicked() {
                        prefs.kind = k;
                    }
                }
            }
            if crate::widgets::chip(ui, Some(G::Favorite), "Favorites", prefs.only_favorites)
                .clicked()
            {
                prefs.only_favorites = !prefs.only_favorites;
            }
            for f in PluginFormat::ALL {
                let on = !prefs.hidden_formats.contains(&f);
                if crate::widgets::chip(ui, None, f.label(), on)
                    .on_hover_text("Show plugins of this format")
                    .clicked()
                {
                    if on {
                        prefs.hidden_formats.push(f);
                    } else {
                        prefs.hidden_formats.retain(|h| *h != f);
                    }
                }
            }
        });

        let hits = search(&plugins, &prefs, &self.text);

        // Keys: arrows choose, Enter loads, Esc clears then closes.
        // Enter and Esc take the focus from the field this frame.
        let has_focus = field.has_focus() || field.lost_focus() || self.selected.is_some();
        if has_focus {
            let (up, down, enter, esc, fav) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                    i.key_pressed(Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, Key::Escape),
                    i.consume_key(egui::Modifiers::COMMAND, Key::D),
                )
            });
            // Ctrl+D: the highlighted plugin in or out of the favorites.
            if fav && let Some(h) = self.selected.and_then(|s| hits.get(s)) {
                let d = plugins[h.index].clone();
                prefs.toggle_favorite(&d);
            }
            if down && !hits.is_empty() {
                self.selected = Some(self.selected.map_or(0, |s| (s + 1).min(hits.len() - 1)));
            }
            if up {
                self.selected = self.selected.and_then(|s| s.checked_sub(1));
            }
            if enter {
                // Nothing highlighted: the best match.
                let pick = self.selected.or((!hits.is_empty()).then_some(0));
                if let Some(h) = pick.and_then(|s| hits.get(s)) {
                    out.load = Some(plugins[h.index].clone());
                }
            }
            if esc {
                if self.text.is_empty() {
                    out.close = true;
                } else {
                    self.text.clear();
                    self.selected = None;
                    field.request_focus();
                }
            }
        }

        // Favorites and recent plugins on top while nothing is typed
        // (§10 items 10, 14); the results below.
        let typed = !self.text.trim().is_empty();
        let mut rows: Vec<Row> = Vec::new();
        let favorites = ordered_favorites(&prefs);
        let has_favorites = !favorites.is_empty();
        if !typed && !favorites.is_empty() {
            for (instrument, heading) in
                [(true, "Favorite instruments"), (false, "Favorite effects")]
            {
                let kind = if instrument {
                    PluginKind::Instrument
                } else {
                    PluginKind::Effect
                };
                if !scope.allows(kind) {
                    continue;
                }
                let mine: Vec<&PluginFavorite> = favorites
                    .iter()
                    .copied()
                    .filter(|f| f.instrument == instrument)
                    .collect();
                if mine.is_empty() {
                    continue;
                }
                rows.push(Row::Heading(heading));
                for f in mine {
                    match plugins.iter().find(|d| f.is(d)) {
                        Some(d) => rows.push(Row::Plugin {
                            d,
                            highlight: &[],
                            result: None,
                        }),
                        None => rows.push(Row::Missing(f)),
                    }
                }
            }
        }
        if !typed && !prefs.only_favorites {
            let recent: Vec<&PluginDescriptor> = prefs
                .recent
                .iter()
                .filter_map(|c| plugins.iter().find(|d| d.plugin.class_id == *c))
                .filter(|d| prefs.passes(d))
                .collect();
            if !recent.is_empty() {
                rows.push(Row::Heading("Recent"));
                rows.extend(recent.into_iter().map(|d| Row::Plugin {
                    d,
                    highlight: &[],
                    result: None,
                }));
            }
            if !hits.is_empty() {
                rows.push(Row::Heading(match scope {
                    Scope::Instruments => "Instruments",
                    Scope::Effects => "Effects",
                    Scope::All => "All plugins",
                }));
            }
        }
        rows.extend(hits.iter().enumerate().map(|(i, h)| Row::Plugin {
            d: &plugins[h.index],
            highlight: &h.highlight,
            result: Some(i),
        }));

        let mut toggle: Option<PluginDescriptor> = None;
        let mut unfavorite: Option<PluginFavorite> = None;
        let mut moved: Option<(PluginFavorite, bool)> = None;
        let mut reset = false;
        egui::ScrollArea::vertical()
            .id_salt((id, "list"))
            .max_height(max_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if hits.is_empty() && (typed || plugins.is_empty() || prefs.only_favorites) {
                    ui.label(RichText::new("Nothing found").color(theme::MUTED));
                    if ui.button("Reset filters").clicked() {
                        reset = true;
                    }
                }
                for row in &rows {
                    match row {
                        Row::Heading(h) => {
                            ui.add_space(6.0);
                            crate::widgets::caption(ui, h);
                        }
                        Row::Plugin {
                            d,
                            highlight,
                            result,
                        } => {
                            let fav = prefs.is_favorite(d);
                            let selected = result.is_some() && *result == self.selected;
                            let r = plugin_row(ui, d, highlight, fav, selected);
                            if r.star {
                                toggle = Some((*d).clone());
                            }
                            if r.row.clicked() {
                                self.selected = *result;
                                if self.click_loads {
                                    out.load = Some((*d).clone());
                                }
                            }
                            if r.row.double_clicked() {
                                out.load = Some((*d).clone());
                            }
                            if result.is_none() && fav && !prefs.favorites_alphabetical {
                                r.row.context_menu(|ui| {
                                    if ui.button("Move up").clicked() {
                                        moved = Some((PluginFavorite::of(d), true));
                                        ui.close();
                                    }
                                    if ui.button("Move down").clicked() {
                                        moved = Some((PluginFavorite::of(d), false));
                                        ui.close();
                                    }
                                    if ui.button("Remove from favorites").clicked() {
                                        toggle = Some((*d).clone());
                                        ui.close();
                                    }
                                });
                            } else {
                                r.row.context_menu(|ui| {
                                    let label = if fav {
                                        "Remove from favorites"
                                    } else {
                                        "Add to favorites"
                                    };
                                    if ui.button(label).clicked() {
                                        toggle = Some((*d).clone());
                                        ui.close();
                                    }
                                });
                            }
                        }
                        Row::Missing(f) => {
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new("★").color(theme::AMBER))
                                            .frame(false),
                                    )
                                    .on_hover_text("Remove from favorites")
                                    .clicked()
                                {
                                    unfavorite = Some((*f).clone());
                                }
                                ui.label(RichText::new(&f.name).color(theme::MUTED).italics());
                                ui.label(RichText::new("not found").size(11.0).color(theme::MUTED));
                            });
                        }
                    }
                }
            });
        if reset {
            prefs.reset_filters();
            self.text.clear();
        }
        if !typed && has_favorites {
            crate::widgets::switch_row(
                ui,
                &mut prefs.favorites_alphabetical,
                theme::AMBER,
                "Sort favorites by name",
            );
        }
        // The keys, as on the board.
        ui.separator();
        ui.horizontal(|ui| {
            for (k, t) in [("↑↓", "choose"), ("Enter", "add"), ("Ctrl+D", "favorite")] {
                ui.label(RichText::new(k).font(theme::mono(10.0)).color(theme::MUTED));
                ui.label(RichText::new(t).size(10.5).color(theme::DIM));
                ui.add_space(6.0);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} of {}", hits.len(), plugins.len()))
                        .font(theme::mono(10.0))
                        .color(theme::DIM),
                );
            });
        });
        if let Some(d) = toggle {
            prefs.toggle_favorite(&d);
        }
        if let Some(f) = unfavorite {
            prefs.favorites.retain(|x| *x != f);
        }
        if let Some((f, up)) = moved
            && let Some(i) = prefs.favorites.iter().position(|x| x.is_same(&f))
        {
            // Past the favorites of the other kind, to the next of this one.
            let same: Vec<usize> = (0..prefs.favorites.len())
                .filter(|&j| prefs.favorites[j].instrument == f.instrument)
                .collect();
            let at = same.iter().position(|&j| j == i).unwrap_or(0);
            let other = if up {
                at.checked_sub(1).map(|a| same[a])
            } else {
                same.get(at + 1).copied()
            };
            if let Some(j) = other {
                prefs.favorites.swap(i, j);
            }
        }
        if let Some(d) = &out.load {
            prefs.note_used(d);
        }
        if scope != Scope::All {
            prefs.kind = before.kind;
        }
        if prefs != before {
            host.set_plugin_prefs(prefs);
        }
        out
    }
}

impl PluginList {
    /// [`PluginList::show`] with the state kept in egui's memory under
    /// `id`.
    pub(crate) fn show_kept(
        ui: &mut egui::Ui,
        id: &str,
        host: &mut dyn Host,
        scope: Scope,
        max_height: f32,
    ) -> ListOut {
        let key = egui::Id::unique(("plugin_list", id));
        let mut state: PluginList =
            ui.data_mut(|d| d.get_temp(key))
                .unwrap_or_else(|| PluginList {
                    focus: true,
                    ..PluginList::default()
                });
        let out = state.show(ui, id, host, scope, max_height);
        ui.data_mut(|d| d.insert_temp(key, state));
        out
    }
}

/// Favorites in the user's order or by name.
fn ordered_favorites(prefs: &PluginPrefs) -> Vec<&PluginFavorite> {
    let mut f: Vec<&PluginFavorite> = prefs.favorites.iter().collect();
    if prefs.favorites_alphabetical {
        f.sort_by_key(|x| x.name.to_lowercase());
    }
    f
}

struct RowResponse {
    row: egui::Response,
    star: bool,
}

/// A plugin row (board R5 · picker): kind glyph, the name with the typed
/// part in amber, the vendor on the right and the star at the end
/// (filled for favorites).
fn plugin_row(
    ui: &mut egui::Ui,
    d: &PluginDescriptor,
    highlight: &[(usize, usize)],
    favorite: bool,
    selected: bool,
) -> RowResponse {
    let (rect, row) =
        ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click_and_drag());
    // Instruments drag onto the Channel Rack.
    if d.kind == PluginKind::Instrument {
        row.dnd_set_drag_payload(crate::browser::BrowserItem::Instrument(d.clone()));
    }
    if selected {
        ui.painter().rect_filled(rect, 6.0, theme::BLUE_BG);
    } else if row.hovered() {
        ui.painter().rect_filled(rect, 6.0, theme::RAISED);
    }
    let (glyph, color) = match d.kind {
        PluginKind::Instrument => (crate::icons::Glyph::Instrument, theme::AMBER),
        PluginKind::Effect => (crate::icons::Glyph::Lfo, theme::TEAL),
    };
    crate::icons::paint_with(
        ui.painter(),
        glyph,
        egui::Rect::from_center_size(rect.left_center() + vec2(16.0, 0.0), vec2(14.0, 14.0)),
        color,
    );
    let star_rect =
        egui::Rect::from_center_size(rect.right_center() - vec2(16.0, 0.0), vec2(22.0, 22.0));
    let star = ui.interact(star_rect, row.id.with("star"), Sense::click());
    let star_color = if favorite {
        theme::AMBER
    } else if star.hovered() {
        theme::TEXT
    } else {
        theme::DIM
    };
    crate::icons::paint_with(
        ui.painter(),
        crate::icons::Glyph::Favorite,
        star_rect.shrink(4.0),
        star_color,
    );
    if favorite {
        ui.painter()
            .circle_filled(star_rect.center(), 2.5, theme::AMBER);
    }
    let star_clicked = star
        .on_hover_text(if favorite {
            "Remove from favorites (Ctrl+D)"
        } else {
            "Add to favorites (Ctrl+D)"
        })
        .clicked();
    // The vendor, right-aligned before the star.
    let vendor = crate::widgets::fit_galley(
        ui.painter(),
        d.plugin.vendor.clone(),
        FontId::proportional(11.0),
        theme::MUTED,
        (rect.width() * 0.35).max(40.0),
    );
    let vendor_x = star_rect.left() - 8.0 - vendor.size().x;
    ui.painter().galley(
        egui::pos2(vendor_x, rect.center().y - vendor.size().y / 2.0),
        vendor,
        theme::MUTED,
    );
    let font = FontId::proportional(12.5);
    let plain = TextFormat::simple(font.clone(), theme::TEXT);
    let mark = TextFormat::simple(crate::theme::strong(12.5), theme::AMBER);
    let mut job = LayoutJob::default();
    let chars: Vec<char> = d.plugin.name.chars().collect();
    let mut i = 0;
    for &(a, b) in highlight {
        let (a, b) = (a.min(chars.len()), b.min(chars.len()));
        if a < i || a >= b {
            continue;
        }
        job.append(&chars[i..a].iter().collect::<String>(), 0.0, plain.clone());
        job.append(&chars[a..b].iter().collect::<String>(), 0.0, mark.clone());
        i = b;
    }
    job.append(&chars[i..].iter().collect::<String>(), 0.0, plain);
    let name_x = rect.left() + 32.0;
    job.wrap = egui::text::TextWrapping::truncate_at_width((vendor_x - name_x - 10.0).max(30.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(
        egui::pos2(name_x, rect.center().y - galley.size().y / 2.0),
        galley,
        Color32::WHITE,
    );
    row.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &d.plugin.name));
    RowResponse {
        row: row.on_hover_text(&d.path),
        star: star_clicked,
    }
}
