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
    /// Shown in a menu: no context menus on the rows (a popup inside a
    /// popup closes both); the star still toggles favorites.
    pub in_menu: bool,
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

        // Search field; Ctrl+F puts the focus into it (§10 item 4).
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.text)
                .id_salt((id, "search"))
                .hint_text("Search plugins (Ctrl+F)")
                .desired_width(f32::INFINITY),
        );
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

        ui.horizontal_wrapped(|ui| {
            if scope == Scope::All {
                for (k, label) in [
                    (KindFilter::All, "All"),
                    (KindFilter::Instruments, "Instruments"),
                    (KindFilter::Effects, "Effects"),
                ] {
                    ui.selectable_value(&mut prefs.kind, k, label);
                }
                ui.separator();
            }
            for f in PluginFormat::ALL {
                let mut on = !prefs.hidden_formats.contains(&f);
                if ui.toggle_value(&mut on, f.label()).changed() {
                    if on {
                        prefs.hidden_formats.retain(|h| *h != f);
                    } else {
                        prefs.hidden_formats.push(f);
                    }
                }
            }
            ui.toggle_value(&mut prefs.only_favorites, "★ Favorites only");
        });

        let hits = search(&plugins, &prefs, &self.text);
        ui.label(
            RichText::new(format!("{} of {} found", hits.len(), plugins.len()))
                .size(11.0)
                .color(theme::MUTED),
        );

        // Keys: arrows choose, Enter loads, Esc clears then closes.
        // Enter and Esc take the focus from the field this frame.
        let has_focus = field.has_focus() || field.lost_focus() || self.selected.is_some();
        if has_focus {
            let (up, down, enter, esc) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                    i.key_pressed(Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, Key::Escape),
                )
            });
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
                rows.push(Row::Heading("All plugins"));
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
                            ui.add_space(4.0);
                            ui.label(RichText::new(*h).strong().size(11.0).color(theme::MUTED));
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
                            }
                            if r.row.double_clicked() {
                                out.load = Some((*d).clone());
                            }
                            if self.in_menu {
                                // Stars only.
                            } else if result.is_none() && fav && !prefs.favorites_alphabetical {
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
            ui.checkbox(&mut prefs.favorites_alphabetical, "Sort favorites by name");
        }
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
    /// `id`, for menus.
    pub(crate) fn show_kept(
        ui: &mut egui::Ui,
        id: &str,
        host: &mut dyn Host,
        scope: Scope,
        max_height: f32,
        menu: bool,
    ) -> ListOut {
        let key = egui::Id::unique(("plugin_list", id));
        let mut state: PluginList =
            ui.data_mut(|d| d.get_temp(key))
                .unwrap_or_else(|| PluginList {
                    focus: true,
                    ..PluginList::default()
                });
        state.in_menu = menu;
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

/// A plugin: star (on hover, always for favorites), the name with the
/// matched part highlighted, vendor and kind.
fn plugin_row(
    ui: &mut egui::Ui,
    d: &PluginDescriptor,
    highlight: &[(usize, usize)],
    favorite: bool,
    selected: bool,
) -> RowResponse {
    let (rect, row) =
        ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click_and_drag());
    // Instruments drag onto the Channel Rack.
    if d.kind == PluginKind::Instrument {
        row.dnd_set_drag_payload(crate::browser::BrowserItem::Instrument(d.clone()));
    }
    if selected {
        ui.painter().rect_filled(rect, 4.0, theme::BLUE_BG);
    } else if row.hovered() {
        ui.painter().rect_filled(rect, 4.0, theme::RAISED);
    }
    let star_rect = egui::Rect::from_min_size(rect.min + vec2(2.0, 1.0), vec2(20.0, 20.0));
    let star = ui.interact(star_rect, row.id.with("star"), Sense::click());
    if favorite || row.hovered() || star.hovered() {
        ui.painter().text(
            star_rect.center(),
            egui::Align2::CENTER_CENTER,
            if favorite { "★" } else { "☆" },
            FontId::proportional(13.0),
            if favorite { theme::AMBER } else { theme::MUTED },
        );
    }
    let star_clicked = star
        .on_hover_text(if favorite {
            "Remove from favorites"
        } else {
            "Add to favorites"
        })
        .clicked();
    let font = FontId::proportional(12.5);
    let plain = TextFormat::simple(font.clone(), theme::TEXT);
    let mark = TextFormat {
        background: theme::AMBER.gamma_multiply(0.35),
        ..plain.clone()
    };
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
    job.append(
        &format!(
            "  {} · {}",
            d.plugin.vendor,
            match d.kind {
                PluginKind::Instrument => "instrument",
                PluginKind::Effect => "effect",
            }
        ),
        0.0,
        TextFormat::simple(FontId::proportional(11.0), theme::MUTED),
    );
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter()
        .galley(rect.min + vec2(24.0, 4.0), galley, Color32::WHITE);
    RowResponse {
        row: row.on_hover_text(&d.path),
        star: star_clicked,
    }
}
