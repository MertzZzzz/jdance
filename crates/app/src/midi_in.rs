// SPDX-License-Identifier: GPL-3.0-or-later
//! MIDI input (spec 0.3.0 §12): the system's input ports through `midir`
//! (WinMM, CoreMIDI, ALSA). Every enabled port is read in Omni mode; notes
//! reach the host from the port's own thread, which also wakes the UI so
//! a note does not wait for the next frame.

use midir::{Ignore, MidiInput, MidiInputConnection};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// What jdance plays from MIDI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidiEvent {
    NoteOn { key: u8, velocity: f32 },
    NoteOff { key: u8 },
}

/// Decodes one message: Note On / Off on any channel; Note On with
/// velocity 0 is an Off (§12.3). Other messages are ignored.
pub fn decode(msg: &[u8]) -> Option<MidiEvent> {
    let (&status, data) = msg.split_first()?;
    let key = *data.first()? & 0x7F;
    match status & 0xF0 {
        0x90 => {
            let v = *data.get(1)? & 0x7F;
            Some(if v == 0 {
                MidiEvent::NoteOff { key }
            } else {
                MidiEvent::NoteOn {
                    key,
                    velocity: f32::from(v) / 127.0,
                }
            })
        }
        0x80 => Some(MidiEvent::NoteOff { key }),
        _ => None,
    }
}

/// Wakes the UI from a port thread.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub struct MidiIn {
    /// Open ports by name.
    open: Vec<(String, MidiInputConnection<()>)>,
    tx: Sender<MidiEvent>,
    rx: Receiver<MidiEvent>,
    waker: Option<Waker>,
    last_activity: Option<Instant>,
    /// Port names seen at the last scan.
    ports: Vec<String>,
}

impl std::fmt::Debug for MidiIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiIn")
            .field(
                "open",
                &self.open.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .field("ports", &self.ports)
            .finish_non_exhaustive()
    }
}

impl Default for MidiIn {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            open: Vec::new(),
            tx,
            rx,
            waker: None,
            last_activity: None,
            ports: Vec::new(),
        }
    }
}

impl MidiIn {
    /// The system's input ports, scanned again (Rescan, §12.1).
    pub fn scan(&mut self) -> &[String] {
        self.ports = MidiInput::new("jdance scan")
            .map(|m| {
                m.ports()
                    .iter()
                    .filter_map(|p| m.port_name(p).ok())
                    .collect()
            })
            .unwrap_or_default();
        &self.ports
    }

    pub fn ports(&self) -> &[String] {
        &self.ports
    }

    pub fn is_open(&self, name: &str) -> bool {
        self.open.iter().any(|(n, _)| n == name)
    }

    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = Some(waker);
    }

    /// Opens exactly the ports named in `enabled` that exist; returns
    /// messages for ports that could not be opened.
    pub fn apply(&mut self, enabled: &[String]) -> Vec<String> {
        let mut errors = Vec::new();
        // Close what is no longer wanted (or gone, §12.5).
        let ports = &self.ports;
        self.open
            .retain(|(n, _)| enabled.contains(n) && ports.contains(n));
        for name in enabled {
            if self.is_open(name) || !self.ports.contains(name) {
                continue;
            }
            match self.connect(name) {
                Ok(c) => self.open.push((name.clone(), c)),
                Err(e) => errors.push(format!("MIDI input {name}: {e}")),
            }
        }
        errors
    }

    fn connect(&self, name: &str) -> Result<MidiInputConnection<()>, String> {
        let mut input = MidiInput::new("jdance").map_err(|e| e.to_string())?;
        input.ignore(Ignore::All);
        let port = input
            .ports()
            .into_iter()
            .find(|p| input.port_name(p).ok().as_deref() == Some(name))
            .ok_or("the device is gone")?;
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        input
            .connect(
                &port,
                "jdance-in",
                move |_, msg, ()| {
                    if let Some(e) = decode(msg)
                        && tx.send(e).is_ok()
                        && let Some(w) = &waker
                    {
                        w();
                    }
                },
                (),
            )
            .map_err(|e| e.to_string())
    }

    /// Events since the last call.
    pub fn take(&mut self) -> Vec<MidiEvent> {
        let events: Vec<MidiEvent> = self.rx.try_iter().collect();
        if !events.is_empty() {
            self.last_activity = Some(Instant::now());
        }
        events
    }

    /// MIDI arrived lately: the activity light (§12.4).
    pub fn active(&self) -> bool {
        self.last_activity
            .is_some_and(|t| t.elapsed() < Duration::from_millis(150))
    }
}

/// Which channel each sounding key started on, so its Off goes there
/// even when the input moved to another channel meanwhile (§12.6).
#[derive(Debug, Default)]
pub struct Sounding {
    keys: Vec<(u8, jdance_domain::ChannelId)>,
}

impl Sounding {
    /// The channel an event plays on: On on `target`, Off where its On was.
    pub fn route(
        &mut self,
        e: MidiEvent,
        target: Option<jdance_domain::ChannelId>,
    ) -> Option<(jdance_domain::ChannelId, u8, Option<f32>)> {
        match e {
            MidiEvent::NoteOn { key, velocity } => {
                let ch = target?;
                self.keys.push((key, ch));
                Some((ch, key, Some(velocity)))
            }
            MidiEvent::NoteOff { key } => {
                let i = self.keys.iter().position(|(k, _)| *k == key)?;
                let (_, ch) = self.keys.remove(i);
                Some((ch, key, None))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdance_domain::ChannelId;

    #[test]
    fn notes_on_every_channel_and_zero_velocity_offs() {
        assert_eq!(
            decode(&[0x93, 60, 127]),
            Some(MidiEvent::NoteOn {
                key: 60,
                velocity: 1.0
            })
        );
        assert_eq!(decode(&[0x90, 60, 0]), Some(MidiEvent::NoteOff { key: 60 }));
        assert_eq!(
            decode(&[0x8F, 61, 40]),
            Some(MidiEvent::NoteOff { key: 61 })
        );
        assert_eq!(decode(&[0xB0, 64, 127]), None, "controllers are not notes");
        assert_eq!(decode(&[0x90]), None, "short");
    }

    #[test]
    fn an_off_goes_to_the_channel_its_on_played() {
        let (a, b) = (ChannelId(1), ChannelId(2));
        let mut s = Sounding::default();
        let on = MidiEvent::NoteOn {
            key: 60,
            velocity: 0.5,
        };
        assert_eq!(s.route(on, Some(a)), Some((a, 60, Some(0.5))));
        // The Piano Roll moved to another channel meanwhile.
        assert_eq!(
            s.route(MidiEvent::NoteOff { key: 60 }, Some(b)),
            Some((a, 60, None))
        );
        assert_eq!(s.route(MidiEvent::NoteOff { key: 60 }, Some(b)), None);
        assert_eq!(s.route(on, None), None, "no channel, nothing plays");
    }
}
