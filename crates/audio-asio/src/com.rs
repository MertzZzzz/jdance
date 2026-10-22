// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows: installed drivers from `HKLM\SOFTWARE\ASIO` (ASIO-003) and
//! the driver COM object, called through its method table (ADR-006).
//!
//! The method order and signatures follow the SDK's `IASIO` interface. On
//! 64-bit Windows there is one calling convention, so the C ABI matches
//! both the `IUnknown` methods and the driver's own.

use crate::abi::{self, BufferInfo, Callbacks, ChannelInfo, Samples};
use crate::driver::{BufferSize, Driver, DriverError, OutputReady};
use jdance_audio_api::DeviceId;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use windows_sys::Win32::System::Com::{CLSCTX_INPROC_SERVER, CLSIDFromString, CoCreateInstance};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows_sys::core::GUID;

const _: () = assert!(
    size_of::<usize>() == 8,
    "the driver ABI here is the 64-bit one"
);

/// The driver interface's method table.
#[repr(C)]
struct Vtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    init: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
    get_driver_name: unsafe extern "C" fn(*mut c_void, *mut u8),
    get_driver_version: unsafe extern "C" fn(*mut c_void) -> i32,
    get_error_message: unsafe extern "C" fn(*mut c_void, *mut u8),
    start: unsafe extern "C" fn(*mut c_void) -> i32,
    stop: unsafe extern "C" fn(*mut c_void) -> i32,
    get_channels: unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> i32,
    get_latencies: unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> i32,
    get_buffer_size:
        unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32, *mut i32, *mut i32) -> i32,
    can_sample_rate: unsafe extern "C" fn(*mut c_void, f64) -> i32,
    get_sample_rate: unsafe extern "C" fn(*mut c_void, *mut f64) -> i32,
    set_sample_rate: unsafe extern "C" fn(*mut c_void, f64) -> i32,
    get_clock_sources: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut i32) -> i32,
    set_clock_source: unsafe extern "C" fn(*mut c_void, i32) -> i32,
    get_sample_position: unsafe extern "C" fn(*mut c_void, *mut Samples, *mut Samples) -> i32,
    get_channel_info: unsafe extern "C" fn(*mut c_void, *mut ChannelInfo) -> i32,
    create_buffers:
        unsafe extern "C" fn(*mut c_void, *mut BufferInfo, i32, i32, *const Callbacks) -> i32,
    dispose_buffers: unsafe extern "C" fn(*mut c_void) -> i32,
    control_panel: unsafe extern "C" fn(*mut c_void) -> i32,
    future: unsafe extern "C" fn(*mut c_void, i32, *mut c_void) -> i32,
    output_ready: unsafe extern "C" fn(*mut c_void) -> i32,
}

/// A created and initialized driver object.
pub struct ComDriver {
    this: *mut c_void,
    name: String,
}

impl std::fmt::Debug for ComDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComDriver")
            .field("name", &self.name)
            .finish()
    }
}

impl ComDriver {
    fn vtbl(&self) -> &Vtbl {
        // SAFETY: a COM object starts with a pointer to its method table,
        // valid while the object lives (we hold a reference).
        unsafe { &**(self.this as *const *const Vtbl) }
    }

    fn check(&self, what: &str, code: i32) -> Result<(), DriverError> {
        if code == abi::ASE_OK || code == abi::ASE_SUCCESS {
            Ok(())
        } else {
            Err(DriverError::code(what, code))
        }
    }

    fn error_message(&self) -> String {
        let mut buf = [0u8; 128];
        // SAFETY: the SDK limits the message to 124 bytes.
        unsafe { (self.vtbl().get_error_message)(self.this, buf.as_mut_ptr()) };
        c_string(&buf)
    }
}

impl Drop for ComDriver {
    fn drop(&mut self) {
        // SAFETY: releases the reference taken by `CoCreateInstance`.
        unsafe { (self.vtbl().release)(self.this) };
    }
}

fn c_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_owned()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Driver for ComDriver {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn channels(&mut self) -> Result<(i32, i32), DriverError> {
        let (mut i, mut o) = (0, 0);
        // SAFETY: valid out-pointers to locals.
        let r = unsafe { (self.vtbl().get_channels)(self.this, &mut i, &mut o) };
        self.check("getChannels", r).map(|()| (i, o))
    }

    fn latencies(&mut self) -> Result<(i32, i32), DriverError> {
        let (mut i, mut o) = (0, 0);
        // SAFETY: as above.
        let r = unsafe { (self.vtbl().get_latencies)(self.this, &mut i, &mut o) };
        self.check("getLatencies", r).map(|()| (i, o))
    }

    fn buffer_size(&mut self) -> Result<BufferSize, DriverError> {
        let (mut min, mut max, mut preferred, mut granularity) = (0, 0, 0, 0);
        // SAFETY: as above.
        let r = unsafe {
            (self.vtbl().get_buffer_size)(
                self.this,
                &mut min,
                &mut max,
                &mut preferred,
                &mut granularity,
            )
        };
        self.check("getBufferSize", r).map(|()| BufferSize {
            min,
            max,
            preferred,
            granularity,
        })
    }

    fn can_sample_rate(&mut self, rate: f64) -> bool {
        // SAFETY: plain value call.
        unsafe { (self.vtbl().can_sample_rate)(self.this, rate) == abi::ASE_OK }
    }

    fn sample_rate(&mut self) -> Result<f64, DriverError> {
        let mut rate = 0.0;
        // SAFETY: valid out-pointer.
        let r = unsafe { (self.vtbl().get_sample_rate)(self.this, &mut rate) };
        self.check("getSampleRate", r).map(|()| rate)
    }

    fn set_sample_rate(&mut self, rate: f64) -> Result<(), DriverError> {
        // SAFETY: plain value call.
        let r = unsafe { (self.vtbl().set_sample_rate)(self.this, rate) };
        self.check("setSampleRate", r)
    }

    fn channel_info(&mut self, channel: i32, input: bool) -> Result<ChannelInfo, DriverError> {
        let mut info = ChannelInfo::query(channel, input);
        // SAFETY: `info` is a valid ASIOChannelInfo with the query fields set.
        let r = unsafe { (self.vtbl().get_channel_info)(self.this, &mut info) };
        self.check("getChannelInfo", r).map(|()| info)
    }

    fn create_buffers(
        &mut self,
        infos: &mut [BufferInfo],
        size: i32,
        callbacks: &'static Callbacks,
    ) -> Result<(), DriverError> {
        // SAFETY: `infos` is a valid array of `len` entries; the callbacks
        // are static, as the driver keeps the pointer.
        let r = unsafe {
            (self.vtbl().create_buffers)(
                self.this,
                infos.as_mut_ptr(),
                infos.len() as i32,
                size,
                callbacks,
            )
        };
        self.check("createBuffers", r)
    }

    fn dispose_buffers(&mut self) -> Result<(), DriverError> {
        // SAFETY: plain call; the session stopped the driver before.
        let r = unsafe { (self.vtbl().dispose_buffers)(self.this) };
        self.check("disposeBuffers", r)
    }

    fn start(&mut self) -> Result<(), DriverError> {
        // SAFETY: plain call after createBuffers.
        let r = unsafe { (self.vtbl().start)(self.this) };
        self.check("start", r)
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        // SAFETY: plain call.
        let r = unsafe { (self.vtbl().stop)(self.this) };
        self.check("stop", r)
    }

    fn control_panel(&mut self) -> Result<(), DriverError> {
        // SAFETY: plain call on the driver's (STA) thread.
        let r = unsafe { (self.vtbl().control_panel)(self.this) };
        self.check("controlPanel", r)
    }

    fn output_ready(&mut self) -> Option<OutputReady> {
        let call = self.vtbl().output_ready;
        // SAFETY: probing support after createBuffers, as the SDK suggests.
        let supported = unsafe { call(self.this) } == abi::ASE_OK;
        supported.then_some(OutputReady {
            call,
            this: self.this,
        })
    }
}

/// Creates and initializes the driver with CLSID `id` (on the worker).
pub fn load_driver(id: &DeviceId) -> Result<Box<dyn Driver>, DriverError> {
    let mut clsid: GUID = GUID::from_u128(0);
    let w = wide(&id.0);
    // SAFETY: `w` is a zero-terminated UTF-16 string; `clsid` an out value.
    let hr = unsafe { CLSIDFromString(w.as_ptr(), &mut clsid) };
    if hr < 0 {
        return Err(DriverError(format!("invalid driver ID {}", id.0)));
    }
    let mut this: *mut c_void = null_mut();
    // ASIO drivers use their CLSID as the interface ID.
    // SAFETY: valid GUIDs and out-pointer; COM is initialized on this thread.
    let hr =
        unsafe { CoCreateInstance(&clsid, null_mut(), CLSCTX_INPROC_SERVER, &clsid, &mut this) };
    if hr < 0 || this.is_null() {
        return Err(DriverError(format!(
            "the driver cannot be created (0x{:08X}); is it installed for 64-bit programs?",
            hr as u32
        )));
    }
    let mut driver = ComDriver {
        this,
        name: String::new(),
    };
    // SAFETY: `init` takes the system handle (none: drivers own their
    // windows); the name is at most 32 bytes.
    let ok = unsafe { (driver.vtbl().init)(driver.this, null_mut()) };
    if ok == 0 {
        let msg = driver.error_message();
        return Err(DriverError(if msg.is_empty() {
            "the driver could not start (is the device connected or in use?)".into()
        } else {
            msg
        }));
    }
    let mut name = [0u8; 32];
    // SAFETY: 32-byte buffer as the SDK specifies.
    unsafe { (driver.vtbl().get_driver_name)(driver.this, name.as_mut_ptr()) };
    driver.name = c_string(&name);
    Ok(Box::new(driver))
}

/// Registered drivers: (CLSID, name, problem).
pub fn list_drivers() -> Vec<(DeviceId, String, Option<String>)> {
    let mut out = Vec::new();
    let root = wide("SOFTWARE\\ASIO");
    let mut key: HKEY = null_mut();
    // SAFETY: valid strings and out-pointer; the key is closed below.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, root.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return out;
    }
    for index in 0.. {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: buffer and length describe `name`.
        let r = unsafe {
            RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut len,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if r != 0 {
            break;
        }
        let sub = String::from_utf16_lossy(&name[..len as usize]);
        let value = |v: &str| -> Option<String> {
            let mut buf = [0u16; 256];
            let mut size = (buf.len() * 2) as u32;
            let (sub_w, v_w) = (wide(&sub), wide(v));
            // SAFETY: buffer and size in bytes describe `buf`.
            let r = unsafe {
                RegGetValueW(
                    key,
                    sub_w.as_ptr(),
                    v_w.as_ptr(),
                    RRF_RT_REG_SZ,
                    null_mut(),
                    buf.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            (r == 0).then(|| {
                let n = (size as usize / 2).saturating_sub(1);
                String::from_utf16_lossy(&buf[..n.min(buf.len())])
            })
        };
        match value("CLSID") {
            Some(clsid) => {
                let label = value("Description")
                    .filter(|d| !d.is_empty())
                    .unwrap_or(sub);
                out.push((DeviceId(clsid), label, None));
            }
            None => out.push((
                DeviceId(format!("registry:{sub}")),
                sub,
                Some("no CLSID in the registry".into()),
            )),
        }
    }
    // SAFETY: opened above.
    unsafe { RegCloseKey(key) };
    out
}
