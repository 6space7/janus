//! `janus` — the desktop browser shell.
//!
//! A real OS window (winit) with browser chrome drawn by the engine itself
//! (tiny-skia rects + `janus-text` glyphs): an **address bar you type into**,
//! **back / forward / reload**, mouse **scrolling**, and **click-to-navigate**
//! links. The page is laid out at the window's logical width and re-flows on
//! resize; everything is scaled by the display's scale factor (HiDPI-aware).
//!
//! Usage: `cargo run -p janus-shell -- https://example.com/`
//! Then: click the address bar, type a URL, press Enter; click links; use the
//! `<` / `>` / `↻` buttons. JS does not run yet.

use std::num::NonZeroU32;
use std::rc::Rc;

use janus_host::Page;
use softbuffer::{Context, Surface};
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Toolbar height in logical px.
const CHROME: f32 = 44.0;

struct App {
    start_url: String,
    window: Option<Rc<Window>>,
    context: Option<Context<Rc<Window>>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    text: janus_text::TextContext,
    page: Option<Page>,
    pixmap: Option<Pixmap>,
    scroll: f32,
    cursor: (f64, f64),
    url: String,
    address: String,
    focused: bool,
    history: Vec<String>,
    hist_index: usize,
}

impl App {
    fn new(start_url: String) -> Self {
        Self {
            start_url,
            window: None,
            context: None,
            surface: None,
            text: janus_text::TextContext::new(),
            page: None,
            pixmap: None,
            scroll: 0.0,
            cursor: (0.0, 0.0),
            url: String::new(),
            address: String::new(),
            focused: false,
            history: Vec::new(),
            hist_index: 0,
        }
    }

    fn scale(&self) -> f32 {
        self.window
            .as_ref()
            .map_or(1.0, |w| w.scale_factor() as f32)
            .max(1.0)
    }

    /// The page's CSS layout width (physical window width ÷ scale factor).
    fn css_width(&self) -> f32 {
        self.window.as_ref().map_or(1024.0, |w| {
            (w.inner_size().width as f32 / self.scale()).max(1.0)
        })
    }

    fn viewport_css_height(&self) -> f32 {
        let s = self.scale();
        self.window.as_ref().map_or(0.0, |w| {
            ((w.inner_size().height as f32 - CHROME * s) / s).max(0.0)
        })
    }

    fn max_scroll(&self) -> f32 {
        let page_h = self.pixmap.as_ref().map_or(0.0, |pm| pm.height() as f32);
        (page_h - self.viewport_css_height()).max(0.0)
    }

    /// Fetch + render `url` at the current width (no history change).
    fn render(&mut self, url: &str) -> bool {
        match janus_host::render_url(url, self.css_width()) {
            Ok(page) => {
                self.pixmap = janus_paint::paint(&page.layout);
                self.page = Some(page);
                self.scroll = 0.0;
                if let Some(w) = &self.window {
                    w.set_title(&format!("Janus — {url}"));
                }
                true
            }
            Err(e) => {
                eprintln!("janus: loading {url}: {e}");
                // Show the failure in-window instead of leaving a blank page.
                let safe = |s: &str| s.replace('<', "(").replace('>', ")");
                let html = format!(
                    "<html><body><h2>Couldn't load this page</h2>\
                     <p>{}</p><p style=\"color:#b00\">{}</p></body></html>",
                    safe(url),
                    safe(&e),
                );
                if let Some(page) = janus_host::render_html(&html, None, self.css_width()) {
                    self.pixmap = janus_paint::paint(&page.layout);
                    self.page = Some(page);
                    self.scroll = 0.0;
                }
                false
            }
        }
    }

    /// User navigation: render and push onto the history stack.
    fn navigate(&mut self, url: String) {
        if self.render(&url) {
            self.history.truncate(self.hist_index + 1);
            self.history.push(url.clone());
            self.hist_index = self.history.len() - 1;
            self.url = url.clone();
            self.address = url;
            self.focused = false;
        }
        self.redraw();
    }

    fn back(&mut self) {
        if self.hist_index > 0 && !self.history.is_empty() {
            self.hist_index -= 1;
            let url = self.history[self.hist_index].clone();
            self.render(&url);
            self.url.clone_from(&url);
            self.address = url;
            self.redraw();
        }
    }

    fn forward(&mut self) {
        if self.hist_index + 1 < self.history.len() {
            self.hist_index += 1;
            let url = self.history[self.hist_index].clone();
            self.render(&url);
            self.url.clone_from(&url);
            self.address = url;
            self.redraw();
        }
    }

    /// Re-layout the loaded page at the current width (resize/scale change).
    fn reflow(&mut self) {
        let width = self.css_width();
        if let Some(page) = &mut self.page {
            if let Some(layout) = janus_layout::layout_document(&page.dom, &page.styles, width) {
                page.layout = layout;
                self.pixmap = janus_paint::paint(&page.layout);
            }
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn present(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let size = window.inner_size();
        let (Some(nzw), Some(nzh)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        let (phys_w, phys_h) = (size.width, size.height);
        let scale = self.scale();
        let chrome_px = (CHROME * scale).round() as u32;

        let Some(mut frame) = Pixmap::new(phys_w, phys_h) else {
            return;
        };
        frame.fill(tiny_skia::Color::WHITE);

        // Page region (nearest-neighbor upscale of the CSS-px pixmap to device px).
        {
            if let Some(pm) = &self.pixmap {
                let (pw, ph) = (pm.width(), pm.height());
                let src = pm.data();
                let dst = frame.data_mut();
                for py in chrome_px..phys_h {
                    let sy = (self.scroll + (py - chrome_px) as f32 / scale) as u32;
                    if sy >= ph {
                        continue;
                    }
                    for px in 0..phys_w {
                        let sx = (px as f32 / scale) as u32;
                        if sx >= pw {
                            continue;
                        }
                        let s = ((sy * pw + sx) * 4) as usize;
                        let d = ((py * phys_w + px) * 4) as usize;
                        dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
                    }
                }
            }
        }

        self.draw_chrome(&mut frame, phys_w as f32, scale);

        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        surface.resize(nzw, nzh).expect("resize");
        let mut buffer = surface.buffer_mut().expect("buffer");
        let fdata = frame.data();
        for (i, out) in buffer.iter_mut().enumerate() {
            let p = i * 4;
            *out = (u32::from(fdata[p]) << 16)
                | (u32::from(fdata[p + 1]) << 8)
                | u32::from(fdata[p + 2]);
        }
        buffer.present().expect("present");
    }

    fn draw_chrome(&mut self, frame: &mut Pixmap, phys_w: f32, s: f32) {
        let can_back = self.hist_index > 0 && !self.history.is_empty();
        let can_fwd = self.hist_index + 1 < self.history.len();
        let shown = if self.focused {
            self.address.clone()
        } else {
            self.url.clone()
        };
        let caret = self
            .focused
            .then(|| self.text.measure(&self.address, 15.0 * s));

        // Toolbar background + bottom border.
        fill_px(frame, 0.0, 0.0, phys_w, CHROME * s, (240, 240, 240));
        fill_px(frame, 0.0, CHROME * s - s, phys_w, s, (208, 208, 208));

        // Address field.
        let ax = 112.0 * s;
        let aw = (phys_w - ax - 10.0 * s).max(0.0);
        let ay = 8.0 * s;
        let ah = CHROME * s - 16.0 * s;
        fill_px(frame, ax, ay, aw, ah, (255, 255, 255));
        stroke_px(frame, ax, ay, aw, ah, s.max(1.0), (190, 190, 190));

        // Buttons + URL text.
        let back = if can_back {
            (40, 40, 40)
        } else {
            (190, 190, 190)
        };
        let fwd = if can_fwd {
            (40, 40, 40)
        } else {
            (190, 190, 190)
        };
        self.text
            .draw_run(frame, 16.0 * s, 9.0 * s, "<", 22.0 * s, rgba(back));
        self.text
            .draw_run(frame, 48.0 * s, 9.0 * s, ">", 22.0 * s, rgba(fwd));
        self.text.draw_run(
            frame,
            80.0 * s,
            11.0 * s,
            "\u{21bb}",
            18.0 * s,
            rgba((40, 40, 40)),
        );
        self.text.draw_run(
            frame,
            ax + 8.0 * s,
            11.0 * s,
            &shown,
            15.0 * s,
            rgba((20, 20, 20)),
        );
        if let Some(w) = caret {
            fill_px(
                frame,
                ax + 8.0 * s + w + 1.0,
                11.0 * s,
                1.5 * s,
                16.0 * s,
                (20, 20, 20),
            );
        }
    }

    fn on_click(&mut self) {
        let (cx, cy) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let s = self.scale();
        if cy < CHROME * s {
            if cx >= 8.0 * s && cx < 40.0 * s {
                self.back();
            } else if cx >= 40.0 * s && cx < 72.0 * s {
                self.forward();
            } else if cx >= 72.0 * s && cx < 104.0 * s {
                let url = self.url.clone();
                if !url.is_empty() {
                    self.render(&url);
                    self.redraw();
                }
            } else if cx >= 108.0 * s {
                self.focused = true;
                self.address.clone_from(&self.url);
                self.redraw();
            } else {
                self.focused = false;
                self.redraw();
            }
            return;
        }
        self.focused = false;
        if let Some(page) = &self.page {
            let x = cx / s;
            let y = (cy - CHROME * s) / s + self.scroll;
            if let Some(url) = page.link_at(x, y) {
                self.navigate(url);
                return;
            }
        }
        self.redraw();
    }

    fn on_key(&mut self, key: &Key, text: Option<&str>) {
        if !self.focused {
            return;
        }
        match key {
            Key::Named(NamedKey::Enter) => {
                let url = normalize_url(&self.address);
                self.navigate(url);
            }
            Key::Named(NamedKey::Backspace) => {
                self.address.pop();
                self.redraw();
            }
            Key::Named(NamedKey::Escape) => {
                self.focused = false;
                self.address.clone_from(&self.url);
                self.redraw();
            }
            _ => {
                if let Some(t) = text {
                    for ch in t.chars().filter(|c| !c.is_control()) {
                        self.address.push(ch);
                    }
                    self.redraw();
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Janus")
            .with_inner_size(LogicalSize::new(1100.0, 780.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = Context::new(window.clone()).expect("softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
        let url = normalize_url(&self.start_url.clone());
        self.navigate(url);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { position, .. } => self.cursor = (position.x, position.y),
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / self.scale(),
                };
                self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll());
                self.redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.on_click(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.on_key(&event.logical_key, event.text.as_deref());
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.reflow();
                self.redraw();
            }
            WindowEvent::RedrawRequested => self.present(),
            _ => {}
        }
    }
}

fn normalize_url(input: &str) -> String {
    let s = input.trim();
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("file:") {
        s.to_string()
    } else {
        format!("https://{s}")
    }
}

fn rgba(c: (u8, u8, u8)) -> (u8, u8, u8, u8) {
    (c.0, c.1, c.2, 255)
}

fn fill_px(frame: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, c: (u8, u8, u8)) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w, h) else {
        return;
    };
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(c.0, c.1, c.2, 255);
    paint.anti_alias = false;
    frame.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
}

fn stroke_px(frame: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, t: f32, c: (u8, u8, u8)) {
    fill_px(frame, x, y, w, t, c); // top
    fill_px(frame, x, y + h - t, w, t, c); // bottom
    fill_px(frame, x, y, t, h, c); // left
    fill_px(frame, x + w - t, y, t, h, c); // right
}

fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://example.com/".to_string());
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(url);
    event_loop.run_app(&mut app).expect("run app");
}
