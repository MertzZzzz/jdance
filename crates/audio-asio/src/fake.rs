// SPDX-License-Identifier: GPL-3.0-or-later
//! A driver written in Rust for tests (spec 0.2.2 ASIO-050): it keeps
//! double buffers like a real driver, calls the host callbacks from its
//! own thread, and can send driver messages or stop calling back.

use crate::abi::{self, AsioTime, BufferInfo, Callbacks, ChannelInfo, Samples, TimeCode, TimeInfo};
use crate::convert::Format;
use crate::driver::{BufferSize, Driver, DriverError, OutputReady};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// What the test driver offers.
#[derive(Debug, Clone)]
pub struct FakeSpec {
    pub inputs: i32,
    pub outputs: i32,
    pub rates: Vec<f64>,
    pub rate: f64,
    pub buffer: BufferSize,
    pub sample_type: i32,
    pub latencies: (i32, i32),
}

impl Default for FakeSpec {
    fn default() -> Self {
        Self {
            inputs: 2,
            outputs: 4,
            rates: vec![44_100.0, 48_000.0],
            rate: 48_000.0,
            buffer: BufferSize {
                min: 64,
                max: 1024,
                preferred: 256,
                granularity: -1,
            },
            sample_type: abi::sample_type::INT32_LSB,
            latencies: (100, 200),
        }
    }
}

/// The test's view of the driver.
#[derive(Debug, Default)]
pub struct FakeState {
    pub loads: AtomicU64,
    pub panels: AtomicU64,
    pub stalled: AtomicBool,
    /// Left output channel as the driver received it (capped).
    pub played: Mutex<Vec<f32>>,
    callbacks: Mutex<Option<&'static Callbacks>>,
}

impl FakeState {
    /// Sends a driver message to the host, as the driver would.
    pub fn message(&self, selector: i32) -> i32 {
        let cb = self.callbacks.lock().ok().and_then(|c| *c);
        cb.map_or(0, |c| {
            (c.asio_message)(selector, 0, std::ptr::null_mut(), std::ptr::null_mut())
        })
    }
}

pub struct FakeDriver {
    spec: FakeSpec,
    state: Arc<FakeState>,
    /// Two halves per channel, as given to the host.
    buffers: Vec<(bool, [Box<[u8]>; 2])>,
    frames: usize,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for FakeDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeDriver")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

impl FakeDriver {
    pub fn new(spec: FakeSpec, state: Arc<FakeState>) -> Self {
        state.loads.fetch_add(1, Ordering::Relaxed);
        Self {
            spec,
            state,
            buffers: Vec::new(),
            frames: 0,
            running: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

/// Raw buffer pointers moved to the driver thread.
struct Halves(Vec<(bool, [*mut u8; 2])>);
// SAFETY: the buffers outlive the thread (`stop` joins it before
// `dispose_buffers` frees them).
unsafe impl Send for Halves {}

impl Driver for FakeDriver {
    fn name(&self) -> String {
        "Fake ASIO".into()
    }

    fn channels(&mut self) -> Result<(i32, i32), DriverError> {
        Ok((self.spec.inputs, self.spec.outputs))
    }

    fn latencies(&mut self) -> Result<(i32, i32), DriverError> {
        Ok(self.spec.latencies)
    }

    fn buffer_size(&mut self) -> Result<BufferSize, DriverError> {
        Ok(self.spec.buffer)
    }

    fn can_sample_rate(&mut self, rate: f64) -> bool {
        self.spec.rates.contains(&rate)
    }

    fn sample_rate(&mut self) -> Result<f64, DriverError> {
        Ok(self.spec.rate)
    }

    fn set_sample_rate(&mut self, rate: f64) -> Result<(), DriverError> {
        if !self.spec.rates.contains(&rate) {
            return Err(DriverError::code("setSampleRate", abi::ASE_NO_CLOCK));
        }
        self.spec.rate = rate;
        Ok(())
    }

    fn channel_info(&mut self, channel: i32, input: bool) -> Result<ChannelInfo, DriverError> {
        let mut info = ChannelInfo::query(channel, input);
        info.sample_type = self.spec.sample_type;
        let name = format!("{} {}", if input { "In" } else { "Out" }, channel + 1);
        info.name[..name.len()].copy_from_slice(name.as_bytes());
        Ok(info)
    }

    fn create_buffers(
        &mut self,
        infos: &mut [BufferInfo],
        size: i32,
        callbacks: &'static Callbacks,
    ) -> Result<(), DriverError> {
        let bytes = Format::of(self.spec.sample_type).map_or(4, Format::bytes);
        self.frames = size as usize;
        self.buffers = infos
            .iter_mut()
            .map(|info| {
                let mut halves = [
                    vec![0u8; self.frames * bytes].into_boxed_slice(),
                    vec![0u8; self.frames * bytes].into_boxed_slice(),
                ];
                info.buffers = [halves[0].as_mut_ptr().cast(), halves[1].as_mut_ptr().cast()];
                (info.is_input != 0, halves)
            })
            .collect();
        if let Ok(mut c) = self.state.callbacks.lock() {
            *c = Some(callbacks);
        }
        Ok(())
    }

    fn dispose_buffers(&mut self) -> Result<(), DriverError> {
        self.buffers.clear();
        Ok(())
    }

    fn start(&mut self) -> Result<(), DriverError> {
        let cb = self
            .state
            .callbacks
            .lock()
            .ok()
            .and_then(|c| *c)
            .ok_or_else(|| DriverError("no buffers".into()))?;
        let halves = Halves(
            self.buffers
                .iter_mut()
                .map(|(input, h)| (*input, [h[0].as_mut_ptr(), h[1].as_mut_ptr()]))
                .collect(),
        );
        let fmt = Format::of(self.spec.sample_type).unwrap_or(Format::F32 { msb: false });
        let (frames, rate) = (self.frames, self.spec.rate);
        let running = Arc::clone(&self.running);
        let state = Arc::clone(&self.state);
        running.store(true, Ordering::Relaxed);
        self.thread = Some(std::thread::spawn(move || {
            let halves = halves;
            let mut position = 0u64;
            let mut half = 0usize;
            let mut ramp = vec![0.0f32; frames];
            let mut out = vec![0.0f32; frames];
            while running.load(Ordering::Relaxed) {
                if state.stalled.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                // Inputs: a ramp of the position, for the capture test.
                for (k, r) in ramp.iter_mut().enumerate() {
                    *r = ((position + k as u64) % 1000) as f32 / 1000.0;
                }
                for (input, h) in &halves.0 {
                    if *input {
                        // SAFETY: see `Halves`.
                        let dst = unsafe {
                            std::slice::from_raw_parts_mut(h[half], frames * fmt.bytes())
                        };
                        fmt.write(&ramp, dst);
                    }
                }
                let mut time = AsioTime {
                    reserved: [0; 4],
                    time_info: TimeInfo {
                        speed: 1.0,
                        system_time: Samples::default(),
                        sample_position: Samples {
                            hi: (position >> 32) as u32,
                            lo: position as u32,
                        },
                        sample_rate: rate,
                        flags: abi::SAMPLE_POSITION_VALID,
                        reserved: [0; 12],
                    },
                    time_code: TimeCode {
                        speed: 0.0,
                        samples: Samples::default(),
                        flags: 0,
                        future: [0; 64],
                    },
                };
                (cb.buffer_switch_time_info)(&mut time, half as i32, 1);
                if let Some((_, h)) = halves.0.iter().find(|(input, _)| !*input) {
                    // SAFETY: see `Halves`.
                    let src = unsafe { std::slice::from_raw_parts(h[half], frames * fmt.bytes()) };
                    fmt.read(src, &mut out);
                    if let Ok(mut p) = state.played.lock()
                        && p.len() < 1 << 16
                    {
                        p.extend_from_slice(&out);
                    }
                }
                position += frames as u64;
                half ^= 1;
                std::thread::sleep(Duration::from_millis(1));
            }
        }));
        Ok(())
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        Ok(())
    }

    fn control_panel(&mut self) -> Result<(), DriverError> {
        self.state.panels.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn output_ready(&mut self) -> Option<OutputReady> {
        None
    }
}

impl Drop for FakeDriver {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AsioBackend;
    use jdance_audio_api::{
        AudioBackend, BufferRequest, CaptureCallback, DeviceId, InputChannels, InputRequest,
        RenderCallback, SampleRates, StreamRequest, StreamState,
    };
    use std::time::Instant;

    /// The callbacks are process-global: one session at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn backend(spec: FakeSpec) -> (AsioBackend, Arc<FakeState>) {
        let state = Arc::new(FakeState::default());
        let s = Arc::clone(&state);
        let backend = AsioBackend::new(
            Arc::new(|| vec![(DeviceId("{FAKE}".into()), "Fake ASIO".into(), None)]),
            Arc::new(move |_id: &DeviceId| {
                Ok(Box::new(FakeDriver::new(spec.clone(), Arc::clone(&s))) as Box<dyn Driver>)
            }),
        );
        (backend, state)
    }

    fn dev() -> DeviceId {
        DeviceId("{FAKE}".into())
    }

    struct Constant(f32);
    impl RenderCallback for Constant {
        fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
            l.fill(self.0);
            r.fill(self.0);
        }
    }

    struct Panics;
    impl RenderCallback for Panics {
        fn render(&mut self, _: &mut [f32], _: &mut [f32]) {
            panic!("render failed");
        }
    }

    struct Keep(Arc<Mutex<Vec<f32>>>);
    impl CaptureCallback for Keep {
        fn capture(&mut self, l: &[f32], _r: &[f32]) {
            if let Ok(mut v) = self.0.lock()
                && v.len() < 4096
            {
                v.extend_from_slice(l);
            }
        }
    }

    fn request(rate: u32) -> StreamRequest {
        StreamRequest {
            sample_rate: rate,
            buffer: BufferRequest::Frames(128),
            first_output_channel: 2,
        }
    }

    fn wait(cond: impl Fn() -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn capabilities_come_from_the_driver() {
        let _g = SERIAL.lock();
        let (b, _) = backend(FakeSpec::default());
        let caps = b.capabilities(&dev()).unwrap();
        assert_eq!(
            caps.sample_rates,
            SampleRates::Discrete(vec![44_100, 48_000])
        );
        assert_eq!((caps.input_channels, caps.output_channels), (2, 4));
        assert!(caps.buffer_size.allows(128) && !caps.buffer_size.allows(100));
        assert!(caps.has_control_panel && caps.reports_latency);
        let names = b.channel_names(&dev()).unwrap();
        assert_eq!(names.inputs, ["In 1", "In 2"]);
        assert_eq!(names.output_pair_label(2), "Outputs 3 / 4 (Out 3/Out 4)");
        b.open_control_panel(&dev()).unwrap();
    }

    #[test]
    fn plays_the_render_output_on_the_chosen_pair() {
        let _g = SERIAL.lock();
        let (b, state) = backend(FakeSpec::default());
        let out = b
            .open_output(&dev(), &request(44_100), Box::new(Constant(0.25)))
            .unwrap();
        assert_eq!(out.info().sample_rate, 44_100, "rate set on the driver");
        assert_eq!(out.info().latency, Some((100, 200)));
        assert!(wait(|| out.stats().read().callbacks > 10));
        assert_eq!(out.stats().read().last_frames, Some(128));
        let played = state.played.lock().unwrap().clone();
        let full = 1.0 / 2_147_483_648.0;
        assert!(
            played
                .iter()
                .rev()
                .take(100)
                .all(|v| (v - 0.25).abs() < full * 2.0)
        );
        assert_eq!(out.state(), StreamState::Running);
    }

    #[test]
    fn records_the_input_of_the_same_driver() {
        let _g = SERIAL.lock();
        let (b, _) = backend(FakeSpec::default());
        let out = b
            .open_output(&dev(), &request(48_000), Box::new(Constant(0.0)))
            .unwrap();
        assert_eq!(
            b.input_devices().unwrap().len(),
            1,
            "only the active driver"
        );
        let got = Arc::new(Mutex::new(Vec::new()));
        let input = b
            .open_input(
                &dev(),
                &InputRequest {
                    sample_rate: 48_000,
                    channels: InputChannels::Mono(1),
                },
                Box::new(Keep(Arc::clone(&got))),
            )
            .unwrap();
        assert_eq!(input.info().round_trip, Some(300));
        assert!(wait(|| got.lock().unwrap().len() >= 256));
        let v = got.lock().unwrap().clone();
        // The driver's ramp: consecutive frames step by 1/1000.
        assert!(v.windows(2).take(100).all(|w| {
            let d = w[1] - w[0];
            (d - 0.001).abs() < 1e-6 || d < -0.9
        }));
        drop(input);
        assert!(
            wait(|| out.stats().read().callbacks > 5),
            "output keeps running"
        );
        drop(out);
    }

    #[test]
    fn a_reset_request_reopens_the_driver_and_stops_the_input() {
        let _g = SERIAL.lock();
        let (b, state) = backend(FakeSpec::default());
        let out = b
            .open_output(&dev(), &request(48_000), Box::new(Constant(0.5)))
            .unwrap();
        let input = b
            .open_input(
                &dev(),
                &InputRequest {
                    sample_rate: 48_000,
                    channels: InputChannels::Stereo(0),
                },
                Box::new(Keep(Arc::new(Mutex::new(Vec::new())))),
            )
            .unwrap();
        assert!(wait(|| out.stats().read().callbacks > 3));
        assert_eq!(
            state.message(abi::SELECTOR_SUPPORTED),
            0,
            "0 = not a selector"
        );
        assert_eq!(state.message(abi::RESET_REQUEST), 1);
        assert!(wait(|| matches!(input.state(), StreamState::Failed(_))));
        let before = out.stats().read().callbacks;
        assert!(
            wait(|| out.stats().read().callbacks > before + 5),
            "playing again"
        );
        assert_eq!(out.state(), StreamState::Running);
        assert_eq!(
            state.loads.load(Ordering::Relaxed),
            1,
            "same driver instance"
        );
    }

    #[test]
    fn a_driver_that_stops_calling_back_fails_the_stream() {
        let _g = SERIAL.lock();
        let (b, state) = backend(FakeSpec::default());
        let out = b
            .open_output(&dev(), &request(48_000), Box::new(Constant(0.0)))
            .unwrap();
        assert!(wait(|| out.stats().read().callbacks > 3));
        state.stalled.store(true, Ordering::Relaxed);
        assert!(wait(|| matches!(out.state(), StreamState::Failed(_))));
    }

    #[test]
    fn a_panicking_render_plays_silence() {
        let _g = SERIAL.lock();
        let (b, state) = backend(FakeSpec::default());
        let out = b
            .open_output(&dev(), &request(48_000), Box::new(Panics))
            .unwrap();
        assert!(wait(|| out.stats().read().panicked));
        assert!(wait(|| state.played.lock().unwrap().len() > 512));
        assert!(state.played.lock().unwrap().iter().all(|v| *v == 0.0));
    }

    #[test]
    fn unsupported_formats_and_rates_are_refused() {
        let _g = SERIAL.lock();
        let (b, _) = backend(FakeSpec {
            sample_type: 32,
            ..FakeSpec::default()
        });
        let e = b
            .open_output(&dev(), &request(48_000), Box::new(Constant(0.0)))
            .err()
            .unwrap();
        assert!(e.to_string().contains("not supported"), "{e}");
        let (b, _) = backend(FakeSpec::default());
        assert!(
            b.open_output(&dev(), &request(96_000), Box::new(Constant(0.0)))
                .is_err()
        );
    }

    #[test]
    fn open_and_close_many_times() {
        let _g = SERIAL.lock();
        let (b, state) = backend(FakeSpec::default());
        for _ in 0..50 {
            let out = b
                .open_output(&dev(), &request(48_000), Box::new(Constant(0.1)))
                .unwrap();
            drop(out);
        }
        assert_eq!(state.loads.load(Ordering::Relaxed), 50);
        assert!(b.input_devices().unwrap().is_empty(), "nothing active");
    }

    /// ASIO-009/031: a driver that hangs in `init` fails the call after
    /// 5 s instead of blocking the caller.
    #[test]
    fn a_hanging_driver_times_out() {
        let b = AsioBackend::new(
            Arc::new(|| vec![(dev(), "Hangs".into(), None)]),
            Arc::new(|_id: &DeviceId| {
                std::thread::sleep(std::time::Duration::from_secs(7));
                Err(DriverError("never answered".into()))
            }),
        );
        let t = Instant::now();
        let e = b
            .open_output(&dev(), &request(48_000), Box::new(Constant(0.0)))
            .err()
            .expect("timeout");
        let waited = t.elapsed().as_secs_f64();
        assert!((4.9..6.5).contains(&waited), "waited {waited} s");
        assert!(e.to_string().contains("5 s"), "{e}");
    }
}
