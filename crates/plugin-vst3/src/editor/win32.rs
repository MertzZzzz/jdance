// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin editor window on Windows.

use jdance_plugin_api::PluginError;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, VK_SPACE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CW_USEDEFAULT, CallNextHookEx, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, GA_ROOT, GetAncestor, GetClassNameW, GetClientRect, IDC_ARROW,
    LoadCursorW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOZORDER, SetWindowPos, SetWindowsHookExW, TranslateMessage, WH_GETMESSAGE, WM_CLOSE,
    WM_KEYDOWN, WM_KEYUP, WM_NULL, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED,
    WS_SYSMENU, WS_VISIBLE,
};

const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

/// Windows the user asked to close (by address).
fn closed() -> &'static Mutex<HashSet<usize>> {
    static CLOSED: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    CLOSED.get_or_init(Default::default)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CLOSE {
        // Closing only hides the editor from the host's point of view;
        // the host removes the view and destroys the window.
        if let Ok(mut c) = closed().lock() {
            c.insert(hwnd as usize);
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
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
            lpszClassName: name.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        unsafe { RegisterClassW(&wc) };
        name
    })
}

fn outer_size(width: u32, height: u32) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    unsafe { AdjustWindowRectEx(&mut r, STYLE, 0, 0) };
    (r.right - r.left, r.bottom - r.top)
}

pub struct NativeWindow {
    hwnd: HWND,
}

impl NativeWindow {
    pub fn new(title: &str, width: u32, height: u32) -> Result<Self, PluginError> {
        let (w, h) = outer_size(width, height);
        let title = wide(title);
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name().as_ptr(),
                title.as_ptr(),
                STYLE | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(PluginError::Failed("CreateWindowExW failed".into()));
        }
        install_key_hook();
        Ok(Self { hwnd })
    }

    pub fn parent(&self) -> *mut c_void {
        self.hwnd
    }

    pub fn resize(&self, width: u32, height: u32) {
        let (w, h) = outer_size(width, height);
        unsafe {
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
