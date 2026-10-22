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
    /// Input channels of the same device (0 = none).
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
    /// Input and output latency in frames as the driver reports them;
    /// `None` when the backend does not report latency (AUD-011).
    pub latency: Option<(u32, u32)>,
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

/// Hardware inputs captured by a recording (spec 0.2.0 §7), zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputChannels {
    Mono(u16),
    /// The pair starting at this channel.
    Stereo(u16),
}

impl InputChannels {
    /// Channels in the recorded file.
    pub fn count(self) -> u16 {
        match self {
            Self::Mono(_) => 1,
            Self::Stereo(_) => 2,
        }
    }

    /// Hardware channels the device needs.
    pub fn needed(self) -> u16 {
        match self {
            Self::Mono(c) => c + 1,
            Self::Stereo(c) => c + 2,
        }
    }

    /// Display name, from driver names when known ("Input 1", "Input 1+2").
    pub fn label(self) -> String {
        match self {
            Self::Mono(c) => format!("Input {}", c + 1),
            Self::Stereo(c) => format!("Input {}+{}", c + 1, c + 2),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputRequest {
    pub sample_rate: u32,
    pub channels: InputChannels,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveInputInfo {
    pub device: DeviceId,
    pub device_name: String,
    pub sample_rate: u32,
    /// Hardware channels of the opened configuration.
    pub device_channels: u16,
    pub channels: InputChannels,
    /// Input plus output latency in frames reported by the driver, to be
    /// taken off recorded takes once (spec 0.2.2 ASIO-042); `None` when
    /// unknown.
    pub round_trip: Option<u32>,
}

/// Driver-provided channel names of a device (spec 0.2.2 ASIO-023);
/// empty when the backend has none.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelNames {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

impl ChannelNames {
    /// «Input 1+2 (Mic 1/Mic 2)» with names, «Input 1+2» without.
    pub fn input_label(&self, channels: InputChannels) -> String {
        let name = |c: u16| self.inputs.get(usize::from(c)).filter(|n| !n.is_empty());
        match channels {
            InputChannels::Mono(c) => match name(c) {
                Some(n) => format!("{} ({n})", channels.label()),
                None => channels.label(),
            },
            InputChannels::Stereo(c) => match (name(c), name(c + 1)) {
                (Some(a), Some(b)) => format!("{} ({a}/{b})", channels.label()),
                _ => channels.label(),
            },
        }
    }

    /// «Outputs 1 / 2 (Main L/Main R)» for the output pair from `first`.
    pub fn output_pair_label(&self, first: u16) -> String {
        let base = format!("Outputs {} / {}", first + 1, first + 2);
        let name = |c: u16| self.outputs.get(usize::from(c)).filter(|n| !n.is_empty());
        match (name(first), name(first + 1)) {
            (Some(a), Some(b)) => format!("{base} ({a}/{b})"),
            _ => base,
        }
    }
}

/// Most device input channels handed to [`CaptureCallback::capture_all`]
/// (mixer track inputs, spec 0.2.3 INP-001).
pub const MAX_LIVE_INPUTS: usize = 32;

/// Receives captured audio on the device thread; same realtime rules as
/// [`RenderCallback`]. Mono captures pass the same slice twice.
pub trait CaptureCallback: Send + 'static {
    fn capture(&mut self, left: &[f32], right: &[f32]);
    /// The same block with every device input channel (at most
    /// [`MAX_LIVE_INPUTS`]), interleaved, `channels` per frame; called
    /// right before [`Self::capture`].
    fn capture_all(&mut self, _frames: &[f32], _channels: usize) {}
}

/// An open input stream; dropping it stops and closes the stream.
pub trait InputStream {
    fn info(&self) -> &ActiveInputInfo;
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
    /// Devices with inputs (may include output devices that also capture).
    fn input_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        Ok(Vec::new())
    }
    /// Input channel count of an input device.
    fn input_channels(&self, _device: &DeviceId) -> Result<u16, AudioError> {
        Ok(0)
    }
    /// Opens a capture stream. A backend that cannot run it alongside the
    /// output fails here; it never aggregates devices behind the scenes.
    fn open_input(
        &self,
        _device: &DeviceId,
        _request: &InputRequest,
        _callback: Box<dyn CaptureCallback>,
    ) -> Result<Box<dyn InputStream>, AudioError> {
        Err(AudioError::Unsupported("this backend cannot record".into()))
    }
    /// Names of the device's channels where the driver provides them.
    fn channel_names(&self, _device: &DeviceId) -> Result<ChannelNames, AudioError> {
        Ok(ChannelNames::default())
    }
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

/// Takes `channels` out of an interleaved device buffer with
/// `device_channels` channels into `left`/`right` (mono: `left` only).
/// Returns the frames written. Realtime-safe.
#[inline]
pub fn deinterleave_input<T: Copy>(
    data: &[T],
    device_channels: usize,
    channels: InputChannels,
    left: &mut [f32],
    right: &mut [f32],
    convert: impl Fn(T) -> f32,
) -> usize {
    if device_channels == 0 {
        return 0;
    }
    let frames = (data.len() / device_channels)
        .min(left.len())
        .min(right.len());
    for (i, frame) in data.chunks_exact(device_channels).take(frames).enumerate() {
        let at = |c: u16| frame.get(usize::from(c)).map_or(0.0, |v| convert(*v));
        match channels {
            InputChannels::Mono(c) => left[i] = at(c),
            InputChannels::Stereo(c) => {
                left[i] = at(c);
                right[i] = at(c + 1);
            }
        }
    }
    frames
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
    fn deinterleave_takes_the_chosen_inputs() {
        let data = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let (mut l, mut r) = ([0.0; 4], [0.0; 4]);
        let n = deinterleave_input(&data, 3, InputChannels::Stereo(1), &mut l, &mut r, |x| x);
        assert_eq!((n, &l[..2], &r[..2]), (2, &[2.0, 5.0][..], &[3.0, 6.0][..]));
        let n = deinterleave_input(&data, 3, InputChannels::Mono(2), &mut l, &mut r, |x| x);
        assert_eq!((n, &l[..2]), (2, &[3.0, 6.0][..]));
        assert_eq!(InputChannels::Stereo(0).label(), "Input 1+2");
        assert_eq!(InputChannels::Mono(2).needed(), 3);
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
