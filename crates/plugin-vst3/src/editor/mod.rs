// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin editor windows (PLG-060..063): a top-level native window per
//! editor with the plugin's view attached, `IPlugFrame` for resize
//! requests and, on Linux, `IRunLoop` for the plugin's file descriptors
//! and timers (PLG-062). UI thread only (PLG-065).

#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
use x11::NativeWindow;

#[cfg(windows)]
mod win32;
#[cfg(windows)]
use win32::NativeWindow;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac::NativeWindow;

use jdance_plugin_api::PluginError;
use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper};

#[cfg(target_os = "linux")]
const PLATFORM: FIDString = kPlatformTypeX11EmbedWindowID;
#[cfg(windows)]
const PLATFORM: FIDString = kPlatformTypeHWND;
#[cfg(target_os = "macos")]
const PLATFORM: FIDString = kPlatformTypeNSView;

/// Space presses taken from plugin editor windows (spec 0.2.3 KEY-004).
static SPACE_PRESSES: AtomicU32 = AtomicU32::new(0);

/// Space pressed in a plugin editor window since the last call: the
/// window frame takes it before the plugin, so it can start and stop the
/// transport while the editor has the focus (KEY-004). Key repeats are
/// not counted (KEY-005); Space typed into a text field of the plugin
/// stays with the plugin where the platform can tell (Windows).
pub fn take_space_presses() -> u32 {
    SPACE_PRESSES.swap(0, Ordering::Relaxed)
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn note_space_press() {
    SPACE_PRESSES.fetch_add(1, Ordering::Relaxed);
}

fn rect_size(r: &ViewRect) -> (u32, u32) {
    (
        (r.right - r.left).max(1) as u32,
        (r.bottom - r.top).max(1) as u32,
    )
}

fn rect(w: u32, h: u32) -> ViewRect {
    ViewRect {
        left: 0,
        top: 0,
        right: w as i32,
        bottom: h as i32,
    }
}

/// `IPlugFrame` (and `IRunLoop` on Linux) for one editor.
struct PlugFrame {
    window: Rc<NativeWindow>,
    /// Size the host and plugin agreed on; anything else is a user resize.
    agreed: Cell<(u32, u32)>,
    #[cfg(target_os = "linux")]
    run_loop: run_loop::RunLoop,
}

#[cfg(not(target_os = "linux"))]
impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

#[cfg(target_os = "linux")]
impl Class for PlugFrame {
    type Interfaces = (IPlugFrame, Linux::IRunLoop);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kInvalidArgument;
        }
        let (w, h) = rect_size(unsafe { &*new_size });
        self.window.resize(w, h);
        self.agreed.set((w, h));
        if let Some(v) = unsafe { ComRef::from_raw(view) } {
            unsafe { v.onSize(new_size) };
        }
        kResultTrue
    }
}

#[cfg(target_os = "linux")]
mod run_loop {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    use vst3::Steinberg::Linux::*;
    use vst3::Steinberg::*;
    use vst3::{ComPtr, ComRef};

    #[derive(Default)]
    pub struct RunLoop {
        handlers: RefCell<Vec<(ComPtr<IEventHandler>, FileDescriptor)>>,
        timers: RefCell<Vec<(ComPtr<ITimerHandler>, Duration, Instant)>>,
    }

    impl RunLoop {
        /// Calls handlers whose descriptors are readable and timers that
        /// are due. Lists are copied first: callbacks may register or
        /// unregister.
        pub fn dispatch(&self) {
            let handlers = self.handlers.borrow().clone();
            if !handlers.is_empty() {
                let mut fds: Vec<libc::pollfd> = handlers
                    .iter()
                    .map(|(_, fd)| libc::pollfd {
                        fd: *fd,
                        events: libc::POLLIN,
                        revents: 0,
                    })
                    .collect();
                let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
                if n > 0 {
                    for ((h, fd), p) in handlers.iter().zip(&fds) {
                        if p.revents != 0 {
                            unsafe { h.onFDIsSet(*fd) };
                        }
                    }
                }
            }
            let now = Instant::now();
            let due: Vec<ComPtr<ITimerHandler>> = {
                let mut timers = self.timers.borrow_mut();
                timers
                    .iter_mut()
                    .filter(|(_, _, next)| *next <= now)
                    .map(|(h, interval, next)| {
                        *next = now + *interval;
                        h.clone()
                    })
                    .collect()
            };
            for h in due {
                unsafe { h.onTimer() };
            }
        }
    }

    impl IRunLoopTrait for super::PlugFrame {
        unsafe fn registerEventHandler(
            &self,
            handler: *mut IEventHandler,
            fd: FileDescriptor,
        ) -> tresult {
            match unsafe { ComRef::from_raw(handler) } {
                Some(h) => {
                    self.run_loop
                        .handlers
                        .borrow_mut()
                        .push((h.to_com_ptr(), fd));
                    kResultTrue
                }
                None => kInvalidArgument,
            }
        }

        unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
            self.run_loop
                .handlers
                .borrow_mut()
                .retain(|(h, _)| h.as_ptr() != handler);
            kResultTrue
        }

        unsafe fn registerTimer(
            &self,
            handler: *mut ITimerHandler,
            milliseconds: TimerInterval,
        ) -> tresult {
            match unsafe { ComRef::from_raw(handler) } {
                Some(h) => {
                    let interval = Duration::from_millis(milliseconds.max(1));
                    self.run_loop.timers.borrow_mut().push((
                        h.to_com_ptr(),
                        interval,
                        Instant::now() + interval,
                    ));
                    kResultTrue
                }
                None => kInvalidArgument,
            }
        }

        unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
            self.run_loop
                .timers
                .borrow_mut()
                .retain(|(h, _, _)| h.as_ptr() != handler);
            kResultTrue
        }
    }
}

/// An open plugin editor. Dropping it detaches the view and closes the
/// window; the plugin instance stays alive (PLG-063).
pub struct Vst3Editor {
    view: ComPtr<IPlugView>,
    frame: ComWrapper<PlugFrame>,
    window: Rc<NativeWindow>,
    _keep_alive: Arc<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for Vst3Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Editor")
            .field("size", &self.frame.agreed.get())
            .finish_non_exhaustive()
    }
}

impl Vst3Editor {
    /// Creates the plugin's view and shows it in a new window.
    /// `Ok(None)` when the plugin has no editor (generic editor only).
    pub(crate) fn open(
        controller: &ComPtr<vst3::Steinberg::Vst::IEditController>,
        keep_alive: Arc<dyn Any + Send + Sync>,
        title: &str,
    ) -> Result<Option<Self>, PluginError> {
        use vst3::Steinberg::Vst::IEditControllerTrait;
        let view = unsafe { controller.createView(c"editor".as_ptr()) };
        let Some(view) = (unsafe { ComPtr::from_raw(view) }) else {
            return Ok(None);
        };
        if unsafe { view.isPlatformTypeSupported(PLATFORM) } != kResultTrue {
            return Err(PluginError::Unsupported(
                "the plugin editor does not support this window system".into(),
            ));
        }
        let mut r = rect(400, 300);
        unsafe { view.getSize(&mut r) };
        let (w, h) = rect_size(&r);
        let window = Rc::new(NativeWindow::new(title, w, h)?);
        let frame = ComWrapper::new(PlugFrame {
            agreed: Cell::new(window.size()),
            window: Rc::clone(&window),
            #[cfg(target_os = "linux")]
            run_loop: run_loop::RunLoop::default(),
        });
        let frame_ptr = frame
            .as_com_ref::<IPlugFrame>()
            .map_or(std::ptr::null_mut(), |f| f.as_ptr());
        unsafe { view.setFrame(frame_ptr) };
        if unsafe { view.attached(window.parent(), PLATFORM) } != kResultOk {
            unsafe { view.setFrame(std::ptr::null_mut()) };
            return Err(PluginError::Failed(
                "the plugin editor failed to open".into(),
            ));
        }
        Ok(Some(Self {
            view,
            frame,
            window,
            _keep_alive: keep_alive,
        }))
    }

    /// Runs the editor's event handling; call every UI frame. Returns
    /// `false` once the user closed the window.
    pub fn poll(&mut self) -> bool {
        if !self.window.poll() {
            return false;
        }
        #[cfg(target_os = "linux")]
        self.frame.run_loop.dispatch();
        // A user resize: let the plugin constrain it, or undo it.
        let now = self.window.size();
        let agreed = &self.frame.agreed;
        if now != agreed.get() {
            let mut r = rect(now.0, now.1);
            let ok = unsafe { self.view.canResize() } == kResultTrue
                && unsafe { self.view.checkSizeConstraint(&mut r) } == kResultTrue;
            if ok {
                let (w, h) = rect_size(&r);
                if (w, h) != now {
                    self.window.resize(w, h);
                }
                unsafe { self.view.onSize(&mut r) };
                agreed.set((w, h));
            } else {
                let (w, h) = agreed.get();
                self.window.resize(w, h);
            }
        }
        true
    }

    /// Content size in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.window.size()
    }

    /// The plugin's own idea of its size.
    pub fn view_size(&self) -> (u32, u32) {
        let mut r = rect(0, 0);
        unsafe { self.view.getSize(&mut r) };
        rect_size(&r)
    }
}

impl Drop for Vst3Editor {
    fn drop(&mut self) {
        unsafe {
            self.view.removed();
            self.view.setFrame(std::ptr::null_mut());
        }
    }
}
