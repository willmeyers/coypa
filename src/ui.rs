//! Material 3 style chips: solid white pills with a hairline outline and dark
//! Inter text, sized to their content; the selected one fills lavender with a
//! leading checkmark. Drawn with SDL3 primitives. Geometry is in points, `s`
//! converts to device pixels.

use sdl3::pixels::Color;
use sdl3::render::{FRect, WindowCanvas};

use crate::clipboard::{ClipItem, MimeKind};
use crate::text::TextRenderer;

// --- Geometry (points) -------------------------------------------------------

pub const CHIP_H: f32 = 21.0;
/// Content-sized width limits.
const CHIP_MIN_W: f32 = 34.0;
const CHIP_MAX_W: f32 = 88.0;
/// Horizontal text padding inside the pill.
const PAD_X: f32 = 8.0;
/// Window content margin (wheel).
const PAD: f32 = 5.0;
/// Radius of the fly-out wheel.
pub const WHEEL_RADIUS: f32 = 74.0;
/// Most chips shown in the wheel.
pub const VISIBLE: usize = 8;
/// Most chips shown in the idle list.
pub const IDLE_MAX: usize = 4;

/// Preview font size in points.
const FONT_PT: f32 = 9.0;
/// Icon slot width (icon + gap to text).
const ICON_SLOT: f32 = 15.0;
/// Checkmark slot width on the selected chip.
const CHECK_SLOT: f32 = 13.0;

// --- Palette (Material 3) ------------------------------------------------------

const CHIP_BG: Color = Color { r: 255, g: 255, b: 255, a: 255 };
const OUTLINE: Color = Color { r: 199, g: 202, b: 201, a: 255 }; // M3 outline-variant
const TEXT: Color = Color { r: 32, g: 33, b: 36, a: 255 };
const SELECTED_BG: Color = Color { r: 232, g: 222, b: 248, a: 255 }; // secondary container
const SELECTED_TEXT: Color = Color { r: 29, g: 25, b: 43, a: 255 };

fn kind_accent(kind: MimeKind) -> Color {
    match kind {
        // M3 primary purple for links, per the reference.
        MimeKind::Link => Color { r: 103, g: 80, b: 164, a: 255 },
        MimeKind::Image => Color { r: 52, g: 168, b: 83, a: 255 },
        MimeKind::Files => Color { r: 232, g: 148, b: 10, a: 255 },
        _ => TEXT,
    }
}

fn scale_alpha(c: Color, f: f32) -> Color {
    Color { a: (c.a as f32 * f.clamp(0.0, 1.0)) as u8, ..c }
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    let x = t - 1.0;
    1.0 + c3 * x * x * x + c1 * x * x
}
fn ease_out_cubic(t: f32) -> f32 {
    let x = 1.0 - t.clamp(0.0, 1.0);
    1.0 - x * x * x
}

// --- Pixel-space primitives ----------------------------------------------------

fn round_inset(dy: f32, h: f32, r: f32) -> f32 {
    if dy < r {
        r - (r * r - (r - dy) * (r - dy)).max(0.0).sqrt()
    } else if dy > h - r {
        let d = dy - (h - r);
        r - (r * r - d * d).max(0.0).sqrt()
    } else {
        0.0
    }
}

fn fill_rounded(canvas: &mut WindowCanvas, rect: FRect, radius: f32, color: Color) {
    canvas.set_draw_color(color);
    let r = radius.min(rect.w / 2.0).min(rect.h / 2.0).max(0.0);
    let rows = rect.h.ceil() as i32;
    for row in 0..rows {
        let inset = round_inset(row as f32 + 0.5, rect.h, r);
        let w = rect.w - inset * 2.0;
        if w > 0.0 {
            let _ = canvas.fill_rect(FRect::new(rect.x + inset, rect.y + row as f32, w, 1.0));
        }
    }
}

fn fill_circle(canvas: &mut WindowCanvas, cx: f32, cy: f32, r: f32, color: Color) {
    fill_rounded(canvas, FRect::new(cx - r, cy - r, r * 2.0, r * 2.0), r, color);
}

/// Ring (circle outline) via per-scanline outer/inner spans.
fn fill_ring(canvas: &mut WindowCanvas, cx: f32, cy: f32, r: f32, thick: f32, color: Color) {
    canvas.set_draw_color(color);
    let ri = (r - thick).max(0.0);
    let steps = (r * 2.0).ceil() as i32;
    for i in 0..steps {
        let dy = i as f32 + 0.5 - r;
        let wo = (r * r - dy * dy).max(0.0).sqrt();
        let wi = if dy.abs() < ri { (ri * ri - dy * dy).max(0.0).sqrt() } else { 0.0 };
        let y = cy - r + i as f32;
        if wi > 0.0 {
            let _ = canvas.fill_rect(FRect::new(cx - wo, y, wo - wi, 1.0));
            let _ = canvas.fill_rect(FRect::new(cx + wi, y, wo - wi, 1.0));
        } else {
            let _ = canvas.fill_rect(FRect::new(cx - wo, y, wo * 2.0, 1.0));
        }
    }
}

fn line(canvas: &mut WindowCanvas, x0: f32, y0: f32, x1: f32, y1: f32, thick: f32, color: Color) {
    canvas.set_draw_color(color);
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let steps = (len * 1.4).ceil() as i32;
    let h = thick / 2.0;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let _ = canvas.fill_rect(FRect::new(x0 + dx * t - h, y0 + dy * t - h, thick, thick));
    }
}

// --- Icons (only links, images, and files get one) ------------------------------

fn has_icon(kind: MimeKind) -> bool {
    matches!(kind, MimeKind::Link | MimeKind::Image | MimeKind::Files)
}

fn draw_icon(canvas: &mut WindowCanvas, kind: MimeKind, cx: f32, cy: f32, sz: f32, color: Color) {
    let th = (sz * 0.17).max(1.2);
    match kind {
        MimeKind::Link => {
            let o = sz * 0.26;
            fill_ring(canvas, cx - o, cy + o, sz * 0.3, th, color);
            fill_ring(canvas, cx + o, cy - o, sz * 0.3, th, color);
            line(canvas, cx - o * 0.7, cy + o * 0.7, cx + o * 0.7, cy - o * 0.7, th, color);
        }
        MimeKind::Image => {
            let f = FRect::new(cx - sz * 0.55, cy - sz * 0.42, sz * 1.1, sz * 0.84);
            fill_rounded(canvas, FRect::new(f.x, f.y, f.w, th), th / 2.0, color);
            fill_rounded(canvas, FRect::new(f.x, f.y + f.h - th, f.w, th), th / 2.0, color);
            fill_rounded(canvas, FRect::new(f.x, f.y, th, f.h), th / 2.0, color);
            fill_rounded(canvas, FRect::new(f.x + f.w - th, f.y, th, f.h), th / 2.0, color);
            fill_circle(canvas, f.x + f.w * 0.33, f.y + f.h * 0.38, sz * 0.09, color);
        }
        MimeKind::Files => {
            let bx = cx - sz * 0.55;
            let by = cy - sz * 0.3;
            fill_rounded(canvas, FRect::new(bx, by - sz * 0.14, sz * 0.45, sz * 0.24), th, color);
            fill_rounded(canvas, FRect::new(bx, by, sz * 1.1, sz * 0.66), th, color);
        }
        _ => {}
    }
}

/// M3 checkmark for the selected chip.
fn draw_check(canvas: &mut WindowCanvas, cx: f32, cy: f32, sz: f32, color: Color) {
    let th = (sz * 0.18).max(1.4);
    line(canvas, cx - sz * 0.42, cy + sz * 0.02, cx - sz * 0.12, cy + sz * 0.32, th, color);
    line(canvas, cx - sz * 0.12, cy + sz * 0.32, cx + sz * 0.46, cy - sz * 0.28, th, color);
}

// --- Chip ---------------------------------------------------------------------

/// Content-sized width (points) of a chip.
pub fn chip_width(tr: &mut TextRenderer, s: f32, item: &ClipItem, selected: bool) -> f32 {
    let mut fixed = PAD_X * 2.0;
    if selected {
        fixed += CHECK_SLOT;
    }
    if has_icon(item.kind) {
        fixed += ICON_SLOT;
    }
    let text_w = tr.measure(&item.preview, FONT_PT * s) / s;
    let text_max = CHIP_MAX_W - fixed;
    (fixed + text_w.min(text_max)).max(CHIP_MIN_W)
}

/// One Material chip. `x` is the left edge, `cy` the vertical center, both in
/// points; `w` the content-sized width from `chip_width`.
fn draw_chip(
    canvas: &mut WindowCanvas,
    tr: &mut TextRenderer,
    s: f32,
    x: f32,
    cy: f32,
    w: f32,
    item: &ClipItem,
    selected: bool,
    alpha: f32,
) {
    let a = alpha.clamp(0.0, 1.0);
    let wpx = w * s;
    let h = CHIP_H * s;
    let xpx = x * s;
    let ypx = cy * s - h / 2.0;
    let r = h / 2.0;

    if selected {
        // Selected: solid fill, no outline.
        fill_rounded(canvas, FRect::new(xpx, ypx, wpx, h), r, scale_alpha(SELECTED_BG, a));
    } else {
        // Hairline outline, then the body inset by 1pt.
        fill_rounded(canvas, FRect::new(xpx, ypx, wpx, h), r, scale_alpha(OUTLINE, a));
        let b = 1.0 * s;
        fill_rounded(
            canvas,
            FRect::new(xpx + b, ypx + b, wpx - b * 2.0, h - b * 2.0),
            r - b,
            scale_alpha(CHIP_BG, a),
        );
    }

    let fg = if selected { SELECTED_TEXT } else { TEXT };
    let mut cursor = xpx + PAD_X * s;

    if selected {
        draw_check(canvas, cursor + 4.0 * s, cy * s, 9.0 * s, scale_alpha(SELECTED_TEXT, a));
        cursor += CHECK_SLOT * s;
    }
    if has_icon(item.kind) {
        let icon_c = if selected { SELECTED_TEXT } else { kind_accent(item.kind) };
        draw_icon(canvas, item.kind, cursor + 5.0 * s, cy * s, 10.0 * s, scale_alpha(icon_c, a));
        cursor += ICON_SLOT * s;
    }

    let avail = (xpx + wpx - PAD_X * s) - cursor;
    let fitted = tr.fit(&item.preview, FONT_PT * s, avail);
    tr.draw(canvas, &fitted, cursor, cy * s, FONT_PT * s, scale_alpha(fg, a));
}

// --- Layout -------------------------------------------------------------------

/// Per-slot jitter so the pile reads as a clump, not a list. Chips overlap,
/// each peeking out from behind the one in front.
const JITTER_X: [f32; IDLE_MAX] = [0.0, 5.0, -3.0, 7.0];
const JITTER_Y: [f32; IDLE_MAX] = [0.0, 5.5, 11.0, 15.5];

/// Resting offset (points) of chip `i` from the clump base.
pub fn idle_slot(i: usize) -> (f32, f32) {
    let i = i.min(IDLE_MAX - 1);
    (JITTER_X[i], JITTER_Y[i])
}

/// Half-extent of the wheel, to keep its anchor clear of display edges.
pub fn wheel_extent() -> f32 {
    WHEEL_RADIUS + CHIP_MAX_W / 2.0 + PAD
}

// --- Wheel geometry -----------------------------------------------------------

fn wheel_angle(i: usize, n: usize) -> f32 {
    let start = -std::f32::consts::FRAC_PI_2;
    start + std::f32::consts::TAU * (i as f32 / n.max(1) as f32)
}

/// Pick the chip whose direction from the wheel center best matches (dx, dy);
/// hold `current` inside the dead-zone.
pub fn wheel_select(dx: f32, dy: f32, n: usize, current: usize) -> usize {
    if n == 0 {
        return 0;
    }
    if (dx * dx + dy * dy).sqrt() < 22.0 {
        return current.min(n - 1);
    }
    let ma = dy.atan2(dx);
    let mut best = 0;
    let mut best_d = f32::MAX;
    for i in 0..n {
        let mut d = (wheel_angle(i, n) - ma).abs() % std::f32::consts::TAU;
        if d > std::f32::consts::PI {
            d = std::f32::consts::TAU - d;
        }
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

// --- Top-level renders ----------------------------------------------------------

fn clear(canvas: &mut WindowCanvas) {
    canvas.set_draw_color(Color { r: 0, g: 0, b: 0, a: 0 });
    canvas.clear();
}

/// The clump. `locals[i]` is chip i's (left, center-y), driven by springs.
pub fn render_idle(
    canvas: &mut WindowCanvas,
    tr: &mut TextRenderer,
    s: f32,
    items: &[ClipItem],
    locals: &[(f32, f32)],
) {
    clear(canvas);
    let n = items.len().min(IDLE_MAX).min(locals.len());
    for i in (0..n).rev() {
        let (x, cy) = locals[i];
        let w = chip_width(tr, s, &items[i], false);
        draw_chip(canvas, tr, s, x, cy, w, &items[i], false, 1.0);
    }
    canvas.present();
}

/// Wheel: chips fly from `anchor` (overlay-local points) out into a circle
/// around it.
pub fn render_wheel(
    canvas: &mut WindowCanvas,
    tr: &mut TextRenderer,
    s: f32,
    items: &[ClipItem],
    selected: usize,
    anim: f32,
    anchor: (f32, f32),
) {
    clear(canvas);
    let n = items.len().min(VISIBLE);
    if n == 0 {
        canvas.present();
        return;
    }

    let sel = selected.min(n - 1);
    let order = (0..n).filter(|&i| i != sel).chain(std::iter::once(sel));
    for i in order {
        let stagger = 0.05 * i as f32;
        let t = ((anim - stagger) / (1.0 - stagger).max(0.2)).clamp(0.0, 1.0);
        let et = ease_out_back(t);
        let is_sel = i == sel;
        let w = chip_width(tr, s, &items[i], is_sel);
        let ang = wheel_angle(i, n);
        let to = (anchor.0 + ang.cos() * WHEEL_RADIUS, anchor.1 + ang.sin() * WHEEL_RADIUS);
        let cx = lerp(anchor.0, to.0, et);
        let cy = lerp(anchor.1, to.1, et);
        draw_chip(canvas, tr, s, cx - w / 2.0, cy, w, &items[i], is_sel, ease_out_cubic(t));
    }

    canvas.present();
}
