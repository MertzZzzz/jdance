// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin editor window on X11 (also under XWayland, ADR-010).

use jdance_plugin_api::PluginError;
use std::cell::Cell;
use std::ffi::c_void;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, GrabMode, ModMask,
    PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub struct NativeWindow {
    conn: RustConnection,
    window: u32,
    wm_delete: u32,
    size: Cell<(u32, u32)>,
    closed: Cell<bool>,
    /// Keycode of Space, grabbed on this window (spec 0.2.3 KEY-004).
    space: Option<u8>,
    /// Time of the last Space release: auto-repeat sends a press with the
    /// same time (KEY-005).
    space_released: Cell<u32>,
}

/// The keycode that produces the Space keysym, if any.
fn space_keycode(conn: &RustConnection) -> Option<u8> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn
        .get_keyboard_mapping(min, max - min + 1)
        .ok()?
        .reply()
        .ok()?;
    let per = usize::from(map.keysyms_per_keycode.max(1));
    map.keysyms
        .chunks(per)
        .position(|syms| syms.contains(&0x20))
        .map(|i| min + i as u8)
}

fn err(e: impl std::fmt::Display) -> PluginError {
    PluginError::Unsupported(format!("X11: {e}"))
}

impl NativeWindow {
    /// `resizable` is left to the window manager on X11.
    pub fn new(
        title: &str,
        width: u32,
        height: u32,
        _resizable: bool,
    ) -> Result<Self, PluginError> {
        let (conn, screen_num) = x11rb::connect(None).map_err(err)?;
        let screen = &conn.setup().roots[screen_num];
        let (root, black) = (screen.root, screen.black_pixel);
        let window = conn.generate_id().map_err(err)?;
        let (w, h) = (width.clamp(1, 16_384), height.clamp(1, 16_384));
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            w as u16,
            h as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(black)
                .event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .map_err(err)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        )
        .map_err(err)?;
        let atom = |name: &[u8]| -> Result<u32, PluginError> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(err)?
                .reply()
                .map_err(err)?
                .atom)
        };
        let protocols = atom(b"WM_PROTOCOLS")?;
        let wm_delete = atom(b"WM_DELETE_WINDOW")?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            protocols,
            AtomEnum::ATOM,
            &[wm_delete],
        )
        .map_err(err)?;
        // Space anywhere in the window, the plugin's embedded view
        // included, comes to us first: a passive grab on the frame.
        let space = space_keycode(&conn);
        if let Some(code) = space {
            let _ = conn.grab_key(
                false,
                window,
                ModMask::ANY,
                code,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            );
        }
        conn.map_window(window).map_err(err)?;
        // Make sure the window exists before the plugin embeds into it.
        conn.sync().map_err(err)?;
        Ok(Self {
            conn,
            window,
            wm_delete,
            size: Cell::new((w, h)),
            closed: Cell::new(false),
            space,
            space_released: Cell::new(u32::MAX),
        })
    }

    pub fn parent(&self) -> *mut c_void {
        self.window as usize as *mut c_void
    }

    /// One coordinate system here (Windows has one per DPI awareness).
    pub fn scope(&self) -> Option<()> {
        None
    }

    pub fn resize(&self, width: u32, height: u32) {
        let (w, h) = (width.clamp(1, 16_384), height.clamp(1, 16_384));
        let _ = self
            .conn
            .configure_window(self.window, &ConfigureWindowAux::new().width(w).height(h));
        let _ = self.conn.flush();
        self.size.set((w, h));
    }

    pub fn size(&self) -> (u32, u32) {
        self.size.get()
    }

    /// Views are not scaled by the host on X11.
    #[allow(dead_code)]
    pub fn scale(&self) -> f32 {
        1.0
    }

    /// Handles window events; returns `false` once the user closed it.
    pub fn poll(&self) -> bool {
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::ClientMessage(e) if e.data.as_data32()[0] == self.wm_delete => {
                    self.closed.set(true);
                }
                Event::ConfigureNotify(e) if e.window == self.window => {
                    self.size.set((u32::from(e.width), u32::from(e.height)));
                }
                Event::KeyPress(e) if Some(e.detail) == self.space => {
                    if e.time != self.space_released.get() {
                        super::note_space_press();
                    }
                }
                Event::KeyRelease(e) if Some(e.detail) == self.space => {
                    self.space_released.set(e.time);
                }
                _ => {}
            }
        }
        !self.closed.get()
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}
