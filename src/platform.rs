//! macOS platform glue: global key tap, synthetic paste, click-through,
//! accessory mode, pasteboard change token.
//!
//! The tap uses raw CoreGraphics FFI rather than the `core-graphics` crate:
//! that wrapper's callback trampoline returns the original event on `None`
//! instead of NULL, so it can never suppress a key.

#![allow(unused)]

use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

use crate::settings::{modbit, Shortcut};
use crate::state::{Shared, SYNTH_MAGIC};

// macOS virtual keycodes.
pub const KC_V: u16 = 9;
const KC_ESC: u16 = 53;
const KC_LEFT: u16 = 123;
const KC_RIGHT: u16 = 124;
const KC_DOWN: u16 = 125;
const KC_UP: u16 = 126;
const KC_COMMA: u16 = 43;

// CGEventType values.
const ET_KEY_DOWN: u32 = 10;
const ET_KEY_UP: u32 = 11;
const ET_TAP_DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
const ET_TAP_DISABLED_USER_INPUT: u32 = 0xFFFF_FFFF;

// CGEventField values.
const F_KEYCODE: u32 = 9;
const F_AUTOREPEAT: u32 = 8;
const F_USER_DATA: u32 = 42;

// CGEventFlags modifier bits.
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_ALTERNATE: u64 = 0x0008_0000;
const FLAG_COMMAND: u64 = 0x0010_0000;

fn is_modifier_keycode(kc: u16) -> bool {
    matches!(kc, 54 | 55 | 56 | 57 | 58 | 59 | 60 | 61 | 62 | 63)
}

fn digit_index(kc: u16) -> Option<usize> {
    match kc {
        18 => Some(0), 19 => Some(1), 20 => Some(2), 21 => Some(3),
        23 => Some(4), 22 => Some(5), 26 => Some(6), 28 => Some(7),
        25 => Some(8),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Global trigger (event tap)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub fn spawn_trigger(shared: Arc<Shared>) {
    std::thread::Builder::new()
        .name("coypa-trigger".into())
        .spawn(move || run_tap(shared))
        .expect("spawn trigger thread");
}

#[cfg(not(target_os = "macos"))]
pub fn spawn_trigger(_shared: Arc<Shared>) {}

#[cfg(target_os = "macos")]
type CGEventRef = *mut c_void;
#[cfg(target_os = "macos")]
type CGEventTapProxy = *mut c_void;
#[cfg(target_os = "macos")]
type TapCallback =
    extern "C" fn(proxy: CGEventTapProxy, etype: u32, event: CGEventRef, user: *mut c_void) -> CGEventRef;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> *mut c_void; // CFMachPortRef
    fn CGEventTapEnable(port: *mut c_void, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
}

/// Live tap port, so the callback can re-enable itself if macOS disables it.
#[cfg(target_os = "macos")]
static TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

#[cfg(target_os = "macos")]
fn run_tap(shared: Arc<Shared>) {
    use core_foundation::base::TCFType;
    use core_foundation::mach_port::CFMachPort;
    use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};

    let events = (1u64 << ET_KEY_DOWN) | (1u64 << ET_KEY_UP);
    // Leaked for the program lifetime; the tap thread outlives main's loop.
    let user = Arc::into_raw(shared.clone()) as *mut c_void;

    let port = unsafe {
        CGEventTapCreate(
            0, // kCGHIDEventTap
            0, // kCGHeadInsertEventTap
            0, // kCGEventTapOptionDefault (active filter — can suppress)
            events,
            tap_callback,
            user,
        )
    };

    if port.is_null() {
        eprintln!(
            "coypa: could not install the keyboard tap.\n\
             Grant Accessibility permission in System Settings ▸ Privacy & Security ▸ \
             Accessibility, then relaunch."
        );
        return;
    }
    TAP_PORT.store(port, Ordering::SeqCst);

    unsafe {
        let mach = CFMachPort::wrap_under_create_rule(port as _);
        let source = match mach.create_runloop_source(0) {
            Ok(s) => s,
            Err(_) => {
                eprintln!("coypa: failed to create run loop source for the tap");
                return;
            }
        };
        CFRunLoop::get_current().add_source(&source, kCFRunLoopCommonModes);
        CGEventTapEnable(port, true);
        shared.tap_ok.store(true, Ordering::SeqCst);
        CFRunLoop::run_current();
    }
}

#[cfg(target_os = "macos")]
extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user: *mut c_void,
) -> CGEventRef {
    // Re-arm the tap if the OS disabled it.
    if etype == ET_TAP_DISABLED_TIMEOUT || etype == ET_TAP_DISABLED_USER_INPUT {
        let port = TAP_PORT.load(Ordering::SeqCst);
        if !port.is_null() {
            unsafe { CGEventTapEnable(port, true) };
        }
        return event;
    }

    let shared: &Shared = unsafe { &*(user as *const Shared) };
    let pass = event; // returning the event passes it through
    let swallow: CGEventRef = ptr::null_mut(); // returning NULL drops it

    // Let our own synthetic paste through untouched.
    let user_data = unsafe { CGEventGetIntegerValueField(event, F_USER_DATA) };
    if user_data == SYNTH_MAGIC
        || shared.passthrough.load(Ordering::SeqCst)
        || shared.now_ms() < shared.passthrough_until.load(Ordering::SeqCst)
    {
        return pass;
    }

    let kc = unsafe { CGEventGetIntegerValueField(event, F_KEYCODE) } as u16;
    let flags = unsafe { CGEventGetFlags(event) };
    let mods = sig_mods(flags);
    let repeat = unsafe { CGEventGetIntegerValueField(event, F_AUTOREPEAT) } != 0;

    // Settings capture owns the keyboard while active.
    if shared.capturing.load(Ordering::SeqCst) {
        if etype == ET_KEY_DOWN {
            if kc == KC_ESC {
                shared.capturing.store(false, Ordering::SeqCst);
            } else if !is_modifier_keycode(kc) {
                shared.set_trigger(Shortcut { keycode: kc, mods });
                shared.capturing.store(false, Ordering::SeqCst);
                shared.settings_dirty.store(true, Ordering::SeqCst);
            }
        }
        return swallow;
    }

    let trigger = shared.trigger();
    let is_trigger = kc == trigger.keycode && mods == trigger.mods;

    if etype == ET_KEY_DOWN {
        if is_trigger {
            if !repeat {
                shared.trigger_down.store(true, Ordering::SeqCst);
                shared.trigger_down_at.store(shared.now_ms(), Ordering::SeqCst);
                shared.selected.store(0, Ordering::SeqCst);
            }
            return swallow; // never let the OS paste while we own the key
        }

        if shared.ring_visible.load(Ordering::SeqCst) {
            let count = shared.item_count.load(Ordering::SeqCst).max(1);
            match kc {
                KC_UP | KC_LEFT => {
                    let s = shared.selected.load(Ordering::SeqCst);
                    shared.selected.store(s.saturating_sub(1), Ordering::SeqCst);
                    return swallow;
                }
                KC_DOWN | KC_RIGHT => {
                    let s = shared.selected.load(Ordering::SeqCst);
                    shared.selected.store((s + 1).min(count - 1), Ordering::SeqCst);
                    return swallow;
                }
                KC_ESC => {
                    shared.cancel.store(true, Ordering::SeqCst);
                    return swallow;
                }
                KC_COMMA => {
                    shared.open_settings.store(true, Ordering::SeqCst);
                    return swallow;
                }
                _ => {
                    if let Some(idx) = digit_index(kc) {
                        if idx < count {
                            shared.selected.store(idx, Ordering::SeqCst);
                        }
                        return swallow;
                    }
                }
            }
        }
        return pass;
    }

    if etype == ET_KEY_UP {
        if kc == trigger.keycode && shared.trigger_down.load(Ordering::SeqCst) {
            shared.trigger_down.store(false, Ordering::SeqCst);
            shared.request_paste.store(true, Ordering::SeqCst);
            return swallow;
        }
        return pass;
    }

    pass
}

#[cfg(target_os = "macos")]
fn sig_mods(flags: u64) -> u32 {
    let mut m = 0;
    if flags & FLAG_COMMAND != 0 { m |= modbit::CMD; }
    if flags & FLAG_SHIFT != 0 { m |= modbit::SHIFT; }
    if flags & FLAG_ALTERNATE != 0 { m |= modbit::ALT; }
    if flags & FLAG_CONTROL != 0 { m |= modbit::CTRL; }
    m
}

// ---------------------------------------------------------------------------
// Synthetic paste (post Cmd+V to the frontmost app)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub fn synthesize_paste(shared: &Shared) {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, EventField};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    // Posted events reach the tap thread asynchronously, so a flag cleared
    // right after `post` would race; use a deadline.
    shared
        .passthrough_until
        .store(shared.now_ms() + 120, Ordering::SeqCst);

    if let Ok(src) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) {
        for down in [true, false] {
            if let Ok(ev) = CGEvent::new_keyboard_event(src.clone(), KC_V, down) {
                ev.set_flags(CGEventFlags::CGEventFlagCommand);
                ev.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, SYNTH_MAGIC);
                ev.post(CGEventTapLocation::HID);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn synthesize_paste(_shared: &Shared) {}

// --- Click-through ---------------------------------------------------------
//
// Don't use `SDL_SetWindowShape`: the shape is both an input region and a
// visual mask (`SDL_RenderPresent` multiplies the framebuffer by its alpha),
// so a transparent shape makes the window invisible.

#[cfg(target_os = "macos")]
#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

/// No Dock icon, no ⌘-Tab entry, never takes focus. Same as `LSUIElement`, but
/// at runtime so it also holds when run from a terminal. Call after SDL video
/// init (SDL creates the NSApplication).
#[cfg(target_os = "macos")]
pub fn become_accessory_app() {
    const NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY: i64 = 1;
    unsafe {
        let cls = objc_getClass(b"NSApplication\0".as_ptr() as *const c_char);
        if cls.is_null() {
            return;
        }
        let msg_id: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const c_void);
        let app = msg_id(
            cls,
            sel_registerName(b"sharedApplication\0".as_ptr() as *const c_char),
        );
        if app.is_null() {
            return;
        }
        let msg_pol: extern "C" fn(*mut c_void, *mut c_void, i64) -> bool =
            std::mem::transmute(objc_msgSend as *const c_void);
        msg_pol(
            app,
            sel_registerName(b"setActivationPolicy:\0".as_ptr() as *const c_char),
            NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY,
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn become_accessory_app() {}

/// O(1) token that bumps on every clipboard write.
///
/// We must poll: SDL only checks the pasteboard in `windowDidBecomeKey`, and
/// the overlay is NOT_FOCUSABLE. Hashing contents instead would pull the whole
/// payload every tick — untenable for images.
#[cfg(target_os = "macos")]
pub fn clipboard_change_count() -> Option<i64> {
    unsafe {
        let cls = objc_getClass(b"NSPasteboard\0".as_ptr() as *const c_char);
        if cls.is_null() {
            return None;
        }
        let msg_id: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const c_void);
        let pb = msg_id(
            cls,
            sel_registerName(b"generalPasteboard\0".as_ptr() as *const c_char),
        );
        if pb.is_null() {
            return None;
        }
        let msg_i64: extern "C" fn(*mut c_void, *mut c_void) -> i64 =
            std::mem::transmute(objc_msgSend as *const c_void);
        Some(msg_i64(
            pb,
            sel_registerName(b"changeCount\0".as_ptr() as *const c_char),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn clipboard_change_count() -> Option<i64> {
    None
}

/// Resolve the `NSWindow*` behind an SDL window.
#[cfg(target_os = "macos")]
fn nswindow(sdl_window: *mut sdl3::sys::video::SDL_Window) -> *mut c_void {
    use sdl3::sys::properties::SDL_GetPointerProperty;
    use sdl3::sys::video::{SDL_GetWindowProperties, SDL_PROP_WINDOW_COCOA_WINDOW_POINTER};
    unsafe {
        let props = SDL_GetWindowProperties(sdl_window);
        SDL_GetPointerProperty(props, SDL_PROP_WINDOW_COCOA_WINDOW_POINTER, ptr::null_mut())
    }
}

/// Make the overlay transparent to clicks. Call every frame: SDL resets
/// `ignoresMouseEvents` inside Cocoa's `mouseMoved:`. Refusing mouse-moved
/// events stops that reset at the source.
#[cfg(target_os = "macos")]
pub fn apply_click_through(sdl_window: *mut sdl3::sys::video::SDL_Window) {
    let win = nswindow(sdl_window);
    if win.is_null() {
        return;
    }
    unsafe {
        let msg_bool: extern "C" fn(*mut c_void, *mut c_void, bool) =
            std::mem::transmute(objc_msgSend as *const c_void);
        msg_bool(
            win,
            sel_registerName(b"setIgnoresMouseEvents:\0".as_ptr() as *const c_char),
            true,
        );
        msg_bool(
            win,
            sel_registerName(b"setAcceptsMouseMovedEvents:\0".as_ptr() as *const c_char),
            false,
        );
    }
}

/// Read back the current `ignoresMouseEvents` value (diagnostics).
#[cfg(target_os = "macos")]
pub fn is_click_through(sdl_window: *mut sdl3::sys::video::SDL_Window) -> Option<bool> {
    let win = nswindow(sdl_window);
    if win.is_null() {
        return None;
    }
    unsafe {
        let sel = sel_registerName(b"ignoresMouseEvents\0".as_ptr() as *const c_char);
        let msg: extern "C" fn(*mut c_void, *mut c_void) -> bool =
            std::mem::transmute(objc_msgSend as *const c_void);
        Some(msg(win, sel))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn apply_click_through(_sdl_window: *mut sdl3::sys::video::SDL_Window) {}

#[cfg(not(target_os = "macos"))]
pub fn is_click_through(_sdl_window: *mut sdl3::sys::video::SDL_Window) -> Option<bool> {
    None
}
