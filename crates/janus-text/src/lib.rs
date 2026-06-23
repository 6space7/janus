//! Real text rendering: shape a run and composite its glyphs onto a pixmap.
//!
//! Per the from-scratch boundary, the *primitives floor* of text — font
//! discovery, complex-script shaping, and glyph rasterization — is reused via
//! `cosmic-text` (which bundles `fontdb` + `rustybuzz` + `swash`), since
//! reinventing shaping is a multi-year correctness problem with no
//! differentiation. The line-box/inline *layout* (where runs sit) is owned by
//! `janus-layout`; this crate only turns a positioned run into pixels.
//!
//! [`TextContext`] holds the (expensive to build) font system and a glyph
//! cache; create one per render and reuse it across runs.

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{Pixmap, PremultipliedColorU8};

const LINE_HEIGHT_FACTOR: f32 = 1.2;

/// Owns the font system and glyph cache used to draw text.
pub struct TextContext {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl std::fmt::Debug for TextContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextContext").finish_non_exhaustive()
    }
}

impl Default for TextContext {
    fn default() -> Self {
        Self::new()
    }
}

impl TextContext {
    /// Build a context, discovering the system's fonts (relatively expensive).
    #[must_use]
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }

    /// Measure the rendered width of `text` at `font_size` (px). Returns 0 if no
    /// font is available.
    #[must_use]
    pub fn measure(&mut self, text: &str, font_size: f32) -> f32 {
        let mut buffer = self.shaped_buffer(text, font_size);
        let buffer = buffer.borrow_with(&mut self.font_system);
        buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0_f32, f32::max)
    }

    /// Draw `text` at top-left `(x, y)` in `color` (straight-alpha RGBA),
    /// compositing glyph coverage onto `pixmap`. A no-op if no font is found.
    pub fn draw_run(
        &mut self,
        pixmap: &mut Pixmap,
        x: f32,
        y: f32,
        text: &str,
        font_size: f32,
        color: (u8, u8, u8, u8),
    ) {
        let (r, g, b, a) = color;
        if a == 0 || text.trim().is_empty() {
            return;
        }
        let buffer = self.shaped_buffer(text, font_size);
        let ox = x.round() as i32;
        let oy = y.round() as i32;
        let text_color = Color::rgba(r, g, b, a);

        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            text_color,
            |px, py, w, h, c| {
                let coverage = c.a();
                if coverage == 0 {
                    return;
                }
                for dy in 0..h as i32 {
                    for dx in 0..w as i32 {
                        blend_pixel(
                            pixmap,
                            ox + px + dx,
                            oy + py + dy,
                            c.r(),
                            c.g(),
                            c.b(),
                            coverage,
                        );
                    }
                }
            },
        );
    }

    fn shaped_buffer(&mut self, text: &str, font_size: f32) -> Buffer {
        let metrics = Metrics::new(font_size, font_size * LINE_HEIGHT_FACTOR);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(
            &mut self.font_system,
            Some(1.0e6),
            Some(font_size * LINE_HEIGHT_FACTOR * 2.0),
        );
        buffer.set_text(&mut self.font_system, text, Attrs::new(), Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }
}

/// Alpha-composite a straight-alpha source pixel over a premultiplied
/// destination pixel in the pixmap (bounds-checked; no-op if out of range).
fn blend_pixel(pixmap: &mut Pixmap, x: i32, y: i32, sr: u8, sg: u8, sb: u8, sa: u8) {
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    if x < 0 || y < 0 || x >= w || y >= h || sa == 0 {
        return;
    }
    let idx = (y * w + x) as usize;
    let pixels = pixmap.pixels_mut();
    let dst = pixels[idx];
    let a = u32::from(sa);
    let inv = 255 - a;

    // src premultiplied = straight * a / 255; dst is already premultiplied.
    let sp = |c: u8| (u32::from(c) * a) / 255;
    let out = |s: u32, d: u8| (s + u32::from(d) * inv / 255).min(255);
    let out_a = (a + u32::from(dst.alpha()) * inv / 255).min(255) as u8;
    let out_r = out(sp(sr), dst.red()).min(u32::from(out_a)) as u8;
    let out_g = out(sp(sg), dst.green()).min(u32::from(out_a)) as u8;
    let out_b = out(sp(sb), dst.blue()).min(u32::from(out_a)) as u8;

    pixels[idx] = PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a).unwrap_or(dst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_run_does_not_panic_and_respects_bounds() {
        let mut ctx = TextContext::new();
        let mut pixmap = Pixmap::new(200, 40).unwrap();
        pixmap.fill(tiny_skia::Color::WHITE);
        // Should never panic regardless of whether a system font is present.
        ctx.draw_run(&mut pixmap, 5.0, 5.0, "Hello", 16.0, (0, 0, 0, 255));
        // Drawing far off-canvas is a safe no-op.
        ctx.draw_run(&mut pixmap, 10_000.0, 10_000.0, "x", 16.0, (0, 0, 0, 255));
        assert_eq!(pixmap.width(), 200);
    }

    #[test]
    fn empty_and_transparent_runs_are_noops() {
        let mut ctx = TextContext::new();
        let mut pixmap = Pixmap::new(10, 10).unwrap();
        pixmap.fill(tiny_skia::Color::WHITE);
        ctx.draw_run(&mut pixmap, 0.0, 0.0, "   ", 16.0, (0, 0, 0, 255));
        ctx.draw_run(&mut pixmap, 0.0, 0.0, "hi", 16.0, (0, 0, 0, 0));
        // Still all white.
        let p = pixmap.pixel(1, 1).unwrap();
        assert_eq!((p.red(), p.green(), p.blue()), (255, 255, 255));
    }
}
