// SPDX-License-Identifier: GPL-3.0-or-later
//! A pattern or a sample dropped on a mixer strip or a lane header names
//! and colours it (spec 0.2.5 §7, §8). The app reads what is being
//! dragged once a frame ([`dragged`]); the strips and headers show where
//! it would land ([`feedback`]) and report the drop ([`released_on`]).

use crate::browser::BrowserItem;
use crate::playlist::{Source, dnd_release};
use crate::project_browser::ProjectItem;
use crate::theme;
use egui::{Color32, FontId, Id, LayerId, Order, Rect, Response, Stroke, StrokeKind, vec2};
use jdance_domain::Project;
use jdance_domain::color::{Rgb, clip_name};
use std::path::Path;

/// The name and colour a drop gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dropped {
    pub name: String,
    /// `None`: the source has no colour; the target keeps its own (§7
    /// item 4).
    pub color: Option<Rgb>,
}

/// What a dragged item would give, if it is a pattern or a sample: a
/// pattern its name and colour, a file its name without the extension,
/// an audio channel or asset its name (and the channel its colour).
pub(crate) fn of_source(p: &Project, s: Source) -> Option<Dropped> {
    match s {
        Source::Pattern(id) => p.pattern(id).map(|x| Dropped {
            name: clip_name(&x.name),
            color: x.color,
        }),
        Source::Channel(id) => p.channel(id).map(|c| Dropped {
            name: clip_name(&c.name),
            color: c.color,
        }),
        Source::Asset(id) => p.asset(id).map(|a| Dropped {
            name: clip_name(&a.name),
            color: None,
        }),
        Source::Automation(_) => None,
    }
}

pub(crate) fn of_file(path: &Path) -> Option<Dropped> {
    let stem = path.file_stem()?.to_string_lossy();
    Some(Dropped {
        name: clip_name(&stem),
        color: None,
    })
}

/// What is being dragged now, as a name and colour.
pub(crate) fn dragged(ctx: &egui::Context, p: &Project) -> Option<Dropped> {
    if let Some(item) = egui::DragAndDrop::payload::<ProjectItem>(ctx) {
        return of_source(p, item.0);
    }
    match egui::DragAndDrop::payload::<BrowserItem>(ctx).as_deref() {
        Some(BrowserItem::Sample(path)) => of_file(path),
        _ => None,
    }
}

/// The drop went onto `resp`.
pub(crate) fn released_on(resp: &Response) -> bool {
    dnd_release::<ProjectItem>(resp).is_some()
        || dnd_release::<BrowserItem>(resp).is_some_and(|i| matches!(*i, BrowserItem::Sample(_)))
}

/// A frame in the source's colour around `rect` and, by the pointer, the
/// name and colour the target would get.
pub(crate) fn feedback(ui: &egui::Ui, rect: Rect, d: &Dropped) {
    let color = d.color.map_or(theme::TEXT, theme::rgb);
    ui.painter()
        .rect_stroke(rect, 3.0, Stroke::new(2.0, color), StrokeKind::Inside);
    let Some(p) = ui.ctx().pointer_latest_pos() else {
        return;
    };
    let painter = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Tooltip, Id::unique("drop_name_label")));
    let galley = painter.layout_no_wrap(d.name.clone(), FontId::proportional(12.0), theme::TEXT);
    let at = p + vec2(14.0, 10.0);
    let r = Rect::from_min_size(at, galley.size() + vec2(22.0, 6.0));
    painter.rect_filled(r, 4.0, theme::RAISED);
    painter.rect_filled(
        Rect::from_min_size(r.min + vec2(5.0, 5.0), vec2(10.0, r.height() - 10.0)),
        2.0,
        d.color.map_or(Color32::TRANSPARENT, theme::rgb),
    );
    painter.galley(r.min + vec2(18.0, 3.0), galley, theme::TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_give_names_and_colours() {
        let mut p = Project::new();
        p.patterns[0].name = "Kick".into();
        p.patterns[0].color = Some([200, 0, 0]);
        let pat = p.patterns[0].id;
        assert_eq!(
            of_source(&p, Source::Pattern(pat)),
            Some(Dropped {
                name: "Kick".into(),
                color: Some([200, 0, 0])
            })
        );
        assert_eq!(
            of_file(Path::new("/x/snare_01.wav")),
            Some(Dropped {
                name: "snare_01".into(),
                color: None
            })
        );
    }
}
