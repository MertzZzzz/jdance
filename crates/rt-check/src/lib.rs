// SPDX-License-Identifier: GPL-3.0-or-later
//! Allocation guard for realtime code (RT-001).
//!
//! A test binary installs [`GuardedAllocator`] as its global allocator and
//! wraps realtime code in [`rt_section`]. Any allocation, reallocation or
//! deallocation on that thread while inside the section is counted; the test
//! then asserts the count is zero.
//!
//! The allocator never panics or aborts on a violation: panicking inside the
//! allocator is not sound, and counting keeps the measured code's behavior
//! unchanged.
//!
//! Safety model: the only `unsafe` code is the [`GlobalAlloc`] impl, which
//! forwards every call unchanged to [`System`]; the bookkeeping uses a
//! `const`-initialized thread-local `Cell` (no allocation, no destructor) and
//! a relaxed atomic counter.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    static IN_RT: Cell<u32> = const { Cell::new(0) };
}

static VIOLATIONS: AtomicUsize = AtomicUsize::new(0);

/// Global allocator that counts heap operations inside [`rt_section`].
#[derive(Debug, Default)]
pub struct GuardedAllocator;

#[inline]
fn note() {
    // `try_with` because the allocator may be called during TLS teardown.
    let in_rt = IN_RT.try_with(|c| c.get() > 0).unwrap_or(false);
    if in_rt {
        VIOLATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards its arguments unchanged to `System`, which
// upholds the `GlobalAlloc` contract; `note()` performs no allocation.
unsafe impl GlobalAlloc for GuardedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: caller upholds `GlobalAlloc::alloc` preconditions.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        // SAFETY: `ptr` was allocated by `System` through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: as above.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Runs `f` as realtime code on the current thread. Sections nest.
pub fn rt_section<R>(f: impl FnOnce() -> R) -> R {
    struct Exit;
    impl Drop for Exit {
        fn drop(&mut self) {
            IN_RT.with(|c| c.set(c.get() - 1));
        }
    }
    IN_RT.with(|c| c.set(c.get() + 1));
    let _exit = Exit;
    f()
}

/// Temporarily leaves the realtime section, e.g. around a third-party plugin
/// call whose behavior the host cannot control (RT-002).
pub fn outside_rt<R>(f: impl FnOnce() -> R) -> R {
    let saved = IN_RT.with(|c| c.replace(0));
    struct Restore(u32);
    impl Drop for Restore {
        fn drop(&mut self) {
            IN_RT.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(saved);
    f()
}

/// Number of heap operations observed inside realtime sections so far.
pub fn violations() -> usize {
    VIOLATIONS.load(Ordering::Relaxed)
}

/// Resets the violation counter (process-wide).
pub fn reset_violations() {
    VIOLATIONS.store(0, Ordering::Relaxed);
}
