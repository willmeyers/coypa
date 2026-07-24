//! User preferences, persisted as a tiny hand-rolled `key = value` file
//! (no serde — keeps the dependency tree lean, per project constraint).
//!
//! Config path: `$XDG_CONFIG_HOME/coypa/config` or `~/.config/coypa/config`
//! (on macOS this resolves under the user's home just fine).

use std::fs;
use std::path::PathBuf;

/// A global trigger shortcut, expressed with macOS virtual key codes and a
/// modifier bitmask that mirrors `CGEventFlags`' meaningful bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// macOS virtual keycode of the main key (e.g. `9` == V).
    pub keycode: u16,
    /// Modifier flags required. Bit layout matches our `mods` helpers below.
    pub mods: u32,
}

pub mod modbit {
    pub const CMD: u32 = 1 << 0;
    pub const SHIFT: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
    pub const CTRL: u32 = 1 << 3;
}

/// macOS virtual keycode for the `V` key.
pub const KEY_V: u16 = 9;

impl Shortcut {
    /// The default trigger: ⌘V — literally hijacking the OS paste key.
    pub fn default_trigger() -> Self {
        Shortcut { keycode: KEY_V, mods: modbit::CMD }
    }

    /// Human-readable form, e.g. `⌘V` or `⌘⇧V`. Used by the settings pane.
    #[allow(dead_code)]
    pub fn describe(&self) -> String {
        let mut s = String::new();
        if self.mods & modbit::CTRL != 0 { s.push('⌃'); }
        if self.mods & modbit::ALT != 0 { s.push('⌥'); }
        if self.mods & modbit::SHIFT != 0 { s.push('⇧'); }
        if self.mods & modbit::CMD != 0 { s.push('⌘'); }
        s.push_str(keycode_name(self.keycode));
        s
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// Trigger shortcut that summons the flywheel.
    pub trigger: Shortcut,
    /// How long (ms) the trigger must be held before the ring appears.
    /// A shorter tap-and-release falls through to a normal paste.
    pub hold_ms: u64,
    /// Max number of items kept on the stack.
    pub max_history: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            trigger: Shortcut::default_trigger(),
            hold_ms: 220,
            max_history: 24,
        }
    }
}

impl Settings {
    pub fn config_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("coypa").join("config"))
    }

    /// Load settings, falling back to defaults for any missing/unparsable key.
    pub fn load() -> Self {
        let mut s = Settings::default();
        let Some(path) = Self::config_path() else { return s };
        let Ok(text) = fs::read_to_string(&path) else { return s };

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "trigger_keycode" => if let Ok(n) = v.parse() { s.trigger.keycode = n; },
                "trigger_mods" => if let Ok(n) = v.parse() { s.trigger.mods = n; },
                "hold_ms" => if let Ok(n) = v.parse() { s.hold_ms = n; },
                "max_history" => if let Ok(n) = v.parse::<usize>() { s.max_history = n.clamp(1, 100); },
                _ => {}
            }
        }
        s
    }

    /// Persist to disk (best-effort; errors are swallowed).
    pub fn save(&self) {
        let Some(path) = Self::config_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let body = format!(
            "# coypa preferences\n\
             trigger_keycode = {}\n\
             trigger_mods = {}\n\
             hold_ms = {}\n\
             max_history = {}\n",
            self.trigger.keycode, self.trigger.mods, self.hold_ms, self.max_history
        );
        let _ = fs::write(&path, body);
    }
}

/// Best-effort mapping from macOS virtual keycodes to a display glyph/name.
#[allow(dead_code)]
pub fn keycode_name(code: u16) -> &'static str {
    match code {
        0 => "A", 1 => "S", 2 => "D", 3 => "F", 4 => "H", 5 => "G",
        6 => "Z", 7 => "X", 8 => "C", 9 => "V", 11 => "B", 12 => "Q",
        13 => "W", 14 => "E", 15 => "R", 16 => "Y", 17 => "T",
        31 => "O", 32 => "U", 34 => "I", 35 => "P", 37 => "L",
        38 => "J", 40 => "K", 45 => "N", 46 => "M",
        18 => "1", 19 => "2", 20 => "3", 21 => "4", 23 => "5",
        22 => "6", 26 => "7", 28 => "8", 25 => "9", 29 => "0",
        49 => "Space", 36 => "Return", 48 => "Tab", 53 => "Esc",
        _ => "?",
    }
}
