//! The paint stage: turn the layout box tree into a display list, then
//! rasterize it to pixels.
//!
//! This is the fork point of the dual-painter architecture: the same geometry
//! that feeds the human painter (here) feeds the LLM semantic painter
//! (`janus-sem`). The [`DisplayItem`] list is the retained, backend-agnostic
//! intermediate; the CPU backend renders it with `tiny-skia` (deterministic —
//! the property golden-image tests and reproducible agent snapshots rely on).
//!
//! Backgrounds and borders render as real filled rects. Text is recorded
//! faithfully in the display list but drawn as a placeholder ink bar for now —
//! real glyph rendering arrives with `janus-text` (rustybuzz/swash). A GPU
//! backend (`wgpu`+`vello`) slots in behind the same display list later; the
//! list is built layer-friendly (painter's order, parent before child).

use janus_layout::{LayoutBox, Rect};
use janus_style::{Color, Edges};
use janus_traits::PixelSize;
use tiny_skia::{Paint, Pixmap, Transform};

/// One drawing command in painter's order.
#[derive(Clone, Debug)]
pub enum DisplayItem {
    /// A filled rectangle (a background).
    Rect {
        /// The rectangle to fill.
        rect: Rect,
        /// Fill color.
        color: Color,
    },
    /// A box border drawn as four edge rects.
    Border {
        /// The border-box rectangle.
        rect: Rect,
        /// Per-side widths.
        widths: Edges<f32>,
        /// Border color.
        color: Color,
    },
    /// A run of text. Recorded with its real string for fidelity; the CPU
    /// backend currently draws a placeholder bar.
    Text {
        /// The text's line box.
        rect: Rect,
        /// The text content.
        text: String,
        /// Text color.
        color: Color,
        /// Font size in px.
        font_size: f32,
    },
}

/// Build the display list for a laid-out tree, in painter's order (each box's
/// background and border before its children; text where it sits in the tree).
#[must_use]
pub fn build_display_list(root: &LayoutBox) -> Vec<DisplayItem> {
    let mut items = Vec::new();
    root.for_each(&mut |b| {
        if b.background_color.a > 0 {
            items.push(DisplayItem::Rect {
                rect: b.rect,
                color: b.background_color,
            });
        }
        if has_visible_border(b) {
            items.push(DisplayItem::Border {
                rect: b.rect,
                widths: b.border,
                color: b.border_color,
            });
        }
        if let Some(text) = &b.text {
            items.push(DisplayItem::Text {
                rect: b.rect,
                text: text.clone(),
                color: b.text_color,
                font_size: b.font_size,
            });
        }
    });
    items
}

fn has_visible_border(b: &LayoutBox) -> bool {
    b.border_color.a > 0
        && (b.border.top > 0.0
            || b.border.right > 0.0
            || b.border.bottom > 0.0
            || b.border.left > 0.0)
}

/// The canvas size needed to contain a laid-out tree (device pixels).
#[must_use]
pub fn canvas_size(root: &LayoutBox) -> PixelSize {
    let w = (root.rect.x + root.rect.width).ceil().max(1.0);
    let h = (root.rect.y + root.rect.height).ceil().max(1.0);
    PixelSize::new(w as u32, h as u32)
}

/// Render a display list onto a fresh white pixmap of `size`, scaling all
/// geometry by `scale` (use `scale > 1.0` for crisp HiDPI / device-pixel output).
#[must_use]
pub fn render(items: &[DisplayItem], size: PixelSize, scale: f32) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(size.width.max(1), size.height.max(1))?;
    pixmap.fill(tiny_skia::Color::WHITE);
    let mut text = janus_text::TextContext::new();
    for item in items {
        paint_item(&mut pixmap, item, &mut text, scale);
    }
    Some(pixmap)
}

/// Lay out-to-pixels convenience: build the list, size the canvas, render at 1×.
#[must_use]
pub fn paint(root: &LayoutBox) -> Option<Pixmap> {
    render(&build_display_list(root), canvas_size(root), 1.0)
}

/// Like [`paint`] but renders at `scale`× device pixels — the page laid out in
/// CSS px is rasterized at higher resolution for crisp HiDPI display.
#[must_use]
pub fn paint_scaled(root: &LayoutBox, scale: f32) -> Option<Pixmap> {
    let base = canvas_size(root);
    let size = PixelSize::new(
        ((base.width as f32 * scale).ceil() as u32).max(1),
        ((base.height as f32 * scale).ceil() as u32).max(1),
    );
    render(&build_display_list(root), size, scale)
}

/// Render the tree and encode it as PNG bytes.
#[must_use]
pub fn paint_png(root: &LayoutBox) -> Option<Vec<u8>> {
    paint(root)?.encode_png().ok()
}

fn paint_item(
    pixmap: &mut Pixmap,
    item: &DisplayItem,
    text: &mut janus_text::TextContext,
    scale: f32,
) {
    match item {
        DisplayItem::Rect { rect, color } => fill_rect(pixmap, scaled(*rect, scale), *color),
        DisplayItem::Border {
            rect,
            widths,
            color,
        } => {
            let r = *rect;
            // Top, bottom, left, right as filled edge strips (all scaled).
            fill_rect(
                pixmap,
                scaled(
                    Rect {
                        height: widths.top,
                        ..r
                    },
                    scale,
                ),
                *color,
            );
            fill_rect(
                pixmap,
                scaled(
                    Rect {
                        y: r.y + r.height - widths.bottom,
                        height: widths.bottom,
                        ..r
                    },
                    scale,
                ),
                *color,
            );
            fill_rect(
                pixmap,
                scaled(
                    Rect {
                        width: widths.left,
                        ..r
                    },
                    scale,
                ),
                *color,
            );
            fill_rect(
                pixmap,
                scaled(
                    Rect {
                        x: r.x + r.width - widths.right,
                        width: widths.right,
                        ..r
                    },
                    scale,
                ),
                *color,
            );
        }
        DisplayItem::Text {
            rect,
            text: run,
            color,
            font_size,
        } => {
            text.draw_run(
                pixmap,
                rect.x * scale,
                rect.y * scale,
                run,
                font_size * scale,
                (color.r, color.g, color.b, color.a),
            );
        }
    }
}

fn scaled(r: Rect, s: f32) -> Rect {
    Rect {
        x: r.x * s,
        y: r.y * s,
        width: r.width * s,
        height: r.height * s,
    }
}

fn fill_rect(pixmap: &mut Pixmap, rect: Rect, color: Color) {
    if color.a == 0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let Some(sk_rect) = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = false; // crisp box edges + deterministic pixels
    pixmap.fill_rect(sk_rect, &paint, Transform::identity(), None);
}

/// A `tiny-skia` CPU [`Rasterizer`](janus_traits::Rasterizer) backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuRasterizer;

impl janus_traits::Rasterizer for CpuRasterizer {
    type DisplayList = Vec<DisplayItem>;
    type Surface = Pixmap;
    type Error = String;

    fn rasterize(
        &mut self,
        list: &Self::DisplayList,
        size: PixelSize,
    ) -> Result<Self::Surface, Self::Error> {
        render(list, size, 1.0).ok_or_else(|| "failed to allocate pixmap".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use janus_css::Stylesheet;

    fn layout(html: &str, width: f32) -> LayoutBox {
        let dom = janus_html::parse(html);
        let styles = janus_style::compute_styles(&dom, &Stylesheet::default());
        janus_layout::layout_document(&dom, &styles, width).expect("a rendered root")
    }

    #[test]
    fn display_list_has_background_and_text() {
        let root = layout(
            "<html><body><div style=\"background:red;height:20px\"></div><p>hi there</p></body></html>",
            200.0,
        );
        let items = build_display_list(&root);
        let has_red = items.iter().any(
            |it| matches!(it, DisplayItem::Rect { color, .. } if *color == Color::rgb(255, 0, 0)),
        );
        let has_text = items
            .iter()
            .any(|it| matches!(it, DisplayItem::Text { .. }));
        assert!(has_red, "expected a red background rect");
        assert!(has_text, "expected text items");
    }

    #[test]
    fn renders_red_background_pixel() {
        let root = layout(
            "<html><body><div style=\"width:50px;height:50px;background:red\"></div></body></html>",
            200.0,
        );
        let pixmap = paint(&root).expect("pixmap");
        // The div sits at (8,8) 50×50; sample a pixel well inside it.
        let px = pixmap.pixel(20, 20).expect("pixel in bounds");
        assert_eq!((px.red(), px.green(), px.blue()), (255, 0, 0));
        // A pixel to the right of the div (still on the page) stays white.
        let bg = pixmap.pixel(150, 20).expect("pixel in bounds");
        assert_eq!((bg.red(), bg.green(), bg.blue()), (255, 255, 255));
    }

    #[test]
    fn encodes_png_with_signature() {
        let root = layout("<html><body><p>hello</p></body></html>", 200.0);
        let png = paint_png(&root).expect("png bytes");
        assert!(
            png.starts_with(&[0x89, b'P', b'N', b'G']),
            "PNG magic header"
        );
    }
}
