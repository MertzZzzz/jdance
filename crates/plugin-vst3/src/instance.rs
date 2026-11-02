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
        // Plugins drawing with OpenGL may release their context here.
        #[cfg(windows)]
        let _gl = crate::editor::GlGuard::new();
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

/// Bus roles chosen for the instance (PLG-031) and the channel count of
/// every audio bus as agreed with the plugin.
#[derive(Debug, Clone)]
struct Buses {
    inputs: usize,
    outputs: usize,
    main_in: Option<usize>,
    aux_in: Option<usize>,
    main_out: usize,
    has_events: bool,
    in_channels: Vec<usize>,
    out_channels: Vec<usize>,
}

impl Buses {
    fn channels_in(&self, bus: Option<usize>) -> usize {
        bus.and_then(|b| self.in_channels.get(b).copied())
            .unwrap_or(0)
    }
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
    // Bus arrangements (PLG-031). First stereo everywhere; then stereo
    // on the main buses and the plugin's own arrangement elsewhere
    // (multi-output instruments with mono outputs); else the plugin keeps
    // its own and the host adapts to it, as VST3 asks of a host when
    // `setBusArrangements` is refused.
    let own = |d, n: usize| -> Vec<SpeakerArrangement> {
        (0..n)
            .map(|i| {
                let mut a: SpeakerArrangement = 0;
                if unsafe { processor.getBusArrangement(d, i as i32, &mut a) } == kResultOk {
                    a
                } else {
                    // No answer: as many channels as the bus reports.
                    let mut info: BusInfo = unsafe { std::mem::zeroed() };
                    unsafe { component.getBusInfo(audio, d, i as i32, &mut info) };
                    match info.channelCount {
                        1 => SpeakerArr::kMono,
                        _ => SpeakerArr::kStereo,
                    }
                }
            })
            .collect()
    };
    let (own_in, own_out) = (own(input, inputs), own(output, outputs));
    let mut main_in_stereo = own_in.clone();
    for i in [main_in, aux_in].into_iter().flatten() {
        main_in_stereo[i] = SpeakerArr::kStereo;
    }
    let mut main_out_stereo = own_out.clone();
    main_out_stereo[main_out] = SpeakerArr::kStereo;
    let proposals = [
        (
            vec![SpeakerArr::kStereo; inputs],
            vec![SpeakerArr::kStereo; outputs],
        ),
        (main_in_stereo, main_out_stereo),
    ];
    for (mut ins, mut outs) in proposals {
        let r = unsafe {
            processor.setBusArrangements(
                ins.as_mut_ptr(),
                inputs as i32,
                outs.as_mut_ptr(),
                outputs as i32,
            )
        };
        if r == kResultTrue {
            break;
        }
    }
    // What the plugin settled on.
    let in_channels: Vec<usize> = own(input, inputs)
        .into_iter()
        .map(|a| a.count_ones() as usize)
        .collect();
    let out_channels: Vec<usize> = own(output, outputs)
        .into_iter()
        .map(|a| a.count_ones() as usize)
        .collect();
    if out_channels[main_out] == 0 {
        return Err(PluginError::Unsupported(
            "plugin's main output has no channels".into(),
        ));
    }
    Ok(Buses {
        inputs,
        outputs,
        main_in,
        aux_in,
        main_out,
        has_events,
        in_channels,
        out_channels,
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
        host: module.host().clone(),
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
        // The handler before any state: plugins report edits and restarts
        // while they load, and some keep a null handler otherwise.
        if let Some(h) = core.handler.as_com_ref::<IComponentHandler>() {
            controller.setComponentHandler(h.as_ptr());
        }

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

        let processor = core
            .component
            .cast::<IAudioProcessor>()
            .ok_or_else(|| PluginError::Unsupported("no IAudioProcessor".into()))?;
        core.processor = Some(processor.clone());
        let buses = configure_buses(&core.component, &processor)?;
        // Only a clear "no" counts: some plugins answer kNotImplemented
        // and process 32-bit floats all the same.
        if processor.canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32) == kResultFalse {
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
        // Some plugins answer kNotImplemented or kResultFalse and work;
        // a plugin that really cannot run refuses `setActive` next.
        let setup_result = processor.setupProcessing(&mut ps);
        if setup_result == kInvalidArgument {
            check(setup_result, "setupProcessing")?;
        }
        check(core.component.setActive(1), "setActive")?;
        core.active = true;
        let latency = processor.getLatencySamples();
        // Some plugins return kNotImplemented here; that is not an error.
        processor.setProcessing(1);
        core.processing = true;

        let params = read_params(&controller);
        let tuning = tuning_route(&controller);
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
        let processor = Vst3Processor {
            tuning,
            ..processor
        };
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

/// How a note's pitch offset reaches the plugin (spec 0.3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TuningRoute {
    /// Not at all: slides are not heard.
    None,
    /// Per-note Note Expression "Tuning".
    Expression,
    /// MIDI pitch bend through the parameter it is mapped to.
    Bend(ParamID),
}

/// Per-note tuning when the controller lists it on bus 0, channel 0;
/// else pitch bend when it maps one there.
unsafe fn tuning_route(controller: &ComPtr<IEditController>) -> TuningRoute {
    if let Some(ne) = controller.cast::<INoteExpressionController>() {
        let n = unsafe { ne.getNoteExpressionCount(0, 0) };
        for i in 0..n.clamp(0, 256) {
            let mut info: NoteExpressionTypeInfo = unsafe { std::mem::zeroed() };
            if unsafe { ne.getNoteExpressionInfo(0, 0, i, &mut info) } == kResultOk
                && info.typeId == NoteExpressionTypeIDs_::kTuningTypeID as NoteExpressionTypeID
            {
                return TuningRoute::Expression;
            }
        }
    }
    if let Some(mm) = controller.cast::<IMidiMapping>() {
        let mut id: ParamID = 0;
        if unsafe {
            mm.getMidiControllerAssignment(
                0,
                0,
                ControllerNumbers_::kPitchBend as CtrlNumber,
                &mut id,
            )
        } == kResultOk
        {
            return TuningRoute::Bend(id);
        }
    }
    TuningRoute::None
}

/// The normalized pitch bend for `semitones` with `range` at full bend.
fn bend_value(semitones: f32, range: f32) -> f64 {
    (0.5 + f64::from(semitones) / (2.0 * f64::from(range.max(1.0)))).clamp(0.0, 1.0)
}

/// A tuning expression event: Tuning spans ±120 semitones around 0.5.
fn tuning_event(e: &NoteEvent, base: usize, semitones: f32) -> Event {
    let mut ev: Event = unsafe { std::mem::zeroed() };
    ev.busIndex = 0;
    ev.sampleOffset = (e.offset as usize).saturating_sub(base) as i32;
    ev.flags = Event_::EventFlags_::kIsLive as u16;
    ev.r#type = Event_::EventTypes_::kNoteExpressionValueEvent as u16;
    ev.__field0.noteExpressionValue = NoteExpressionValueEvent {
        typeId: NoteExpressionTypeIDs_::kTuningTypeID as NoteExpressionTypeID,
        noteId: e.note_id,
        value: (0.5 + f64::from(semitones) / 240.0).clamp(0.0, 1.0),
    };
    ev
}

/// The first number in `s` (either decimal separator), the number of
/// decimals it was written with, and the text after it.
fn leading_number(s: &str) -> Option<(f64, i32, &str)> {
    let b = s.as_bytes();
    let start = (0..b.len()).find(|&i| {
        b[i].is_ascii_digit()
            || (matches!(b[i], b'-' | b'+' | b'.' | b',')
                && b.get(i + 1).is_some_and(u8::is_ascii_digit))
    })?;
    let mut end = start + 1;
    let mut point = None;
    while end < b.len() {
        match b[end] {
            c if c.is_ascii_digit() => {}
            b'.' | b',' if point.is_none() && b.get(end + 1).is_some_and(u8::is_ascii_digit) => {
                point = Some(end);
            }
            _ => break,
        }
        end += 1;
    }
    if matches!(b[start], b'.' | b',') {
        point = Some(start);
    }
    let number: String = s[start..end].replace(',', ".");
    let decimals = point.map_or(0, |p| (end - p - 1) as i32);
    Some((number.parse().ok()?, decimals, &s[end..]))
}

/// Whether `typed` names the value the plugin shows as `shown`: the same
/// words, or the same number at the precision shown ("5" and "5.00 ms").
fn same_value_text(typed: &str, shown: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<String>().to_lowercase();
    if norm(typed) == norm(shown) {
        return true;
    }
    match (leading_number(typed), leading_number(shown)) {
        // Only a number with the same units or nothing after it ("1/16"
        // is not 1, "100% L" is not "100% R").
        (Some((a, _, rest)), Some((b, decimals, shown_rest)))
            if !rest.chars().any(|c| c.is_ascii_digit())
                && (norm(rest).is_empty() || norm(rest) == norm(shown_rest)) =>
        {
            (a - b).abs() <= 0.5 * 10f64.powi(-decimals) + 1e-9
        }
        _ => false,
    }
}

/// Most steps searched one by one in [`find_by_text`].
const MAX_SEARCHED_STEPS: u32 = 4096;

/// The normalized value whose text, as `show` writes it, names `typed`:
/// for plugins whose own parse is no help (Xtressor and Camel Strip read
/// "0.0 dB" as 0.0 with an identity plain-to-normalized conversion; Ozone
/// parses nothing). Stepped parameters are tried step by step; continuous
/// ones by the number in their text, sampled and then bisected, so any
/// monotonic stretch of the curve is found.
fn find_by_text(typed: &str, step_count: u32, show: impl Fn(f64) -> String) -> Option<f64> {
    if step_count > 0 {
        if step_count > MAX_SEARCHED_STEPS {
            return None;
        }
        let n = f64::from(step_count);
        return (0..=step_count)
            .map(|k| f64::from(k) / n)
            .find(|&v| same_value_text(typed, &show(v)));
    }
    let Some((want, _, _)) = leading_number(typed) else {
        // A name on a continuous scale ("slow", "Msec"): look it up.
        const NAMED: u32 = 512;
        return (0..=NAMED)
            .map(|k| f64::from(k) / f64::from(NAMED))
            .find(|&v| same_value_text(typed, &show(v)));
    };
    let number = |v: f64| leading_number(&show(v)).map(|(x, _, _)| x);
    const SAMPLES: u32 = 64;
    let mut prev: Option<(f64, f64)> = None;
    for k in 0..=SAMPLES {
        let v = f64::from(k) / f64::from(SAMPLES);
        let Some(x) = number(v) else {
            prev = None;
            continue;
        };
        if same_value_text(typed, &show(v)) {
            return Some(v);
        }
        if let Some((pv, px)) = prev
            && (px - want) * (x - want) < 0.0
        {
            // Bisect the bracket [pv, v], keeping `lo` on `px`'s side.
            let (mut lo, mut hi) = (pv, v);
            for _ in 0..48 {
                let mid = 0.5 * (lo + hi);
                if same_value_text(typed, &show(mid)) {
                    return Some(mid);
                }
                match number(mid) {
                    Some(m) if (m - want) * (px - want) > 0.0 => lo = mid,
                    Some(_) => hi = mid,
                    None => break,
                }
            }
        }
        prev = Some((v, x));
    }
    None
}

/// Left becomes the mix of left and right, for a mono bus.
fn mix_into_left(pair: &mut [Vec<f32>; 2], n: usize) {
    let [l, r] = pair;
    for (a, b) in l[..n].iter_mut().zip(&r[..n]) {
        *a = 0.5 * (*a + *b);
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
        NoteKind::Tuning { semitones, .. } => return tuning_event(e, base, semitones),
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
    /// Channel pointers per bus; heap storage keeps them stable.
    in_ptrs: Vec<Vec<*mut f32>>,
    out_ptrs: Vec<Vec<*mut f32>>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    in_changes: ComWrapper<ParamChanges>,
    out_changes: ComWrapper<ParamChanges>,
    events: ComWrapper<EventList>,
    /// Events the plugin sends (ignored); some plugins write here without
    /// checking for null.
    out_events: ComWrapper<EventList>,
    context: ProcessContext,
    continuous: i64,
    from_ui: rtrb::Consumer<(u32, f64)>,
    to_ui: rtrb::Producer<(u32, f64)>,
    /// Automation points for the next call: (id, offset, value).
    automation: Vec<(u32, u32, f64)>,
    /// Where note tuning goes (slide notes).
    tuning: TuningRoute,
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
        let (inputs, outputs) = (buses.inputs, buses.outputs);
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
            in_ptrs: Vec::with_capacity(inputs),
            out_ptrs: Vec::with_capacity(outputs),
            in_buses: Vec::with_capacity(inputs),
            out_buses: Vec::with_capacity(outputs),
            in_changes: ComWrapper::new(ParamChanges::new()),
            out_changes: ComWrapper::new(ParamChanges::new()),
            events: ComWrapper::new(EventList::new()),
            out_events: ComWrapper::new(EventList::new()),
            context: unsafe { std::mem::zeroed() },
            continuous: 0,
            from_ui,
            to_ui,
            automation: Vec::with_capacity(AUTOMATION_POINTS),
            tuning: TuningRoute::None,
            _core: core,
        };
        // The main and sidechain buses get the host's left and right
        // (a mono bus: the left, which holds the mix); extra channels and
        // other buses read silence and write into scratch.
        for i in 0..p.buses.inputs {
            let pair = if Some(i) == p.buses.main_in {
                [p.input[0].as_mut_ptr(), p.input[1].as_mut_ptr()]
            } else if Some(i) == p.buses.aux_in {
                [p.sidechain[0].as_mut_ptr(), p.sidechain[1].as_mut_ptr()]
            } else {
                [p.silence.as_mut_ptr(); 2]
            };
            let ptrs = (0..p.buses.in_channels[i])
                .map(|c| pair.get(c).copied().unwrap_or(p.silence.as_mut_ptr()))
                .collect();
            p.in_ptrs.push(ptrs);
        }
        for i in 0..p.buses.outputs {
            let pair = if i == p.buses.main_out {
                [p.output[0].as_mut_ptr(), p.output[1].as_mut_ptr()]
            } else {
                [p.scratch.as_mut_ptr(); 2]
            };
            let ptrs = (0..p.buses.out_channels[i])
                .map(|c| pair.get(c).copied().unwrap_or(p.scratch.as_mut_ptr()))
                .collect();
            p.out_ptrs.push(ptrs);
        }
        for ptrs in &mut p.in_ptrs {
            p.in_buses.push(AudioBusBuffers {
                numChannels: ptrs.len() as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: ptrs.as_mut_ptr(),
                },
            });
        }
        for ptrs in &mut p.out_ptrs {
            p.out_buses.push(AudioBusBuffers {
                numChannels: ptrs.len() as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: ptrs.as_mut_ptr(),
                },
            });
        }
        p
    }

    /// A mono main or sidechain input takes the mix of left and right.
    fn mix_mono_inputs(&mut self, n: usize) {
        if self.buses.channels_in(self.buses.main_in) == 1 {
            mix_into_left(&mut self.input, n);
        }
        if self.buses.channels_in(self.buses.aux_in) == 1 {
            mix_into_left(&mut self.sidechain, n);
        }
    }

    /// The main output into `left` and `right`; a mono output feeds both.
    fn copy_output(&self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len();
        left.copy_from_slice(&self.output[0][..n]);
        if self.buses.out_channels[self.buses.main_out] == 1 {
            right.copy_from_slice(&self.output[0][..n]);
        } else {
            right.copy_from_slice(&self.output[1][..n]);
        }
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
        c.timeSigNumerator = i32::from(t.time_sig.0);
        c.timeSigDenominator = i32::from(t.time_sig.1);
        c.projectTimeSamples = t.project_time_samples + offset as i64;
        c.projectTimeMusic = t.project_time_quarters + offset as f64 * quarters_per_sample;
        c.barPositionMusic = t.bar_start_quarters(c.projectTimeMusic);
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
            outputEvents: self
                .out_events
                .as_com_ref::<IEventList>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            processContext: &mut self.context,
        };
        let ok = unsafe { self.processor.process(&mut data) } == kResultOk;
        self.in_changes.clear();
        self.events.clear();
        self.out_events.clear();
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
            self.mix_mono_inputs(n);
            self.chunk_automation(done, n);
            self.fill_context(transport, done);
            // On failure the input passes through unchanged.
            if self.run(n) {
                self.copy_output(&mut left[range.clone()], &mut right[range]);
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
                if let NoteKind::Tuning {
                    semitones,
                    bend_range,
                } = e.kind
                {
                    match self.tuning {
                        TuningRoute::Expression if self.buses.has_events => {
                            self.events.push(tuning_event(e, done, semitones));
                        }
                        TuningRoute::Bend(id) => {
                            // One bend for the channel: a later point at
                            // the same offset replaces the earlier one.
                            let offset = (e.offset as usize).saturating_sub(done) as i32;
                            self.in_changes
                                .add(id, offset, bend_value(semitones, bend_range));
                        }
                        _ => {}
                    }
                    continue;
                }
                if self.buses.has_events {
                    self.events.push(vst_event(e, done));
                }
            }
            self.chunk_automation(done, n);
            self.fill_context(transport, done);
            let range = done..done + n;
            if self.run(n) {
                self.copy_output(&mut left[range.clone()], &mut right[range]);
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
    pub fn open_editor(
        &self,
        title: &str,
        scaling: jdance_plugin_api::EditorScaling,
    ) -> Result<Option<crate::Vst3Editor>, PluginError> {
        crate::Vst3Editor::open(&self.controller, self.core.clone(), title, scaling)
    }

    /// The plugin reports work in progress through `IProgress` (for
    /// example its state restoring in the background).
    pub fn busy(&self) -> bool {
        self.core.handler.busy()
    }

    /// The plugin's latency now. After `LatencyChanged` the host reads it
    /// here and keeps the instance (and its open editor) instead of
    /// recreating it; plugins report the new value at once.
    pub fn latency(&self) -> u32 {
        self.core
            .processor
            .as_ref()
            .map_or(0, |p| unsafe { p.getLatencySamples() })
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
        // Older edits from the plugin must not follow this value to the
        // processor.
        self.core.handler.discard_edits(id);
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

    /// The plugin's own parse, checked against the plugin's own text for
    /// the result. Many plugins get `getParamValueByString` wrong: they
    /// return the plain value instead of the normalized one (Camel Strip
    /// "0.0 dB" → 0.0, Xtressor "5.0" → 5.0) or cannot read the decimal
    /// comma they write themselves (Ozone "40,00"). When the result does
    /// not read back as what was typed, its plain-to-normalized
    /// conversion and the other decimal separator are tried, then the
    /// value is looked up by the plugin's own text ([`find_by_text`]);
    /// failing all, the plugin's answer stands.
    fn param_from_text(&self, id: u32, text: &str) -> Option<f64> {
        let parse = |t: &str| {
            let mut s: String128 = [0; 128];
            copy_wstring(t, &mut s);
            let mut v = 0.0;
            (unsafe {
                self.controller
                    .getParamValueByString(id, s.as_mut_ptr(), &mut v)
            } == kResultOk
                && v.is_finite())
            .then_some(v)
        };
        let swapped: String = text
            .chars()
            .map(|c| match c {
                ',' => '.',
                '.' => ',',
                c => c,
            })
            .collect();
        let answers: Vec<f64> = [
            parse(text),
            (swapped != text).then(|| parse(&swapped)).flatten(),
        ]
        .into_iter()
        .flatten()
        .collect();
        let reads_back =
            |v: f64| (0.0..=1.0).contains(&v) && same_value_text(text, &self.param_text(id, v));
        for &v in &answers {
            if reads_back(v) {
                return Some(v);
            }
            let plain = unsafe { self.controller.plainParamToNormalized(id, v) };
            if plain.is_finite() && reads_back(plain) {
                return Some(plain);
            }
        }
        let step_count = self
            .params
            .iter()
            .find(|p| p.id == id)
            .map_or(0, |p| p.step_count);
        find_by_text(text, step_count, |v| self.param_text(id, v)).or(answers.first().copied())
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
        if out.contains(&PluginEvent::ParamsChanged) {
            // Titles, ranges or the parameter list itself changed.
            self.params = unsafe { read_params(&self.controller) };
        }
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

#[cfg(test)]
mod tests {
    use super::{find_by_text, leading_number, same_value_text};

    #[test]
    fn numbers_are_read_with_either_separator() {
        assert_eq!(leading_number("40,00"), Some((40.0, 2, "")));
        assert_eq!(leading_number("-12.5 dB"), Some((-12.5, 1, " dB")));
        assert_eq!(leading_number("Left -100"), Some((-100.0, 0, "")));
        assert_eq!(leading_number("off"), None);
    }

    #[test]
    fn typed_values_match_at_the_precision_shown() {
        assert!(same_value_text("5", "5.00 ms"));
        assert!(same_value_text("0,0 dB", "0.0 dB"));
        assert!(same_value_text("Graph Mode", "graph mode"));
        assert!(!same_value_text("0.0 dB", "-12.0 dB"));
        assert!(!same_value_text("632", "631"));
        assert!(
            !same_value_text("1/16", "1/4"),
            "a note value is not its first number"
        );
        assert!(!same_value_text("on", "off"));
        assert!(!same_value_text("100% L", "100% R"), "Lair's pan");
        assert!(same_value_text("5ms", "5.00 ms"));
    }

    #[test]
    fn values_are_found_by_the_plugins_own_text() {
        // Camel Strip: -12..+12 dB, one decimal.
        let db = |v: f64| format!("{:.1} dB", -12.0 + 24.0 * v);
        let v = find_by_text("0.0 dB", 0, db).unwrap();
        assert_eq!(db(v), "0.0 dB");
        assert_eq!(db(find_by_text("3", 0, db).unwrap()), "3.0 dB");
        // Decreasing and non-linear (a frequency knob drawn backwards).
        let hz = |v: f64| format!("{:.0} Hz", 20.0 * 1000f64.powf(1.0 - v));
        assert_eq!(hz(find_by_text("440 Hz", 0, hz).unwrap()), "440 Hz");
        // Ozone writes a decimal comma and parses nothing.
        let comma = |v: f64| format!("{:.2}", 200.0 * v).replace('.', ",");
        assert_eq!(comma(find_by_text("40,00", 0, comma).unwrap()), "40,00");
        // Stepped: by name.
        let onoff = |v: f64| if v < 0.5 { "Off" } else { "On" }.to_owned();
        assert_eq!(find_by_text("on", 1, onoff), Some(1.0));
        // Names on a continuous scale (soothe2 "slow"/"fast").
        let speed = |v: f64| if v < 0.3 { "fast" } else { "slow" }.to_owned();
        assert_eq!(speed(find_by_text("slow", 0, speed).unwrap()), "slow");
        // Lair's pan: "100% L" .. "C" .. "100% R".
        let pan = |v: f64| match (v - 0.5) * 200.0 {
            p if p < -0.5 => format!("{:.0}% L", -p),
            p if p > 0.5 => format!("{p:.0}% R"),
            _ => "C".to_owned(),
        };
        assert_eq!(find_by_text("100% L", 0, pan), Some(0.0));
        // Out of range or not a value of this parameter: nothing.
        assert_eq!(find_by_text("30 dB", 0, db), None);
        assert_eq!(find_by_text("loud", 0, db), None);
        assert_eq!(find_by_text("Maybe", 1, onoff), None);
    }
}
