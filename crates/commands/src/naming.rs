// SPDX-License-Identifier: GPL-3.0-or-later
//! Name and colour of lanes, mixer tracks, channels and patterns, set in
//! one place for the colour window, drops and name presets (spec 0.2.5
//! §6–§9, «Общие условия» item 2).

use crate::Edit;
use jdance_domain::color::{Rgb, clip_name};
use jdance_domain::{ChannelId, LaneId, PatternId, Project, TrackId};

/// Something that has a name and a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Named {
    Lane(LaneId),
    Track(TrackId),
    Channel(ChannelId),
    Pattern(PatternId),
}

/// The current name and colour of `target`; `None` if it does not exist.
/// The colour is `None` where the target uses its default.
pub fn name_color(p: &Project, target: Named) -> Option<(String, Option<Rgb>)> {
    match target {
        Named::Lane(id) => p.playlist.lane(id).map(|l| (l.name.clone(), Some(l.color))),
        Named::Track(id) => p.mixer.track(id).map(|t| (t.name.clone(), t.color)),
        Named::Channel(id) => p.channel(id).map(|c| (c.name.clone(), c.color)),
        Named::Pattern(id) => p.pattern(id).map(|x| (x.name.clone(), x.color)),
    }
}

/// The colour the D button gives `target` (spec 0.2.5 §6): what it shows
/// without a colour of its own.
pub fn default_color(p: &Project, target: Named) -> Rgb {
    match target {
        Named::Lane(id) => {
            let i = p
                .playlist
                .lanes
                .iter()
                .position(|l| l.id == id)
                .unwrap_or(0);
            jdance_domain::LANE_COLORS[i % jdance_domain::LANE_COLORS.len()]
        }
        Named::Track(id) => {
            let i = p
                .mixer
                .tracks
                .iter()
                .position(|t| t.id == id)
                .map_or(0, |i| i + 1);
            jdance_domain::color_or_default(None, i)
        }
        Named::Channel(id) => {
            jdance_domain::color_or_default(None, p.channel_index(id).unwrap_or(0))
        }
        Named::Pattern(_) => jdance_domain::color::DEFAULT_PATTERN_COLOR,
    }
}

/// The colour `target` shows: its own or its default.
pub fn shown_color(p: &Project, target: Named) -> Rgb {
    name_color(p, target)
        .and_then(|(_, c)| c)
        .unwrap_or_else(|| default_color(p, target))
}

/// Sets a colour directly, outside the history: the live view of the
/// colour window, undone by writing the old colour back.
pub fn write_color(p: &mut Project, target: Named, color: Option<Rgb>) {
    match target {
        Named::Lane(id) => {
            if let (Some(l), Some(c)) = (p.playlist.lane_mut(id), color) {
                l.color = c;
            }
        }
        Named::Track(id) => {
            if let Some(t) = p.mixer.track_mut(id) {
                t.color = color;
            }
        }
        Named::Channel(id) => {
            if let Some(c) = p.channel_mut(id) {
                c.color = color;
            }
        }
        Named::Pattern(id) => {
            if let Some(x) = p.pattern_mut(id) {
                x.color = color;
            }
        }
    }
}

/// One edit giving every target `name` (cut to the name limit) and
/// `color`, each when given; `None` when nothing would change, so a drop
/// of the same values leaves no undo step (spec 0.2.5 §7 item 9).
/// Targets that do not exist are skipped.
pub fn set_name_color(
    p: &Project,
    targets: &[Named],
    name: Option<&str>,
    color: Option<Rgb>,
) -> Option<Edit> {
    let name = name.map(clip_name).filter(|n| !n.is_empty());
    let mut edits = Vec::new();
    for &t in targets {
        let Some((old_name, old_color)) = name_color(p, t) else {
            continue;
        };
        if let Some(n) = &name
            && *n != old_name
        {
            edits.push(match t {
                Named::Lane(id) => Edit::RenameLane(id, n.clone()),
                Named::Track(id) => Edit::RenameTrack(id, n.clone()),
                Named::Channel(id) => Edit::RenameChannel(id, n.clone()),
                Named::Pattern(id) => Edit::RenamePattern(id, n.clone()),
            });
        }
        if let Some(c) = color
            && Some(c) != old_color
        {
            edits.push(match t {
                Named::Lane(id) => Edit::SetLaneColor(id, c),
                Named::Track(id) => Edit::SetTrackColor(id, Some(c)),
                Named::Channel(id) => Edit::SetChannelColor(id, Some(c)),
                Named::Pattern(id) => Edit::SetPatternColor(id, Some(c)),
            });
        }
    }
    (!edits.is_empty()).then_some(Edit::Batch(edits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;
    use jdance_domain::{MixerTrack, Playlist};

    fn project() -> (Project, TrackId, LaneId, PatternId) {
        let mut p = Project::new();
        let t = TrackId(p.ids.next());
        p.mixer.tracks.push(MixerTrack::new(t, "Insert 1"));
        p.playlist = Playlist::with_lanes(2, || p.ids.next());
        let lane = p.playlist.lanes[0].id;
        let pat = p.patterns[0].id;
        (p, t, lane, pat)
    }

    #[test]
    fn names_and_colours_apply_together_and_undo_in_one_step() {
        let (mut p, t, lane, pat) = project();
        let mut h = History::default();
        let red = [200, 30, 30];
        let e = set_name_color(
            &p,
            &[Named::Track(t), Named::Lane(lane)],
            Some("Kick"),
            Some(red),
        )
        .unwrap();
        h.execute(&mut p, e).unwrap();
        assert_eq!(p.mixer.track(t).unwrap().name, "Kick");
        assert_eq!(p.mixer.track(t).unwrap().color, Some(red));
        assert_eq!(p.playlist.lane(lane).unwrap().color, red);
        h.undo(&mut p).unwrap();
        assert_eq!(p.mixer.track(t).unwrap().name, "Insert 1");
        assert_eq!(p.mixer.track(t).unwrap().color, None);
        // Pattern colour.
        let e = set_name_color(&p, &[Named::Pattern(pat)], None, Some(red)).unwrap();
        h.execute(&mut p, e).unwrap();
        assert_eq!(p.pattern(pat).unwrap().color, Some(red));
    }

    #[test]
    fn same_values_no_colour_and_long_names() {
        let (p, t, ..) = project();
        assert_eq!(
            set_name_color(&p, &[Named::Track(t)], Some("Insert 1"), None),
            None
        );
        // A file without colour keeps the target's colour (§7 item 4).
        let e = set_name_color(&p, &[Named::Track(t)], Some("snare_01"), None).unwrap();
        assert_eq!(
            e,
            Edit::Batch(vec![Edit::RenameTrack(t, "snare_01".into())])
        );
        let long = "x".repeat(200);
        let Some(Edit::Batch(e)) = set_name_color(&p, &[Named::Track(t)], Some(&long), None) else {
            panic!()
        };
        assert_eq!(e, [Edit::RenameTrack(t, "x".repeat(64))]);
    }
}
