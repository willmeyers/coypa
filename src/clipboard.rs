//! The clipboard stack.
//!
//! Reads through SDL3's clipboard API, a thin wrapper over the native
//! pasteboard, so every mime-type representation is available without per-OS
//! clipboard code.

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

use sdl3::sys::clipboard as sysclip;
use sdl3::sys::stdinc::SDL_free;

/// Drives the chip's icon and accent, and how the item is restored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MimeKind {
    Text,
    /// A copied hyperlink (plain text that is a single URL).
    Link,
    Html,
    Rtf,
    Image,
    Files,
    Other,
}

/// Does this text read as a single copied hyperlink?
fn looks_like_url(t: &str) -> bool {
    let t = t.trim();
    !t.is_empty()
        && !t.contains(char::is_whitespace)
        && (t.starts_with("http://") || t.starts_with("https://") || t.starts_with("www."))
}

/// One representation of a copied item for a particular mime type.
#[derive(Clone)]
pub struct Repr {
    pub mime: String,
    pub data: Vec<u8>,
}

/// A single entry on the clipboard stack.
#[derive(Clone)]
pub struct ClipItem {
    /// Canonical mime type. Kept in full for a future hover label.
    #[allow(dead_code)]
    pub primary_mime: String,
    pub kind: MimeKind,
    /// Every representation offered, for a full-fidelity restore.
    pub reprs: Vec<Repr>,
    /// Plain-text form, if the item had one (used for preview + simple paste).
    pub text: Option<String>,
    /// Short, single-line preview string (for an upcoming hover label).
    #[allow(dead_code)]
    pub preview: String,
    /// Size in bytes of the primary representation.
    #[allow(dead_code)]
    pub size: usize,
    /// Cheap content signature, for change detection and dedupe.
    pub signature: u64,
}

impl ClipItem {
    /// Kept for a future hover label.
    #[allow(dead_code)]
    pub fn pretty_size(&self) -> String {
        pretty_bytes(self.size)
    }
}

pub struct ClipboardManager {
    items: VecDeque<ClipItem>,
    max: usize,
    /// Fallback change detection where no native token exists.
    last_signature: u64,
    /// Native change token, when the platform offers one.
    last_change: Option<i64>,
}

impl ClipboardManager {
    pub fn new(max: usize) -> Self {
        ClipboardManager {
            items: VecDeque::new(),
            max,
            last_signature: 0,
            last_change: None,
        }
    }

    pub fn items(&self) -> &VecDeque<ClipItem> {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Push a freshly-read item onto the top of the stack, skipping consecutive
    /// duplicates. Returns true if the stack changed.
    pub fn push(&mut self, item: ClipItem) -> bool {
        if let Some(front) = self.items.front() {
            if same_content(front, &item) {
                return false;
            }
        }
        // Lift an existing duplicate rather than adding another.
        if let Some(pos) = self.items.iter().position(|i| same_content(i, &item)) {
            self.items.remove(pos);
        }
        self.items.push_front(item);
        while self.items.len() > self.max {
            self.items.pop_back();
        }
        true
    }

    /// Capture the clipboard if it changed. Returns true if an item was added.
    ///
    /// An unchanged clipboard costs one integer read where a native change
    /// token exists; the payload is only pulled on a real change.
    pub fn poll_os(&mut self) -> bool {
        if let Some(count) = crate::platform::clipboard_change_count() {
            if self.last_change == Some(count) {
                return false;
            }
            self.last_change = Some(count);
        } else {
            // No native token: fall back to hashing.
            let sig = unsafe { quick_signature() };
            if sig == self.last_signature {
                return false;
            }
            self.last_signature = sig;
        }

        match unsafe { read_current() } {
            Some(item) => self.push(item),
            None => false,
        }
    }

    /// Pop item `index` off the stack and put it on the clipboard to be pasted.
    ///
    /// Index 0 is not rewritten: it is already the live clipboard, and its
    /// owning app may publish richer representations than we captured.
    ///
    /// Main thread only (NSPasteboard).
    pub fn pop_for_paste(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        let item = self.items.remove(index).unwrap();
        let ok = if index == 0 { true } else { unsafe { set_os_clipboard(&item) } };
        // Adopt the current token so our own write doesn't read back as new.
        self.last_change = crate::platform::clipboard_change_count();
        if self.last_change.is_none() {
            self.last_signature = unsafe { quick_signature() };
        }
        ok
    }
}

/// Dedupe on what an item *is*, not how it was advertised: two captures of the
/// same clipboard can differ in signature when mime lists get reordered.
fn same_content(a: &ClipItem, b: &ClipItem) -> bool {
    if a.signature == b.signature {
        return true;
    }
    if a.kind != b.kind {
        return false;
    }
    match (&a.text, &b.text) {
        (Some(ta), Some(tb)) => ta == tb,
        // Binary: same kind and size is as close as we get cheaply.
        (None, None) => a.size == b.size && a.size > 0,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Low-level clipboard access (unsafe SDL sys bridge)
// ---------------------------------------------------------------------------

/// Types we probe for, richest-first within each family.
const PROBE_MIMES: &[&str] = &[
    "image/png",
    "image/tiff",
    "image/jpeg",
    "image/gif",
    "image/bmp",
    "image/webp",
    "text/uri-list",
    "text/html",
    "text/rtf",
    "application/rtf",
    "text/plain",
];

/// Mime types currently on the clipboard.
///
/// Deliberately avoids `SDL_GetClipboardMimeTypes`: that cache is only
/// refreshed by clipboard-update events, which this app never receives, so the
/// only thing that fills it is our own writes. `SDL_HasClipboardData` is live.
unsafe fn read_mimes() -> Vec<String> {
    let mut out = Vec::new();
    for m in PROBE_MIMES {
        if let Ok(c) = CString::new(*m) {
            if sysclip::SDL_HasClipboardData(c.as_ptr()) {
                out.push((*m).to_string());
            }
        }
    }
    out
}

/// Read the raw bytes the clipboard holds for a given mime type.
unsafe fn read_data(mime: &str) -> Option<Vec<u8>> {
    let cmime = CString::new(mime).ok()?;
    let mut size: usize = 0;
    let ptr = sysclip::SDL_GetClipboardData(cmime.as_ptr(), &mut size);
    if ptr.is_null() {
        return None;
    }
    let out = if size == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(ptr as *const u8, size).to_vec()
    };
    SDL_free(ptr);
    Some(out)
}

/// Fetch the plain-text representation via the dedicated text API (handles the
/// common case where SDL exposes text but not an explicit `text/plain` entry).
unsafe fn read_text() -> Option<String> {
    if !sysclip::SDL_HasClipboardText() {
        return None;
    }
    let ptr = sysclip::SDL_GetClipboardText();
    if ptr.is_null() {
        return None;
    }
    let s = CStr::from_ptr(ptr).to_string_lossy().into_owned();
    SDL_free(ptr as *mut c_void);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// A cheap signature of the current clipboard, good enough to detect changes
/// without pulling every (possibly large) representation on every poll.
unsafe fn quick_signature() -> u64 {
    // Sort so apps re-offering the same content with a shuffled mime order
    // don't read as a "new" clipboard.
    let mut mimes = read_mimes();
    mimes.sort();
    let mut h = Fnv::new();
    for m in &mimes {
        h.write(m.as_bytes());
        h.write(b"\x1f");
    }
    // Fold in the text content (bounded) so edits with identical mime sets are
    // still noticed.
    if let Some(t) = read_text() {
        let bytes = t.as_bytes();
        h.write(&bytes[..bytes.len().min(4096)]);
        h.write_u64(bytes.len() as u64);
    }
    // If there's an image, its byte length disambiguates cheaply.
    if let Some(img_mime) = mimes.iter().find(|m| m.starts_with("image/")) {
        if let Ok(cm) = CString::new(img_mime.as_str()) {
            let mut size: usize = 0;
            // Peek only the size; SDL still allocates, so free immediately.
            let ptr = sysclip::SDL_GetClipboardData(cm.as_ptr(), &mut size);
            if !ptr.is_null() {
                SDL_free(ptr);
            }
            h.write_u64(size as u64);
        }
    }
    h.finish()
}

/// Pick the mime that best characterizes the item, and its coarse kind.
fn classify(mimes: &[String]) -> (String, MimeKind) {
    let has = |needle: &str| mimes.iter().any(|m| m == needle);
    let starts = |pfx: &str| mimes.iter().find(|m| m.starts_with(pfx)).cloned();

    if let Some(img) = mimes.iter().find(|m| *m == "image/png").cloned().or_else(|| starts("image/")) {
        return (img, MimeKind::Image);
    }
    if has("text/uri-list") {
        return ("text/uri-list".into(), MimeKind::Files);
    }
    if has("text/html") {
        return ("text/html".into(), MimeKind::Html);
    }
    if let Some(rtf) = starts("text/rtf").or_else(|| starts("application/rtf")).or_else(|| starts("public.rtf")) {
        return (rtf, MimeKind::Rtf);
    }
    if has("text/plain") {
        return ("text/plain".into(), MimeKind::Text);
    }
    if let Some(t) = starts("text/") {
        return (t, MimeKind::Text);
    }
    match mimes.first() {
        Some(m) => (m.clone(), MimeKind::Other),
        None => ("text/plain".into(), MimeKind::Text),
    }
}

/// Read the OS clipboard fully into a `ClipItem`.
unsafe fn read_current() -> Option<ClipItem> {
    let mut mimes = read_mimes();
    let text = read_text();

    // Ensure text-only clipboards still classify as text.
    if mimes.is_empty() {
        if text.is_some() {
            mimes.push("text/plain".into());
        } else {
            return None;
        }
    }

    let (primary_mime, mut kind) = classify(&mimes);
    // Plain text that is a single URL gets the link treatment.
    if kind == MimeKind::Text {
        if let Some(t) = &text {
            if looks_like_url(t) {
                kind = MimeKind::Link;
            }
        }
    }

    // Keep every representation for a full-fidelity restore, but under a
    // budget: one image is advertised under many types, and copying a
    // multi-megabyte screenshot once per type is waste.
    const REPR_BUDGET: usize = 24 * 1024 * 1024;
    let mut reprs = Vec::new();
    let mut primary_size = 0usize;
    let mut budget = REPR_BUDGET;

    if let Some(data) = read_data(&primary_mime) {
        primary_size = data.len();
        budget = budget.saturating_sub(data.len());
        reprs.push(Repr { mime: primary_mime.clone(), data });
    }
    for m in &mimes {
        if *m == primary_mime {
            continue;
        }
        if budget == 0 {
            break;
        }
        if let Some(data) = read_data(m) {
            if data.len() > budget {
                continue;
            }
            budget -= data.len();
            reprs.push(Repr { mime: m.clone(), data });
        }
    }
    // Fall back to text bytes if the primary had no readable data.
    if primary_size == 0 {
        if let Some(t) = &text {
            primary_size = t.len();
        }
    }

    // An empty item would sit at the top and paste nothing when selected.
    let has_content = primary_size > 0
        || text.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false)
        || reprs.iter().any(|r| !r.data.is_empty());
    if !has_content {
        return None;
    }

    let preview = build_preview(kind, text.as_deref(), &primary_mime, &reprs);
    // Built from data already in hand; never re-reads the clipboard.
    let signature = {
        let mut h = Fnv::new();
        h.write(primary_mime.as_bytes());
        h.write_u64(primary_size as u64);
        if let Some(t) = &text {
            let b = t.as_bytes();
            h.write(&b[..b.len().min(4096)]);
            h.write_u64(b.len() as u64);
        } else if let Some(r) = reprs.iter().find(|r| r.mime == primary_mime) {
            // Binary payloads: hash the head and tail, not the whole blob.
            let d = &r.data;
            h.write(&d[..d.len().min(4096)]);
            if d.len() > 4096 {
                h.write(&d[d.len() - 4096..]);
            }
        }
        h.finish()
    };

    Some(ClipItem {
        primary_mime,
        kind,
        reprs,
        text,
        preview,
        size: primary_size,
        signature,
    })
}

/// Build a compact, single-line preview for the UI.
fn build_preview(kind: MimeKind, text: Option<&str>, primary: &str, reprs: &[Repr]) -> String {
    match kind {
        MimeKind::Image => {
            // The icon already says "image".
            primary.rsplit('/').next().unwrap_or("image").to_uppercase()
        }
        MimeKind::Files => {
            let list = text.unwrap_or("");
            let names: Vec<&str> = list
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.rsplit('/').next().unwrap_or(l).trim())
                .collect();
            match names.as_slice() {
                [] => "Files".into(),
                [one] => decode_percent(one),
                many => format!("{} + {} more", decode_percent(many[0]), many.len() - 1),
            }
        }
        _ => {
            let mut raw = text
                .map(|t| t.to_owned())
                .or_else(|| reprs.iter().find(|r| r.mime == primary).map(|r| String::from_utf8_lossy(&r.data).into_owned()))
                .unwrap_or_default();
            // Prefer readable content over markup.
            if kind == MimeKind::Rtf && raw.starts_with("{\\rtf") {
                raw = strip_rtf(&raw);
            } else if kind == MimeKind::Html && text.is_none() {
                raw = strip_tags(&raw);
            }
            let flat = collapse_ws(&raw);
            truncate_chars(&flat, 120)
        }
    }
}

// ---------------------------------------------------------------------------
// Writing back to the OS clipboard (full fidelity via a data provider)
// ---------------------------------------------------------------------------

/// Owned payload handed to SDL; it serves the item's representations on demand
/// and is dropped by SDL's cleanup callback when the clipboard is replaced.
struct Payload {
    mimes: Vec<CString>,
    reprs: Vec<Repr>,
}

unsafe extern "C" fn provide(
    userdata: *mut c_void,
    mime_type: *const c_char,
    size: *mut usize,
) -> *const c_void {
    if userdata.is_null() || mime_type.is_null() {
        if !size.is_null() {
            *size = 0;
        }
        return std::ptr::null();
    }
    let payload = &*(userdata as *const Payload);
    let req = CStr::from_ptr(mime_type).to_string_lossy();
    if let Some(r) = payload.reprs.iter().find(|r| r.mime == req) {
        *size = r.data.len();
        r.data.as_ptr() as *const c_void
    } else {
        *size = 0;
        std::ptr::null()
    }
}

unsafe extern "C" fn cleanup(userdata: *mut c_void) {
    if !userdata.is_null() {
        drop(Box::from_raw(userdata as *mut Payload));
    }
}

/// Write an item back to the OS clipboard. Text-only items use the simple text
/// API; anything richer is served through a multi-mime data provider so every
/// original representation survives the round-trip.
unsafe fn set_os_clipboard(item: &ClipItem) -> bool {
    let only_text = item.reprs.len() <= 1
        && matches!(item.kind, MimeKind::Text | MimeKind::Link)
        && item.text.is_some();

    if only_text {
        if let Some(t) = &item.text {
            if let Ok(c) = CString::new(sanitize_nul(t)) {
                return sysclip::SDL_SetClipboardText(c.as_ptr());
            }
        }
    }

    if item.reprs.is_empty() {
        // Last resort: nothing structured to offer.
        if let Some(t) = &item.text {
            if let Ok(c) = CString::new(sanitize_nul(t)) {
                return sysclip::SDL_SetClipboardText(c.as_ptr());
            }
        }
        return false;
    }

    let mimes: Vec<CString> = item
        .reprs
        .iter()
        .filter_map(|r| CString::new(r.mime.clone()).ok())
        .collect();
    let payload = Box::new(Payload { mimes, reprs: item.reprs.clone() });
    let mime_ptrs: Vec<*const c_char> = payload.mimes.iter().map(|c| c.as_ptr()).collect();
    let n = mime_ptrs.len();
    let ud = Box::into_raw(payload) as *mut c_void;

    sysclip::SDL_SetClipboardData(Some(provide), Some(cleanup), ud, mime_ptrs.as_ptr(), n)
}

// ---------------------------------------------------------------------------
// Small helpers (kept local to avoid extra deps)
// ---------------------------------------------------------------------------

fn sanitize_nul(s: &str) -> String {
    s.replace('\0', "")
}

/// Crude RTF → text for previews. Never alters the stored bytes.
fn strip_rtf(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    let mut depth = 0i32;
    // Header groups like {\fonttbl ...} carry no user text.
    let mut skip_group_until: Option<i32> = None;
    while let Some(c) = chars.next() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if let Some(d) = skip_group_until {
                    if depth < d {
                        skip_group_until = None;
                    }
                }
            }
            '\\' => {
                let mut word = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphabetic() {
                        word.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                // Numeric parameter, then an optional single trailing space.
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_digit() || n == '-' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                if matches!(word.as_str(), "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "generator") {
                    skip_group_until = Some(depth);
                }
                if word == "par" || word == "line" {
                    out.push(' ');
                }
            }
            _ => {
                if skip_group_until.is_none() {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// Strip HTML tags for a text preview.
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn decode_percent(s: &str) -> String {
    // Minimal percent-decoding for file:// uri-list entries.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn pretty_bytes(n: usize) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    // Decimals only below 10, to keep chips short.
    if v >= 10.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// FNV-1a, for content signatures.
struct Fnv(u64);
impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf29ce484222325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.write(&v.to_le_bytes());
    }
    fn finish(&self) -> u64 {
        self.0
    }
}
