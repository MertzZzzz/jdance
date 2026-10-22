// SPDX-License-Identifier: GPL-3.0-or-later
//! ASIO host adapter (ADR-006, spec 0.2.2 Part 3): third-party ASIO
//! drivers behind the platform-neutral `audio-api`.
//!
//! The session logic (formats, buffers, callbacks, reset and watchdog)
//! does not depend on COM and is tested on every OS with a driver written
//! in Rust ([`fake`]). The COM driver and the registry list are Windows
//! only ([`com`]); [`AsioBackend::system`] exists only there.
#![allow(unsafe_code)]

pub mod abi;
pub mod convert;
pub mod driver;
pub mod session;
pub mod worker;

#[cfg(windows)]
pub mod com;
#[cfg(any(test, feature = "test-driver"))]
pub mod fake;

use jdance_audio_api::{
    ActiveInputInfo, ActiveStreamInfo, AudioBackend, AudioError, BackendId, BufferRequest,
    BufferSizeCaps, CaptureCallback, ChannelNames, DeviceCapabilities, DeviceId, DeviceInfo,
    InputRequest, InputStream, OutputStream, RenderCallback, SampleRates, StreamRequest,
    StreamState, StreamStats,
};
use session::{Config, Shared};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use worker::{Cmd, Loader, Probe, ask};

pub const BACKEND_ID: &str = "asio";

/// Lists the installed drivers: ID, name and why it is unavailable.
pub type Lister = dyn Fn() -> Vec<(DeviceId, String, Option<String>)> + Send + Sync;

/// The ASIO backend: one worker thread, at most one running driver.
pub struct AsioBackend {
    tx: Mutex<Sender<Cmd>>,
    list: Arc<Lister>,
    /// The device of the running output, for inputs and names.
    active: Arc<Mutex<Option<DeviceId>>>,
}

impl std::fmt::Debug for AsioBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsioBackend").finish_non_exhaustive()
    }
}

impl AsioBackend {
    /// A backend over any driver source (the system's or a test's).
    pub fn new(list: Arc<Lister>, loader: Arc<Loader>) -> Self {
        Self {
            tx: Mutex::new(worker::spawn(loader)),
            list,
            active: Arc::new(Mutex::new(None)),
        }
    }

    /// The drivers registered with Windows (`HKLM\SOFTWARE\ASIO`).
    #[cfg(windows)]
    pub fn system() -> Self {
        Self::new(Arc::new(com::list_drivers), Arc::new(com::load_driver))
    }

    fn tx(&self) -> Result<Sender<Cmd>, AudioError> {
        self.tx
            .lock()
            .map(|t| t.clone())
            .map_err(|_| AudioError::Backend("the ASIO worker is unavailable".into()))
    }

    fn probe(&self, device: &DeviceId) -> Result<Probe, AudioError> {
        let tx = self.tx()?;
        ask(&tx, |r| Cmd::Probe(device.clone(), r))
    }
}

fn name_of(list: &Lister, device: &DeviceId) -> String {
    list()
        .into_iter()
        .find(|(id, ..)| id == device)
        .map_or_else(|| device.0.clone(), |(_, n, _)| n)
}

impl AudioBackend for AsioBackend {
    fn id(&self) -> BackendId {
        BackendId(BACKEND_ID.into())
    }

    fn display_name(&self) -> String {
        "ASIO".into()
    }

    /// ASIO-020: unavailable drivers stay listed with the reason.
    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        Ok((self.list)()
            .into_iter()
            .map(|(id, name, problem)| DeviceInfo {
                id,
                name: match problem {
                    Some(p) => format!("{name} (unavailable: {p})"),
                    None => name,
                },
                is_default: false,
                stable_id: true,
            })
            .collect())
    }

    fn capabilities(&self, device: &DeviceId) -> Result<DeviceCapabilities, AudioError> {
        let p = self.probe(device)?;
        let sizes = p.buffer.allowed();
        let buffer_size = if sizes.len() > 1 {
            BufferSizeCaps::Discrete(sizes)
        } else {
            BufferSizeCaps::FixedByDriver(sizes[0])
        };
        let changeable = matches!(buffer_size, BufferSizeCaps::Discrete(_));
        Ok(DeviceCapabilities {
            can_change_sample_rate_from_app: p.rates.len() > 1,
            sample_rates: SampleRates::Discrete(p.rates),
            default_sample_rate: p.current_rate,
            buffer_size,
            output_channels: p.outputs,
            input_channels: p.inputs,
            sample_formats: p.format.map(convert::Format::api).into_iter().collect(),
            reports_latency: true,
            has_control_panel: true,
            can_change_buffer_from_app: changeable,
        })
    }

    fn open_output(
        &self,
        device: &DeviceId,
        request: &StreamRequest,
        callback: Box<dyn RenderCallback>,
    ) -> Result<Box<dyn OutputStream>, AudioError> {
        let tx = self.tx()?;
        let shared = Arc::new(Shared::default());
        let config = Config {
            sample_rate: request.sample_rate,
            buffer: match request.buffer {
                BufferRequest::Default => None,
                BufferRequest::Frames(f) => Some(f),
            },
            first_output: request.first_output_channel,
            inputs: None,
        };
        let opened = ask(&tx, |reply| Cmd::Open {
            device: device.clone(),
            config,
            render: callback,
            shared: Arc::clone(&shared),
            reply,
        })?;
        if let Ok(mut a) = self.active.lock() {
            *a = Some(device.clone());
        }
        Ok(Box::new(AsioOutput {
            info: ActiveStreamInfo {
                backend: self.id(),
                device: device.clone(),
                device_name: name_of(self.list.as_ref(), device),
                sample_rate: opened.sample_rate,
                channels: 2,
                sample_format: opened.format.api(),
                requested_buffer: request.buffer,
                first_output_channel: request.first_output_channel,
                latency: Some(opened.latency),
            },
            shared,
            tx,
            active: Arc::clone(&self.active),
        }))
    }

    /// ASIO-040: with ASIO the inputs come from the playing driver.
    fn input_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let active = self.active.lock().ok().and_then(|a| a.clone());
        Ok(self
            .devices()?
            .into_iter()
            .filter(|d| Some(&d.id) == active.as_ref())
            .collect())
    }

    fn input_channels(&self, device: &DeviceId) -> Result<u16, AudioError> {
        Ok(self.probe(device)?.inputs)
    }

    fn open_input(
        &self,
        device: &DeviceId,
        request: &InputRequest,
        callback: Box<dyn CaptureCallback>,
    ) -> Result<Box<dyn InputStream>, AudioError> {
        let tx = self.tx()?;
        let (opened, generation, shared) = ask(&tx, |reply| Cmd::SetInput {
            device: device.clone(),
            channels: request.channels,
            capture: callback,
            reply,
        })?;
        let probe = self.probe(device).ok();
        Ok(Box::new(AsioInput {
            info: ActiveInputInfo {
                device: device.clone(),
                device_name: name_of(self.list.as_ref(), device),
                sample_rate: opened.sample_rate,
                device_channels: probe.map_or(0, |p| p.inputs),
                channels: request.channels,
                round_trip: Some(opened.latency.0 + opened.latency.1),
            },
            generation,
            shared,
            tx,
        }))
    }

    fn channel_names(&self, device: &DeviceId) -> Result<ChannelNames, AudioError> {
        Ok(self.probe(device)?.names)
    }

    fn open_control_panel(&self, device: &DeviceId) -> Result<(), AudioError> {
        let tx = self.tx()?;
        ask(&tx, |r| Cmd::ControlPanel(device.clone(), r))
    }
}

struct AsioOutput {
    info: ActiveStreamInfo,
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    active: Arc<Mutex<Option<DeviceId>>>,
}

impl OutputStream for AsioOutput {
    fn info(&self) -> &ActiveStreamInfo {
        &self.info
    }

    fn stats(&self) -> Arc<StreamStats> {
        Arc::clone(&self.shared.stats)
    }

    fn state(&self) -> StreamState {
        match self.shared.failure() {
            Some(e) => StreamState::Failed(e),
            None => StreamState::Running,
        }
    }
}

impl Drop for AsioOutput {
    fn drop(&mut self) {
        let _ = ask(&self.tx, Cmd::Close);
        if let Ok(mut a) = self.active.lock() {
            *a = None;
        }
    }
}

struct AsioInput {
    info: ActiveInputInfo,
    /// Session generation when opened: a reopened driver (reset request)
    /// dropped this input, and the take stops (ASIO-035).
    generation: u64,
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
}

impl InputStream for AsioInput {
    fn info(&self) -> &ActiveInputInfo {
        &self.info
    }

    fn state(&self) -> StreamState {
        if let Some(e) = self.shared.failure() {
            StreamState::Failed(e)
        } else if self.shared.generation.load(Ordering::Relaxed) != self.generation {
            StreamState::Failed("the ASIO driver was reset; choose the input again".into())
        } else {
            StreamState::Running
        }
    }
}

impl Drop for AsioInput {
    fn drop(&mut self) {
        let _ = ask(&self.tx, Cmd::ClearInput);
    }
}

/// Windows: COM apartment and message pump of the worker thread (driver
/// panels are windows). Elsewhere nothing.
mod platform {
    #[cfg(windows)]
    pub fn init_thread() {
        use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        // SAFETY: called once at the start of the worker thread.
        unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    }

    #[cfg(windows)]
    pub fn exit_thread() {
        // SAFETY: pairs with `init_thread` on the same thread.
        unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
    }

    #[cfg(windows)]
    pub fn pump_messages() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };
        // SAFETY: plain message loop on this thread with a local MSG.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    #[cfg(not(windows))]
    pub fn init_thread() {}
    #[cfg(not(windows))]
    pub fn exit_thread() {}
    #[cfg(not(windows))]
    pub fn pump_messages() {}
}
