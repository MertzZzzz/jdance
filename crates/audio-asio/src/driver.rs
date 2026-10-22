// SPDX-License-Identifier: GPL-3.0-or-later
//! One loaded ASIO driver, as the session sees it. The Windows COM driver
//! ([`crate::com`]) and the test driver ([`crate::fake`]) implement it.

use crate::abi::{BufferInfo, Callbacks, ChannelInfo};
use std::ffi::c_void;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverError(pub String);

impl DriverError {
    pub fn code(what: &str, code: i32) -> Self {
        Self(format!("{what}: {} ({code})", crate::abi::error_text(code)))
    }
}

/// `getBufferSize`: `granularity` −1 means powers of two from `min` to
/// `max`; 0 means only `preferred`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferSize {
    pub min: i32,
    pub max: i32,
    pub preferred: i32,
    pub granularity: i32,
}

impl BufferSize {
    /// Sizes the app may ask for (at most 64 entries).
    pub fn allowed(&self) -> Vec<u32> {
        let (min, max) = (self.min.max(1), self.max.max(self.min.max(1)));
        let mut v = Vec::new();
        if self.granularity == -1 {
            let mut f = 1i32;
            while f <= max && v.len() < 64 {
                if f >= min {
                    v.push(f as u32);
                }
                f = f.saturating_mul(2);
            }
        } else if self.granularity > 0 {
            let mut f = min;
            while f <= max && v.len() < 64 {
                v.push(f as u32);
                f += self.granularity;
            }
        }
        if v.is_empty() {
            v.push(self.preferred.max(1) as u32);
        }
        v
    }

    /// The size to use: the request when the driver allows it, else the
    /// driver's preferred size.
    pub fn choose(&self, request: Option<u32>) -> u32 {
        match request {
            Some(f) if self.allowed().contains(&f) => f,
            _ => self.preferred.max(1) as u32,
        }
    }
}

/// `outputReady`, callable from the driver's callback thread.
#[derive(Debug, Clone, Copy)]
pub struct OutputReady {
    pub call: unsafe extern "C" fn(this: *mut c_void) -> i32,
    pub this: *mut c_void,
}

// SAFETY: the driver object outlives the running session (the session
// stops the driver before releasing it), and ASIO allows `outputReady`
// from the callback thread.
unsafe impl Send for OutputReady {}

/// A loaded and initialized driver. Lives on the adapter's worker thread;
/// only [`OutputReady`] is used from the callback thread.
pub trait Driver {
    fn name(&self) -> String;
    fn channels(&mut self) -> Result<(i32, i32), DriverError>;
    fn latencies(&mut self) -> Result<(i32, i32), DriverError>;
    fn buffer_size(&mut self) -> Result<BufferSize, DriverError>;
    fn can_sample_rate(&mut self, rate: f64) -> bool;
    fn sample_rate(&mut self) -> Result<f64, DriverError>;
    fn set_sample_rate(&mut self, rate: f64) -> Result<(), DriverError>;
    fn channel_info(&mut self, channel: i32, input: bool) -> Result<ChannelInfo, DriverError>;
    /// The driver fills `infos[k].buffers` with double buffers of `size`
    /// frames and calls `callbacks` once started.
    fn create_buffers(
        &mut self,
        infos: &mut [BufferInfo],
        size: i32,
        callbacks: &'static Callbacks,
    ) -> Result<(), DriverError>;
    fn dispose_buffers(&mut self) -> Result<(), DriverError>;
    fn start(&mut self) -> Result<(), DriverError>;
    fn stop(&mut self) -> Result<(), DriverError>;
    fn control_panel(&mut self) -> Result<(), DriverError>;
    /// `Some` when the driver supports `outputReady`.
    fn output_ready(&mut self) -> Option<OutputReady>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_sizes_follow_the_granularity() {
        let pow2 = BufferSize {
            min: 64,
            max: 1024,
            preferred: 256,
            granularity: -1,
        };
        assert_eq!(pow2.allowed(), [64, 128, 256, 512, 1024]);
        assert_eq!(pow2.choose(Some(128)), 128);
        assert_eq!(pow2.choose(Some(100)), 256, "not allowed: preferred");
        assert_eq!(pow2.choose(None), 256);
        let step = BufferSize {
            min: 96,
            max: 480,
            preferred: 192,
            granularity: 96,
        };
        assert_eq!(step.allowed(), [96, 192, 288, 384, 480]);
        let fixed = BufferSize {
            min: 512,
            max: 512,
            preferred: 512,
            granularity: 0,
        };
        assert_eq!(fixed.allowed(), [512]);
    }
}
