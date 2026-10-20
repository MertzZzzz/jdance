// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-neutral audio I/O interfaces.
//!
//! Backends (CPAL, the Windows ASIO host, …) implement [`AudioBackend`].
//! Nothing platform-specific leaks through these types; the UI builds the
//! settings screen from [`DeviceCapabilities`] instead of assuming what a
//! backend can do (Amendment 01 §5, AUD-010).

mod settings;
mod stats;

pub use settings::{DeviceKey, DeviceSettings, UserAudioSettings};
pub use stats::{StatsReading, StreamStats};

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Backend identifier, stable across releases (used in user settings).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackendId(pub String);

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Device identifier within one backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(pub String);

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub is_default: bool,
    /// `false` when the OS gives no persistent ID and `id` is derived from
    /// the device name (AUD-012).
    pub stable_id: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleRates {
    Discrete(Vec<u32>),
    Ranges(Vec<(u32, u32)>),
    Unknown,
}

impl SampleRates {
    pub fn contains(&self, rate: u32) -> bool {
        match self {
            SampleRates::Discrete(v) => v.contains(&rate),
            SampleRates::Ranges(r) => r.iter().any(|(lo, hi)| (*lo..=*hi).contains(&rate)),
            SampleRates::Unknown => true,
        }
    }

    /// Common rates offered in the UI, filtered by what the device allows.
    pub fn offered(&self) -> Vec<u32> {
        const COMMON: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
        match self {
            SampleRates::Discrete(v) => {
                let mut v = v.clone();
                v.sort_unstable();
                v.dedup();
                v
            }
            _ => COMMON.into_iter().filter(|r| self.contains(*r)).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferSizeCaps {
    Discrete(Vec<u32>),
    Range {
        min: u32,
        max: u32,
    },
    /// The driver decides; the app can only display the value.
    FixedByDriver(u32),
    Unknown,
}

impl BufferSizeCaps {
    pub fn allows(&self, frames: u32) -> bool {
        match self {
            BufferSizeCaps::Discrete(v) => v.contains(&frames),
            BufferSizeCaps::Range { min, max } => (*min..=*max).contains(&frames),
            BufferSizeCaps::FixedByDriver(f) => *f == frames,
            BufferSizeCaps::Unknown => false,
        }
    }

    /// Power-of-two sizes offered in the UI.
    pub fn offered(&self) -> Vec<u32> {
        match self {
            BufferSizeCaps::Discrete(v) => v.clone(),
            BufferSizeCaps::Range { .. } => (5..=13)
                .map(|p| 1u32 << p)
                .filter(|f| self.allows(*f))
                .collect(),
            BufferSizeCaps::FixedByDriver(f) => vec![*f],
            BufferSizeCaps::Unknown => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleFormat {
    U8,
    I16,
    I24,
    I32,
    F32,
    F64,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCapabilities {
    pub sample_rates: SampleRates,
    pub default_sample_rate: Option<u32>,
    pub buffer_size: BufferSizeCaps,
    pub output_channels: u16,
    /// Display only: recording is outside MVP scope.
    pub input_channels: u16,
    pub sample_formats: Vec<SampleFormat>,
    pub reports_latency: bool,
    pub has_control_panel: bool,
    pub can_change_buffer_from_app: bool,
    pub can_change_sample_rate_from_app: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BufferRequest {
    /// Let the backend/driver choose.
    #[default]
    Default,
    Frames(u32),
}

/// What the app asks for. The stream actually opened is described by
/// [`ActiveStreamInfo`] and may differ only where the backend says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRequest {
    pub sample_rate: u32,
    pub buffer: BufferRequest,
    /// Zero-based index of the left channel of the stereo output pair.
    pub first_output_channel: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveStreamInfo {
    pub backend: BackendId,
    pub device: DeviceId,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: SampleFormat,
    pub requested_buffer: BufferRequest,
    pub first_output_channel: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    Running,
    /// The backend reported an error; the stream must be reopened.
    Failed(String),
}

/// Produces audio on the device thread. Implementations must follow the
/// realtime rules (section 8): the adapter calls `render` from the
/// driver callback.
pub trait RenderCallback: Send + 'static {
    /// Fills `left` and `right` (equal length, any size).
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);
}

/// An open output stream; dropping it stops and closes the stream.
pub trait OutputStream {
    fn info(&self) -> &ActiveStreamInfo;
    fn stats(&self) -> Arc<StreamStats>;
    fn state(&self) -> StreamState;
}

pub trait AudioBackend {
    fn id(&self) -> BackendId;
    fn display_name(&self) -> String;
    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError>;
    fn capabilities(&self, device: &DeviceId) -> Result<DeviceCapabilities, AudioError>;
    fn open_output(
        &self,
        device: &DeviceId,
        request: &StreamRequest,
        callback: Box<dyn RenderCallback>,
    ) -> Result<Box<dyn OutputStream>, AudioError>;
    /// Opens the driver's own settings dialog where supported.
    fn open_control_panel(&self, _device: &DeviceId) -> Result<(), AudioError> {
        Err(AudioError::Unsupported(
            "this backend has no driver control panel".into(),
        ))
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AudioError {
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("device is busy or used exclusively by another application")]
    DeviceBusy,
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("backend error: {0}")]
    Backend(String),
}

/// Checks `request` against `caps` before opening a stream.
pub fn validate_request(
    caps: &DeviceCapabilities,
    request: &StreamRequest,
) -> Result<(), AudioError> {
    if !caps.sample_rates.contains(request.sample_rate) {
        return Err(AudioError::InvalidConfig(format!(
            "sample rate {} Hz is not supported by the device",
            request.sample_rate
        )));
    }
    if let BufferRequest::Frames(f) = request.buffer {
        if !caps.can_change_buffer_from_app {
            return Err(AudioError::InvalidConfig(
                "the buffer size can't be changed from the app for this device".into(),
            ));
        }
        if !caps.buffer_size.allows(f) {
            return Err(AudioError::InvalidConfig(format!(
                "buffer size {f} is outside the device's supported range"
            )));
        }
    }
    if u32::from(request.first_output_channel) + 2 > u32::from(caps.output_channels.max(1))
        && !(caps.output_channels == 1 && request.first_output_channel == 0)
    {
        return Err(AudioError::InvalidConfig(format!(
            "output pair {}-{} exceeds the device's {} output channels",
            request.first_output_channel + 1,
            request.first_output_channel + 2,
            caps.output_channels
        )));
    }
    Ok(())
}

/// Writes the stereo pair into an interleaved device buffer with
/// `channels` channels, starting at `first` (mono devices get the sum ×½).
/// Other channels are zeroed. Realtime-safe.
#[inline]
pub fn interleave_pair<T: Copy>(
    left: &[f32],
    right: &[f32],
    out: &mut [T],
    channels: usize,
    first: usize,
    silence: T,
    convert: impl Fn(f32) -> T,
) {
    if channels == 0 {
        return;
    }
    for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
        frame.fill(silence);
        let (l, r) = (
            left.get(i).copied().unwrap_or(0.0),
            right.get(i).copied().unwrap_or(0.0),
        );
        if channels == 1 {
            frame[0] = convert((l + r) * 0.5);
        } else if first + 1 < channels {
            frame[first] = convert(l);
            frame[first + 1] = convert(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> DeviceCapabilities {
        DeviceCapabilities {
            sample_rates: SampleRates::Ranges(vec![(44_100, 48_000)]),
            default_sample_rate: Some(48_000),
            buffer_size: BufferSizeCaps::Range { min: 64, max: 2048 },
            output_channels: 4,
            input_channels: 0,
            sample_formats: vec![SampleFormat::F32],
            reports_latency: false,
            has_control_panel: false,
            can_change_buffer_from_app: true,
            can_change_sample_rate_from_app: true,
        }
    }

    #[test]
    fn validation() {
        let ok = StreamRequest {
            sample_rate: 48_000,
            buffer: BufferRequest::Frames(256),
            first_output_channel: 2,
        };
        assert!(validate_request(&caps(), &ok).is_ok());
        assert!(
            validate_request(
                &caps(),
                &StreamRequest {
                    sample_rate: 96_000,
                    ..ok
                }
            )
            .is_err()
        );
        assert!(
            validate_request(
                &caps(),
                &StreamRequest {
                    buffer: BufferRequest::Frames(32),
                    ..ok
                }
            )
            .is_err()
        );
        assert!(
            validate_request(
                &caps(),
                &StreamRequest {
                    first_output_channel: 3,
                    ..ok
                }
            )
            .is_err()
        );
        let mut fixed = caps();
        fixed.can_change_buffer_from_app = false;
        assert!(validate_request(&fixed, &ok).is_err());
        assert!(
            validate_request(
                &fixed,
                &StreamRequest {
                    buffer: BufferRequest::Default,
                    ..ok
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn offered_values() {
        assert_eq!(
            SampleRates::Ranges(vec![(44_100, 48_000)]).offered(),
            [44_100, 48_000]
        );
        assert_eq!(
            BufferSizeCaps::Range { min: 100, max: 600 }.offered(),
            [128, 256, 512]
        );
    }

    #[test]
    fn interleave_maps_pair_and_zeroes_rest() {
        let mut out = [9.0f32; 8];
        interleave_pair(&[1.0, 2.0], &[3.0, 4.0], &mut out, 4, 2, 0.0, |x| x);
        assert_eq!(out, [0.0, 0.0, 1.0, 3.0, 0.0, 0.0, 2.0, 4.0]);
        let mut mono = [0i16; 2];
        interleave_pair(&[1.0, 0.0], &[0.0, 0.0], &mut mono, 1, 0, 0, |x| {
            (x * 100.0) as i16
        });
        assert_eq!(mono, [50, 0]);
    }
}
