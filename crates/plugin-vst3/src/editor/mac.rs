// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin editor window on macOS (main thread only).

use jdance_plugin_api::PluginError;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::ffi::c_void;

pub struct NativeWindow {
    window: Retained<NSWindow>,
}

impl NativeWindow {
    pub fn new(title: &str, width: u32, height: u32) -> Result<Self, PluginError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            PluginError::Unsupported("plugin editors open on the main thread only".into())
        })?;
        let _app = NSApplication::sharedApplication(mtm);
        let rect = NSRect::new(
            NSPoint::new(200.0, 200.0),
            NSSize::new(f64::from(width), f64::from(height)),
        );
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // Closing hides the window; the host releases it.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        window.makeKeyAndOrderFront(None);
        Ok(Self { window })
    }

    pub fn parent(&self) -> *mut c_void {
        self.window.contentView().map_or(std::ptr::null_mut(), |v| {
            Retained::as_ptr(&v) as *mut c_void
        })
    }

    pub fn resize(&self, width: u32, height: u32) {
        self.window
            .setContentSize(NSSize::new(f64::from(width), f64::from(height)));
    }

    pub fn size(&self) -> (u32, u32) {
        let r = self.window.contentRectForFrameRect(self.window.frame());
        (r.size.width.round() as u32, r.size.height.round() as u32)
    }

    /// Returns `false` once the user closed the window.
    pub fn poll(&self) -> bool {
        self.window.isVisible() || self.window.isMiniaturized()
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        self.window.close();
    }
}
