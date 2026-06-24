//! `janus` — the desktop browser shell.
//!
//! Opens an OS window (winit), renders a page with the engine to a `tiny-skia`
//! pixmap, and presents it via a CPU framebuffer (softbuffer). Supports mouse
//! scrolling and **click-to-navigate**: a click is hit-tested against the
//! Semantic Surface's link geometry and follows the link.
//!
//! Usage: `cargo run -p janus-shell -- https://example.com/`
//!
//! v1 limitations: layout is at a fixed width (no reflow on resize), there is
//! no URL bar or back button yet, and JS does not run — those are next.

use std::num::NonZeroU32;
use std::rc::Rc;

use janus_host::Page;
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Fixed layout width in CSS px (clicks/coordinates are treated 1:1 with the
/// framebuffer, so on a HiDPI display the page appears in the top-left region).
const LAYOUT_WIDTH: f32 = 1024.0;

struct App {
    start_url: String,
    window: Option<Rc<Window>>,
    context: Option<Context<Rc<Window>>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    page: Option<Page>,
    pixmap: Option<tiny_skia::Pixmap>,
    scroll: u32,
    cursor: (f64, f64),
}

impl App {
    fn new(start_url: String) -> Self {
        Self {
            start_url,
            window: None,
            context: None,
            surface: None,
            page: None,
            pixmap: None,
            scroll: 0,
            cursor: (0.0, 0.0),
        }
    }

    fn load(&mut self, url: &str) {
        match janus_host::render_url(url, LAYOUT_WIDTH) {
            Ok(page) => {
                self.pixmap = janus_paint::paint(&page.layout);
                self.scroll = 0;
                self.page = Some(page);
                if let Some(window) = &self.window {
                    window.set_title(&format!("Janus — {url}"));
                    window.request_redraw();
                }
            }
            Err(e) => eprintln!("janus: loading {url}: {e}"),
        }
    }

    fn max_scroll(&self) -> u32 {
        let win_h = self.window.as_ref().map_or(0, |w| w.inner_size().height);
        self.pixmap
            .as_ref()
            .map_or(0, |pm| pm.height().saturating_sub(win_h))
    }

    fn present(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        surface.resize(w, h).expect("resize framebuffer");
        let mut buffer = surface.buffer_mut().expect("framebuffer");

        let (win_w, win_h) = (size.width as usize, size.height as usize);
        let scroll = self.scroll as usize;
        match &self.pixmap {
            Some(pm) => {
                let (pw, ph) = (pm.width() as usize, pm.height() as usize);
                let data = pm.data();
                for y in 0..win_h {
                    let sy = y + scroll;
                    for x in 0..win_w {
                        let dst = y * win_w + x;
                        buffer[dst] = if sy < ph && x < pw {
                            let p = (sy * pw + x) * 4;
                            (u32::from(data[p]) << 16)
                                | (u32::from(data[p + 1]) << 8)
                                | u32::from(data[p + 2])
                        } else {
                            0x00ff_ffff
                        };
                    }
                }
            }
            None => buffer.fill(0x00ff_ffff),
        }
        buffer.present().expect("present framebuffer");
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Janus")
            .with_inner_size(LogicalSize::new(1024.0, 768.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = Context::new(window.clone()).expect("softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);

        let url = self.start_url.clone();
        self.load(&url);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { position, .. } => self.cursor = (position.x, position.y),
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                let next = (self.scroll as f32 - dy).clamp(0.0, self.max_scroll() as f32);
                self.scroll = next as u32;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(page) = &self.page {
                    let x = self.cursor.0 as f32;
                    let y = self.cursor.1 as f32 + self.scroll as f32;
                    if let Some(url) = page.link_at(x, y) {
                        self.load(&url);
                    }
                }
            }
            WindowEvent::RedrawRequested => self.present(),
            WindowEvent::Resized(_) => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
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
