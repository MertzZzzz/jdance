// SPDX-License-Identifier: GPL-3.0-or-later
//! COM objects the host gives to plugins (PLG-036).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComWrapper, Interface};

use jdance_plugin_api::PluginEvent;

fn same_id(tuid: *const TUID, guid: &vst3::com_scrape_types::Guid) -> bool {
    !tuid.is_null() && unsafe { (*tuid).map(|b| b as u8) } == *guid
}

unsafe fn query<C: Class + 'static>(
    wrapper: &ComWrapper<C>,
    iid: *mut TUID,
    obj: *mut *mut c_void,
) -> tresult {
    let Some(unknown) = wrapper.to_com_ptr::<FUnknown>() else {
        return kNoInterface;
    };
    let ptr = unknown.as_ptr();
    unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, obj) }
}

/// `IHostApplication`: host name and message/attribute-list factory;
/// `IPlugInterfaceSupport`: which optional interfaces the host offers
/// (JUCE and others ask before using them).
pub struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication, IPlugInterfaceSupport);
}

/// Host-side interfaces a plugin may ask about through
/// `IPlugInterfaceSupport`.
fn host_supports(iid: &vst3::com_scrape_types::Guid) -> bool {
    [
        IComponent::IID,
        IAudioProcessor::IID,
        IEditController::IID,
        IConnectionPoint::IID,
        IComponentHandler::IID,
        IComponentHandler2::IID,
        IUnitHandler::IID,
        IProgress::IID,
        IPlugView::IID,
        IPlugFrame::IID,
        IPlugViewContentScaleSupport::IID,
    ]
    .contains(iid)
}

impl IPlugInterfaceSupportTrait for HostApp {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        if iid.is_null() {
            return kInvalidArgument;
        }
        let id = unsafe { (*iid).map(|b| b as u8) };
        if host_supports(&id) {
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        copy_wstring("jdance", unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if same_id(cid, &IMessage::IID) {
            unsafe { query(&ComWrapper::new(Message::default()), iid, obj) }
        } else if same_id(cid, &IAttributeList::IID) {
            unsafe { query(&ComWrapper::new(AttributeList::default()), iid, obj) }
        } else {
            unsafe { *obj = std::ptr::null_mut() };
            kResultFalse
        }
    }
}

pub fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let cap = dst.len() - 1;
    let mut n = 0;
    for (d, s) in dst.iter_mut().take(cap).zip(src.encode_utf16()) {
        *d = s as TChar;
        n += 1;
    }
    dst[n] = 0;
}

pub fn wstring(src: &[TChar]) -> String {
    let len = src.iter().position(|c| *c == 0).unwrap_or(src.len());
    String::from_utf16_lossy(&src[..len].iter().map(|c| *c as u16).collect::<Vec<_>>())
}

pub fn cstring(src: &[std::ffi::c_char]) -> String {
    let bytes: Vec<u8> = src
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `IComponentHandler`: edits and restart requests from the plugin. May
/// be called from any thread, so it only records into a mutex-guarded
/// list read by the UI thread.
#[derive(Default)]
pub struct ComponentHandler {
    events: Mutex<Vec<PluginEvent>>,
    restart: AtomicI32,
}

impl ComponentHandler {
    pub fn take_events(&self) -> Vec<PluginEvent> {
        let mut out = self
            .events
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default();
        let flags = self.restart.swap(0, Ordering::AcqRel);
        if flags & RestartFlags_::kLatencyChanged as i32 != 0 {
            out.push(PluginEvent::LatencyChanged);
        }
        if flags & (RestartFlags_::kParamValuesChanged | RestartFlags_::kParamTitlesChanged) as i32
            != 0
        {
            out.push(PluginEvent::ParamsChanged);
        }
        if flags & (RestartFlags_::kIoChanged | RestartFlags_::kReloadComponent) as i32 != 0 {
            out.push(PluginEvent::ReloadRequested);
        }
        out
    }

    pub fn push(&self, event: PluginEvent) {
        if let Ok(mut e) = self.events.lock() {
            e.push(event);
        }
    }
}

impl Class for ComponentHandler {
    type Interfaces = (
        IComponentHandler,
        IComponentHandler2,
        IUnitHandler,
        IProgress,
    );
}

/// Accepted and ignored: some plugins stop when the host does not offer
/// these.
impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        kResultFalse
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

impl IUnitHandlerTrait for ComponentHandler {
    unsafe fn notifyUnitSelection(&self, _unit: UnitID) -> tresult {
        kResultOk
    }

    unsafe fn notifyProgramListChange(&self, _list: ProgramListID, _program: i32) -> tresult {
        // Program names may have changed: refresh like a title change.
        self.restart
            .fetch_or(RestartFlags_::kParamTitlesChanged as i32, Ordering::AcqRel);
        kResultOk
    }
}

impl IProgressTrait for ComponentHandler {
    unsafe fn start(
        &self,
        _kind: IProgress_::ProgressType,
        _description: *const tchar,
        id: *mut IProgress_::ID,
    ) -> tresult {
        if !id.is_null() {
            unsafe { *id = 1 };
        }
        kResultOk
    }

    unsafe fn update(&self, _id: IProgress_::ID, _value: ParamValue) -> tresult {
        kResultOk
    }

    unsafe fn finish(&self, _id: IProgress_::ID) -> tresult {
        kResultOk
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.push(PluginEvent::ParamChanged { id, value });
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        self.restart.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

/// `IMessage` for `IConnectionPoint` traffic between component and
/// controller. Locked rather than borrowed: plugins send messages from
/// any thread, and a failed borrow would abort the program.
pub struct Message {
    id: Mutex<CString>,
    attributes: ComWrapper<AttributeList>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: Mutex::new(CString::default()),
            attributes: ComWrapper::new(AttributeList::default()),
        }
    }
}

/// The guard even after a panic elsewhere: these are plain data.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        // Stays valid until the ID is replaced (the CString's heap buffer).
        lock(&self.id).as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        if !id.is_null() {
            *lock(&self.id) = unsafe { CStr::from_ptr(id) }.to_owned();
        }
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<TChar>),
    Bin(Vec<u8>),
}

#[derive(Default)]
pub struct AttributeList {
    map: Mutex<HashMap<CString, Attr>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

unsafe fn key(id: IAttributeList_::AttrID) -> Option<CString> {
    (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_owned())
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: IAttributeList_::AttrID, value: i64) -> tresult {
        let Some(k) = (unsafe { key(id) }) else {
            return kInvalidArgument;
        };
        lock(&self.map).insert(k, Attr::Int(value));
        kResultOk
    }

    unsafe fn getInt(&self, id: IAttributeList_::AttrID, value: *mut i64) -> tresult {
        match unsafe { key(id) }.and_then(|k| match lock(&self.map).get(&k) {
            Some(Attr::Int(v)) => Some(*v),
            _ => None,
        }) {
            Some(v) => {
                unsafe { *value = v };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: IAttributeList_::AttrID, value: f64) -> tresult {
        let Some(k) = (unsafe { key(id) }) else {
            return kInvalidArgument;
        };
        lock(&self.map).insert(k, Attr::Float(value));
        kResultOk
    }

    unsafe fn getFloat(&self, id: IAttributeList_::AttrID, value: *mut f64) -> tresult {
        match unsafe { key(id) }.and_then(|k| match lock(&self.map).get(&k) {
            Some(Attr::Float(v)) => Some(*v),
            _ => None,
        }) {
            Some(v) => {
                unsafe { *value = v };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: IAttributeList_::AttrID, string: *const TChar) -> tresult {
        let Some(k) = (unsafe { key(id) }) else {
            return kInvalidArgument;
        };
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut s = Vec::new();
        let mut i = 0;
        loop {
            let c = unsafe { *string.add(i) };
            s.push(c);
            if c == 0 {
                break;
            }
            i += 1;
        }
        lock(&self.map).insert(k, Attr::Str(s));
        kResultOk
    }

    unsafe fn getString(
        &self,
        id: IAttributeList_::AttrID,
        string: *mut TChar,
        size_in_bytes: u32,
    ) -> tresult {
        let Some(k) = (unsafe { key(id) }) else {
            return kInvalidArgument;
        };
        let map = lock(&self.map);
        let Some(Attr::Str(s)) = map.get(&k) else {
            return kResultFalse;
        };
        let cap = size_in_bytes as usize / std::mem::size_of::<TChar>();
        if cap == 0 || string.is_null() {
            return kInvalidArgument;
        }
        let n = s.len().min(cap);
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), string, n);
            *string.add(n - 1) = 0;
        }
        kResultOk
    }

    unsafe fn setBinary(
        &self,
        id: IAttributeList_::AttrID,
        data: *const c_void,
        size: u32,
    ) -> tresult {
        let Some(k) = (unsafe { key(id) }) else {
            return kInvalidArgument;
        };
        let bytes = if size == 0 || data.is_null() {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) }.to_vec()
        };
        lock(&self.map).insert(k, Attr::Bin(bytes));
        kResultOk
    }

    unsafe fn getBinary(
        &self,
        id: IAttributeList_::AttrID,
        data: *mut *const c_void,
        size: *mut u32,
    ) -> tresult {
        let Some(k) = (unsafe { key(id) }) else {
            return kInvalidArgument;
        };
        let map = lock(&self.map);
        let Some(Attr::Bin(b)) = map.get(&k) else {
            return kResultFalse;
        };
        // The pointer stays valid until the attribute is replaced, as in
        // the SDK's host implementation.
        unsafe {
            *data = b.as_ptr() as *const c_void;
            *size = b.len() as u32;
        }
        kResultOk
    }
}

/// In-memory `IBStream` for plugin state.
#[derive(Default)]
pub struct MemStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl MemStream {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            data: RefCell::new(bytes.to_vec()),
            pos: Cell::new(0),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.data.borrow().clone()
    }
}

impl Class for MemStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: i32, read: *mut i32) -> tresult {
        let data = self.data.borrow();
        let pos = self.pos.get().min(data.len());
        let n = (num_bytes.max(0) as usize).min(data.len() - pos);
        if n > 0 && !buffer.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(data[pos..].as_ptr(), buffer as *mut u8, n) };
        }
        self.pos.set(pos + n);
        if !read.is_null() {
            unsafe { *read = n as i32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, num_bytes: i32, written: *mut i32) -> tresult {
        if num_bytes < 0 || (num_bytes > 0 && buffer.is_null()) {
            return kInvalidArgument;
        }
        let n = num_bytes as usize;
        let mut data = self.data.borrow_mut();
        let pos = self.pos.get();
        if data.len() < pos + n {
            data.resize(pos + n, 0);
        }
        if n > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(buffer as *const u8, data[pos..].as_mut_ptr(), n)
            };
        }
        self.pos.set(pos + n);
        if !written.is_null() {
            unsafe { *written = n as i32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let base = match mode as IBStream_::IStreamSeekMode {
            IBStream_::IStreamSeekMode_::kIBSeekSet => 0,
            IBStream_::IStreamSeekMode_::kIBSeekCur => self.pos.get() as i64,
            IBStream_::IStreamSeekMode_::kIBSeekEnd => len,
            _ => return kInvalidArgument,
        };
        let new = base + pos;
        if new < 0 {
            return kInvalidArgument;
        }
        self.pos.set(new as usize);
        if !result.is_null() {
            unsafe { *result = new };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        unsafe { *pos = self.pos.get() as i64 };
        kResultOk
    }
}

/// Points per parameter per block (AUT-030: `MAX_BLOCK / 64` plus
/// breakpoints).
pub const MAX_POINTS: usize = 64;
/// Distinct parameters changed in one block.
pub const MAX_QUEUES: usize = 128;

/// One parameter's points for a block; storage is pre-allocated.
pub struct ParamQueue {
    id: Cell<ParamID>,
    points: RefCell<Vec<(i32, f64)>>,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.points.borrow().len() as i32
    }

    unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut ParamValue) -> tresult {
        match self.points.borrow().get(index as usize) {
            Some(&(o, v)) if !offset.is_null() && !value.is_null() => {
                unsafe {
                    *offset = o;
                    *value = v;
                }
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addPoint(&self, offset: i32, value: ParamValue, index: *mut i32) -> tresult {
        let mut p = self.points.borrow_mut();
        if p.len() == p.capacity() {
            return kResultFalse;
        }
        // Keep points sorted by offset; equal offsets replace.
        let at = p.partition_point(|(o, _)| *o < offset);
        if p.get(at).is_some_and(|(o, _)| *o == offset) {
            p[at].1 = value;
        } else {
            p.insert(at, (offset, value));
        }
        if !index.is_null() {
            unsafe { *index = at as i32 };
        }
        kResultOk
    }
}

/// `IParameterChanges` with fixed capacity: no allocation once built.
pub struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    used: Cell<usize>,
}

impl ParamChanges {
    pub fn new() -> Self {
        Self {
            queues: (0..MAX_QUEUES)
                .map(|_| {
                    ComWrapper::new(ParamQueue {
                        id: Cell::new(0),
                        points: RefCell::new(Vec::with_capacity(MAX_POINTS)),
                    })
                })
                .collect(),
            used: Cell::new(0),
        }
    }

    pub fn clear(&self) {
        for q in &self.queues[..self.used.get()] {
            q.points.borrow_mut().clear();
        }
        self.used.set(0);
    }

    fn queue_for(&self, id: ParamID) -> Option<(usize, &ComWrapper<ParamQueue>)> {
        let used = self.used.get();
        if let Some(i) = self.queues[..used].iter().position(|q| q.id.get() == id) {
            return Some((i, &self.queues[i]));
        }
        let q = self.queues.get(used)?;
        q.id.set(id);
        self.used.set(used + 1);
        Some((used, q))
    }

    /// Host side: adds a point; returns false when full (RT-011).
    pub fn add(&self, id: ParamID, offset: i32, value: f64) -> bool {
        match self.queue_for(id) {
            Some((_, q)) => unsafe { q.addPoint(offset, value, std::ptr::null_mut()) == kResultOk },
            None => false,
        }
    }

    /// Host side: calls `f(id, value)` with the last point of each queue.
    pub fn for_each_last(&self, mut f: impl FnMut(ParamID, f64)) {
        for q in &self.queues[..self.used.get()] {
            if let Some(&(_, v)) = q.points.borrow().last() {
                f(q.id.get(), v);
            }
        }
    }
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        self.used.get() as i32
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= self.used.get() {
            return std::ptr::null_mut();
        }
        self.queues[index as usize]
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut i32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        match self.queue_for(unsafe { *id }) {
            Some((i, q)) => {
                if !index.is_null() {
                    unsafe { *index = i as i32 };
                }
                q.as_com_ref::<IParamValueQueue>()
                    .map_or(std::ptr::null_mut(), |r| r.as_ptr())
            }
            None => std::ptr::null_mut(),
        }
    }
}

/// Events per block; extra events are dropped (RT-011).
pub const MAX_EVENTS: usize = 512;

/// `IEventList` with fixed capacity: no allocation once built.
pub struct EventList {
    events: RefCell<Vec<Event>>,
}

impl EventList {
    pub fn new() -> Self {
        Self {
            events: RefCell::new(Vec::with_capacity(MAX_EVENTS)),
        }
    }

    pub fn clear(&self) {
        self.events.borrow_mut().clear();
    }

    /// Host side; returns false when full.
    pub fn push(&self, e: Event) -> bool {
        let mut v = self.events.borrow_mut();
        if v.len() == v.capacity() {
            return false;
        }
        v.push(e);
        true
    }
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> i32 {
        self.events.borrow().len() as i32
    }

    unsafe fn getEvent(&self, index: i32, e: *mut Event) -> tresult {
        match self.events.borrow().get(index as usize) {
            Some(ev) if !e.is_null() => {
                unsafe { *e = *ev };
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        if self.push(unsafe { *e }) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}
