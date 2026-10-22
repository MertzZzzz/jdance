// SPDX-License-Identifier: GPL-3.0-or-later
//! Recording one hardware input (spec 0.2.0 §7).
//!
//! The capture callback copies frames into a preallocated ring; a writer
//! thread empties it into a float WAV named `*.wav.part` and renames the
//! file when the take ends. No file I/O happens on the audio thread. A
//! full ring stops the take (the written part stays valid); a write error
//! ends it with the frames written so far. Files left as `.part` by a
//! crash are found at startup and repaired on request.
//!
//! Monitoring passes the input to the output through a second ring that
//! the output callback receives by message (it never allocates or frees
//! on its thread).

use jdance_audio_api::CaptureCallback;
use rtrb::{Consumer, Producer, RingBuffer};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::Duration;

/// Ring length: what the writer may fall behind before the take stops.
pub const RING_SECONDS: u32 = 20;
/// Monitor ring length in samples (stereo pairs); more is dropped so the
/// monitor stays short.
const MONITOR_SAMPLES: usize = 8_192;
/// Live input ring for mixer track inputs, in frames of all channels
/// (spec 0.2.3 INP-005).
const LIVE_FRAMES: usize = 8_192;
/// Largest output callback the live input serves, in frames.
const LIVE_BLOCK: usize = 8_192;
/// Frames of live input kept beyond one callback; more is dropped so the
/// monitoring delay stays short when the input clock runs fast.
const LIVE_SLACK: usize = 1_024;
/// Frames per waveform peak sent to the UI while recording (REC-016).
pub const PEAK_FRAMES: usize = 256;
/// Peak queue length; the UI drains it every frame, more is dropped (only
/// the display loses detail).
const PEAK_QUEUE: usize = 16_384;
/// Unfinished takes end with this after `.wav`.
pub const PART_SUFFIX: &str = ".wav.part";
const HEADER: u64 = 44;

/// State shared by the capture callback, the writer and the UI.
#[derive(Debug, Default)]
pub struct Shared {
    /// Peaks since the last read, as `f32` bits.
    peak: [AtomicU32; 2],
    pub recording: AtomicBool,
    /// A take waits for the transport to start (after the count-in); the
    /// output callback then sets `recording` (spec 0.2.3 REC-013).
    pub armed: AtomicBool,
    pub overflow: AtomicBool,
    pub monitor: AtomicBool,
    /// Some mixer track monitors its input: the capture feeds the engine
    /// (INP-004).
    pub live: AtomicBool,
    /// Output callbacks that found too little live input (INP-005).
    pub live_underruns: AtomicU64,
    /// Frames captured in the current take.
    pub captured: AtomicU64,
}

impl Shared {
    /// Peaks since the last call (the meter decays in the UI).
    pub fn take_peaks(&self) -> (f32, f32) {
        let get = |a: &AtomicU32| f32::from_bits(a.swap(0, Relaxed));
        (get(&self.peak[0]), get(&self.peak[1]))
    }
}

/// The capture side: always meters, records while `recording` is set,
/// monitors while `monitor` is set.
pub struct Capture {
    shared: Arc<Shared>,
    rec: Producer<f32>,
    mon: Producer<f32>,
    /// Waveform of the take for the Playlist: the largest absolute value
    /// per [`PEAK_FRAMES`] frames (REC-014/016).
    peaks: Producer<f32>,
    peak_acc: f32,
    peak_n: usize,
    /// All input channels, interleaved, for the engine.
    live: Producer<f32>,
}

impl CaptureCallback for Capture {
    fn capture_all(&mut self, frames: &[f32], _channels: usize) {
        if self.shared.live.load(Relaxed) && self.live.slots() >= frames.len() {
            for v in frames {
                let _ = self.live.push(*v);
            }
        }
    }

    fn capture(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        for (slot, p) in self
            .shared
            .peak
            .iter()
            .zip([peak(&left[..n]), peak(&right[..n])])
        {
            slot.fetch_max(p.to_bits(), Relaxed);
        }
        if self.shared.recording.load(Relaxed) {
            if self.rec.slots() < 2 * n {
                // Never continue with lost frames.
                self.shared.overflow.store(true, Relaxed);
                self.shared.recording.store(false, Relaxed);
            } else {
                for i in 0..n {
                    let _ = self.rec.push(left[i]);
                    let _ = self.rec.push(right[i]);
                    self.peak_acc = self.peak_acc.max(left[i].abs()).max(right[i].abs());
                    self.peak_n += 1;
                    if self.peak_n == PEAK_FRAMES {
                        let _ = self.peaks.push(self.peak_acc);
                        self.peak_acc = 0.0;
                        self.peak_n = 0;
                    }
                }
                self.shared.captured.fetch_add(n as u64, Relaxed);
            }
        }
        if self.shared.monitor.load(Relaxed) {
            for i in 0..n {
                if self.mon.slots() < 2 {
                    break;
                }
                let _ = self.mon.push(left[i]);
                let _ = self.mon.push(right[i]);
            }
        }
    }
}

/// Readers of a capture: the recording ring, the monitor ring and the
/// take's waveform peaks.
pub struct CaptureOutputs {
    pub ring: Consumer<f32>,
    pub monitor: Consumer<f32>,
    pub peaks: Consumer<f32>,
    /// All channels interleaved, for [`LiveInputMix`].
    pub live: Consumer<f32>,
}

/// The capture callback and its readers for an input at `rate`.
pub fn capture(shared: Arc<Shared>, rate: u32) -> (Capture, CaptureOutputs) {
    let (rec, ring) = RingBuffer::new(2 * (rate * RING_SECONDS) as usize);
    let (mon, monitor) = RingBuffer::new(MONITOR_SAMPLES);
    let (peaks, peaks_out) = RingBuffer::new(PEAK_QUEUE);
    let (live, live_out) = RingBuffer::new(LIVE_FRAMES * jdance_audio_api::MAX_LIVE_INPUTS);
    let cap = Capture {
        shared,
        rec,
        mon,
        peaks,
        peak_acc: 0.0,
        peak_n: 0,
        live,
    };
    (
        cap,
        CaptureOutputs {
            ring,
            monitor,
            peaks: peaks_out,
            live: live_out,
        },
    )
}

/// Output side of the mixer track inputs (INP-005): takes the frames of
/// one output callback from the live ring into a preallocated block. A new
/// ring (with its channel count) arrives through `inbox`; the old one
/// leaves through `trash`, like [`MonitorMix`].
pub struct LiveInputMix {
    inbox: Consumer<(Consumer<f32>, usize)>,
    trash: Producer<Consumer<f32>>,
    current: Option<(Consumer<f32>, usize)>,
    block: Vec<f32>,
    shared: Arc<Shared>,
}

/// UI side of [`LiveInputMix`].
pub struct LiveInputControl {
    inbox: Producer<(Consumer<f32>, usize)>,
    trash: Consumer<Consumer<f32>>,
}

pub fn live_input_mix(shared: Arc<Shared>) -> (LiveInputMix, LiveInputControl) {
    let (inbox_tx, inbox) = RingBuffer::new(4);
    let (trash, trash_rx) = RingBuffer::new(8);
    (
        LiveInputMix {
            inbox,
            trash,
            current: None,
            block: vec![0.0; LIVE_BLOCK * jdance_audio_api::MAX_LIVE_INPUTS],
            shared,
        },
        LiveInputControl {
            inbox: inbox_tx,
            trash: trash_rx,
        },
    )
}

impl LiveInputControl {
    /// Hands the output callback a new input ring of `channels` channels.
    pub fn send(&mut self, ring: Consumer<f32>, channels: usize) {
        let _ = self.inbox.push((ring, channels));
    }

    /// Frees rings the output callback is done with.
    pub fn collect(&mut self) {
        while self.trash.pop().is_ok() {}
    }
}

impl LiveInputMix {
    /// The live input for an output callback of `frames` frames and its
    /// channel count; silence fills what the input has not delivered.
    /// Realtime-safe.
    pub fn take(&mut self, frames: usize) -> (&[f32], usize) {
        if let Ok(next) = self.inbox.pop()
            && let Some((old, _)) = self.current.replace(next)
        {
            let _ = self.trash.push(old);
        }
        let Some((ring, ch)) = &mut self.current else {
            return (&[], 0);
        };
        let ch = (*ch).clamp(1, jdance_audio_api::MAX_LIVE_INPUTS);
        if !self.shared.live.load(Relaxed) {
            // Nothing monitored: drop what came in so a later start is
            // not late.
            let n = ring.slots();
            if let Ok(c) = ring.read_chunk(n) {
                c.commit_all();
            }
            return (&[], 0);
        }
        let frames = frames.min(LIVE_BLOCK);
        let need = frames * ch;
        // Clock drift: keep at most one callback plus some slack queued.
        let excess = ring.slots().saturating_sub(need + LIVE_SLACK * ch) / ch * ch;
        if excess > 0
            && let Ok(c) = ring.read_chunk(excess)
        {
            c.commit_all();
        }
        let got = ring.slots().min(need) / ch * ch;
        let block = &mut self.block[..need];
        if let Ok(c) = ring.read_chunk(got) {
            let (a, b) = c.as_slices();
            block[..a.len()].copy_from_slice(a);
            block[a.len()..a.len() + b.len()].copy_from_slice(b);
            c.commit_all();
        }
        if got < need {
            block[got..].fill(0.0);
            self.shared.live_underruns.fetch_add(1, Relaxed);
        }
        (&self.block[..need], ch)
    }
}

/// Output-side monitor: adds the input to the rendered block. A new ring
/// arrives through `inbox`; the old one leaves through `trash` to be freed
/// on the UI thread.
pub struct MonitorMix {
    inbox: Consumer<Consumer<f32>>,
    trash: Producer<Consumer<f32>>,
    current: Option<Consumer<f32>>,
}

/// UI side of [`MonitorMix`].
pub struct MonitorControl {
    inbox: Producer<Consumer<f32>>,
    trash: Consumer<Consumer<f32>>,
}

pub fn monitor_mix() -> (MonitorMix, MonitorControl) {
    let (inbox_tx, inbox) = RingBuffer::new(4);
    let (trash, trash_rx) = RingBuffer::new(8);
    (
        MonitorMix {
            inbox,
            trash,
            current: None,
        },
        MonitorControl {
            inbox: inbox_tx,
            trash: trash_rx,
        },
    )
}

impl MonitorControl {
    pub fn send(&mut self, ring: Consumer<f32>) {
        let _ = self.inbox.push(ring);
    }

    /// Frees rings the output callback is done with.
    pub fn collect(&mut self) {
        while self.trash.pop().is_ok() {}
    }
}

impl MonitorMix {
    /// Realtime-safe.
    pub fn add(&mut self, left: &mut [f32], right: &mut [f32]) {
        if let Ok(next) = self.inbox.pop()
            && let Some(old) = self.current.replace(next)
        {
            let _ = self.trash.push(old);
        }
        let Some(ring) = &mut self.current else {
            return;
        };
        // Clock drift between input and output: keep the delay short.
        let excess = ring.slots().saturating_sub(MONITOR_SAMPLES / 2) & !1;
        if excess > 0
            && let Ok(chunk) = ring.read_chunk(excess)
        {
            chunk.commit_all();
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if ring.slots() < 2 {
                break;
            }
            *l += ring.pop().unwrap_or(0.0);
            *r += ring.pop().unwrap_or(0.0);
        }
    }
}

/// A float WAV written as it grows; sizes are patched by [`Self::finish`]
/// (or by [`repair`] after a crash).
pub struct WavWriter {
    file: BufWriter<File>,
    channels: u16,
    frames: u64,
}

fn header(channels: u16, rate: u32, frames: u64) -> [u8; HEADER as usize] {
    let block = u32::from(channels) * 4;
    let data = u32::try_from(frames * u64::from(block)).unwrap_or(u32::MAX - 64);
    let mut h = [0u8; HEADER as usize];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&3u16.to_le_bytes()); // IEEE float
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * block).to_le_bytes());
    h[32..34].copy_from_slice(&(block as u16).to_le_bytes());
    h[34..36].copy_from_slice(&32u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

impl WavWriter {
    pub fn create(path: &Path, channels: u16, rate: u32) -> std::io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&header(channels, rate, 0))?;
        // The rate is needed again only by `finish`, which reads it back.
        Ok(Self {
            file,
            channels,
            frames: 0,
        })
    }

    /// Interleaved stereo frames; a mono file keeps the left channel.
    pub fn write_stereo(&mut self, frames: &[f32]) -> std::io::Result<()> {
        for f in frames.chunks_exact(2) {
            self.file.write_all(&f[0].to_le_bytes())?;
            if self.channels == 2 {
                self.file.write_all(&f[1].to_le_bytes())?;
            }
        }
        self.frames += (frames.len() / 2) as u64;
        Ok(())
    }

    pub fn finish(mut self, rate: u32) -> std::io::Result<u64> {
        self.file.flush()?;
        let mut f = self.file.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(self.channels, rate, self.frames))?;
        f.sync_all()?;
        Ok(self.frames)
    }
}

/// Fixes the sizes of a float WAV that was not finished (crash, power
/// loss): keeps every whole frame on disk. Returns (channels, rate,
/// frames).
pub fn repair(path: &Path) -> std::io::Result<(u16, u32, u64)> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_owned());
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut h = [0u8; HEADER as usize];
    f.read_exact(&mut h)?;
    if &h[0..4] != b"RIFF" || &h[8..12] != b"WAVE" || &h[36..40] != b"data" {
        return Err(bad("not a jdance recording"));
    }
    let channels = u16::from_le_bytes([h[22], h[23]]);
    let rate = u32::from_le_bytes([h[24], h[25], h[26], h[27]]);
    if channels == 0 || channels > 2 || rate == 0 {
        return Err(bad("not a jdance recording"));
    }
    let block = u64::from(channels) * 4;
    let len = f.metadata()?.len();
    let frames = len.saturating_sub(HEADER) / block;
    f.set_len(HEADER + frames * block)?;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&header(channels, rate, frames))?;
    f.sync_all()?;
    Ok((channels, rate, frames))
}

/// `path` without `.part`, numbered if that name is taken.
pub fn final_name(part: &Path) -> PathBuf {
    let name = part
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let stem = name.strip_suffix(PART_SUFFIX).unwrap_or(&name).to_owned();
    let dir = part.parent().unwrap_or(Path::new("."));
    (1..)
        .map(|n| {
            if n == 1 {
                dir.join(format!("{stem}.wav"))
            } else {
                dir.join(format!("{stem}-{n}.wav"))
            }
        })
        .find(|p| !p.exists())
        .expect("unbounded")
}

/// Unfinished takes in `dirs`.
pub fn unfinished(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(PART_SUFFIX))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A finished take.
#[derive(Debug)]
pub struct Take {
    pub path: PathBuf,
    pub frames: u64,
    /// Why the take ended early (ring overflow, disk error); the file holds
    /// the frames before it.
    pub problem: Option<String>,
}

/// A take being written.
pub struct Session {
    pub part: PathBuf,
    thread: JoinHandle<(Consumer<f32>, Result<Take, String>)>,
}

impl Session {
    /// Starts writing `channels` at `rate` to `part`; the capture side
    /// starts pushing when `shared.recording` is set by the caller.
    pub fn start(
        part: PathBuf,
        channels: u16,
        rate: u32,
        mut ring: Consumer<f32>,
        shared: Arc<Shared>,
    ) -> Result<Self, (Consumer<f32>, String)> {
        // Stale frames from an earlier take.
        let stale = ring.slots();
        if let Ok(c) = ring.read_chunk(stale) {
            c.commit_all();
        }
        let mut writer = match WavWriter::create(&part, channels, rate) {
            Ok(w) => w,
            Err(e) => return Err((ring, format!("cannot create {}: {e}", part.display()))),
        };
        let path = part.clone();
        let thread = std::thread::Builder::new()
            .name("jdance-recorder".into())
            .spawn(move || {
                let mut problem = None;
                let mut buf = Vec::with_capacity(8192);
                let mut last_round = false;
                loop {
                    let n = ring.slots() & !1;
                    if n > 0 {
                        let take = n.min(16_384);
                        buf.clear();
                        if let Ok(chunk) = ring.read_chunk(take) {
                            let (a, b) = chunk.as_slices();
                            buf.extend_from_slice(a);
                            buf.extend_from_slice(b);
                            chunk.commit_all();
                        }
                        if let Err(e) = writer.write_stereo(&buf) {
                            shared.recording.store(false, Relaxed);
                            problem = Some(format!("writing the recording failed: {e}"));
                            break;
                        }
                        continue;
                    }
                    if !shared.recording.load(Relaxed) && !shared.armed.load(Relaxed) {
                        // One more pass for a block pushed while stopping.
                        if last_round {
                            break;
                        }
                        last_round = true;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                if problem.is_none() && shared.overflow.load(Relaxed) {
                    problem = Some(
                        "recording stopped: the disk could not keep up (input buffer full)".into(),
                    );
                }
                let result = writer
                    .finish(rate)
                    .and_then(|frames| {
                        let done = final_name(&path);
                        std::fs::rename(&path, &done)?;
                        Ok(Take {
                            path: done,
                            frames,
                            problem: problem.clone(),
                        })
                    })
                    .map_err(|e| {
                        problem.unwrap_or_else(|| format!("finishing the recording failed: {e}"))
                    });
                (ring, result)
            });
        match thread {
            Ok(thread) => Ok(Self { part, thread }),
            Err(e) => {
                // The ring went with the closure: the caller reopens the
                // input for a new one.
                let _ = std::fs::remove_file(&part);
                Err((
                    RingBuffer::new(2).1,
                    format!("cannot start the recorder: {e}"),
                ))
            }
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Waits for the writer (after `recording` was cleared).
    pub fn join(self) -> (Option<Consumer<f32>>, Result<Take, String>) {
        match self.thread.join() {
            Ok((ring, r)) => (Some(ring), r),
            Err(_) => (None, Err("the recorder thread crashed".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_input_reaches_the_output_callback_with_bounded_delay() {
        let shared = Arc::new(Shared::default());
        let (mut cap, out) = capture(Arc::clone(&shared), 48_000);
        let (mut mix, mut control) = live_input_mix(Arc::clone(&shared));
        control.send(out.live, 3);
        // Nothing monitored: no feed, and stale input is dropped.
        cap.capture_all(&[0.5; 30], 3);
        assert_eq!(mix.take(10), (&[][..], 0));
        shared.live.store(true, Relaxed);
        let frames: Vec<f32> = (0..300).map(|i| i as f32).collect();
        cap.capture_all(&frames, 3);
        let (block, ch) = mix.take(100);
        assert_eq!((ch, block.len()), (3, 300));
        assert_eq!(&block[..6], &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        // Too little input: the rest is silence and counted.
        cap.capture_all(&frames[..30], 3);
        let (block, _) = mix.take(100);
        assert_eq!(block[29], 29.0);
        assert!(block[30..].iter().all(|v| *v == 0.0));
        assert_eq!(shared.live_underruns.load(Relaxed), 1);
        // A fast input clock: what exceeds one callback plus the slack
        // is dropped, oldest first.
        let lots: Vec<f32> = (0..3 * 4_000).map(|i| i as f32).collect();
        cap.capture_all(&lots, 3);
        let (block, _) = mix.take(100);
        let first = block[0] as usize / 3;
        assert_eq!(first, 4_000 - 100 - LIVE_SLACK);
        control.collect();
    }

    #[test]
    fn takes_send_waveform_peaks_while_recording() {
        let shared = Arc::new(Shared::default());
        let (mut cap, mut out) = capture(Arc::clone(&shared), 48_000);
        cap.capture(&[0.9; 512], &[0.9; 512]);
        assert_eq!(out.peaks.slots(), 0, "only while recording");
        shared.recording.store(true, Relaxed);
        let left: Vec<f32> = (0..1000)
            .map(|i| if i == 300 { -0.8 } else { 0.1 })
            .collect();
        cap.capture(&left, &[0.2; 1000]);
        let peaks: Vec<f32> = std::iter::from_fn(|| out.peaks.pop().ok()).collect();
        assert_eq!(peaks, [0.2, 0.8, 0.2], "per 256 frames; the rest waits");
    }

    #[test]
    fn an_armed_take_waits_for_the_transport_and_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(Shared::default());
        let (mut cap, out) = capture(Arc::clone(&shared), 48_000);
        let ring = out.ring;
        let part = dir.path().join(format!("Armed{PART_SUFFIX}"));
        shared.armed.store(true, Relaxed);
        let session = Session::start(part, 2, 48_000, ring, Arc::clone(&shared))
            .map_err(|e| e.1)
            .unwrap();
        // During the count-in nothing is captured and the writer waits.
        cap.capture(&[0.5; 480], &[0.5; 480]);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!session.is_finished());
        assert_eq!(shared.captured.load(Relaxed), 0);
        // Stop during the count-in: an empty take.
        shared.armed.store(false, Relaxed);
        let (_, take) = session.join();
        assert_eq!(take.unwrap().frames, 0);
    }

    #[test]
    fn take_is_written_finished_and_readable() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(Shared::default());
        let (mut cap, out) = capture(Arc::clone(&shared), 48_000);
        let ring = out.ring;
        let part = dir.path().join(format!("Take{PART_SUFFIX}"));
        let session = Session::start(part.clone(), 2, 48_000, ring, Arc::clone(&shared))
            .map_err(|e| e.1)
            .unwrap();
        cap.capture(&[0.5; 64], &[0.25; 64]);
        assert_eq!(shared.captured.load(Relaxed), 0, "not recording yet");
        shared.recording.store(true, Relaxed);
        for _ in 0..100 {
            cap.capture(&[0.5; 480], &[-0.25; 480]);
        }
        shared.recording.store(false, Relaxed);
        let (ring, take) = session.join();
        assert!(ring.is_some());
        let take = take.unwrap();
        assert_eq!(take.frames, 48_000);
        assert!(take.problem.is_none());
        assert_eq!(take.path, dir.path().join("Take.wav"));
        assert!(!part.exists());
        let mut r = hound::WavReader::open(&take.path).unwrap();
        assert_eq!(r.spec().sample_format, hound::SampleFormat::Float);
        assert_eq!(r.duration(), 48_000);
        let s: Vec<f32> = r.samples::<f32>().take(2).map(Result::unwrap).collect();
        assert_eq!(s, [0.5, -0.25]);
        let (l, rr) = shared.take_peaks();
        assert_eq!((l, rr), (0.5, 0.25));
    }

    #[test]
    fn overflow_stops_the_take_and_keeps_what_was_written() {
        let shared = Arc::new(Shared::default());
        let (mut cap, _out) = capture(Arc::clone(&shared), 100);
        shared.recording.store(true, Relaxed);
        // 20 s at 100 Hz fit; the next block does not (nobody reads).
        cap.capture(&[0.1; 2000], &[0.1; 2000]);
        assert!(shared.recording.load(Relaxed));
        cap.capture(&[0.1; 10], &[0.1; 10]);
        assert!(shared.overflow.load(Relaxed));
        assert!(!shared.recording.load(Relaxed));
        assert_eq!(shared.captured.load(Relaxed), 2000);
    }

    #[test]
    fn unfinished_files_are_found_and_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join(format!("Crash{PART_SUFFIX}"));
        let mut w = WavWriter::create(&part, 1, 44_100).unwrap();
        w.write_stereo(&[0.5, 0.0, 0.25, 0.0, 0.125, 0.0]).unwrap();
        drop(w); // no finish: sizes stay 0
        // A torn last sample.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .unwrap()
            .write_all(&[1, 2])
            .unwrap();
        assert_eq!(
            unfinished(&[dir.path().to_path_buf()]),
            std::slice::from_ref(&part)
        );
        assert_eq!(repair(&part).unwrap(), (1, 44_100, 3));
        let r = hound::WavReader::open(&part).unwrap();
        assert_eq!(r.duration(), 3);
        assert_eq!(final_name(&part), dir.path().join("Crash.wav"));
    }

    #[test]
    fn monitor_adds_the_input_and_hands_old_rings_back() {
        let shared = Arc::new(Shared::default());
        shared.monitor.store(true, Relaxed);
        let (mut cap, out) = capture(Arc::clone(&shared), 48_000);
        let mon = out.monitor;
        let (mut mix, mut ctl) = monitor_mix();
        ctl.send(mon);
        cap.capture(&[0.5; 4], &[0.25; 4]);
        let (mut l, mut r) = ([1.0; 8], [0.0; 8]);
        mix.add(&mut l, &mut r);
        assert_eq!(&l[..5], &[1.5, 1.5, 1.5, 1.5, 1.0]);
        assert_eq!(r[0], 0.25);
        let (_c2, out2) = capture(Arc::clone(&shared), 48_000);
        let mon2 = out2.monitor;
        ctl.send(mon2);
        mix.add(&mut l, &mut r);
        ctl.collect();
    }
}
