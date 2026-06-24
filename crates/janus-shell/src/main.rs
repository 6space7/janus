//! `janus` — the desktop browser, with a modern egui UI.
//!
//! The window chrome (toolbar, editable address bar, back/forward/reload) is a
//! real egui UI; the page itself is rendered by the engine to a `tiny-skia`
//! pixmap and shown as a texture in the central panel. Clicking the page
//! hit-tests the Semantic Surface to follow links; the page re-flows when the
//! window width changes. Cookies persist across navigations.
//!
//! Usage: `cargo run -p janus-shell -- https://example.com/`
//! JS does not run yet, so heavily-dynamic sites render sparsely; content sites
//! (Wikipedia, Hacker News, blogs, docs) work well.

use eframe::egui;
use janus_host::{CookieJar, Page};

fn main() -> eframe::Result<()> {
    let start = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://example.com/".to_string());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Janus",
        options,
        Box::new(|_cc| Ok(Box::new(Janus::new(start)))),
    )
}

struct Janus {
    page: Option<Page>,
    texture: Option<egui::TextureHandle>,
    tex_size: (usize, usize),
    url: String,
    address: String,
    status: String,
    history: Vec<String>,
    hist_index: usize,
    jar: CookieJar,
    last_width: f32,
    last_ppp: f32,
    started: bool,
}

impl Janus {
    fn new(start: String) -> Self {
        Self {
            page: None,
            texture: None,
            tex_size: (0, 0),
            url: String::new(),
            address: start.clone(),
            status: String::new(),
            history: Vec::new(),
            hist_index: 0,
            jar: CookieJar::new(),
            last_width: 1024.0,
            last_ppp: 1.0,
            started: false,
        }
    }

    /// Fetch + render `url` at `width` CSS px; update the texture. Returns true
    /// on success.
    fn load(&mut self, ctx: &egui::Context, url: &str, width: f32) -> bool {
        match janus_host::render_url_with_jar(url, width.max(1.0), &mut self.jar) {
            Ok(page) => {
                if let Some(pm) = janus_paint::paint_scaled(&page.layout, ctx.pixels_per_point()) {
                    let size = [pm.width() as usize, pm.height() as usize];
                    let image = egui::ColorImage::from_rgba_unmultiplied(size, pm.data());
                    self.texture =
                        Some(ctx.load_texture("page", image, egui::TextureOptions::LINEAR));
                    self.tex_size = (size[0], size[1]);
                }
                self.url = url.to_string();
                self.address = url.to_string();
                self.status = format!("Loaded {url}");
                self.page = Some(page);
                true
            }
            Err(e) => {
                self.status = format!("Couldn't load {url} — {e}");
                false
            }
        }
    }

    fn navigate(&mut self, ctx: &egui::Context, url: String, width: f32) {
        if self.load(ctx, &url, width) {
            self.history.truncate(self.hist_index + 1);
            self.history.push(url);
            self.hist_index = self.history.len() - 1;
        }
    }

    fn back(&mut self, ctx: &egui::Context, width: f32) {
        if self.hist_index > 0 {
            self.hist_index -= 1;
            let url = self.history[self.hist_index].clone();
            self.load(ctx, &url, width);
        }
    }

    fn forward(&mut self, ctx: &egui::Context, width: f32) {
        if self.hist_index + 1 < self.history.len() {
            self.hist_index += 1;
            let url = self.history[self.hist_index].clone();
            self.load(ctx, &url, width);
        }
    }

    /// Re-layout the loaded page at a new width without refetching.
    fn reflow(&mut self, ctx: &egui::Context, width: f32) {
        if let Some(page) = &mut self.page {
            if let Some(layout) = janus_layout::layout_document(&page.dom, &page.styles, width) {
                page.layout = layout;
                if let Some(pm) = janus_paint::paint_scaled(&page.layout, ctx.pixels_per_point()) {
                    let size = [pm.width() as usize, pm.height() as usize];
                    let image = egui::ColorImage::from_rgba_unmultiplied(size, pm.data());
                    self.texture =
                        Some(ctx.load_texture("page", image, egui::TextureOptions::LINEAR));
                    self.tex_size = (size[0], size[1]);
                }
            }
        }
    }
}

impl eframe::App for Janus {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Toolbar.
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let can_back = self.hist_index > 0 && !self.history.is_empty();
                let can_fwd = self.hist_index + 1 < self.history.len();
                if ui.add_enabled(can_back, egui::Button::new("◀")).clicked() {
                    self.back(ctx, self.last_width);
                }
                if ui.add_enabled(can_fwd, egui::Button::new("▶")).clicked() {
                    self.forward(ctx, self.last_width);
                }
                if ui.button("⟳").clicked() && !self.url.is_empty() {
                    let url = self.url.clone();
                    self.load(ctx, &url, self.last_width);
                }
                let edit = egui::TextEdit::singleline(&mut self.address)
                    .hint_text("Enter a URL and press Enter")
                    .desired_width(ui.available_width());
                let resp = ui.add(edit);
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let url = normalize_url(&self.address);
                    self.navigate(ctx, url, self.last_width);
                }
            });
            ui.add_space(4.0);
        });

        // Status bar.
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(&self.status);
        });

        // Page.
        egui::CentralPanel::default().show(ctx, |ui| {
            let width = ui.available_width();
            let ppp = ctx.pixels_per_point();

            if !self.started {
                self.started = true;
                self.last_width = width;
                self.last_ppp = ppp;
                let url = normalize_url(&self.address.clone());
                self.navigate(ctx, url, width);
            } else if (width - self.last_width).abs() > 1.0 || (ppp - self.last_ppp).abs() > 0.01 {
                // Re-render on width *or* DPI change (e.g. dragged to another monitor)
                // so the texture is always rasterized at the current device resolution.
                self.last_width = width;
                self.last_ppp = ppp;
                self.reflow(ctx, width);
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(texture) = &self.texture {
                        // The texture is rasterized at `ppp` device px per CSS px;
                        // display it back at logical points so it stays crisp 1:1.
                        let size =
                            egui::vec2(self.tex_size.0 as f32 / ppp, self.tex_size.1 as f32 / ppp);
                        let resp = ui.add(
                            egui::Image::new(texture)
                                .fit_to_exact_size(size)
                                .sense(egui::Sense::click()),
                        );
                        if resp.clicked() {
                            if let (Some(pos), Some(page)) =
                                (resp.interact_pointer_pos(), self.page.as_ref())
                            {
                                let local = pos - resp.rect.min;
                                if let Some(url) = page.link_at(local.x, local.y) {
                                    self.navigate(ctx, url, self.last_width);
                                }
                            }
                        }
                    } else {
                        ui.label("Loading…");
                    }
                });
        });
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
