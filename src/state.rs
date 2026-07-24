//! Lock-free state shared between the render thread and the event-tap thread.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use crate::settings::Shortcut;

/// Stamped on synthetic events so the tap passes its own injections.
pub const SYNTH_MAGIC: i64 = 0x00C0_FFEE;

pub struct Shared {
    /// Trigger is currently held down.
    pub trigger_down: AtomicBool,
    /// Monotonic ms timestamp of when the trigger went down.
    pub trigger_down_at: AtomicU64,
    /// The wheel is on screen.
    pub ring_visible: AtomicBool,
    /// Currently highlighted card index.
    pub selected: AtomicUsize,
    /// Selectable item count, for nav clamping.
    pub item_count: AtomicUsize,

    /// tap → main: promote `selected` and paste it.
    pub request_paste: AtomicBool,
    /// tap → main: dismiss the ring without pasting.
    pub cancel: AtomicBool,
    /// tap → main: open the settings pane.
    pub open_settings: AtomicBool,
    /// main → tap: capture the next keypress as a new trigger shortcut.
    pub capturing: AtomicBool,
    /// tap → main: a new shortcut was captured; persist settings.
    pub settings_dirty: AtomicBool,
    /// Time to quit.
    pub quit: AtomicBool,
    /// Guard so our own synthetic events flow through the tap untouched.
    pub passthrough: AtomicBool,
    /// Monotonic ms until which all trigger events pass through. Posted
    /// events reach the tap asynchronously, so a flag cleared after
    /// `CGEventPost` can race; a deadline can't.
    pub passthrough_until: AtomicU64,
    /// True once the event tap was installed (Accessibility granted).
    pub tap_ok: AtomicBool,

    /// Live trigger configuration, readable from the tap thread.
    pub trig_keycode: AtomicU16,
    pub trig_mods: AtomicU32,
    pub hold_ms: AtomicU64,

    start: Instant,
}

impl Shared {
    pub fn new(trigger: Shortcut, hold_ms: u64, start: Instant) -> Self {
        Shared {
            trigger_down: AtomicBool::new(false),
            trigger_down_at: AtomicU64::new(0),
            ring_visible: AtomicBool::new(false),
            selected: AtomicUsize::new(0),
            item_count: AtomicUsize::new(0),
            request_paste: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            open_settings: AtomicBool::new(false),
            capturing: AtomicBool::new(false),
            settings_dirty: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            passthrough: AtomicBool::new(false),
            passthrough_until: AtomicU64::new(0),
            tap_ok: AtomicBool::new(false),
            trig_keycode: AtomicU16::new(trigger.keycode),
            trig_mods: AtomicU32::new(trigger.mods),
            hold_ms: AtomicU64::new(hold_ms),
            start,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    pub fn trigger(&self) -> Shortcut {
        Shortcut {
            keycode: self.trig_keycode.load(Ordering::SeqCst),
            mods: self.trig_mods.load(Ordering::SeqCst),
        }
    }

    pub fn set_trigger(&self, s: Shortcut) {
        self.trig_keycode.store(s.keycode, Ordering::SeqCst);
        self.trig_mods.store(s.mods, Ordering::SeqCst);
    }
}
