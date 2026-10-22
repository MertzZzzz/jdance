// SPDX-License-Identifier: GPL-3.0-or-later
//! Test VST3 effects for host tests (PLG-080). Not shipped.
//!
//! One module with several effect classes, each with a separate edit
//! controller connected through `IConnectionPoint`:
//! - **JD Test Gain**: gain (`0` id, normalized `v` → linear `2v`) and a
//!   bypass parameter flagged `kIsBypass`;
//! - **JD Test Latency**: reports [`LATENCY`] samples and delays its input
//!   by exactly that much;
//! - **JD Test Sidechain**: output = aux (sidechain) input × gain;
//! - **JD Test Logger**: gain, records every incoming parameter point
//!   (id, sample offset, value) into its component state, has a
//!   non-automatable parameter, and echoes the gain as an output
//!   parameter change.
//! - **JD Test Synth**: sine voices from note events.
//! - **JD Test Drums**: the synth with a stereo main output and separate
//!   outputs (one stereo, three mono), like multi-output drum
//!   instruments; it accepts only its own bus arrangement.
//! - **JD Test Mono**: gain with one mono input and one mono output; it
//!   refuses stereo.
//!
//! `JDANCE_TEST_FX_FAULT=crash|hang` makes `GetPluginFactory` abort or
//! never return, for scanner blocklist and timeout tests.

// VST3 enum types differ between Windows and other platforms, so casts
// that are no-ops on one OS are needed on another.
#![allow(
    unsafe_code,
    non_snake_case,
    clippy::missing_safety_doc,
    clippy::unnecessary_cast
)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, uid};

/// Latency of the latency class, in samples.
pub const LATENCY: usize = 64;
const LOG_CAPACITY: usize = 4096;

const P_GAIN: ParamID = 0;
const P_FIXED: ParamID = 1;
const P_OUT: ParamID = 2;
const P_BYPASS: ParamID = 3;

const STATE_MAGIC: &[u8; 4] = b"JDTF";
const CTRL_MAGIC: &[u8; 4] = b"CTRL";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Gain = 0,
    Latency = 1,
    Sidechain = 2,
    Logger = 3,
    Synth = 4,
    Drums = 5,
    Mono = 6,
}

const MODES: [Mode; 7] = [
    Mode::Gain,
    Mode::Latency,
    Mode::Sidechain,
    Mode::Logger,
    Mode::Synth,
    Mode::Drums,
    Mode::Mono,
];

/// Output buses of the drums class.
const DRUM_OUTS: [SpeakerArrangement; 5] = [
    SpeakerArr::kStereo,
    SpeakerArr::kStereo,
    SpeakerArr::kMono,
    SpeakerArr::kMono,
    SpeakerArr::kMono,
];

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Gain => "JD Test Gain",
            Mode::Latency => "JD Test Latency",
            Mode::Sidechain => "JD Test Sidechain",
            Mode::Logger => "JD Test Logger",
            Mode::Synth => "JD Test Synth",
            Mode::Drums => "JD Test Drums",
            Mode::Mono => "JD Test Mono",
        }
    }

    fn processor_cid(self) -> TUID {
        uid(0x4A44_5446, 0x5845_0000, 0, self as u32)
    }

    fn controller_cid(self) -> TUID {
        uid(0x4A44_5446, 0x5845_0000, 1, self as u32)
    }

    fn params(self) -> &'static [ParamID] {
        match self {
            Mode::Gain => &[P_GAIN, P_BYPASS],
            Mode::Logger => &[P_GAIN, P_FIXED, P_OUT],
            Mode::Latency | Mode::Sidechain | Mode::Synth | Mode::Drums | Mode::Mono => &[P_GAIN],
        }
    }
}

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let n = src.len().min(dst.len() - 1);
    for (d, s) in dst.iter_mut().zip(&src.as_bytes()[..n]) {
        *d = *s as c_char;
    }
    dst[n] = 0;
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut n = 0;
    let cap = dst.len() - 1;
    for (d, s) in dst.iter_mut().take(cap).zip(src.encode_utf16()) {
        *d = s as TChar;
        n += 1;
    }
    dst[n] = 0;
}

unsafe fn write_all(stream: *mut IBStream, bytes: &[u8]) -> bool {
    let Some(s) = (unsafe { ComRef::from_raw(stream) }) else {
        return false;
    };
    let mut written = 0;
    let r = unsafe {
        s.write(
            bytes.as_ptr() as *mut c_void,
            bytes.len() as i32,
            &mut written,
        )
    };
    r == kResultOk && written == bytes.len() as i32
}

unsafe fn read_exact(stream: *mut IBStream, buf: &mut [u8]) -> bool {
    let Some(s) = (unsafe { ComRef::from_raw(stream) }) else {
        return false;
    };
    let mut read = 0;
    let r = unsafe { s.read(buf.as_mut_ptr() as *mut c_void, buf.len() as i32, &mut read) };
    r == kResultOk && read == buf.len() as i32
}

/// Reads the gain from a component state stream.
unsafe fn read_component_gain(stream: *mut IBStream) -> Option<f64> {
    let mut head = [0u8; 13];
    if !unsafe { read_exact(stream, &mut head) } || &head[..4] != STATE_MAGIC {
        return None;
    }
    Some(f64::from_le_bytes(head[5..13].try_into().ok()?))
}

struct Dsp {
    delay: [[f32; LATENCY]; 2],
    pos: usize,
    log: Vec<(ParamID, i32, f64)>,
    /// Synth voices: (key, phase increment per sample, phase, amplitude).
    voices: Vec<(u8, f64, f64, f32)>,
    sample_rate: f64,
}

/// Synth event log entries use these ids: (id, offset, key).
pub const LOG_NOTE_ON: u32 = 0x1000;
pub const LOG_NOTE_OFF: u32 = 0x1001;

struct Processor {
    mode: Mode,
    gain: AtomicU64,
    bypass: AtomicBool,
    sidechain_active: AtomicBool,
    dsp: Mutex<Dsp>,
}

impl Class for Processor {
    type Interfaces = (IComponent, IAudioProcessor, IConnectionPoint);
}

impl Processor {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            gain: AtomicU64::new(0.5f64.to_bits()),
            bypass: AtomicBool::new(false),
            sidechain_active: AtomicBool::new(false),
            dsp: Mutex::new(Dsp {
                delay: [[0.0; LATENCY]; 2],
                pos: 0,
                log: Vec::with_capacity(LOG_CAPACITY),
                voices: Vec::with_capacity(32),
                sample_rate: 48_000.0,
            }),
        }
    }

    fn gain(&self) -> f64 {
        f64::from_bits(self.gain.load(Ordering::Relaxed))
    }

    fn input_buses(&self) -> i32 {
        match self.mode {
            Mode::Sidechain => 2,
            Mode::Synth | Mode::Drums => 0,
            _ => 1,
        }
    }

    fn output_buses(&self) -> i32 {
        if self.mode == Mode::Drums {
            DRUM_OUTS.len() as i32
        } else {
            1
        }
    }

    fn is_instrument(&self) -> bool {
        matches!(self.mode, Mode::Synth | Mode::Drums)
    }

    /// The bus's own arrangement.
    fn arrangement(&self, dir: BusDirection, index: usize) -> SpeakerArrangement {
        match self.mode {
            Mode::Mono => SpeakerArr::kMono,
            Mode::Drums if dir == BusDirections_::kOutput as BusDirection => DRUM_OUTS[index],
            _ => SpeakerArr::kStereo,
        }
    }
}

/// `n` values at `ptr`, or none for a null pointer.
unsafe fn slice_or_empty<'a, T>(ptr: *const T, n: i32) -> &'a [T] {
    if ptr.is_null() || n <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, n as usize) }
    }
}

impl Processor {
    /// Mono gain: one channel in, one channel out.
    unsafe fn mono(&self, data: &ProcessData, n: usize) {
        if n == 0 || data.numInputs < 1 || data.numOutputs < 1 {
            return;
        }
        let (input, output) = unsafe { (&*data.inputs, &*data.outputs) };
        if input.numChannels != 1 || output.numChannels != 1 {
            return;
        }
        let gain = (2.0 * self.gain()) as f32;
        let src = unsafe { *input.__field0.channelBuffers32 };
        let dst = unsafe { *output.__field0.channelBuffers32 };
        for i in 0..n {
            unsafe { *dst.add(i) = *src.add(i) * gain };
        }
    }

    /// Sine voices from note events; logs every event (offset, key).
    unsafe fn synth(&self, data: &ProcessData, dsp: &mut Dsp, n: usize) {
        let mut events = Vec::new();
        if let Some(list) = unsafe { ComRef::from_raw(data.inputEvents) } {
            for i in 0..unsafe { list.getEventCount() } {
                let mut e: Event = unsafe { std::mem::zeroed() };
                if unsafe { list.getEvent(i, &mut e) } == kResultOk {
                    events.push(e);
                }
            }
        }
        let on = Event_::EventTypes_::kNoteOnEvent as u16;
        let off = Event_::EventTypes_::kNoteOffEvent as u16;
        let outs = if data.numOutputs > 0 && unsafe { (*data.outputs).numChannels } == 2 {
            unsafe { std::slice::from_raw_parts((*data.outputs).__field0.channelBuffers32, 2) }
        } else {
            &[]
        };
        let gain = (2.0 * self.gain()) as f32;
        let mut k = 0;
        for i in 0..n {
            while let Some(e) = events.get(k).filter(|e| e.sampleOffset as usize <= i) {
                k += 1;
                let (key, id) = if e.r#type == on {
                    let note = unsafe { e.__field0.noteOn };
                    let key = note.pitch.clamp(0, 127) as u8;
                    let inc = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0) / dsp.sample_rate;
                    if dsp.voices.len() < dsp.voices.capacity() {
                        dsp.voices.push((key, inc, 0.0, note.velocity * 0.25));
                    }
                    (key, LOG_NOTE_ON)
                } else if e.r#type == off {
                    let key = unsafe { e.__field0.noteOff }.pitch.clamp(0, 127) as u8;
                    if let Some(v) = dsp.voices.iter().position(|v| v.0 == key) {
                        dsp.voices.remove(v);
                    }
                    (key, LOG_NOTE_OFF)
                } else {
                    continue;
                };
                if dsp.log.len() < LOG_CAPACITY {
                    dsp.log.push((id, e.sampleOffset, f64::from(key)));
                }
            }
            let mut y = 0.0f32;
            for v in &mut dsp.voices {
                y += (v.2 * std::f64::consts::TAU).sin() as f32 * v.3;
                v.2 = (v.2 + v.1).fract();
            }
            for ch in outs {
                unsafe { *ch.add(i) = y * gain };
            }
        }
    }
}

impl IPluginBaseTrait for Processor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IConnectionPointTrait for Processor {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        if other.is_null() {
            kInvalidArgument
        } else {
            kResultOk
        }
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = self.mode.controller_cid() };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        if media_type == MediaTypes_::kEvent as MediaType {
            return i32::from(
                self.is_instrument() && dir == BusDirections_::kInput as BusDirection,
            );
        }
        if media_type != MediaTypes_::kAudio as MediaType {
            return 0;
        }
        if dir == BusDirections_::kInput as BusDirection {
            self.input_buses()
        } else {
            self.output_buses()
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if media_type == MediaTypes_::kEvent as MediaType {
            if index != 0 || unsafe { self.getBusCount(media_type, dir) } == 0 {
                return kInvalidArgument;
            }
            let bus = unsafe { &mut *bus };
            bus.mediaType = media_type;
            bus.direction = dir;
            bus.channelCount = 1;
            copy_wstring("Notes", &mut bus.name);
            bus.busType = BusTypes_::kMain as BusType;
            bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
            return kResultOk;
        }
        if media_type != MediaTypes_::kAudio as MediaType
            || index < 0
            || index >= unsafe { self.getBusCount(media_type, dir) }
        {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media_type;
        bus.direction = dir;
        bus.channelCount = self.arrangement(dir, index as usize).count_ones() as i32;
        if index == 0 {
            let name = if dir == BusDirections_::kInput as BusDirection {
                "Input"
            } else {
                "Output"
            };
            copy_wstring(name, &mut bus.name);
            bus.busType = BusTypes_::kMain as BusType;
            bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        } else {
            let name = if dir == BusDirections_::kInput as BusDirection {
                "Sidechain"
            } else {
                "Separate"
            };
            copy_wstring(name, &mut bus.name);
            bus.busType = BusTypes_::kAux as BusType;
            bus.flags = 0;
        }
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        state: TBool,
    ) -> tresult {
        if media_type == MediaTypes_::kAudio as MediaType
            && dir == BusDirections_::kInput as BusDirection
            && index == 1
        {
            self.sidechain_active.store(state != 0, Ordering::Relaxed);
        }
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        if let Ok(mut d) = self.dsp.lock() {
            d.delay = [[0.0; LATENCY]; 2];
            d.pos = 0;
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match unsafe { read_component_gain(state) } {
            Some(g) => {
                self.gain.store(g.to_bits(), Ordering::Relaxed);
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let mut out = Vec::new();
        out.extend_from_slice(STATE_MAGIC);
        out.push(self.mode as u8);
        out.extend_from_slice(&self.gain().to_le_bytes());
        if let Ok(d) = self.dsp.lock() {
            out.extend_from_slice(&(d.log.len() as u32).to_le_bytes());
            for (id, offset, value) in &d.log {
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&offset.to_le_bytes());
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        if unsafe { write_all(state, &out) } {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins != self.input_buses() || num_outs != self.output_buses() {
            return kResultFalse;
        }
        let ins = unsafe { slice_or_empty(inputs, num_ins) };
        let outs = unsafe { slice_or_empty(outputs, num_outs) };
        let input = BusDirections_::kInput as BusDirection;
        let output = BusDirections_::kOutput as BusDirection;
        let own = ins
            .iter()
            .enumerate()
            .all(|(i, a)| *a == self.arrangement(input, i))
            && outs
                .iter()
                .enumerate()
                .all(|(i, a)| *a == self.arrangement(output, i));
        if own { kResultTrue } else { kResultFalse }
    }

    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        let count = unsafe { self.getBusCount(MediaTypes_::kAudio as MediaType, dir) };
        if index < 0 || index >= count {
            return kInvalidArgument;
        }
        unsafe { *arr = self.arrangement(dir, index as usize) };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        if self.mode == Mode::Latency {
            LATENCY as u32
        } else {
            0
        }
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        let setup = unsafe { &*setup };
        if setup.symbolicSampleSize != SymbolicSampleSizes_::kSample32 as i32 {
            return kResultFalse;
        }
        if let Ok(mut d) = self.dsp.lock() {
            d.sample_rate = setup.sampleRate;
        }
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &*data };
        let Ok(mut dsp) = self.dsp.lock() else {
            return kInternalError;
        };
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for i in 0..unsafe { changes.getParameterCount() } {
                let Some(q) = (unsafe { ComRef::from_raw(changes.getParameterData(i)) }) else {
                    continue;
                };
                let id = unsafe { q.getParameterId() };
                for p in 0..unsafe { q.getPointCount() } {
                    let (mut offset, mut value) = (0, 0.0);
                    if unsafe { q.getPoint(p, &mut offset, &mut value) } != kResultOk {
                        continue;
                    }
                    if self.mode == Mode::Logger && dsp.log.len() < LOG_CAPACITY {
                        dsp.log.push((id, offset, value));
                    }
                    match id {
                        P_GAIN => self.gain.store(value.to_bits(), Ordering::Relaxed),
                        P_BYPASS => self.bypass.store(value > 0.5, Ordering::Relaxed),
                        _ => {}
                    }
                }
            }
        }
        if self.mode == Mode::Logger
            && let Some(out) = unsafe { ComRef::from_raw(data.outputParameterChanges) }
        {
            let mut index = 0;
            if let Some(q) = unsafe { ComRef::from_raw(out.addParameterData(&P_OUT, &mut index)) } {
                unsafe { q.addPoint(0, self.gain(), &mut index) };
            }
        }

        let n = data.numSamples as usize;
        if self.is_instrument() {
            unsafe { self.synth(data, &mut dsp, n) };
            return kResultOk;
        }
        if self.mode == Mode::Mono {
            unsafe { self.mono(data, n) };
            return kResultOk;
        }
        if n == 0 || data.numInputs < 1 || data.numOutputs < 1 {
            return kResultOk;
        }
        let gain = if self.bypass.load(Ordering::Relaxed) {
            1.0
        } else {
            (2.0 * self.gain()) as f32
        };
        let inputs = unsafe { std::slice::from_raw_parts(data.inputs, data.numInputs as usize) };
        let output = unsafe { &*data.outputs };
        let source = if self.mode == Mode::Sidechain {
            inputs
                .get(1)
                .filter(|b| b.numChannels == 2 && self.sidechain_active.load(Ordering::Relaxed))
        } else {
            inputs.first().filter(|b| b.numChannels == 2)
        };
        if output.numChannels != 2 {
            return kResultOk;
        }
        let outs = unsafe { std::slice::from_raw_parts(output.__field0.channelBuffers32, 2) };
        for ch in 0..2 {
            let out = unsafe { std::slice::from_raw_parts_mut(outs[ch], n) };
            let Some(src) = source else {
                out.fill(0.0);
                continue;
            };
            let ins = unsafe { std::slice::from_raw_parts(src.__field0.channelBuffers32, 2) };
            let pos = dsp.pos;
            for (i, o) in out.iter_mut().enumerate() {
                // Inputs may alias outputs: read before writing.
                let x = unsafe { *ins[ch].add(i) };
                *o = if self.mode == Mode::Latency {
                    let slot = (pos + i) % LATENCY;
                    let y = dsp.delay[ch][slot];
                    dsp.delay[ch][slot] = x;
                    y * gain
                } else {
                    x * gain
                };
            }
        }
        dsp.pos = (dsp.pos + n) % LATENCY;
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

struct Controller {
    mode: Mode,
    values: RefCell<[f64; 4]>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    connected: Cell<bool>,
}

impl Class for Controller {
    type Interfaces = (IEditController, IConnectionPoint);
}

impl Controller {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            values: RefCell::new([0.5, 0.25, 0.0, 0.0]),
            handler: RefCell::new(None),
            connected: Cell::new(false),
        }
    }

    fn has(&self, id: ParamID) -> bool {
        self.mode.params().contains(&id)
    }
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        self.handler.borrow_mut().take();
        kResultOk
    }
}

impl IConnectionPointTrait for Controller {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        self.connected.set(!other.is_null());
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        self.connected.set(false);
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        match unsafe { read_component_gain(state) } {
            Some(g) => {
                self.values.borrow_mut()[P_GAIN as usize] = g;
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let mut buf = [0u8; 12];
        if !unsafe { read_exact(state, &mut buf) } || &buf[..4] != CTRL_MAGIC {
            return kResultFalse;
        }
        let fixed = f64::from_le_bytes(buf[4..12].try_into().unwrap_or_default());
        self.values.borrow_mut()[P_FIXED as usize] = fixed;
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let mut out = CTRL_MAGIC.to_vec();
        out.extend_from_slice(&self.values.borrow()[P_FIXED as usize].to_le_bytes());
        if unsafe { write_all(state, &out) } {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getParameterCount(&self) -> i32 {
        self.mode.params().len() as i32
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let Some(&id) = self.mode.params().get(index as usize) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        use ParameterInfo_::ParameterFlags_::*;
        let (title, units, steps, default, flags) = match id {
            P_GAIN => ("Gain", "x", 0, 0.5, kCanAutomate),
            P_FIXED => ("Fixed", "", 0, 0.25, 0),
            P_OUT => ("Output", "", 0, 0.0, kIsReadOnly),
            _ => ("Bypass", "", 1, 0.0, kCanAutomate | kIsBypass),
        };
        info.id = id;
        copy_wstring(title, &mut info.title);
        copy_wstring(title, &mut info.shortTitle);
        copy_wstring(units, &mut info.units);
        info.stepCount = steps;
        info.defaultNormalizedValue = default;
        info.unitId = 0;
        info.flags = flags as i32;
        kResultOk
    }

    unsafe fn getParamStringByValue(
        &self,
        id: ParamID,
        value: ParamValue,
        string: *mut String128,
    ) -> tresult {
        if !self.has(id) {
            return kInvalidArgument;
        }
        let text = match id {
            P_GAIN => format!("{:.3}", 2.0 * value),
            P_BYPASS => (if value > 0.5 { "On" } else { "Off" }).to_owned(),
            _ => format!("{value:.3}"),
        };
        copy_wstring(&text, unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        id: ParamID,
        string: *mut TChar,
        value: *mut ParamValue,
    ) -> tresult {
        if !self.has(id) {
            return kInvalidArgument;
        }
        let mut len = 0;
        while unsafe { *string.add(len) } != 0 {
            len += 1;
        }
        let units = unsafe { std::slice::from_raw_parts(string as *const u16, len) };
        let Ok(text) = String::from_utf16(units) else {
            return kInvalidArgument;
        };
        let Ok(plain) = text.trim().parse::<f64>() else {
            return kInvalidArgument;
        };
        unsafe { *value = if id == P_GAIN { plain / 2.0 } else { plain }.clamp(0.0, 1.0) };
        kResultOk
    }

    unsafe fn normalizedParamToPlain(&self, id: ParamID, value: ParamValue) -> ParamValue {
        if id == P_GAIN { 2.0 * value } else { value }
    }

    unsafe fn plainParamToNormalized(&self, id: ParamID, plain: ParamValue) -> ParamValue {
        if id == P_GAIN { plain / 2.0 } else { plain }
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        if self.has(id) {
            self.values.borrow()[id as usize]
        } else {
            0.0
        }
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        if !self.has(id) {
            return kInvalidArgument;
        }
        self.values.borrow_mut()[id as usize] = value;
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.borrow_mut() = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }

    /// Only the gain class has an editor.
    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        if self.mode != Mode::Gain {
            return std::ptr::null_mut();
        }
        let view = ComWrapper::new(View::default());
        let Some(ptr) = view.to_com_ptr::<IPlugView>() else {
            return std::ptr::null_mut();
        };
        // The view needs its own pointer for IPlugFrame::resizeView.
        view.me.set(ptr.as_ptr());
        ptr.into_raw()
    }
}

/// Initial editor size; the view asks for [`ATTACHED_SIZE`] when attached
/// and, on Linux, for [`TIMER_SIZE`] from a run-loop timer.
pub const INITIAL_SIZE: (i32, i32) = (400, 300);
pub const ATTACHED_SIZE: (i32, i32) = (480, 320);
pub const TIMER_SIZE: (i32, i32) = (500, 330);

/// Test editor: draws nothing; exercises attach, resize requests and the
/// Linux run loop.
struct View {
    me: Cell<*mut IPlugView>,
    size: Cell<(i32, i32)>,
    frame: RefCell<Option<ComPtr<IPlugFrame>>>,
    attached: Cell<bool>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            me: Cell::new(std::ptr::null_mut()),
            size: Cell::new(INITIAL_SIZE),
            frame: RefCell::new(None),
            attached: Cell::new(false),
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl Class for View {
    type Interfaces = (IPlugView,);
}

#[cfg(target_os = "linux")]
impl Class for View {
    type Interfaces = (IPlugView, Linux::ITimerHandler);
}

impl View {
    fn request(&self, (w, h): (i32, i32)) {
        let frame = self.frame.borrow().clone();
        if let Some(f) = frame {
            let mut r = ViewRect {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            unsafe { f.resizeView(self.me.get(), &mut r) };
        }
    }
}

#[cfg(target_os = "linux")]
impl Linux::ITimerHandlerTrait for View {
    unsafe fn onTimer(&self) {
        if self.attached.get() && self.size.get() != TIMER_SIZE {
            self.request(TIMER_SIZE);
        }
    }
}

impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, kind: FIDString) -> tresult {
        let kind = unsafe { std::ffi::CStr::from_ptr(kind) }.to_bytes();
        if matches!(kind, b"X11EmbedWindowID" | b"HWND" | b"NSView") {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn attached(&self, parent: *mut c_void, _kind: FIDString) -> tresult {
        if parent.is_null() {
            return kInvalidArgument;
        }
        self.attached.set(true);
        self.request(ATTACHED_SIZE);
        #[cfg(target_os = "linux")]
        {
            let frame = self.frame.borrow().clone();
            if let Some(run_loop) = frame.and_then(|f| f.cast::<Linux::IRunLoop>()) {
                let me = self.me.get() as *mut FUnknown;
                let timer =
                    unsafe { ComRef::from_raw(me) }.and_then(|u| u.cast::<Linux::ITimerHandler>());
                if let Some(t) = timer {
                    unsafe { Linux::IRunLoopTrait::registerTimer(&run_loop, t.as_ptr(), 10) };
                }
            }
        }
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        #[cfg(target_os = "linux")]
        {
            let frame = self.frame.borrow().clone();
            if let Some(run_loop) = frame.and_then(|f| f.cast::<Linux::IRunLoop>()) {
                let me = self.me.get() as *mut FUnknown;
                if let Some(t) =
                    unsafe { ComRef::from_raw(me) }.and_then(|u| u.cast::<Linux::ITimerHandler>())
                {
                    unsafe { Linux::IRunLoopTrait::unregisterTimer(&run_loop, t.as_ptr()) };
                }
            }
        }
        self.attached.set(false);
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _code: i16, _modifiers: i16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _code: i16, _modifiers: i16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        let (w, h) = self.size.get();
        unsafe {
            *size = ViewRect {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            }
        };
        kResultOk
    }

    unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kInvalidArgument;
        }
        let r = unsafe { &*new_size };
        self.size.set((r.right - r.left, r.bottom - r.top));
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        *self.frame.borrow_mut() = unsafe { ComRef::from_raw(frame) }.map(|f| f.to_com_ptr());
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultFalse
    }

    unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
        kResultFalse
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

/// Class index → (mode, is_controller).
fn class_at(index: i32) -> Option<(Mode, bool)> {
    let i = usize::try_from(index).ok()?;
    MODES.get(i / 2).map(|m| (*m, i % 2 == 1))
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_cstring("jdance tests", &mut info.vendor);
        copy_cstring("https://github.com/MertzZzzz/jdance", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        (MODES.len() * 2) as i32
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let Some((mode, controller)) = class_at(index) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        info.cid = if controller {
            mode.controller_cid()
        } else {
            mode.processor_cid()
        };
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        let category = if controller {
            "Component Controller Class"
        } else {
            "Audio Module Class"
        };
        copy_cstring(category, &mut info.category);
        copy_cstring(mode.name(), &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let cid = unsafe { *(cid as *const TUID) };
        let instance = MODES.iter().find_map(|&m| {
            if cid == m.processor_cid() {
                ComWrapper::new(Processor::new(m)).to_com_ptr::<FUnknown>()
            } else if cid == m.controller_cid() {
                ComWrapper::new(Controller::new(m)).to_com_ptr::<FUnknown>()
            } else {
                None
            }
        });
        match instance {
            Some(instance) => {
                let ptr = instance.as_ptr();
                unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, obj) }
            }
            None => {
                unsafe { *obj = std::ptr::null_mut() };
                kInvalidArgument
            }
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let Some((mode, controller)) = class_at(index) else {
            return kInvalidArgument;
        };
        let mut base: PClassInfo = unsafe { std::mem::zeroed() };
        unsafe { self.getClassInfo(index, &mut base) };
        let info = unsafe { &mut *info };
        info.cid = base.cid;
        info.cardinality = base.cardinality;
        info.category = base.category;
        info.name = base.name;
        info.classFlags = if controller {
            0
        } else {
            ComponentFlags_::kDistributable as u32
        };
        let sub = match mode {
            Mode::Sidechain => "Fx|Dynamics",
            Mode::Synth => "Instrument|Synth",
            Mode::Drums => "Instrument|Drum",
            _ => "Fx",
        };
        copy_cstring(sub, &mut info.subCategories);
        copy_cstring("jdance tests", &mut info.vendor);
        copy_cstring("1.0.0", &mut info.version);
        copy_cstring("VST 3.8.0", &mut info.sdkVersion);
        kResultOk
    }
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleExit() -> bool {
    true
}

// Older hosts look up the capitalized names.
#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn BundleEntry(bundle: *mut c_void) -> bool {
    bundleEntry(bundle)
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn BundleExit() -> bool {
    bundleExit()
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    match std::env::var("JDANCE_TEST_FX_FAULT").as_deref() {
        Ok("crash") => std::process::abort(),
        Ok("hang") => loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        },
        _ => {}
    }
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map_or(std::ptr::null_mut(), ComPtr::into_raw)
}
