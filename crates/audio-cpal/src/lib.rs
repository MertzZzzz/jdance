// SPDX-License-Identifier: GPL-3.0-or-later
//! CPAL adapter for `jdance-audio-api`.
//!
//! One [`CpalBackend`] per CPAL host compiled in for the platform (WASAPI on
//! Windows, CoreAudio on macOS, ALSA and JACK on Linux). CPAL types do not
//! leave this crate.
//!
//! Known gaps (see docs/audio/cpal-capabilities.md): CPAL reports no output
//! latency and no driver control panel, and on some hosts the buffer size
//! range is unknown before a stream starts.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use jdance_audio_api::{
    ActiveInputInfo, ActiveStreamInfo, AudioBackend, AudioError, BackendId, BufferRequest,
    BufferSizeCaps, CaptureCallback, DeviceCapabilities, DeviceId, DeviceInfo, InputRequest,
    InputStream, OutputStream, RenderCallback, SampleFormat, SampleRates, StreamRequest,
    StreamState, StreamStats, deinterleave_input, interleave_pair, validate_request,
};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;
use std::sync::Arc;

/// Frames rendered per engine call; longer device callbacks are split.
const SCRATCH_FRAMES: usize = 8192;

/// All CPAL hosts available on this machine, default host first.
pub fn backends() -> Vec<CpalBackend> {
    let default = cpal::default_host().id();
    let mut ids = cpal::available_hosts();
    ids.sort_by_key(|id| *id != default);
    ids.into_iter()
        .filter_map(|id| cpal::host_from_id(id).ok())
        .map(|host| CpalBackend { host })
        .collect()
}

pub struct CpalBackend {
    host: cpal::Host,
}

impl std::fmt::Debug for CpalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpalBackend")
            .field("host", &self.host.id())
            .finish()
    }
}

fn backend_err(e: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(e.to_string())
}

fn map_err(e: cpal::Error) -> AudioError {
    use cpal::ErrorKind as K;
    match e.kind() {
        K::DeviceBusy => AudioError::DeviceBusy,
        K::DeviceNotAvailable => AudioError::DeviceNotFound(e.to_string()),
        K::UnsupportedConfig | K::InvalidInput => AudioError::InvalidConfig(e.to_string()),
        K::UnsupportedOperation => AudioError::Unsupported(e.to_string()),
        _ => backend_err(e),
    }
}

fn map_format(f: cpal::SampleFormat) -> SampleFormat {
    match f {
        cpal::SampleFormat::U8 => SampleFormat::U8,
        cpal::SampleFormat::I16 => SampleFormat::I16,
        cpal::SampleFormat::I24 => SampleFormat::I24,
        cpal::SampleFormat::I32 => SampleFormat::I32,
        cpal::SampleFormat::F32 => SampleFormat::F32,
        cpal::SampleFormat::F64 => SampleFormat::F64,
        _ => SampleFormat::Other,
    }
}

/// Preference order when several formats are offered.
fn format_rank(f: cpal::SampleFormat) -> u8 {
    match f {
        cpal::SampleFormat::F32 => 0,
        cpal::SampleFormat::I32 => 1,
        cpal::SampleFormat::I24 => 2,
        cpal::SampleFormat::I16 => 3,
        cpal::SampleFormat::F64 => 4,
        cpal::SampleFormat::U8 => 5,
        _ => u8::MAX,
    }
}

impl CpalBackend {
    fn device(&self, id: &DeviceId) -> Result<cpal::Device, AudioError> {
        let cpal_id = cpal::DeviceId::from_str(&id.0).map_err(map_err)?;
        self.host
            .device_by_id(&cpal_id)
            .ok_or_else(|| AudioError::DeviceNotFound(id.0.clone()))
    }

    fn output_ranges(
        &self,
        device: &cpal::Device,
    ) -> Result<Vec<cpal::SupportedStreamConfigRange>, AudioError> {
        Ok(device
            .supported_output_configs()
            .map_err(map_err)?
            .collect())
    }
}

impl AudioBackend for CpalBackend {
    fn id(&self) -> BackendId {
        BackendId(self.host.id().to_string())
    }

    fn display_name(&self) -> String {
        self.host.id().name().to_owned()
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let default_id = self.host.default_output_device().and_then(|d| d.id().ok());
        let mut out = Vec::new();
        for device in self.host.output_devices().map_err(map_err)? {
            let Ok(id) = device.id() else { continue };
            let name = device
                .description()
                .map(|d| d.name().to_owned())
                .unwrap_or_else(|_| id.id().to_owned());
            out.push(DeviceInfo {
                is_default: default_id.as_ref() == Some(&id),
                id: DeviceId(id.to_string()),
                name,
                stable_id: true,
            });
        }
        Ok(out)
    }

    fn capabilities(&self, id: &DeviceId) -> Result<DeviceCapabilities, AudioError> {
        let device = self.device(id)?;
        let ranges = self.output_ranges(&device)?;
        if ranges.is_empty() {
            return Err(AudioError::Unsupported(
                "device has no output configurations".into(),
            ));
        }
        let mut rates: Vec<(u32, u32)> = ranges
            .iter()
            .map(|r| (r.min_sample_rate(), r.max_sample_rate()))
            .collect();
        rates.sort_unstable();
        rates.dedup();
        let mut formats: Vec<SampleFormat> = ranges
            .iter()
            .map(|r| map_format(r.sample_format()))
            .collect();
        formats.sort_by_key(|f| *f as u8);
        formats.dedup();

        let mut buf: Option<(u32, u32)> = None;
        for r in &ranges {
            if let cpal::SupportedBufferSize::Range { min, max } = *r.buffer_size() {
                buf = Some(buf.map_or((min, max), |(a, b)| (a.min(min), b.max(max))));
            }
        }
        let buffer_size = match buf {
            Some((min, max)) => BufferSizeCaps::Range { min, max },
            None => BufferSizeCaps::Unknown,
        };
        Ok(DeviceCapabilities {
            sample_rates: SampleRates::Ranges(rates),
            default_sample_rate: device.default_output_config().ok().map(|c| c.sample_rate()),
            can_change_buffer_from_app: buf.is_some(),
            buffer_size,
            output_channels: ranges.iter().map(|r| r.channels()).max().unwrap_or(0),
            input_channels: device
                .supported_input_configs()
                .map(|it| it.map(|r| r.channels()).max().unwrap_or(0))
                .unwrap_or(0),
            sample_formats: formats,
            reports_latency: false,
            has_control_panel: false,
            can_change_sample_rate_from_app: true,
        })
    }

    fn open_output(
        &self,
        id: &DeviceId,
        request: &StreamRequest,
        callback: Box<dyn RenderCallback>,
    ) -> Result<Box<dyn OutputStream>, AudioError> {
        let caps = self.capabilities(id)?;
        validate_request(&caps, request)?;
        let device = self.device(id)?;
        let needed = if caps.output_channels == 1 {
            1
        } else {
            request.first_output_channel + 2
        };
        let range = self
            .output_ranges(&device)?
            .into_iter()
            .filter(|r| r.channels() >= needed && r.contains_rate(request.sample_rate))
            .min_by_key(|r| (format_rank(r.sample_format()), r.channels()))
            .ok_or_else(|| {
                AudioError::InvalidConfig(format!(
                    "no output configuration with {needed} channels at {} Hz",
                    request.sample_rate
                ))
            })?;
        let format = range.sample_format();
        let config = cpal::StreamConfig {
            channels: range.channels(),
            sample_rate: request.sample_rate,
            buffer_size: match request.buffer {
                BufferRequest::Default => cpal::BufferSize::Default,
                BufferRequest::Frames(f) => cpal::BufferSize::Fixed(f),
            },
        };
        let stats = Arc::new(StreamStats::default());
        let first = usize::from(request.first_output_channel);
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, first, callback, &stats),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, first, callback, &stats),
            cpal::SampleFormat::I24 => {
                build::<cpal::I24>(&device, &config, first, callback, &stats)
            }
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, first, callback, &stats),
            cpal::SampleFormat::F64 => build::<f64>(&device, &config, first, callback, &stats),
            cpal::SampleFormat::U8 => build::<u8>(&device, &config, first, callback, &stats),
            other => Err(AudioError::Unsupported(format!("sample format {other:?}"))),
        }?;
        stream.play().map_err(map_err)?;
        let device_name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| id.0.clone());
        Ok(Box::new(CpalStream {
            _stream: stream,
            stats,
            info: ActiveStreamInfo {
                backend: self.id(),
                device: id.clone(),
                device_name,
                sample_rate: config.sample_rate,
                channels: config.channels,
                sample_format: map_format(format),
                requested_buffer: request.buffer,
                first_output_channel: request.first_output_channel,
            },
        }))
    }

    fn input_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let default_id = self.host.default_input_device().and_then(|d| d.id().ok());
        let mut out = Vec::new();
        for device in self.host.input_devices().map_err(map_err)? {
            let Ok(id) = device.id() else { continue };
            let name = device
                .description()
                .map(|d| d.name().to_owned())
                .unwrap_or_else(|_| id.id().to_owned());
            out.push(DeviceInfo {
                is_default: default_id.as_ref() == Some(&id),
                id: DeviceId(id.to_string()),
                name,
                stable_id: true,
            });
        }
        Ok(out)
    }

    fn input_channels(&self, id: &DeviceId) -> Result<u16, AudioError> {
        Ok(self
            .device(id)?
            .supported_input_configs()
            .map_err(map_err)?
            .map(|r| r.channels())
            .max()
            .unwrap_or(0))
    }

    fn open_input(
        &self,
        id: &DeviceId,
        request: &InputRequest,
        callback: Box<dyn CaptureCallback>,
    ) -> Result<Box<dyn InputStream>, AudioError> {
        let device = self.device(id)?;
        let needed = request.channels.needed();
        let range = device
            .supported_input_configs()
            .map_err(map_err)?
            .filter(|r| r.channels() >= needed && r.contains_rate(request.sample_rate))
            .min_by_key(|r| (format_rank(r.sample_format()), r.channels()))
            .ok_or_else(|| {
                AudioError::InvalidConfig(format!(
                    "the input device has no configuration with {needed} channels at {} Hz \
                     (the output's rate); recording needs one shared rate",
                    request.sample_rate
                ))
            })?;
        let format = range.sample_format();
        let config = cpal::StreamConfig {
            channels: range.channels(),
            sample_rate: request.sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };
        let stats = Arc::new(StreamStats::default());
        let ch = request.channels;
        let stream = match format {
            cpal::SampleFormat::F32 => build_input::<f32>(&device, &config, ch, callback, &stats),
            cpal::SampleFormat::I32 => build_input::<i32>(&device, &config, ch, callback, &stats),
            cpal::SampleFormat::I24 => {
                build_input::<cpal::I24>(&device, &config, ch, callback, &stats)
            }
            cpal::SampleFormat::I16 => build_input::<i16>(&device, &config, ch, callback, &stats),
            cpal::SampleFormat::F64 => build_input::<f64>(&device, &config, ch, callback, &stats),
            cpal::SampleFormat::U8 => build_input::<u8>(&device, &config, ch, callback, &stats),
            other => Err(AudioError::Unsupported(format!("sample format {other:?}"))),
        }?;
        stream.play().map_err(map_err)?;
        let device_name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| id.0.clone());
        Ok(Box::new(CpalInput {
            _stream: stream,
            stats,
            info: ActiveInputInfo {
                device: id.clone(),
                device_name,
                sample_rate: config.sample_rate,
                device_channels: config.channels,
                channels: ch,
            },
        }))
    }
}

fn build_input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: jdance_audio_api::InputChannels,
    mut callback: Box<dyn CaptureCallback>,
    stats: &Arc<StreamStats>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let device_channels = usize::from(config.channels);
    // Allocated here, before the stream starts; never resized afterwards.
    let mut left = vec![0.0f32; SCRATCH_FRAMES];
    let mut right = vec![0.0f32; SCRATCH_FRAMES];
    let rt_stats = Arc::clone(stats);
    let err_stats = Arc::clone(stats);
    let mono = matches!(channels, jdance_audio_api::InputChannels::Mono(_));
    let data_callback = move |data: &[T], _: &cpal::InputCallbackInfo| {
        let frames = data.len() / device_channels.max(1);
        rt_stats.note_callback(frames as u32);
        if rt_stats.is_panicked() {
            return;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            for chunk in data.chunks(SCRATCH_FRAMES * device_channels.max(1)) {
                let n = deinterleave_input(
                    chunk,
                    device_channels,
                    channels,
                    &mut left,
                    &mut right,
                    <f32 as FromSample<T>>::from_sample_,
                );
                if mono {
                    callback.capture(&left[..n], &left[..n]);
                } else {
                    callback.capture(&left[..n], &right[..n]);
                }
            }
        }));
        if let Err(payload) = result {
            std::mem::forget(payload);
            rt_stats.note_panic();
        }
    };
    let error_callback = move |e: cpal::Error| err_stats.set_error(e.to_string());
    device
        .build_input_stream::<T, _, _>(*config, data_callback, error_callback, None)
        .map_err(map_err)
}

struct CpalInput {
    _stream: cpal::Stream,
    stats: Arc<StreamStats>,
    info: ActiveInputInfo,
}

impl InputStream for CpalInput {
    fn info(&self) -> &ActiveInputInfo {
        &self.info
    }

    fn state(&self) -> StreamState {
        match self.stats.read() {
            r if r.panicked => StreamState::Failed("the recording input crashed (panic)".into()),
            r => match r.error {
                Some(e) => StreamState::Failed(e),
                None => StreamState::Running,
            },
        }
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    first: usize,
    mut callback: Box<dyn RenderCallback>,
    stats: &Arc<StreamStats>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = usize::from(config.channels);
    // Allocated here, before the stream starts; never resized afterwards.
    let mut left = vec![0.0f32; SCRATCH_FRAMES];
    let mut right = vec![0.0f32; SCRATCH_FRAMES];
    let rt_stats = Arc::clone(stats);
    let err_stats = Arc::clone(stats);
    let silence = T::EQUILIBRIUM;

    let data_callback = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        let frames = data.len() / channels.max(1);
        rt_stats.note_callback(frames as u32);
        if rt_stats.is_panicked() {
            data.fill(silence);
            return;
        }
        if frames > SCRATCH_FRAMES {
            rt_stats.note_oversized();
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut done = 0;
            while done < frames {
                let n = (frames - done).min(SCRATCH_FRAMES);
                callback.render(&mut left[..n], &mut right[..n]);
                interleave_pair(
                    &left[..n],
                    &right[..n],
                    &mut data[done * channels..(done + n) * channels],
                    channels,
                    first,
                    silence,
                    T::from_sample,
                );
                done += n;
            }
        }));
        if let Err(payload) = result {
            // RT-031: silence from now on and flag the fault. The payload is
            // leaked rather than freed on the audio thread.
            std::mem::forget(payload);
            rt_stats.note_panic();
            data.fill(silence);
        }
    };
    let error_callback = move |e: cpal::Error| err_stats.set_error(e.to_string());
    device
        .build_output_stream::<T, _, _>(*config, data_callback, error_callback, None)
        .map_err(map_err)
}

struct CpalStream {
    _stream: cpal::Stream,
    stats: Arc<StreamStats>,
    info: ActiveStreamInfo,
}

impl OutputStream for CpalStream {
    fn info(&self) -> &ActiveStreamInfo {
        &self.info
    }

    fn stats(&self) -> Arc<StreamStats> {
        Arc::clone(&self.stats)
    }

    fn state(&self) -> StreamState {
        match self.stats.read() {
            r if r.panicked => StreamState::Failed("the audio engine crashed (panic)".into()),
            r => match r.error {
                Some(e) => StreamState::Failed(e),
                None => StreamState::Running,
            },
        }
    }
}
