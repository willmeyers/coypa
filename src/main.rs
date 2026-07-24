//! coypa — a clipboard stack.
//!
//! Copies become chips that clump behind the cursor, each trailing on its own
//! spring. Holding the paste key flies them out into a wheel; releasing pastes
//! the selected chip and pops it off the stack.
//!
//! The overlay is one static fullscreen transparent window that never moves —
//! chips animate in rendering only, so motion stays smooth. Clicks pass through
//! via `ignoresMouseEvents`, re-asserted each frame.

mod clipboard;
mod platform;
mod settings;
mod state;
mod text;
mod ui;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sdl3::event::Event;
use sdl3::render::BlendMode;
use sdl3::video::WindowFlags;

use clipboard::ClipboardManager;
use settings::Settings;
use state::Shared;

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const WHEEL_ANIM_MS: f32 = 240.0;
/// Clump offset from the cursor hotspot. The y-offset clears the arrow cursor
/// so chips sit below what you're pointing at.
const FOLLOW_OFF: (f32, f32) = (10.0, 38.0);
/// Per-chip spring stiffness (1/s), newest first: the top chip tracks tightly,
/// older ones drag, so a fast move stretches the clump then it regathers.
const CHIP_K: [f32; 4] = [16.0, 10.5, 7.5, 5.5];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    Wheel,
}

fn main() {
    // `COYPA_DEBUG=1` logs each capture (kind, mime, size, read time).
    let debug = std::env::var_os("COYPA_DEBUG").is_some();
    let mut prefs = Settings::load();
    let start = Instant::now();
    let shared = Arc::new(Shared::new(prefs.trigger, prefs.hold_ms, start));

    platform::spawn_trigger(shared.clone());

    let sdl = sdl3::init().expect("SDL_Init");
    let video = sdl.video().expect("SDL video subsystem");
    let mut event_pump = sdl.event_pump().expect("event pump");
    // No Dock icon, no ⌘-Tab entry.
    platform::become_accessory_app();

    let bounds = video
        .get_primary_display()
        .and_then(|d| d.get_bounds())
        .expect("primary display bounds");
    let origin = (bounds.x() as f32, bounds.y() as f32);
    let (disp_w, disp_h) = (bounds.width() as f32, bounds.height() as f32);

    let flags = WindowFlags::BORDERLESS
        | WindowFlags::ALWAYS_ON_TOP
        | WindowFlags::TRANSPARENT
        | WindowFlags::NOT_FOCUSABLE
        | WindowFlags::UTILITY;

    let window = video
        .window("coypa", bounds.width(), bounds.height())
        .set_flags(flags)
        .high_pixel_density()
        .hidden()
        .position(bounds.x(), bounds.y())
        .build()
        .expect("create overlay window");

    let mut canvas = window.into_canvas();
    canvas.set_blend_mode(BlendMode::Blend);
    let win_raw = canvas.window().raw();
    platform::apply_click_through(win_raw);

    // Pixels-per-point (2.0 on Retina); constant for the display.
    let s = canvas
        .output_size()
        .map(|(pw, _)| pw as f32 / disp_w)
        .unwrap_or(1.0);

    let mut stack = ClipboardManager::new(prefs.max_history);

    // Headless check of the selection→paste path.
    if std::env::var_os("COYPA_SELFTEST").is_some() {
        selftest(&mut stack, &mut event_pump);
        return;
    }

    let t0 = Instant::now();
    let got = stack.poll_os();
    if debug {
        eprintln!(
            "coypa: startup poll captured={} in {:?}; change_count={:?}; stack={}",
            got,
            t0.elapsed(),
            platform::clipboard_change_count(),
            stack.len()
        );
        if let Some(it) = stack.items().front() {
            eprintln!(
                "coypa:   top = {:?} {} ({}) \"{}\"",
                it.kind, it.primary_mime, it.pretty_size(), it.preview
            );
        }
    }

    let mut tr = text::TextRenderer::new();
    let mut mode = Mode::Idle;
    let mut anim_start = start;
    let mut last_poll = Instant::now();
    let mut shown = false;
    // Wheel center in overlay-local points.
    let mut anchor = (0.0f32, 0.0f32);
    // Per-chip spring state in overlay-local points, newest first.
    let mut chip_pos: Vec<(f32, f32)> = Vec::new();
    let mut prev_top_sig = 0u64;
    let mut idle_dirty = true;
    let mut last_frame = Instant::now();
    let mut last_ct_check = Instant::now();
    let mut warned_ct = false;

    'run: loop {
        for event in event_pump.poll_iter() {
            if let Event::Quit { .. } = event {
                break 'run;
            }
        }
        if shared.quit.load(Ordering::SeqCst) {
            break 'run;
        }

        if last_poll.elapsed() >= POLL_INTERVAL {
            let t0 = Instant::now();
            if stack.poll_os() {
                idle_dirty = true;
                if debug {
                    if let Some(it) = stack.items().front() {
                        eprintln!(
                            "coypa: captured {:?} {} ({}) in {:?} — \"{}\"",
                            it.kind,
                            it.primary_mime,
                            it.pretty_size(),
                            t0.elapsed(),
                            it.preview
                        );
                    }
                }
            }
            last_poll = Instant::now();
        }
        let n = stack.len().min(ui::VISIBLE);
        shared.item_count.store(n, Ordering::SeqCst);

        if shared.settings_dirty.swap(false, Ordering::SeqCst) {
            prefs.trigger = shared.trigger();
            prefs.save();
        }
        if shared.open_settings.swap(false, Ordering::SeqCst) {
            shared.capturing.store(true, Ordering::SeqCst);
        }

        let (gx, gy) = global_mouse();
        let (mx, my) = (gx - origin.0, gy - origin.1); // overlay-local points
        let now = Instant::now();
        let dt = (now - last_frame).as_secs_f32().min(0.1);
        last_frame = now;

        // Hold detection → fly the clump out into the wheel.
        if mode == Mode::Idle
            && shared.trigger_down.load(Ordering::SeqCst)
            && !stack.is_empty()
            && !shared.capturing.load(Ordering::SeqCst)
        {
            let held = shared
                .now_ms()
                .saturating_sub(shared.trigger_down_at.load(Ordering::SeqCst));
            if held >= shared.hold_ms.load(Ordering::SeqCst) {
                mode = Mode::Wheel;
                anim_start = Instant::now();
                shared.selected.store(0, Ordering::SeqCst);
                shared.ring_visible.store(true, Ordering::SeqCst);
                let ext = ui::wheel_extent();
                anchor = (mx.clamp(ext, disp_w - ext), my.clamp(ext, disp_h - ext));
            }
        }

        // Cancel (Esc) → chips burst back out of the wheel center.
        if shared.cancel.swap(false, Ordering::SeqCst) && mode == Mode::Wheel {
            mode = Mode::Idle;
            shared.ring_visible.store(false, Ordering::SeqCst);
            idle_dirty = true;
            for p in chip_pos.iter_mut() {
                *p = anchor;
            }
        }

        // Paste (trigger released) → pop the chosen item, burst back, synth ⌘V.
        if shared.request_paste.swap(false, Ordering::SeqCst) {
            let was_wheel = mode == Mode::Wheel;
            let idx = if was_wheel { shared.selected.load(Ordering::SeqCst) } else { 0 };
            mode = Mode::Idle;
            shared.ring_visible.store(false, Ordering::SeqCst);
            idle_dirty = true;
            if was_wheel {
                for p in chip_pos.iter_mut() {
                    *p = anchor;
                }
            }

            // Pasting consumes the item.
            if idx < stack.len() {
                stack.pop_for_paste(idx);
                if idx < chip_pos.len() {
                    chip_pos.remove(idx);
                }
                // The new top isn't a fresh copy; don't replay the spawn.
                prev_top_sig = stack.items().front().map(|it| it.signature).unwrap_or(0);
            }
            platform::synthesize_paste(&shared);
        }

        // Visibility: hide entirely when there's nothing to show.
        let want_shown = n > 0;
        if want_shown != shown {
            if want_shown {
                canvas.window_mut().show();
                idle_dirty = true;
            } else {
                canvas.window_mut().hide();
            }
            shown = want_shown;
        }

        // SDL resets `ignoresMouseEvents` on mouse events; re-assert it.
        if shown {
            platform::apply_click_through(win_raw);
            // A startup check alone can't prove the flag holds.
            if !warned_ct && last_ct_check.elapsed() >= Duration::from_millis(500) {
                last_ct_check = Instant::now();
                match platform::is_click_through(win_raw) {
                    Some(true) => {}
                    Some(false) => {
                        warned_ct = true;
                        eprintln!(
                            "coypa: warning — click-through was reset; clicks over chips may be captured"
                        );
                    }
                    None => {
                        warned_ct = true;
                        eprintln!("coypa: warning — no NSWindow; click-through unavailable");
                    }
                }
            }
        }

        let mut settled = false;
        if shown {
            match mode {
                Mode::Wheel => {
                    let anim = (anim_start.elapsed().as_millis() as f32 / WHEEL_ANIM_MS).min(1.0);
                    let cur = shared.selected.load(Ordering::SeqCst);
                    let sel = ui::wheel_select(mx - anchor.0, my - anchor.1, n, cur);
                    shared.selected.store(sel, Ordering::SeqCst);

                    let items: Vec<_> = stack.items().iter().take(ui::VISIBLE).cloned().collect();
                    ui::render_wheel(&mut canvas, &mut tr, s, &items, sel, anim, anchor);
                }
                Mode::Idle => {
                    let items: Vec<_> = stack.items().iter().take(ui::IDLE_MAX).cloned().collect();
                    let m = items.len();

                    // A new copy spawns at the cursor and drops into the clump.
                    let top_sig = items.first().map(|it| it.signature).unwrap_or(0);
                    if top_sig != prev_top_sig {
                        prev_top_sig = top_sig;
                        chip_pos.insert(0, (mx, my));
                        chip_pos.truncate(ui::IDLE_MAX);
                        idle_dirty = true;
                    }
                    while chip_pos.len() < m {
                        chip_pos.push((mx + FOLLOW_OFF.0, my + FOLLOW_OFF.1));
                    }
                    chip_pos.truncate(m);

                    // Clump base trails the cursor, clamped on screen.
                    let max_w = items
                        .iter()
                        .fold(0.0f32, |a, it| a.max(ui::chip_width(&mut tr, s, it, false)));
                    let clump_h = ui::idle_slot(m.saturating_sub(1)).1 + ui::CHIP_H;
                    let bx = (mx + FOLLOW_OFF.0).clamp(6.0, (disp_w - max_w - 18.0).max(6.0));
                    let by = (my + FOLLOW_OFF.1).clamp(6.0, (disp_h - clump_h - 6.0).max(6.0));

                    settled = true;
                    for (i, p) in chip_pos.iter_mut().enumerate() {
                        let slot = ui::idle_slot(i);
                        let (tx, ty) = (bx + slot.0, by + slot.1);
                        let k = CHIP_K[i.min(CHIP_K.len() - 1)];
                        let f = 1.0 - (-dt * k).exp();
                        p.0 += (tx - p.0) * f;
                        p.1 += (ty - p.1) * f;
                        if (tx - p.0).abs() > 0.3 || (ty - p.1).abs() > 0.3 {
                            settled = false;
                        }
                    }

                    if !settled || idle_dirty {
                        idle_dirty = false;
                        let locals: Vec<(f32, f32)> = chip_pos
                            .iter()
                            .map(|p| (p.0, p.1 + ui::CHIP_H / 2.0))
                            .collect();
                        ui::render_idle(&mut canvas, &mut tr, s, &items, &locals);
                    }
                }
            }
        }

        let sleep_ms = match mode {
            Mode::Wheel => 8,
            Mode::Idle if !settled => 8,
            Mode::Idle => 14,
        };
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }

    prefs.save();
}

/// Build a stack, pop a non-top item, then read the clipboard back from
/// another process and report whether it matches.
fn selftest(stack: &mut ClipboardManager, event_pump: &mut sdl3::EventPump) {
    use std::process::{Command, Stdio};

    fn set_clipboard(text: &str) {
        use std::io::Write;
        let mut c = Command::new("pbcopy").stdin(Stdio::piped()).spawn().expect("pbcopy");
        c.stdin.as_mut().unwrap().write_all(text.as_bytes()).unwrap();
        c.wait().unwrap();
    }

    let (a, b) = ("coypa-selftest-AAA", "coypa-selftest-BBB");
    set_clipboard(a);
    std::thread::sleep(Duration::from_millis(250));
    stack.poll_os();
    set_clipboard(b);
    std::thread::sleep(Duration::from_millis(250));
    stack.poll_os();

    println!("stack after two copies: {}", stack.len());
    for (i, it) in stack.items().iter().enumerate() {
        println!("  [{i}] {:?} {} \"{}\"", it.kind, it.primary_mime, it.preview);
    }
    if stack.len() < 2 {
        println!("FAIL: expected 2 items");
        return;
    }

    // This is what releasing on a non-top wheel selection does.
    let before = stack.len();
    let ok = stack.pop_for_paste(1);
    println!("pop_for_paste(1) -> {ok}; stack {before} -> {}", stack.len());
    if stack.len() != before - 1 {
        println!("FAIL: pasted item was not popped from the stack");
    }
    if stack.items().iter().any(|it| it.preview == a) {
        println!("FAIL: popped item is still on the stack");
    }

    // Pump events meanwhile so lazy provider callbacks can be serviced.
    let mut child = Command::new("pbpaste").stdout(Stdio::piped()).spawn().expect("pbpaste");
    let deadline = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < deadline {
        for _ in event_pump.poll_iter() {}
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.wait_with_output().expect("pbpaste output");
    let got = String::from_utf8_lossy(&out.stdout).to_string();
    println!("clipboard now: {got:?}");
    println!(
        "{}",
        if got == a { "PASS: eager (plain-text) promote delivers the chosen item" } else { "FAIL: clipboard does not hold the promoted item" }
    );

    // Phase 2: the lazy provider path. Anything that isn't a single
    // plain-text representation is served through SDL_SetClipboardData's
    // on-demand callback — the path most real copies take.
    println!("\n--- phase 2: lazy provider (RTF) ---");
    let rtf = r"{\rtf1\ansi\deff0 {\fonttbl{\f0 Helvetica;}}\f0\fs28 coypa lazy path}";
    {
        use std::io::Write;
        let mut c = Command::new("pbcopy")
            .arg("-Prefer").arg("rtf")
            .stdin(Stdio::piped()).spawn().expect("pbcopy rtf");
        c.stdin.as_mut().unwrap().write_all(rtf.as_bytes()).unwrap();
        c.wait().unwrap();
    }
    std::thread::sleep(Duration::from_millis(250));
    stack.poll_os();
    set_clipboard("coypa-selftest-CCC");
    std::thread::sleep(Duration::from_millis(250));
    stack.poll_os();

    println!("stack in phase 2: {}", stack.len());
    for (i, it) in stack.items().iter().enumerate() {
        println!("  [{i}] {:?} {} \"{}\"", it.kind, it.primary_mime,
                 it.preview.chars().take(40).collect::<String>());
    }
    let rtf_idx = stack.items().iter().position(|it| it.primary_mime.contains("rtf"));
    match rtf_idx {
        None => println!("FAIL: RTF item never captured"),
        Some(i) => {
            println!("popping RTF at index {i}");
            let before = stack.len();
            let ok = stack.pop_for_paste(i);
            println!("pop_for_paste({i}) -> {ok}; stack {before} -> {}", stack.len());

            let mut child = Command::new("pbpaste")
                .arg("-Prefer").arg("rtf")
                .stdout(Stdio::piped()).spawn().expect("pbpaste rtf");
            let deadline = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < deadline {
                for _ in event_pump.poll_iter() {}
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let out = child.wait_with_output().expect("pbpaste rtf output");
            let got = String::from_utf8_lossy(&out.stdout).to_string();
            println!("clipboard now ({} bytes): {:?}", got.len(), got.chars().take(90).collect::<String>());
            println!(
                "{}",
                if got.contains("coypa lazy path") {
                    "PASS: lazy provider delivers the chosen item"
                } else {
                    "FAIL: lazy provider returned nothing — wheel-paste of rich/image items is broken"
                }
            );
        }
    }
}

/// Global mouse position in desktop coordinates.
fn global_mouse() -> (f32, f32) {
    let mut x = 0.0f32;
    let mut y = 0.0f32;
    unsafe {
        sdl3::sys::mouse::SDL_GetGlobalMouseState(&mut x, &mut y);
    }
    (x, y)
}
