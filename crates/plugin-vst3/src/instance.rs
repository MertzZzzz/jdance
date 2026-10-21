// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin instances: lifecycle (PLG-030), buses (PLG-031), processing
//! (PLG-041/042) and state (PLG-070).

use crate::com::{
    ComponentHandler, EventList, HostApp, MemStream, ParamChanges, copy_wstring, wstring,
};
use crate::module::Vst3Module;
use crate::uid;
use jdance_plugin_api::{
    NoteEvent, NoteKind, ParamFlags, ParamInfo, PluginController, PluginError, PluginEvent,
    PluginProcessor, PluginState, ProcessSetupInfo, Transport,
};
use std::ffi::c_void;
use std::sync::Arc;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper, Interface};

/// Capacity of the UI ↔ processor parameter queues.
const PARAM_QUEUE: usize = 1024;
/// Automation points per block (all parameters).
const AUTOMATION_POINTS: usize = 2048;

fn check(r: tresult, what: &str) -> Result<(), PluginError> {
    if r == kResultOk {
        Ok(())
    } else {
        Err(PluginError::Failed(format!("{what} returned {r}")))
    }
}

/// Everything shared by the two halves of an instance. Dropped when both
/// halves are gone; tears the plugin down in reverse order (PLG-030 step
/// 12). Field order matters: the module is released last.
struct Core {
    processing: bool,
    active: bool,
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    /// Separate controller object, initialized by the host.
    separate: Option<ComPtr<IEditController>>,
    controller: Option<ComPtr<IEditController>>,
    processor: Option<ComPtr<IAudioProcessor>>,
    component_initialized: bool,
    component: ComPtr<IComponent>,
    handler: ComWrapper<ComponentHandler>,
    host: ComWrapper<HostApp>,
    module: Arc<Vst3Module>,
}

// SAFETY: the processor half only calls `IAudioProcessor::process` on the
// audio thread; everything else is called from the thread that owns the
// controller half, or from `Drop` after both halves are gone.
unsafe impl Send for Core {}
unsafe impl Sync for Core {}

impl Drop for Core {
    fn drop(&mut self) {
        unsafe {
            if let Some(p) = &self.processor
                && self.processing
            {
                p.setProcessing(0);
            }
            if self.active {
                self.component.setActive(0);
            }
            if let Some((a, b)) = self.connections.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(std::ptr::null_mut());
            }
            if let Some(c) = self.separate.take() {
                c.terminate();
            }
            self.controller = None;
            self.processor = None;
            if self.component_initialized {
                self.component.terminate();
            }
        }
        let _ = (&self.handler, &self.host, &self.module);
    }
}

fn host_unknown(core: &Core) -> *mut FUnknown {
    core.host
        .as_com_ref::<FUnknown>()
        .map_or(std::ptr::null_mut(), |r| r.as_ptr())
}

unsafe fn create<I: Interface>(module: &Vst3Module, cid: &TUID) -> Result<ComPtr<I>, PluginError> {
    let mut obj: *mut c_void = std::ptr::null_mut();
    let r = unsafe {
        module.factory().createInstance(
            cid.as_ptr() as FIDString,
            I::IID.as_ptr() as FIDString,
            &mut obj,
        )
    };
    if r != kResultOk {
        return Err(PluginError::Failed(format!("createInstance returned {r}")));
    }
    unsafe { ComPtr::from_raw(obj as *mut I) }
        .ok_or_else(|| PluginError::Failed("createInstance returned null".into()))
}

fn stream(bytes: &[u8]) -> ComWrapper<MemStream> {
    ComWrapper::new(MemStream::from_bytes(bytes))
}

fn stream_ptr(s: &ComWrapper<MemStream>) -> *mut IBStream {
    s.as_com_ref::<IBStream>()
        .map_or(std::ptr::null_mut(), |r| r.as_ptr())
}

/// Bus roles chosen for the instance (PLG-031).
#[derive(Debug, Clone, Copy)]
struct Buses {
    inputs: usize,
    outputs: usize,
    main_in: Option<usize>,
    aux_in: Option<usize>,
    main_out: usize,
    has_events: bool,
}

unsafe fn configure_buses(
    component: &ComPtr<IComponent>,
    processor: &ComPtr<IAudioProcessor>,
) -> Result<Buses, PluginError> {
    let audio = MediaTypes_::kAudio as MediaType;
    let event = MediaTypes_::kEvent as MediaType;
    let input = BusDirections_::kInput as BusDirection;
    let output = BusDirections_::kOutput as BusDirection;
    let count = |m, d| unsafe { component.getBusCount(m, d) }.max(0) as usize;
    let bus_type = |d, i: usize| {
        let mut info: BusInfo = unsafe { std::mem::zeroed() };
        (unsafe { component.getBusInfo(audio, d, i as i32, &mut info) } == kResultOk)
            .then_some(info.busType)
    };
    let (inputs, outputs) = (count(audio, input), count(audio, output));
    let has_events = count(event, input) > 0;
    if outputs == 0 {
        return Err(PluginError::Unsupported(
            "plugin has no audio output".into(),
        ));
    }
    let main = BusTypes_::kMain as BusType;
    let aux = BusTypes_::kAux as BusType;
    let main_in = (0..inputs).find(|&i| bus_type(input, i) == Some(main));
    let aux_in = (0..inputs).find(|&i| bus_type(input, i) == Some(aux));
    let main_out = (0..outputs)
        .find(|&i| bus_type(output, i) == Some(main))
        .unwrap_or(0);
    unsafe {
        for i in 0..inputs {
            let on = Some(i) == main_in || Some(i) == aux_in;
            component.activateBus(audio, input, i as i32, u8::from(on));
        }
        for i in 0..outputs {
            component.activateBus(audio, output, i as i32, u8::from(i == main_out));
        }
        // The first event input carries notes for instruments
        // (PLG-031); other event buses stay off.
        for d in [input, output] {
            for i in 0..count(event, d) {
                let on = d == input && i == 0;
                component.activateBus(event, d, i as i32, u8::from(on));
            }
        }
    }
    let mut ins = vec![SpeakerArr::kStereo; inputs];
    let mut outs = vec![SpeakerArr::kStereo; outputs];
    let r = unsafe {
        processor.setBusArrangements(
            ins.as_mut_ptr(),
            inputs as i32,
            outs.as_mut_ptr(),
            outputs as i32,
        )
    };
    if r != kResultTrue {
        // Mono-only plugins are adapted in a later step (PLG-031).
        return Err(PluginError::Unsupported(
            "plugin does not accept a stereo bus arrangement".into(),
        ));
    }
    Ok(Buses {
        inputs,
        outputs,
        main_in,
        aux_in,
        main_out,
        has_events,
    })
}

unsafe fn read_params(controller: &ComPtr<IEditController>) -> Vec<ParamInfo> {
    use ParameterInfo_::ParameterFlags_::*;
    let n = unsafe { controller.getParameterCount() }.max(0);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        if unsafe { controller.getParameterInfo(i, &mut info) } != kResultOk {
            continue;
        }
        let f = i64::from(info.flags);
        out.push(ParamInfo {
            id: info.id,
            name: wstring(&info.title),
            units: wstring(&info.units),
            default_normalized: info.defaultNormalizedValue,
            step_count: info.stepCount.max(0) as u32,
            flags: ParamFlags {
                automatable: f & kCanAutomate as i64 != 0,
                read_only: f & kIsReadOnly as i64 != 0,
                is_bypass: f & kIsBypass as i64 != 0,
            },
        });
    }
    out
}

/// Creates and activates an instance of `class_id` (PLG-030 steps 2–10).
/// `state`, if given, is restored before activation.
pub fn instantiate(
    module: &Arc<Vst3Module>,
    class_id: &str,
    setup: &ProcessSetupInfo,
    state: Option<&PluginState>,
) -> Result<(Vst3Processor, Vst3Controller), PluginError> {
    let cid = uid::from_hex(class_id)
        .ok_or_else(|| PluginError::NotFound(format!("bad class id {class_id}")))?;
    if !module
        .descriptors()
        .iter()
        .any(|d| d.plugin.class_id == class_id)
    {
        return Err(PluginError::NotFound(format!(
            "{class_id} is not in {}",
            module.path().display()
        )));
    }
    let component = unsafe { create::<IComponent>(module, &cid)? };
    let mut core = Core {
        processing: false,
        active: false,
        connections: None,
        separate: None,
        controller: None,
        processor: None,
        component_initialized: false,
        component,
        handler: ComWrapper::new(ComponentHandler::default()),
        host: ComWrapper::new(HostApp),
        module: Arc::clone(module),
    };
    unsafe {
        check(
            core.component.initialize(host_unknown(&core)),
            "IComponent::initialize",
        )?;
        core.component_initialized = true;

        // Controller: the component itself, or a separate class.
        let controller = match core.component.cast::<IEditController>() {
            Some(c) => c,
            None => {
                let mut ccid: TUID = [0; 16];
                check(
                    core.component.getControllerClassId(&mut ccid),
                    "getControllerClassId",
                )?;
                let c = create::<IEditController>(module, &ccid)?;
                check(
                    c.initialize(host_unknown(&core)),
                    "IEditController::initialize",
                )?;
                core.separate = Some(c.clone());
                if let (Some(a), Some(b)) = (
                    core.component.cast::<IConnectionPoint>(),
                    c.cast::<IConnectionPoint>(),
                ) {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                    core.connections = Some((a, b));
                }
                c
            }
        };
        core.controller = Some(controller.clone());

        // State: component first, then its copy to the controller.
        let component_state = match state {
            Some(s) if !s.component.is_empty() => {
                let st = stream(&s.component);
                check(
                    core.component.setState(stream_ptr(&st)),
                    "IComponent::setState",
                )?;
                s.component.clone()
            }
            _ => {
                let st = stream(&[]);
                core.component.getState(stream_ptr(&st));
                st.bytes()
            }
        };
        let st = stream(&component_state);
        controller.setComponentState(stream_ptr(&st));
        if let Some(s) = state
            && !s.controller.is_empty()
        {
            let st = stream(&s.controller);
            controller.setState(stream_ptr(&st));
        }
        if let Some(h) = core.handler.as_com_ref::<IComponentHandler>() {
            controller.setComponentHandler(h.as_ptr());
        }

        let processor = core
            .component
            .cast::<IAudioProcessor>()
            .ok_or_else(|| PluginError::Unsupported("no IAudioProcessor".into()))?;
        core.processor = Some(processor.clone());
        let buses = configure_buses(&core.component, &processor)?;
        if processor.canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32) != kResultOk {
            return Err(PluginError::Unsupported(
                "no 32-bit float processing".into(),
            ));
        }
        let max_block = setup.max_block.max(1);
        let mut ps = ProcessSetup {
            processMode: if setup.offline {
                ProcessModes_::kOffline
            } else {
                ProcessModes_::kRealtime
            } as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: max_block as i32,
            sampleRate: setup.sample_rate,
        };
        check(processor.setupProcessing(&mut ps), "setupProcessing")?;
        check(core.component.setActive(1), "setActive")?;
        core.active = true;
        let latency = processor.getLatencySamples();
        // Some plugins return kNotImplemented here; that is not an error.
        processor.setProcessing(1);
        core.processing = true;

        let params = read_params(&controller);
        let core = Arc::new(core);
        let (to_rt, from_ui) = rtrb::RingBuffer::new(PARAM_QUEUE);
        let (to_ui, from_rt) = rtrb::RingBuffer::new(PARAM_QUEUE);
        let processor = Vst3Processor::new(
            Arc::clone(&core),
            processor,
            buses,
            setup,
            latency,
            from_ui,
            to_ui,
        );
        let controller = Vst3Controller {
            core,
            controller,
            params,
            to_rt,
            from_rt,
        };
        Ok((processor, controller))
    }
}

/// A note event in VST3 form, `base` samples into the block.
fn vst_event(e: &NoteEvent, base: usize) -> Event {
    let mut ev: Event = unsafe { std::mem::zeroed() };
    ev.busIndex = 0;
    ev.sampleOffset = (e.offset as usize).saturating_sub(base) as i32;
    ev.flags = Event_::EventFlags_::kIsLive as u16;
    match e.kind {
        NoteKind::On { velocity } => {
            ev.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
            ev.__field0.noteOn = NoteOnEvent {
                channel: 0,
                pitch: i16::from(e.key),
                tuning: 0.0,
                velocity,
                length: 0,
                noteId: e.note_id,
            };
        }
        NoteKind::Off => {
            ev.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
            ev.__field0.noteOff = NoteOffEvent {
                channel: 0,
                pitch: i16::from(e.key),
                velocity: 0.0,
                noteId: e.note_id,
                tuning: 0.0,
            };
        }
    }
    ev
}

/// Realtime half (PLG-011). All buffers are allocated here (PLG-041).
pub struct Vst3Processor {
    processor: ComPtr<IAudioProcessor>,
    buses: Buses,
    max_block: usize,
    offline: bool,
    sample_rate: f64,
    latency: u32,
    input: [Vec<f32>; 2],
    sidechain: [Vec<f32>; 2],
    output: [Vec<f32>; 2],
    silence: Vec<f32>,
    scratch: Vec<f32>,
    /// Channel pointer pairs per bus; heap storage keeps them stable.
    in_ptrs: Vec<[*mut f32; 2]>,
    out_ptrs: Vec<[*mut f32; 2]>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    in_changes: ComWrapper<ParamChanges>,
    out_changes: ComWrapper<ParamChanges>,
    events: ComWrapper<EventList>,
    context: ProcessContext,
    continuous: i64,
    from_ui: rtrb::Consumer<(u32, f64)>,
    to_ui: rtrb::Producer<(u32, f64)>,
    /// Automation points for the next call: (id, offset, value).
    automation: Vec<(u32, u32, f64)>,
    /// Keeps the plugin alive; dropped last.
    _core: Arc<Core>,
}

// SAFETY: the raw pointers point into buffers owned by this struct; the
// plugin's audio processor is only used from the thread that owns this
// value (VST3 allows `process` on any single thread).
unsafe impl Send for Vst3Processor {}

impl std::fmt::Debug for Vst3Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Processor")
            .field("latency", &self.latency)
            .field("buses", &self.buses)
            .finish_non_exhaustive()
    }
}

impl Vst3Processor {
    fn new(
        core: Arc<Core>,
        processor: ComPtr<IAudioProcessor>,
        buses: Buses,
        setup: &ProcessSetupInfo,
        latency: u32,
        from_ui: rtrb::Consumer<(u32, f64)>,
        to_ui: rtrb::Producer<(u32, f64)>,
    ) -> Self {
        let n = setup.max_block.max(1);
        let buf = || [vec![0.0f32; n], vec![0.0f32; n]];
        let mut p = Self {
            processor,
            buses,
            max_block: n,
            offline: setup.offline,
            sample_rate: setup.sample_rate,
            latency,
            input: buf(),
            sidechain: buf(),
            output: buf(),
            silence: vec![0.0; n],
            scratch: vec![0.0; n],
            in_ptrs: vec![[std::ptr::null_mut(); 2]; buses.inputs],
            out_ptrs: vec![[std::ptr::null_mut(); 2]; buses.outputs],
            in_buses: Vec::with_capacity(buses.inputs),
            out_buses: Vec::with_capacity(buses.outputs),
            in_changes: ComWrapper::new(ParamChanges::new()),
            out_changes: ComWrapper::new(ParamChanges::new()),
            events: ComWrapper::new(EventList::new()),
            context: unsafe { std::mem::zeroed() },
            continuous: 0,
            from_ui,
            to_ui,
            automation: Vec::with_capacity(AUTOMATION_POINTS),
            _core: core,
        };
        for i in 0..buses.inputs {
            p.in_ptrs[i] = if Some(i) == buses.main_in {
                [p.input[0].as_mut_ptr(), p.input[1].as_mut_ptr()]
            } else if Some(i) == buses.aux_in {
                [p.sidechain[0].as_mut_ptr(), p.sidechain[1].as_mut_ptr()]
            } else {
                [p.silence.as_mut_ptr(); 2]
            };
        }
        for i in 0..buses.outputs {
            p.out_ptrs[i] = if i == buses.main_out {
                [p.output[0].as_mut_ptr(), p.output[1].as_mut_ptr()]
            } else {
                [p.scratch.as_mut_ptr(); 2]
            };
        }
        for ptrs in &mut p.in_ptrs {
            p.in_buses.push(AudioBusBuffers {
                numChannels: 2,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: ptrs.as_mut_ptr(),
                },
            });
        }
        for ptrs in &mut p.out_ptrs {
            p.out_buses.push(AudioBusBuffers {
                numChannels: 2,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: ptrs.as_mut_ptr(),
                },
            });
        }
        p
    }

    fn fill_context(&mut self, t: &Transport, offset: usize) {
        use ProcessContext_::StatesAndFlags_::*;
        let c = &mut self.context;
        let mut state = kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kContTimeValid;
        if t.playing {
            state |= kPlaying;
        }
        let quarters_per_sample = t.tempo_bpm / 60.0 / self.sample_rate;
        c.sampleRate = self.sample_rate;
        c.tempo = t.tempo_bpm;
        c.timeSigNumerator = 4;
        c.timeSigDenominator = 4;
        c.projectTimeSamples = t.project_time_samples + offset as i64;
        c.projectTimeMusic = t.project_time_quarters + offset as f64 * quarters_per_sample;
        c.barPositionMusic = (c.projectTimeMusic / 4.0).floor() * 4.0;
        state |= kBarPositionValid;
        if let Some((a, b)) = t.cycle {
            state |= kCycleActive | kCycleValid;
            c.cycleStartMusic = a;
            c.cycleEndMusic = b;
        }
        c.continousTimeSamples = self.continuous;
        c.state = state as u32;
    }

    /// Moves queued automation points inside `[start, start + n)` into the
    /// parameter changes of this chunk, rebased to it.
    fn chunk_automation(&mut self, start: usize, n: usize) {
        let end = start + n;
        let in_changes = &self.in_changes;
        self.automation.retain(|&(id, offset, value)| {
            let o = offset as usize;
            if o >= start && o < end {
                in_changes.add(id, (o - start) as i32, value);
                false
            } else {
                true
            }
        });
    }

    fn run(&mut self, n: usize) -> bool {
        self.out_changes.clear();
        let mut data = ProcessData {
            processMode: if self.offline {
                ProcessModes_::kOffline
            } else {
                ProcessModes_::kRealtime
            } as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: n as i32,
            numInputs: self.buses.inputs as i32,
            numOutputs: self.buses.outputs as i32,
            inputs: self.in_buses.as_mut_ptr(),
            outputs: self.out_buses.as_mut_ptr(),
            inputParameterChanges: self
                .in_changes
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            outputParameterChanges: self
                .out_changes
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            inputEvents: self
                .events
                .as_com_ref::<IEventList>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut self.context,
        };
        let ok = unsafe { self.processor.process(&mut data) } == kResultOk;
        self.in_changes.clear();
        self.events.clear();
        let to_ui = &mut self.to_ui;
        self.out_changes.for_each_last(|id, v| {
            // A full queue drops the update (RT-011); the next one wins.
            let _ = to_ui.push((id, v));
        });
        self.continuous += n as i64;
        ok
    }
}

impl PluginProcessor for Vst3Processor {
    fn process(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        sidechain: Option<(&[f32], &[f32])>,
        transport: &Transport,
    ) {
        let total = left.len().min(right.len());
        while let Ok((id, v)) = self.from_ui.pop() {
            if !self.in_changes.add(id, 0, v) {
                break;
            }
        }
        let mut done = 0;
        while done < total {
            let n = (total - done).min(self.max_block);
            let range = done..done + n;
            self.input[0][..n].copy_from_slice(&left[range.clone()]);
            self.input[1][..n].copy_from_slice(&right[range.clone()]);
            match sidechain {
                Some((sl, sr)) if sl.len() >= done + n && sr.len() >= done + n => {
                    self.sidechain[0][..n].copy_from_slice(&sl[range.clone()]);
                    self.sidechain[1][..n].copy_from_slice(&sr[range.clone()]);
                }
                _ => {
                    self.sidechain[0][..n].fill(0.0);
                    self.sidechain[1][..n].fill(0.0);
                }
            }
            self.silence[..n].fill(0.0);
            self.chunk_automation(done, n);
            self.fill_context(transport, done);
            // On failure the input passes through unchanged.
            if self.run(n) {
                left[range.clone()].copy_from_slice(&self.output[0][..n]);
                right[range].copy_from_slice(&self.output[1][..n]);
            }
            done += n;
        }
        self.automation.clear();
    }

    fn process_instrument(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        events: &[NoteEvent],
        transport: &Transport,
    ) {
        let total = left.len().min(right.len());
        while let Ok((id, v)) = self.from_ui.pop() {
            if !self.in_changes.add(id, 0, v) {
                break;
            }
        }
        let mut done = 0;
        let mut next = 0;
        while done < total {
            let n = (total - done).min(self.max_block);
            for ch in 0..2 {
                self.input[ch][..n].fill(0.0);
                self.sidechain[ch][..n].fill(0.0);
            }
            self.silence[..n].fill(0.0);
            // Events of this chunk, offsets relative to its start.
            while let Some(e) = events.get(next).filter(|e| (e.offset as usize) < done + n) {
                next += 1;
                if self.buses.has_events {
                    self.events.push(vst_event(e, done));
                }
            }
            self.chunk_automation(done, n);
            self.fill_context(transport, done);
            let range = done..done + n;
            if self.run(n) {
                left[range.clone()].copy_from_slice(&self.output[0][..n]);
                right[range].copy_from_slice(&self.output[1][..n]);
            } else {
                left[range.clone()].fill(0.0);
                right[range].fill(0.0);
            }
            done += n;
        }
        self.automation.clear();
    }

    fn queue_param(&mut self, id: u32, offset: u32, value: f64) {
        if self.automation.len() < self.automation.capacity() {
            self.automation.push((id, offset, value.clamp(0.0, 1.0)));
        }
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn has_sidechain_input(&self) -> bool {
        self.buses.aux_in.is_some()
    }
}

/// Non-realtime half (PLG-011): parameters, state, events.
pub struct Vst3Controller {
    controller: ComPtr<IEditController>,
    params: Vec<ParamInfo>,
    to_rt: rtrb::Producer<(u32, f64)>,
    from_rt: rtrb::Consumer<(u32, f64)>,
    core: Arc<Core>,
}

impl std::fmt::Debug for Vst3Controller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Controller")
            .field("params", &self.params.len())
            .finish_non_exhaustive()
    }
}

impl Vst3Controller {
    /// Opens the plugin's own editor in a new window (UI thread only;
    /// macOS: main thread). `Ok(None)` when the plugin has no editor.
    pub fn open_editor(&self, title: &str) -> Result<Option<crate::Vst3Editor>, PluginError> {
        crate::Vst3Editor::open(&self.controller, self.core.clone(), title)
    }
}

impl PluginController for Vst3Controller {
    fn params(&self) -> &[ParamInfo] {
        &self.params
    }

    fn param_normalized(&self, id: u32) -> f64 {
        unsafe { self.controller.getParamNormalized(id) }
    }

    fn set_param_normalized(&mut self, id: u32, value: f64) {
        let value = value.clamp(0.0, 1.0);
        unsafe { self.controller.setParamNormalized(id, value) };
        let _ = self.to_rt.push((id, value));
    }

    fn param_text(&self, id: u32, value: f64) -> String {
        let mut s: String128 = [0; 128];
        if unsafe { self.controller.getParamStringByValue(id, value, &mut s) } == kResultOk {
            wstring(&s)
        } else {
            format!("{value:.3}")
        }
    }

    fn param_from_text(&self, id: u32, text: &str) -> Option<f64> {
        let mut s: String128 = [0; 128];
        copy_wstring(text, &mut s);
        let mut v = 0.0;
        (unsafe {
            self.controller
                .getParamValueByString(id, s.as_mut_ptr(), &mut v)
        } == kResultOk)
            .then_some(v)
    }

    fn state(&mut self) -> Result<PluginState, PluginError> {
        let c = stream(&[]);
        check(
            unsafe { self.core.component.getState(stream_ptr(&c)) },
            "IComponent::getState",
        )?;
        let e = stream(&[]);
        // Controllers without state return kNotImplemented: empty blob.
        unsafe { self.controller.getState(stream_ptr(&e)) };
        Ok(PluginState {
            component: c.bytes(),
            controller: e.bytes(),
        })
    }

    fn poll(&mut self) -> Vec<PluginEvent> {
        let mut out = self.core.handler.take_events();
        for e in &out {
            // Edits in the plugin's GUI go to the processor (PLG-042).
            if let PluginEvent::ParamChanged { id, value } = *e {
                let _ = self.to_rt.push((id, value));
            }
        }
        while let Ok((id, value)) = self.from_rt.pop() {
            unsafe { self.controller.setParamNormalized(id, value) };
            out.push(PluginEvent::ParamChanged { id, value });
        }
        out
    }
}
