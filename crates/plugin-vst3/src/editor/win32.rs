// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin editor window on Windows.

use jdance_plugin_api::PluginError;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BLACK_BRUSH, GetDC, GetStockObject, HBRUSH, HDC, ReleaseDC,
};
use windows_sys::Win32::Graphics::OpenGL::{
    GetPixelFormat, HGLRC, wglGetCurrentContext, wglGetCurrentDC, wglMakeCurrent,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetActiveWindow, GetFocus, SetFocus, VK_SPACE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CW_USEDEFAULT, CallNextHookEx, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, GA_ROOT, GW_CHILD, GW_HWNDNEXT, GetAncestor, GetClassNameW,
    GetClientRect, GetWindow, GetWindowRect, GetWindowThreadProcessId, IDC_ARROW, LoadCursorW, MSG,
    PM_REMOVE, PeekMessageW, RegisterClassW, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SetWindowPos, SetWindowsHookExW, ShowWindow, TranslateMessage, WH_GETMESSAGE, WM_CLOSE,
    WM_DPICHANGED, WM_KEYDOWN, WM_KEYUP, WM_NULL, WM_SETFOCUS, WM_TIMER, WNDCLASSW, WS_CAPTION,
    WS_CLIPCHILDREN, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_THICKFRAME,
};

/// The frame never paints over the plugin's own window (WS_CLIPCHILDREN):
/// without it, plugin graphics flicker or show the frame's background.
const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
const RESIZABLE: u32 = WS_THICKFRAME | WS_MAXIMIZEBOX;

/// Per-monitor DPI functions of Windows 10 1607 and later, looked up at
/// run time so older systems still start.
mod dpi {
    use super::{HWND, RECT};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

    /// `DPI_AWARENESS_CONTEXT` handles.
    pub type Context = isize;
    pub const UNAWARE: Context = -1;
    pub const PER_MONITOR_V2: Context = -4;

    type SetThreadContext = unsafe extern "system" fn(Context) -> Context;
    type GetWindowContext = unsafe extern "system" fn(HWND) -> Context;
    type GetDpiForWindow = unsafe extern "system" fn(HWND) -> u32;
    type AdjustForDpi = unsafe extern "system" fn(*mut RECT, u32, i32, u32, u32) -> i32;

    fn proc_address(name: &std::ffi::CStr) -> Option<unsafe extern "system" fn() -> isize> {
        let user32: Vec<u16> = "user32.dll\0".encode_utf16().collect();
        unsafe {
            let module = GetModuleHandleW(user32.as_ptr());
            if module.is_null() {
                return None;
            }
            GetProcAddress(module, name.as_ptr().cast())
        }
    }

    macro_rules! lookup {
        ($name:literal, $ty:ty) => {{
            static F: std::sync::OnceLock<Option<$ty>> = std::sync::OnceLock::new();
            *F.get_or_init(|| {
                proc_address($name).map(|f| unsafe { std::mem::transmute::<_, $ty>(f) })
            })
        }};
    }

    /// Sets this thread's DPI awareness until dropped: windows created
    /// meanwhile (the frame and the plugin's own) keep it.
    pub struct Scope(Option<Context>);

    pub fn enter(context: Context) -> Scope {
        let set = lookup!(c"SetThreadDpiAwarenessContext", SetThreadContext);
        Scope(set.map(|f| unsafe { f(context) }).filter(|old| *old != 0))
    }

    /// The DPI awareness `hwnd` was created with, if the system says.
    pub fn of_window(hwnd: HWND) -> Option<Context> {
        lookup!(c"GetWindowDpiAwarenessContext", GetWindowContext)
            .map(|f| unsafe { f(hwnd) })
            .filter(|c| *c != 0)
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            if let (Some(old), Some(set)) = (
                self.0,
                lookup!(c"SetThreadDpiAwarenessContext", SetThreadContext),
            ) {
                unsafe { set(old) };
            }
        }
    }

    /// The window's DPI (96 = 100 %).
    pub fn of(hwnd: HWND) -> u32 {
        lookup!(c"GetDpiForWindow", GetDpiForWindow)
            .map(|f| unsafe { f(hwnd) })
            .filter(|d| *d > 0)
            .unwrap_or(96)
    }

    /// `AdjustWindowRectExForDpi`, or `false` where it does not exist.
    pub fn adjust(r: &mut RECT, style: u32, dpi: u32) -> bool {
        lookup!(c"AdjustWindowRectExForDpi", AdjustForDpi)
            .is_some_and(|f| unsafe { f(r, style, 0, 0, dpi) } != 0)
    }
}

pub use dpi::Scope as DpiScope;

/// Sets the DPI awareness for an editor about to be created: `system`
/// lets Windows scale a plugin that does not scale itself (blurry but the
/// right size), otherwise the plugin draws at the monitor's resolution.
pub fn dpi_scope(system: bool) -> DpiScope {
    dpi::enter(if system {
        dpi::UNAWARE
    } else {
        dpi::PER_MONITOR_V2
    })
}

/// Runs the due timers of this thread: plugins' thread timers and those
/// of their hidden windows. Windows makes `WM_TIMER` only when no other
/// message, not even a repaint, is waiting; the app redraws every frame
/// while playing or while an editor is open, so its window always has a
/// repaint waiting and plugin timers never fired. Plugins that draw or
/// load on a timer stood still: VPS Avenger ignored clicks (it repaints
/// through a timer on a hidden window), JUCE plugins stopped loading
/// content. Asked for by type, a due timer is handed out at once.
pub fn run_timers() {
    let _gl = GlGuard::new();
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // Bounded: a timer handler that sets a 0 ms timer cannot hold the
    // thread.
    for _ in 0..32 {
        if unsafe {
            PeekMessageW(
                &mut msg,
                std::ptr::null_mut(),
                WM_TIMER,
                WM_TIMER,
                PM_REMOVE,
            )
        } == 0
        {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Keeps the app's OpenGL context away from a plugin while the UI thread
/// calls into it, and current again afterwards. Plugins that draw with
/// OpenGL switch contexts and leave none current; VPS Avenger, closing,
/// deletes its textures without making its own context current, so it
/// deleted the app's: the app's window went black for good. With no
/// context current, such calls do nothing.
pub struct GlGuard {
    context: HGLRC,
    dc: HDC,
}

impl GlGuard {
    pub fn new() -> Self {
        let context = unsafe { wglGetCurrentContext() };
        let dc = unsafe { wglGetCurrentDC() };
        if !context.is_null() {
            unsafe { wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut()) };
        }
        Self { context, dc }
    }
}

impl Drop for GlGuard {
    fn drop(&mut self) {
        if unsafe { wglGetCurrentContext() } != self.context {
            unsafe { wglMakeCurrent(self.dc, self.context) };
        }
    }
}

/// Windows the user asked to close (by address).
fn closed() -> &'static Mutex<HashSet<usize>> {
    static CLOSED: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    CLOSED.get_or_init(Default::default)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            // Closing only hides the editor from the host's point of view;
            // the host removes the view and destroys the window.
            if let Ok(mut c) = closed().lock() {
                c.insert(hwnd as usize);
            }
            0
        }
        // The keyboard goes to the plugin's window (knobs, text fields,
        // the mouse wheel), not to the empty frame.
        WM_SETFOCUS => {
            let child = unsafe { GetWindow(hwnd, GW_CHILD) };
            if !child.is_null() {
                unsafe { SetFocus(child) };
            }
            0
        }
        // The new scale reaches the plugin from `poll`; its size follows
        // from the plugin, not from the suggested rectangle.
        WM_DPICHANGED => 0,
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

const CLASS_NAME: &str = "JdancePluginEditor";

/// The window class of `hwnd`, lower case.
fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize]).to_lowercase()
}

/// Space for the transport (spec 0.2.3 KEY-004): a message hook on the UI
/// thread sees keys before the plugin's own window does. Space in an
/// editor window, unless a text field has the focus, counts as a
/// transport press and is replaced by `WM_NULL`, so the plugin never gets
/// it (nor the `WM_CHAR` it would become).
unsafe extern "system" fn key_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && wp == PM_REMOVE as WPARAM && lp != 0 {
        let msg = unsafe { &mut *(lp as *mut MSG) };
        let space = (msg.message == WM_KEYDOWN || msg.message == WM_KEYUP)
            && msg.wParam == WPARAM::from(VK_SPACE);
        if space && class_of(unsafe { GetAncestor(msg.hwnd, GA_ROOT) }) == CLASS_NAME.to_lowercase()
        {
            let focus = class_of(unsafe { GetFocus() });
            let text = focus.starts_with("edit") || focus.starts_with("richedit");
            if !text {
                // Bit 30: the key was already down (auto-repeat, KEY-005).
                if msg.message == WM_KEYDOWN && msg.lParam & (1 << 30) == 0 {
                    super::note_space_press();
                }
                msg.message = WM_NULL;
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wp, lp) }
}

fn install_key_hook() {
    static HOOK: OnceLock<usize> = OnceLock::new();
    HOOK.get_or_init(|| unsafe {
        SetWindowsHookExW(
            WH_GETMESSAGE,
            Some(key_hook),
            std::ptr::null_mut(),
            GetCurrentThreadId(),
        ) as usize
    });
}

fn class_name() -> &'static [u16] {
    static CLASS: OnceLock<Vec<u16>> = OnceLock::new();
    CLASS.get_or_init(|| {
        let name = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
            // Uncovered parts of the frame (while resizing) are black,
            // not left unpainted.
            hbrBackground: unsafe { GetStockObject(BLACK_BRUSH) } as HBRUSH,
            lpszClassName: name.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        unsafe { RegisterClassW(&wc) };
        name
    })
}

/// Window size for a client area of `width` × `height` at `dpi` (the
/// frame's borders and title grow with the scale).
fn outer_size(style: u32, width: u32, height: u32, dpi: u32) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    if !dpi::adjust(&mut r, style, dpi) {
        unsafe { AdjustWindowRectEx(&mut r, style, 0, 0) };
    }
    (r.right - r.left, r.bottom - r.top)
}

/// The app's own window that is active now: the editor's owner, so it
/// stays in front of the app and minimizes with it.
fn owner() -> HWND {
    let w = unsafe { GetActiveWindow() };
    let mut pid = 0;
    if w.is_null() || unsafe { GetWindowThreadProcessId(w, &mut pid) } == 0 {
        return std::ptr::null_mut();
    }
    if pid == unsafe { GetCurrentProcessId() } {
        w
    } else {
        std::ptr::null_mut()
    }
}

pub struct NativeWindow {
    hwnd: HWND,
    style: u32,
    /// The window's DPI awareness: sizes are read and set in it.
    context: Option<dpi::Context>,
}

impl NativeWindow {
    /// Create inside a [`dpi_scope`]: the window keeps its DPI awareness.
    pub fn new(title: &str, width: u32, height: u32, resizable: bool) -> Result<Self, PluginError> {
        let style = if resizable { STYLE | RESIZABLE } else { STYLE };
        let title = wide(title);
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name().as_ptr(),
                title.as_ptr(),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                width as i32,
                height as i32,
                owner(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(PluginError::Failed("CreateWindowExW failed".into()));
        }
        let w = Self {
            hwnd,
            style,
            context: dpi::of_window(hwnd),
        };
        // The exact client size at the monitor's DPI, then shown.
        w.resize(width, height);
        unsafe { ShowWindow(hwnd, SW_SHOW) };
        install_key_hook();
        Ok(w)
    }

    pub fn parent(&self) -> *mut c_void {
        self.hwnd
    }

    /// Puts this thread in the window's DPI awareness until dropped. The
    /// app's thread is per-monitor aware; a window Windows scales measures
    /// in unscaled pixels, and so does the plugin inside it. Mixing them
    /// took the scaled size for a user resize and shrank the window below
    /// the plugin (Serum, VPS Avenger, OTT at 125 %).
    pub fn scope(&self) -> Option<DpiScope> {
        self.context.map(dpi::enter)
    }

    /// Scale of the window's monitor (1.0 = 96 DPI); 1.0 for a window
    /// that Windows scales.
    pub fn scale(&self) -> f32 {
        dpi::of(self.hwnd) as f32 / 96.0
    }

    /// Sets the client area to exactly `width` × `height`.
    pub fn resize(&self, width: u32, height: u32) {
        let (w, h) = outer_size(self.style, width, height, dpi::of(self.hwnd));
        let set = |w: i32, h: i32| unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
        set(w, h);
        // Whatever the frame's real borders are (themes, rounding at odd
        // scales): correct by what is missing.
        let (cw, ch) = self.size();
        if (cw, ch) != (width, height) {
            let mut r: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetWindowRect(self.hwnd, &mut r) };
            set(
                r.right - r.left + width as i32 - cw as i32,
                r.bottom - r.top + height as i32 - ch as i32,
            );
        }
    }

    /// Whether the plugin's window, or one inside it, is set up for
    /// OpenGL (has a pixel format).
    pub fn draws_opengl(&self) -> bool {
        fn any_gl(first: HWND) -> bool {
            let mut w = first;
            while !w.is_null() {
                let dc = unsafe { GetDC(w) };
                let format = unsafe { GetPixelFormat(dc) };
                unsafe { ReleaseDC(w, dc) };
                if format > 0 || any_gl(unsafe { GetWindow(w, GW_CHILD) }) {
                    return true;
                }
                w = unsafe { GetWindow(w, GW_HWNDNEXT) };
            }
            false
        }
        any_gl(unsafe { GetWindow(self.hwnd, GW_CHILD) })
    }

    pub fn size(&self) -> (u32, u32) {
        let mut r: RECT = unsafe { std::mem::zeroed() };
        unsafe { GetClientRect(self.hwnd, &mut r) };
        ((r.right - r.left) as u32, (r.bottom - r.top) as u32)
    }

    /// Dispatches this window's messages (the app's event loop does it
    /// too); returns `false` once the user closed it.
    pub fn poll(&self) -> bool {
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { PeekMessageW(&mut msg, self.hwnd, 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        !closed()
            .lock()
            .is_ok_and(|c| c.contains(&(self.hwnd as usize)))
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd) };
        if let Ok(mut c) = closed().lock() {
            c.remove(&(self.hwnd as usize));
        }
    }
}
