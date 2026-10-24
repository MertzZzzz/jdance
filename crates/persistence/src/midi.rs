// SPDX-License-Identifier: GPL-3.0-or-later
//! Standard MIDI File reader (spec 0.2.9 §6): the notes of formats 0, 1
//! and 2, with times converted to the document's ticks. Written from the
//! published file format description: chunks, variable-length numbers,
//! running status, meta and SysEx events.

use jdance_domain::{Note, PPQ};

/// One note of the file: times in the document's ticks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiNote {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    /// `0.0..=1.0`.
    pub velocity: f32,
    pub track: u16,
    pub channel: u8,
}

/// What a MIDI file holds that jdance uses.
#[derive(Debug, Clone, PartialEq)]
pub struct MidiFile {
    /// 0, 1 or 2.
    pub format: u16,
    pub tracks: u16,
    /// Sorted by start, then key.
    pub notes: Vec<MidiNote>,
    /// The first tempo of the file, if it sets one.
    pub tempo_bpm: Option<f64>,
    /// Events read in all tracks (meta and SysEx included).
    pub events: usize,
}

impl MidiFile {
    /// The notes as piano-roll notes from tick 0 (the earliest note at
    /// 0), at most `max_start` ticks in; the rest are counted.
    pub fn piano_notes(&self, max_start: u64) -> (Vec<Note>, usize) {
        let first = self.notes.iter().map(|n| n.start).min().unwrap_or(0);
        let mut out = Vec::new();
        let mut dropped = 0;
        for n in &self.notes {
            let start = n.start - first;
            if start > max_start || start > u64::from(u32::MAX) {
                dropped += 1;
                continue;
            }
            out.push(Note {
                id: 0,
                start: start as u32,
                length: n.length.clamp(1, u64::from(u32::MAX)) as u32,
                key: n.key,
                velocity: n.velocity,
                muted: false,
            });
        }
        (out, dropped)
    }
}

/// Reads a Standard MIDI File.
pub fn read_midi(bytes: &[u8]) -> Result<MidiFile, String> {
    // RMID: a RIFF wrapper around a whole MIDI file (spec 0.3.0 §3.3).
    let bytes = match find(bytes, b"MThd") {
        Some(at) if bytes.starts_with(b"RIFF") => &bytes[at..],
        _ => bytes,
    };
    let mut r = Reader { b: bytes, at: 0 };
    if r.take(4)? != b"MThd" {
        return Err("not a MIDI file (no MThd header)".into());
    }
    let header_len = r.u32()? as usize;
    if header_len < 6 {
        return Err("MIDI header too short".into());
    }
    let format = r.u16()?;
    // The track count of the header is not trusted: some programs write
    // a wrong one, and every MTrk chunk is read (§3.1).
    let _declared_tracks = r.u16()?;
    let division = r.u16()?;
    r.skip(header_len - 6)?;
    if format > 2 {
        return Err(format!("unknown MIDI file format {format}"));
    }
    if division & 0x8000 != 0 {
        return Err("SMPTE time in MIDI files is not supported (ticks per quarter only)".into());
    }
    let ppq = u64::from(division).max(1);
    let to_ticks = |t: u64| -> u64 {
        ((u128::from(t) * u128::from(PPQ) + u128::from(ppq) / 2) / u128::from(ppq)) as u64
    };
    let mut notes = Vec::new();
    let mut tempo_bpm = None;
    let mut track = 0u16;
    let mut events = 0usize;
    let mut first_error = None;
    while bytes.len() - r.at >= 8 {
        let id = r.take(4)?;
        let len = r.u32()? as usize;
        let rest = bytes.len() - r.at;
        if id != b"MTrk" {
            // Unknown chunks are skipped, as the format asks; garbage is
            // stepped over to the next track.
            if id.iter().all(u8::is_ascii_alphanumeric) && len <= rest {
                r.skip(len)?;
            } else {
                match find(&bytes[r.at - 8 + 1..], b"MTrk") {
                    Some(k) => r.at = r.at - 8 + 1 + k,
                    None => break,
                }
            }
            continue;
        }
        // A length past the end, or 0 for a track that has events: up to
        // the next track or the end (§3.2).
        let len = if len > rest || (len == 0 && rest > 0) {
            find(&bytes[r.at..], b"MTrk").unwrap_or(rest)
        } else {
            len
        };
        let body = r.take(len)?;
        if let Err(e) = read_track(body, track, &mut notes, &mut tempo_bpm, &mut events) {
            first_error.get_or_insert(e);
        }
        track += 1;
    }
    if notes.is_empty()
        && let Some(e) = first_error
    {
        return Err(e);
    }
    let mut notes: Vec<MidiNote> = notes
        .into_iter()
        .map(|n: MidiNote| {
            let start = to_ticks(n.start);
            let end = to_ticks(n.start + n.length);
            MidiNote {
                start,
                length: (end - start).max(1),
                ..n
            }
        })
        .collect();
    notes.sort_by_key(|n| (n.start, n.key, n.track, n.channel));
    Ok(MidiFile {
        format,
        tracks: track,
        notes,
        tempo_bpm,
        events,
    })
}

/// Where `needle` first occurs in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Notes of one track chunk in file ticks; notes still on at the end
/// stop there, also when the track is damaged (its error is returned
/// after the notes read so far).
fn read_track(
    body: &[u8],
    track: u16,
    notes: &mut Vec<MidiNote>,
    tempo_bpm: &mut Option<f64>,
    events: &mut usize,
) -> Result<(), String> {
    let mut r = Reader { b: body, at: 0 };
    let mut now = 0u64;
    let mut status = 0u8;
    // Sounding notes per (channel, key): start and velocity, oldest first.
    let mut on: Vec<(u8, u8, u64, u8)> = Vec::new();
    let mut close = |on: &mut Vec<(u8, u8, u64, u8)>, ch: u8, key: u8, at: u64| {
        if let Some(i) = on.iter().position(|o| o.0 == ch && o.1 == key) {
            let (_, _, start, vel) = on.remove(i);
            notes.push(MidiNote {
                start,
                length: at - start,
                key,
                velocity: f32::from(vel) / 127.0,
                track,
                channel: ch,
            });
        }
    };
    let result = read_events(
        &mut r,
        &mut now,
        &mut status,
        &mut on,
        &mut close,
        tempo_bpm,
        events,
    );
    while let Some(&(ch, key, _, _)) = on.first() {
        close(&mut on, ch, key, now);
    }
    result
}

/// Sounding notes: channel, key, start and velocity.
type Sounding = Vec<(u8, u8, u64, u8)>;

fn read_events(
    r: &mut Reader<'_>,
    now: &mut u64,
    status: &mut u8,
    on: &mut Sounding,
    close: &mut impl FnMut(&mut Sounding, u8, u8, u64),
    tempo_bpm: &mut Option<f64>,
    events: &mut usize,
) -> Result<(), String> {
    let body_len = r.b.len();
    while r.at < body_len {
        *now += u64::from(r.var()?);
        *events += 1;
        let now = *now;
        let mut first = r.u8()?;
        match first {
            0xFF => {
                let kind = r.u8()?;
                let len = r.var()? as usize;
                let data = r.take(len)?;
                if kind == 0x51 && data.len() == 3 && tempo_bpm.is_none() {
                    let us =
                        u32::from(data[0]) << 16 | u32::from(data[1]) << 8 | u32::from(data[2]);
                    if us > 0 {
                        *tempo_bpm = Some(60_000_000.0 / f64::from(us));
                    }
                }
                if kind == 0x2F {
                    break;
                }
                continue;
            }
            0xF0 | 0xF7 => {
                let len = r.var()? as usize;
                r.skip(len)?;
                continue;
            }
            // System common and real-time bytes some writers leave in
            // tracks (spec 0.3.0 §3.4).
            0xF1 | 0xF3 => {
                r.skip(1)?;
                continue;
            }
            0xF2 => {
                r.skip(2)?;
                continue;
            }
            0xF4..=0xFE => continue,
            _ => {}
        }
        // Running status: a data byte repeats the last status.
        let data0 = if first < 0x80 {
            if *status == 0 {
                return Err("MIDI data before any status byte".into());
            }
            let d = first;
            first = *status;
            d
        } else {
            *status = first;
            r.u8()?
        };
        let ch = first & 0x0F;
        match first & 0xF0 {
            0x90 => {
                let vel = r.u8()?;
                let key = data0 & 0x7F;
                if vel == 0 {
                    close(on, ch, key, now);
                } else {
                    // A second note on the same key ends the first.
                    close(on, ch, key, now);
                    on.push((ch, key, now, vel & 0x7F));
                }
            }
            0x80 => {
                r.u8()?;
                close(on, ch, data0 & 0x7F, now);
            }
            0xA0 | 0xB0 | 0xE0 => {
                r.u8()?;
            }
            0xC0 | 0xD0 => {}
            _ => return Err(format!("unknown MIDI status {first:#04x}")),
        }
    }
    Ok(())
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.b.len());
        let end = end.ok_or("the MIDI file ends too early")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// A variable-length number: 7 bits per byte, at most four bytes.
    fn var(&mut self) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..4 {
            let b = self.u8()?;
            v = v << 7 | u32::from(b & 0x7F);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("bad variable-length number in the MIDI file".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(mut v: u32) -> Vec<u8> {
        let mut out = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            out.insert(0, (v & 0x7F) as u8 | 0x80);
            v >>= 7;
        }
        out
    }

    fn chunk(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut c = id.to_vec();
        c.extend((body.len() as u32).to_be_bytes());
        c.extend(body);
        c
    }

    fn file(format: u16, division: u16, tracks: &[Vec<u8>]) -> Vec<u8> {
        let mut head = format.to_be_bytes().to_vec();
        head.extend((tracks.len() as u16).to_be_bytes());
        head.extend(division.to_be_bytes());
        let mut f = chunk(b"MThd", &head);
        for t in tracks {
            f.extend(chunk(b"MTrk", t));
        }
        f
    }

    /// Events as (delta, bytes) into a track body ending with End of Track.
    fn track(events: &[(u32, &[u8])]) -> Vec<u8> {
        let mut t = Vec::new();
        for (d, e) in events {
            t.extend(var(*d));
            t.extend(*e);
        }
        t.extend([0, 0xFF, 0x2F, 0]);
        t
    }

    #[test]
    fn format_1_with_running_status_tempo_and_zero_velocity_offs() {
        // 480 ticks per quarter: times double to 960 PPQ.
        let conductor = track(&[(0, &[0xFF, 0x51, 3, 0x07, 0xA1, 0x20])]); // 120 BPM
        let melody = track(&[
            (0, &[0x90, 60, 100]),
            (480, &[60, 0]), // running status, velocity 0 = off
            (0, &[64, 127]),
            (240, &[0x80, 64, 0]),
            (0, &[0xF0, 2, 0x7E, 0xF7]), // SysEx skipped
            (0, &[0xC0, 5]),             // program change, one data byte
            (0, &[0xB0, 7, 100]),
        ]);
        let bass = track(&[(960, &[0x91, 36, 64]), (480, &[0x81, 36, 0])]);
        let f = read_midi(&file(1, 480, &[conductor, melody, bass])).unwrap();
        assert_eq!((f.format, f.tracks), (1, 3));
        assert_eq!(f.tempo_bpm, Some(120.0));
        let n: Vec<(u64, u64, u8)> = f.notes.iter().map(|n| (n.start, n.length, n.key)).collect();
        assert_eq!(n, [(0, 960, 60), (960, 480, 64), (1920, 960, 36)]);
        assert!((f.notes[0].velocity - 100.0 / 127.0).abs() < 1e-6);
        assert_eq!((f.notes[2].track, f.notes[2].channel), (2, 1));
    }

    #[test]
    fn unclosed_notes_end_with_the_track_and_bad_files_are_refused() {
        let t = track(&[(0, &[0x90, 72, 90]), (96, &[0xFF, 0x01, 1, b'x'])]);
        let f = read_midi(&file(0, 96, &[t])).unwrap();
        assert_eq!((f.notes[0].start, f.notes[0].length), (0, 960));
        assert!(read_midi(b"RIFF....").is_err());
        assert!(read_midi(&file(0, 0xE728, &[])).is_err(), "SMPTE");
        let mut cut = file(0, 96, &[track(&[(0, &[0x90, 60, 90])])]);
        cut.truncate(cut.len() - 3);
        let _ = read_midi(&cut); // a cut file returns, never panics
        assert!(
            read_midi(&file(0, 96, &[vec![0x00, 60, 1]])).is_err(),
            "no status"
        );
    }

    #[test]
    fn piano_notes_start_at_zero_and_drop_what_is_too_late() {
        let t = track(&[
            (960, &[0x90, 60, 127]),
            (960, &[0x80, 60, 0]),
            (96_000, &[0x90, 62, 127]),
            (960, &[0x80, 62, 0]),
        ]);
        let f = read_midi(&file(0, 960, &[t])).unwrap();
        let (notes, dropped) = f.piano_notes(10_000);
        assert_eq!(dropped, 1);
        assert_eq!(
            (notes[0].start, notes[0].length, notes[0].velocity),
            (0, 960, 1.0)
        );
    }

    /// Spec 0.3.0 §3: files real programs write that 0.2.9 read as empty.
    #[test]
    fn wrong_track_counts_rmid_system_bytes_and_bad_lengths_still_give_notes() {
        let tempo = track(&[(0, &[0xFF, 0x51, 3, 0x07, 0xA1, 0x20])]);
        let notes = track(&[(0, &[0xF8]), (0, &[0x90, 60, 100]), (96, &[0x80, 60, 0])]);
        // The header says one track; the notes are in the second.
        let mut f = file(1, 96, &[tempo.clone(), notes.clone()]);
        f[10..12].copy_from_slice(&1u16.to_be_bytes());
        let m = read_midi(&f).unwrap();
        assert_eq!(
            (m.tracks, m.notes.len()),
            (2, 1),
            "real-time 0xF8 skipped too"
        );
        // RMID: RIFF … "RMID" "data" <len> <the file>.
        let mut rmid = b"RIFF".to_vec();
        rmid.extend(((f.len() + 12) as u32).to_le_bytes());
        rmid.extend(b"RMIDdata");
        rmid.extend((f.len() as u32).to_le_bytes());
        rmid.extend(&f);
        assert_eq!(read_midi(&rmid).unwrap().notes.len(), 1);
        // A track length of 0 and one past the end.
        let mut zero = file(0, 96, &[notes.clone()]);
        zero[18..22].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(read_midi(&zero).unwrap().notes.len(), 1);
        let mut long = file(0, 96, &[notes.clone()]);
        long[18..22].copy_from_slice(&9999u32.to_be_bytes());
        assert_eq!(read_midi(&long).unwrap().notes.len(), 1);
        // A damaged second track keeps the first one's notes.
        let bad = vec![0x00, 0x90, 64, 100, 0x10, 0xF5];
        let mut both = file(1, 96, &[notes, bad]);
        both.truncate(both.len() - 1);
        assert_eq!(read_midi(&both).unwrap().notes.len(), 2);
    }
}
