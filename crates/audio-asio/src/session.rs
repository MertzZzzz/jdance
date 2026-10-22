// SPDX-License-Identifier: GPL-3.0-or-later
//! A running ASIO session: buffers for the output pair and the recorded
//! inputs, the driver callbacks and the realtime path (spec 0.2.2
//! ASIO-031…037).
//!
//! ASIO callbacks carry no context pointer, so one session at a time is
//! active per process (Amendment 01 §6): its realtime state sits in
//! [`ACTIVE`] while the driver runs. Only the driver's callback thread
//! touches it then; the worker thread installs it before `start` and takes
//! it back after `stop`.

use crate::abi::{self, AsioBool, AsioTime, BufferInfo, Callbacks};
use crate::convert::Format;
use crate::driver::{BufferSize, Driver, DriverError, OutputReady};
use jdance_audio_api::{CaptureCallback, InputChannels, RenderCallback, StreamStats};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// What the session plays and records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub sample_rate: u32,
    /// Requested buffer size; `None` = the driver's preferred size.
    pub buffer: Option<u32>,
    pub first_output: u16,
    pub inputs: Option<InputChannels>,
}

/// State shared with the stream handles and the worker.
#[derive(Debug, Default)]
pub struct Shared {
    pub stats: Arc<StreamStats>,
    /// Driver callbacks so far (watchdog).
    pub callbacks: AtomicU64,
    /// Sample position of the last buffer, when the driver reports it.
    pub position: AtomicU64,
    /// Inside a callback: the worker waits for it before freeing state.
    pub in_callback: AtomicBool,
    /// Set when the session failed; the stream reports it.
    pub failure: Mutex<Option<String>>,
    /// Increased each time the driver is reopened (reset request, sample
    /// rate change); an input opened earlier reports itself stopped.
    pub generation: AtomicU64,
}

impl Shared {
    pub fn fail(&self, message: String) {
        if let Ok(mut f) = self.failure.lock() {
            f.get_or_insert(message);
        }
    }

    pub fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|f| f.clone())
    }
}

/// Messages from the driver, read by the worker. Static because the
/// callbacks have no context.
#[derive(Debug)]
pub struct Signals {
    pub reset: AtomicBool,
    pub latencies_changed: AtomicBool,
    pub rate_changed: AtomicBool,
    pub overloads: AtomicU64,
}

pub static SIGNALS: Signals = Signals {
    reset: AtomicBool::new(false),
    latencies_changed: AtomicBool::new(false),
    rate_changed: AtomicBool::new(false),
    overloads: AtomicU64::new(0),
};

impl Signals {
    fn clear(&self) {
        self.reset.store(false, Ordering::Relaxed);
        self.latencies_changed.store(false, Ordering::Relaxed);
        self.rate_changed.store(false, Ordering::Relaxed);
    }
}

/// Realtime state of the running session.
struct Rt {
    frames: usize,
    /// Every opened input channel (up to `MAX_LIVE_INPUTS`): format and
    /// the two halves.
    inputs: Vec<(Format, [*mut c_void; 2])>,
    /// Indices into `inputs` of the recorded left and right channels.
    selection: (usize, usize),
    /// One input channel, then all of them interleaved (track inputs).
    chan: Vec<f32>,
    all: Vec<f32>,
    outputs: [(Format, [*mut c_void; 2]); 2],
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    render: Box<dyn RenderCallback>,
    capture: Option<Box<dyn CaptureCallback>>,
    output_ready: Option<OutputReady>,
    shared: Arc<Shared>,
    panicked: bool,
}

// SAFETY: the raw buffer pointers belong to the driver, valid from
// `createBuffers` to `disposeBuffers`; the session disposes them only after
// taking this state back from the callback thread.
unsafe impl Send for Rt {}

static ACTIVE: AtomicPtr<Rt> = AtomicPtr::new(null_mut());

static CALLBACKS: Callbacks = Callbacks {
    buffer_switch,
    sample_rate_did_change,
    asio_message,
    buffer_switch_time_info,
};

impl Rt {
    /// One buffer half: inputs → capture, render → outputs. Realtime-safe.
    fn run(&mut self, index: i32) {
        let half = usize::from(index & 1 == 1);
        let n = self.frames;
        let (l, r) = (&mut self.in_l[..n], &mut self.in_r[..n]);
        let n_in = self.inputs.len();
        let chan = &mut self.chan[..n];
        for (k, (fmt, bufs)) in self.inputs.iter().enumerate() {
            // SAFETY: the driver's buffer holds `frames` samples of this
            // channel's format while the session runs.
            let src =
                unsafe { std::slice::from_raw_parts(bufs[half] as *const u8, n * fmt.bytes()) };
            fmt.read(src, chan);
            for (i, v) in chan.iter().enumerate() {
                self.all[i * n_in + k] = *v;
            }
            if k == self.selection.0 {
                l.copy_from_slice(chan);
            }
            if k == self.selection.1 {
                r.copy_from_slice(chan);
            }
        }
        if let Some(c) = &mut self.capture
            && n_in > 0
        {
            c.capture_all(&self.all[..n * n_in], n_in);
            c.capture(l, r);
        }
        let (ol, or) = (&mut self.out_l[..n], &mut self.out_r[..n]);
        ol.fill(0.0);
        or.fill(0.0);
        if !self.panicked {
            let render = &mut self.render;
            if catch_unwind(AssertUnwindSafe(|| render.render(ol, or)))
                .map_err(std::mem::forget)
                .is_err()
            {
                // ADR-005: silence until the stream is reopened.
                self.panicked = true;
                self.shared.stats.note_panic();
                ol.fill(0.0);
                or.fill(0.0);
            }
        }
        for ((fmt, bufs), src) in self.outputs.iter().zip([&*ol, &*or]) {
            // SAFETY: as for the inputs.
            let dst =
                unsafe { std::slice::from_raw_parts_mut(bufs[half] as *mut u8, n * fmt.bytes()) };
            fmt.write(src, dst);
        }
        self.shared.stats.note_callback(n as u32);
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        if let Some(o) = self.output_ready {
            // SAFETY: see `OutputReady`.
            unsafe { (o.call)(o.this) };
        }
    }
}

fn process(index: i32, position: Option<u64>) {
    let p = ACTIVE.load(Ordering::Acquire);
    if p.is_null() {
        return;
    }
    // SAFETY: `p` stays valid until the worker swaps it out and sees
    // `in_callback` false; only the driver's callback thread runs here.
    let rt = unsafe { &mut *p };
    rt.shared.in_callback.store(true, Ordering::Release);
    if let Some(pos) = position {
        rt.shared.position.store(pos, Ordering::Relaxed);
    }
    rt.run(index);
    rt.shared.in_callback.store(false, Ordering::Release);
}

extern "C" fn buffer_switch(index: i32, _direct: AsioBool) {
    // No panic may cross into the driver (ADR-005).
    let _ = catch_unwind(|| process(index, None)).map_err(std::mem::forget);
}

extern "C" fn buffer_switch_time_info(
    params: *mut AsioTime,
    index: i32,
    _direct: AsioBool,
) -> *mut AsioTime {
    let _ = catch_unwind(|| {
        // SAFETY: the driver passes a valid `ASIOTime` or null.
        let position = unsafe { params.as_ref() }
            .filter(|t| t.time_info.flags & abi::SAMPLE_POSITION_VALID != 0)
            .map(|t| t.time_info.sample_position.get());
        process(index, position);
    })
    .map_err(std::mem::forget);
    params
}

extern "C" fn sample_rate_did_change(_rate: f64) {
    SIGNALS.rate_changed.store(true, Ordering::Relaxed);
}

extern "C" fn asio_message(
    selector: i32,
    value: i32,
    _message: *mut c_void,
    _opt: *mut f64,
) -> i32 {
    match selector {
        abi::SELECTOR_SUPPORTED => i32::from(matches!(
            value,
            abi::ENGINE_VERSION
                | abi::RESET_REQUEST
                | abi::RESYNC_REQUEST
                | abi::LATENCIES_CHANGED
                | abi::SUPPORTS_TIME_INFO
                | abi::OVERLOAD
        )),
        abi::ENGINE_VERSION => 2,
        // Handled outside the callback by reopening the driver (ASIO-007).
        abi::RESET_REQUEST => {
            SIGNALS.reset.store(true, Ordering::Relaxed);
            1
        }
        // The SDK asks hosts to treat this as a reset request.
        abi::BUFFER_SIZE_CHANGE => 0,
        abi::RESYNC_REQUEST => 1,
        abi::LATENCIES_CHANGED => {
            SIGNALS.latencies_changed.store(true, Ordering::Relaxed);
            1
        }
        abi::SUPPORTS_TIME_INFO => 1,
        abi::SUPPORTS_TIME_CODE => 0,
        abi::OVERLOAD => {
            SIGNALS.overloads.fetch_add(1, Ordering::Relaxed);
            1
        }
        _ => 0,
    }
}

/// What the open session reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    pub sample_rate: u32,
    pub frames: u32,
    pub format: Format,
    /// Input and output latency in frames.
    pub latency: (u32, u32),
}

/// The session on the worker thread.
pub struct Session {
    pub driver: Box<dyn Driver>,
    pub config: Config,
    pub shared: Arc<Shared>,
    pub opened: Opened,
    /// Held while the driver is stopped; `None` while it runs.
    rt: Option<Box<Rt>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("driver", &self.driver.name())
            .field("config", &self.config)
            .field("opened", &self.opened)
            .finish_non_exhaustive()
    }
}

/// The callbacks of a session, kept across reopening.
pub struct Hooks {
    pub render: Box<dyn RenderCallback>,
    pub capture: Option<Box<dyn CaptureCallback>>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field("capture", &self.capture.is_some())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Configures the driver, creates the buffers and starts it.
    pub fn open(
        mut driver: Box<dyn Driver>,
        config: Config,
        callbacks: Hooks,
        shared: Arc<Shared>,
    ) -> Result<Self, (Box<dyn Driver>, Hooks, DriverError)> {
        match prepare(driver.as_mut(), &config, callbacks, &shared) {
            Ok((rt, opened)) => {
                let mut s = Self {
                    driver,
                    config,
                    shared,
                    opened,
                    rt: Some(rt),
                };
                match s.start() {
                    Ok(()) => Ok(s),
                    Err(e) => {
                        let cb = s.dispose();
                        Err((s.driver, cb, e))
                    }
                }
            }
            Err((cb, e)) => Err((driver, cb, e)),
        }
    }

    fn start(&mut self) -> Result<(), DriverError> {
        let rt = self.rt.take().expect("stopped session holds its state");
        SIGNALS.clear();
        ACTIVE.store(Box::into_raw(rt), Ordering::Release);
        if let Err(e) = self.driver.start() {
            self.take_back();
            return Err(e);
        }
        Ok(())
    }

    /// Takes the realtime state back after the driver stopped.
    fn take_back(&mut self) {
        let p = ACTIVE.swap(null_mut(), Ordering::AcqRel);
        // A callback that started before the swap finishes first.
        let mut spins = 0u32;
        while self.shared.in_callback.load(Ordering::Acquire) && spins < 10_000 {
            std::thread::sleep(std::time::Duration::from_micros(100));
            spins += 1;
        }
        if !p.is_null() {
            // SAFETY: created by `Box::into_raw` in `start`; no callback
            // uses it any more.
            self.rt = Some(unsafe { Box::from_raw(p) });
        }
    }

    /// Stops the driver and disposes its buffers; returns the callbacks.
    pub fn dispose(&mut self) -> Hooks {
        let _ = self.driver.stop();
        self.take_back();
        let _ = self.driver.dispose_buffers();
        let rt = self.rt.take().expect("state taken back");
        Hooks {
            render: rt.render,
            capture: rt.capture,
        }
    }

    /// Reopens with `config` (new inputs, a reset request, a new rate).
    pub fn reconfigure(&mut self, config: Config, callbacks: Hooks) -> Result<(), DriverError> {
        let (rt, opened) =
            prepare(self.driver.as_mut(), &config, callbacks, &self.shared).map_err(|(_, e)| e)?;
        self.rt = Some(rt);
        self.config = config;
        self.opened = opened;
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
        self.start()
    }

    pub fn latencies(&mut self) -> (u32, u32) {
        self.driver
            .latencies()
            .map(|(i, o)| (i.max(0) as u32, o.max(0) as u32))
            .unwrap_or(self.opened.latency)
    }
}

/// Rate, buffer size, formats and buffers for `config`.
#[allow(clippy::type_complexity)]
fn prepare(
    driver: &mut dyn Driver,
    config: &Config,
    callbacks: Hooks,
    shared: &Arc<Shared>,
) -> Result<(Box<Rt>, Opened), (Hooks, DriverError)> {
    let fail = |cb, e: DriverError| Err((cb, e));
    let (ins, outs) = match driver.channels() {
        Ok(c) => c,
        Err(e) => return fail(callbacks, e),
    };
    let first = i32::from(config.first_output);
    if first + 2 > outs {
        return fail(
            callbacks,
            DriverError(format!(
                "output pair {}-{} exceeds the driver's {outs} outputs",
                first + 1,
                first + 2
            )),
        );
    }
    // Every input (up to MAX_LIVE_INPUTS) is opened: the recording uses
    // one or two of them, mixer track inputs any (spec 0.2.3 INP-001).
    let selection = match config.inputs {
        None => None,
        Some(InputChannels::Mono(c)) => Some((i32::from(c), i32::from(c))),
        Some(InputChannels::Stereo(c)) => Some((i32::from(c), i32::from(c) + 1)),
    };
    let live = ins.clamp(0, jdance_audio_api::MAX_LIVE_INPUTS as i32);
    if selection.is_some_and(|(a, b)| a.max(b) >= live) {
        return fail(
            callbacks,
            DriverError(format!("the driver has {ins} inputs")),
        );
    }
    let input_channels: Vec<i32> = if selection.is_some() {
        (0..live).collect()
    } else {
        Vec::new()
    };
    let rate = f64::from(config.sample_rate);
    let current = driver.sample_rate().unwrap_or(0.0);
    if (current - rate).abs() > 0.5 {
        if !driver.can_sample_rate(rate) {
            return fail(
                callbacks,
                DriverError(format!(
                    "the driver does not run at {} Hz",
                    config.sample_rate
                )),
            );
        }
        if let Err(e) = driver.set_sample_rate(rate) {
            return fail(callbacks, e);
        }
    }
    let sizes: BufferSize = match driver.buffer_size() {
        Ok(b) => b,
        Err(e) => return fail(callbacks, e),
    };
    let frames = sizes.choose(config.buffer);
    let mut formats = Vec::new();
    for (ch, input) in input_channels
        .iter()
        .map(|c| (*c, true))
        .chain([(first, false), (first + 1, false)])
    {
        let info = match driver.channel_info(ch, input) {
            Ok(i) => i,
            Err(e) => return fail(callbacks, e),
        };
        match Format::of(info.sample_type) {
            Some(f) => formats.push(f),
            None => {
                return fail(
                    callbacks,
                    DriverError(format!(
                        "channel {} uses sample type {}, which is not supported",
                        info.name(),
                        info.sample_type
                    )),
                );
            }
        }
    }
    let mut infos: Vec<BufferInfo> = input_channels
        .iter()
        .map(|c| (*c, true))
        .chain([(first, false), (first + 1, false)])
        .map(|(c, input)| BufferInfo {
            is_input: AsioBool::from(input),
            channel_num: c,
            buffers: [null_mut(); 2],
        })
        .collect();
    if let Err(e) = driver.create_buffers(&mut infos, frames as i32, &CALLBACKS) {
        return fail(callbacks, e);
    }
    let latency = driver
        .latencies()
        .map(|(i, o)| (i.max(0) as u32, o.max(0) as u32))
        .unwrap_or((0, 0));
    let n_in = input_channels.len();
    let inputs = (0..n_in).map(|k| (formats[k], infos[k].buffers)).collect();
    let outputs = [
        (formats[n_in], infos[n_in].buffers),
        (formats[n_in + 1], infos[n_in + 1].buffers),
    ];
    let n = frames as usize;
    let (sl, sr) = selection.unwrap_or((0, 0));
    let rt = Box::new(Rt {
        frames: n,
        inputs,
        selection: (sl as usize, sr as usize),
        chan: vec![0.0; n],
        all: vec![0.0; n * n_in],
        outputs,
        in_l: vec![0.0; n],
        in_r: vec![0.0; n],
        out_l: vec![0.0; n],
        out_r: vec![0.0; n],
        render: callbacks.render,
        capture: callbacks.capture,
        output_ready: driver.output_ready(),
        shared: Arc::clone(shared),
        panicked: false,
    });
    let opened = Opened {
        sample_rate: config.sample_rate,
        frames,
        format: formats[n_in],
        latency,
    };
    Ok((rt, opened))
}
