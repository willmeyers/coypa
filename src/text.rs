//! Text rendering: Inter (packaged, SIL OFL) rasterised by `fontdue` and
//! blitted with SDL fill-rects. Glyphs are cached per (char, px), and runs of
//! equal alpha are merged so a preview costs hundreds of rects, not thousands.

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};
use sdl3::pixels::Color;
use sdl3::render::{FRect, WindowCanvas};

const FONT_BYTES: &[u8] = include_bytes!("../assets/Inter.ttf");

/// Quantization step for alpha-run merging (16 levels).
const ALPHA_STEP: u16 = 16;

struct Glyph {
    metrics: Metrics,
    coverage: Vec<u8>,
}

pub struct TextRenderer {
    font: Font,
    cache: HashMap<(char, u32), Glyph>,
}

impl TextRenderer {
    pub fn new() -> Self {
        let font = Font::from_bytes(FONT_BYTES, FontSettings::default())
            .expect("embedded Inter.ttf should parse");
        TextRenderer { font, cache: HashMap::new() }
    }

    fn glyph(&mut self, ch: char, px: f32) -> &Glyph {
        let key = (ch, (px * 4.0).round() as u32);
        self.cache.entry(key).or_insert_with(|| {
            let (metrics, coverage) = self.font.rasterize(ch, px);
            Glyph { metrics, coverage }
        })
    }

    /// Advance width of `text` at `px`, including kerning.
    pub fn measure(&mut self, text: &str, px: f32) -> f32 {
        let mut w = 0.0;
        let mut prev: Option<char> = None;
        for ch in text.chars() {
            if let Some(p) = prev {
                w += self.font.horizontal_kern(p, ch, px).unwrap_or(0.0);
            }
            w += self.glyph(ch, px).metrics.advance_width;
            prev = Some(ch);
        }
        w
    }

    /// Ellipsize `text` so it fits in `max_w` device pixels at `px`.
    pub fn fit(&mut self, text: &str, px: f32, max_w: f32) -> String {
        if self.measure(text, px) <= max_w {
            return text.to_string();
        }
        let ell = '…';
        let ell_w = self.glyph(ell, px).metrics.advance_width;
        let mut out = String::new();
        let mut w = 0.0;
        for ch in text.chars() {
            let adv = self.glyph(ch, px).metrics.advance_width;
            if w + adv + ell_w > max_w {
                break;
            }
            out.push(ch);
            w += adv;
        }
        out.push(ell);
        out
    }

    /// Draw `text` with its left edge at `x`, vertically centered on
    /// `y_center`, all in device pixels.
    pub fn draw(
        &mut self,
        canvas: &mut WindowCanvas,
        text: &str,
        x: f32,
        y_center: f32,
        px: f32,
        color: Color,
    ) {
        let lm = self.font.horizontal_line_metrics(px);
        let baseline = match lm {
            Some(m) => y_center + (m.ascent + m.descent) / 2.0,
            None => y_center + px * 0.35,
        };

        let mut pen = x;
        let mut prev: Option<char> = None;
        for ch in text.chars() {
            if let Some(p) = prev {
                pen += self.font.horizontal_kern(p, ch, px).unwrap_or(0.0);
            }
            // Copy out what we need so the borrow on the cache ends here.
            let (metrics, coverage) = {
                let g = self.glyph(ch, px);
                (g.metrics, g.coverage.clone())
            };
            blit(canvas, &metrics, &coverage, pen, baseline, color);
            pen += metrics.advance_width;
            prev = Some(ch);
        }
    }
}

/// Blit a glyph's coverage bitmap, merging runs of equal alpha.
fn blit(
    canvas: &mut WindowCanvas,
    m: &Metrics,
    cov: &[u8],
    pen_x: f32,
    baseline: f32,
    color: Color,
) {
    if m.width == 0 || m.height == 0 {
        return;
    }
    let gx = pen_x + m.xmin as f32;
    let gy = baseline - m.height as f32 - m.ymin as f32;

    for row in 0..m.height {
        let mut col = 0;
        while col < m.width {
            let a = quant(cov[row * m.width + col]);
            if a == 0 {
                col += 1;
                continue;
            }
            let start = col;
            while col < m.width && quant(cov[row * m.width + col]) == a {
                col += 1;
            }
            let alpha = (a as u32 * color.a as u32 / 255) as u8;
            canvas.set_draw_color(Color { a: alpha, ..color });
            let _ = canvas.fill_rect(FRect::new(
                gx + start as f32,
                gy + row as f32,
                (col - start) as f32,
                1.0,
            ));
        }
    }
}

fn quant(a: u8) -> u16 {
    (((a as u16 + ALPHA_STEP / 2) / ALPHA_STEP) * ALPHA_STEP).min(255)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_parses_and_measures() {
        let mut tr = TextRenderer::new();
        let w = tr.measure("Hello, coypa", 18.0);
        assert!(w > 40.0, "text should have real width, got {w}");
        // Ellipsizing to a tight width shortens and appends …
        let fitted = tr.fit("a very long preview string that cannot fit", 18.0, 80.0);
        assert!(fitted.ends_with('…'));
        assert!(tr.measure(&fitted, 18.0) <= 80.0 + 12.0);
        // Odd input shouldn't panic (unknown glyphs fall back to notdef).
        let _ = tr.fit("emoji 🙂 and\ttabs", 18.0, 200.0);
        let _ = tr.measure("", 18.0);
    }
}
