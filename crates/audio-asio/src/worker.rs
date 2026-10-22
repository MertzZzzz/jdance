// SPDX-License-Identifier: GPL-3.0-or-later
//! The adapter's worker thread: owns the loaded driver and the session.
//! Every driver call except `outputReady` happens here (ASIO-004, COM
//! STA with a message loop on Windows); callers wait at most
//! [`REPLY_TIMEOUT`] (ASIO-009). Between commands it watches the driver:
//! reset requests reopen it (ASIO-035), a driver that stops calling back
//! fails the stream (ASIO-036).

use crate::driver::{Driver, DriverError};
use crate::session::{Config, Hooks, Opened, SIGNALS, Session, Shared};
use jdance_audio_api::{
    AudioError, CaptureCallback, ChannelNames, DeviceId, InputChannels, RenderCallback,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(10);

/// Loads and initializes a driver by device ID (runs on the worker).
pub type Loader = dyn Fn(&DeviceId) -> Result<Box<dyn Driver>, DriverError> + Send + Sync;

/// What a driver offers, read without starting it.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub inputs: u16,
    pub outputs: u16,
    pub rates: Vec<u32>,
    pub current_rate: Option<u32>,
    pub buffer: crate::driver::BufferSize,
    pub format: Option<crate::convert::Format>,
    pub names: ChannelNames,
}

type Reply<T> = Sender<Result<T, AudioError>>;

pub enum Cmd {
    Probe(DeviceId, Reply<Probe>),
    Open {
        device: DeviceId,
        config: Config,
        render: Box<dyn RenderCallback>,
        shared: Arc<Shared>,
        reply: Reply<Opened>,
    },
    SetInput {
        device: DeviceId,
        channels: InputChannels,
        capture: Box<dyn CaptureCallback>,
        reply: Reply<(Opened, u64, Arc<Shared>)>,
    },
    ClearInput(Reply<()>),
    ControlPanel(DeviceId, Reply<()>),
    Close(Reply<()>),
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Cmd::Probe(..) => "Probe",
            Cmd::Open { .. } => "Open",
            Cmd::SetInput { .. } => "SetInput",
            Cmd::ClearInput(_) => "ClearInput",
            Cmd::ControlPanel(..) => "ControlPanel",
            Cmd::Close(_) => "Close",
        })
    }
}

/// Sends `make(reply)` to the worker and waits for the answer.
pub fn ask<T>(tx: &Sender<Cmd>, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T, AudioError> {
    let (reply, rx) = mpsc::channel();
    tx.send(make(reply))
        .map_err(|_| AudioError::Backend("the ASIO worker has stopped".into()))?;
    match rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(r) => r,
        Err(RecvTimeoutError::Timeout) => Err(AudioError::Backend(
            "the ASIO driver did not answer within 5 s".into(),
        )),
        Err(RecvTimeoutError::Disconnected) => {
            Err(AudioError::Backend("the ASIO worker has stopped".into()))
        }
    }
}

fn backend_err(e: DriverError) -> AudioError {
    AudioError::Backend(e.0)
}

const RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

fn probe(driver: &mut dyn Driver) -> Result<Probe, DriverError> {
    let (ins, outs) = driver.channels()?;
    let rates = RATES
        .into_iter()
        .filter(|r| driver.can_sample_rate(f64::from(*r)))
        .collect();
    let current_rate = driver.sample_rate().ok().map(|r| r.round() as u32);
    let buffer = driver.buffer_size()?;
    let format = driver
        .channel_info(0, false)
        .ok()
        .and_then(|i| crate::convert::Format::of(i.sample_type));
    let name = |d: &mut dyn Driver, c: i32, input: bool| {
        d.channel_info(c, input)
            .map(|i| i.name())
            .unwrap_or_default()
    };
    let names = ChannelNames {
        inputs: (0..ins).map(|c| name(driver, c, true)).collect(),
        outputs: (0..outs).map(|c| name(driver, c, false)).collect(),
    };
    Ok(Probe {
        inputs: ins.max(0) as u16,
        outputs: outs.max(0) as u16,
        rates,
        current_rate,
        buffer,
        format,
        names,
    })
}

struct State {
    loader: Arc<Loader>,
    /// The running session and the device it uses.
    session: Option<(DeviceId, Session)>,
    /// Watchdog: callbacks seen and since when they did not advance.
    seen: (u64, Instant),
}

pub fn spawn(loader: Arc<Loader>) -> Sender<Cmd> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("jdance-asio".into())
        .spawn(move || run(loader, rx));
    tx
}

fn run(loader: Arc<Loader>, rx: Receiver<Cmd>) {
    crate::platform::init_thread();
    let mut st = State {
        loader,
        session: None,
        seen: (0, Instant::now()),
    };
    loop {
        crate::platform::pump_messages();
        match rx.recv_timeout(TICK) {
            Ok(cmd) => st.handle(cmd),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        st.watch();
    }
    if let Some((_, mut s)) = st.session.take() {
        let _ = s.dispose();
    }
    crate::platform::exit_thread();
}

impl State {
    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Probe(device, reply) => {
                let r = match &mut self.session {
                    Some((d, s)) if *d == device => probe(s.driver.as_mut()),
                    // Many drivers load once per process: probing another
                    // one while a session runs may fail; that is reported.
                    _ => (self.loader)(&device).and_then(|mut d| probe(d.as_mut())),
                };
                let _ = reply.send(r.map_err(backend_err));
            }
            Cmd::Open {
                device,
                config,
                render,
                shared,
                reply,
            } => {
                if let Some((_, mut s)) = self.session.take() {
                    let _ = s.dispose();
                }
                let r = (self.loader)(&device).and_then(|driver| {
                    Session::open(
                        driver,
                        config,
                        Hooks {
                            render,
                            capture: None,
                        },
                        shared,
                    )
                    .map_err(|(_, _, e)| e)
                });
                let _ = reply.send(match r {
                    Ok(s) => {
                        let opened = s.opened.clone();
                        self.session = Some((device, s));
                        self.seen = (0, Instant::now());
                        Ok(opened)
                    }
                    Err(e) => Err(backend_err(e)),
                });
            }
            Cmd::SetInput {
                device,
                channels,
                capture,
                reply,
            } => {
                let r = match &mut self.session {
                    Some((d, s)) if *d == device => {
                        let mut hooks = s.dispose();
                        hooks.capture = Some(capture);
                        let config = Config {
                            inputs: Some(channels),
                            ..s.config
                        };
                        match s.reconfigure(config, hooks) {
                            Ok(()) => Ok((
                                s.opened.clone(),
                                s.shared.generation.load(Ordering::Relaxed),
                                Arc::clone(&s.shared),
                            )),
                            Err(e) => {
                                s.shared.fail(e.0.clone());
                                Err(backend_err(e))
                            }
                        }
                    }
                    // ASIO-040: inputs come from the driver that plays.
                    _ => Err(AudioError::Unsupported(
                        "with ASIO the input comes from the active output driver".into(),
                    )),
                };
                let _ = reply.send(r);
            }
            Cmd::ClearInput(reply) => {
                if let Some((_, s)) = &mut self.session
                    && s.config.inputs.is_some()
                {
                    let mut hooks = s.dispose();
                    hooks.capture = None;
                    let config = Config {
                        inputs: None,
                        ..s.config
                    };
                    if let Err(e) = s.reconfigure(config, hooks) {
                        s.shared.fail(e.0);
                    }
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::ControlPanel(device, reply) => {
                let r = match &mut self.session {
                    Some((d, s)) if *d == device => s.driver.control_panel(),
                    _ => (self.loader)(&device).and_then(|mut d| d.control_panel()),
                };
                let _ = reply.send(r.map_err(backend_err));
            }
            Cmd::Close(reply) => {
                if let Some((_, mut s)) = self.session.take() {
                    let _ = s.dispose();
                }
                let _ = reply.send(Ok(()));
            }
        }
    }

    /// Reset requests, rate changes, latency updates and the watchdog.
    fn watch(&mut self) {
        let Some((_, s)) = &mut self.session else {
            return;
        };
        if s.shared.failure().is_some() {
            return;
        }
        let reset = SIGNALS.reset.swap(false, Ordering::Relaxed);
        let rate = SIGNALS.rate_changed.swap(false, Ordering::Relaxed);
        if reset || rate {
            // ASIO-035: reopen with the same configuration; the driver may
            // have a new buffer size or rate (from its control panel).
            let mut hooks = s.dispose();
            let mut config = s.config;
            if rate && let Ok(r) = s.driver.sample_rate() {
                config.sample_rate = r.round() as u32;
            }
            // An input opened before stops: the take ends (ASIO-035).
            hooks.capture = None;
            config.inputs = None;
            if let Err(e) = s.reconfigure(config, hooks) {
                s.shared
                    .fail(format!("the driver could not be reopened: {}", e.0));
            }
            self.seen = (0, Instant::now());
            return;
        }
        if SIGNALS.latencies_changed.swap(false, Ordering::Relaxed) {
            s.opened.latency = s.latencies();
        }
        // ASIO-036: no callback for max(1 s, 20 buffers).
        let count = s.shared.callbacks.load(Ordering::Relaxed);
        let now = Instant::now();
        if count != self.seen.0 {
            self.seen = (count, now);
            return;
        }
        let buffer = f64::from(s.opened.frames) / f64::from(s.opened.sample_rate.max(1));
        let limit = Duration::from_secs_f64((20.0 * buffer).max(1.0));
        if now.duration_since(self.seen.1) > limit {
            s.shared
                .fail("the ASIO driver stopped calling back (device disconnected?)".into());
        }
    }
}
