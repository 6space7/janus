//! `janus` — the desktop browser, with a modern egui UI.
//!
//! The window chrome (tab strip, toolbar, editable address bar, back/forward/
//! reload) is a real egui UI; each tab's page is rendered by the engine to a
//! `tiny-skia` pixmap and shown as a texture in the central panel. Clicking the
//! page hit-tests the Semantic Surface to follow links; the page re-flows when
//! the window width changes. Each tab has its own history and cookie jar.
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

/// One browser tab: its loaded page, rendered texture, address/history, and an
/// isolated cookie jar.
struct Tab {
    page: Option<Page>,
    texture: Option<egui::TextureHandle>,
    tex_size: (usize, usize),
    /// The (width, pixels_per_point) the current texture was rendered at, so we
    /// know when an activated/resized tab needs re-rendering.
    rendered: Option<(f32, f32)>,
    url: String,
    address: String,
    status: String,
    title: String,
    history: Vec<String>,
    hist_index: usize,
    jar: CookieJar,
}

impl Tab {
    fn new() -> Self {
        Self {
            page: None,
            texture: None,
            tex_size: (0, 0),
            rendered: None,
            url: String::new(),
            address: String::new(),
            status: String::new(),
            title: "New Tab".to_string(),
            history: Vec::new(),
            hist_index: 0,
            jar: CookieJar::new(),
        }
    }

    /// A short label for the tab strip.
    fn label(&self) -> String {
        let s = self.title.trim();
        let s = if s.is_empty() || s == "New Tab" {
            host_of(&self.url).unwrap_or_else(|| "New Tab".to_string())
        } else {
            s.to_string()
        };
        if s.chars().count() > 24 {
            format!("{}…", s.chars().take(24).collect::<String>())
        } else {
            s
        }
    }

    /// Rasterize the current page's layout into the texture at `(width, ppp)`.
    fn paint(&mut self, ctx: &egui::Context, width: f32, ppp: f32) {
        let Some(page) = &self.page else { return };
        if let Some(pm) = janus_paint::paint_scaled(&page.layout, ppp) {
            let size = [pm.width() as usize, pm.height() as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, pm.data());
            self.texture = Some(ctx.load_texture("page", image, egui::TextureOptions::LINEAR));
            self.tex_size = (size[0], size[1]);
            self.rendered = Some((width, ppp));
        }
    }

    /// Fetch + render `url` at `(width, ppp)`; update the texture. Returns true
    /// on success.
    fn load(&mut self, ctx: &egui::Context, url: &str, width: f32, ppp: f32) -> bool {
        match janus_host::render_url_with_jar(url, width.max(1.0), &mut self.jar) {
            Ok(page) => {
                self.title = page.title().unwrap_or_default();
                self.page = Some(page);
                self.paint(ctx, width, ppp);
                self.url = url.to_string();
                self.address = url.to_string();
                self.status = format!("Loaded {url}");
                true
            }
            Err(e) => {
                self.status = format!("Couldn't load {url} — {e}");
                false
            }
        }
    }

    fn navigate(&mut self, ctx: &egui::Context, url: String, width: f32, ppp: f32) {
        if self.load(ctx, &url, width, ppp) {
            self.history.truncate(self.hist_index + 1);
            self.history.push(url);
            self.hist_index = self.history.len() - 1;
        }
    }

    fn back(&mut self, ctx: &egui::Context, width: f32, ppp: f32) {
        if self.hist_index > 0 {
            self.hist_index -= 1;
            let url = self.history[self.hist_index].clone();
            self.load(ctx, &url, width, ppp);
        }
    }

    fn forward(&mut self, ctx: &egui::Context, width: f32, ppp: f32) {
        if self.hist_index + 1 < self.history.len() {
            self.hist_index += 1;
            let url = self.history[self.hist_index].clone();
            self.load(ctx, &url, width, ppp);
        }
    }

    /// Re-layout the loaded page at a new width (reusing cached images), then
    /// repaint. No refetch.
    fn reflow(&mut self, ctx: &egui::Context, width: f32, ppp: f32) {
        if let Some(page) = &mut self.page {
            if let Some(layout) = janus_layout::layout_document_with_images(
                &page.dom,
                &page.styles,
                width,
                &page.images,
            ) {
                page.layout = layout;
            }
        }
        self.paint(ctx, width, ppp);
    }

    fn can_back(&self) -> bool {
        self.hist_index > 0 && !self.history.is_empty()
    }
    fn can_forward(&self) -> bool {
        self.hist_index + 1 < self.history.len()
    }
}

struct Janus {
    tabs: Vec<Tab>,
    active: usize,
    last_width: f32,
    last_ppp: f32,
    started: bool,
    /// The URL the first tab opens to on the first frame.
    initial_url: String,
}

impl Janus {
    fn new(start: String) -> Self {
        Self {
            tabs: vec![Tab::new()],
            active: 0,
            last_width: 1024.0,
            last_ppp: 1.0,
            started: false,
            initial_url: start,
        }
    }
}

impl eframe::App for Janus {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let ppp = ctx.pixels_per_point();

        // Tab strip.
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                let mut switch_to = None;
                let mut close = None;
                let multiple = self.tabs.len() > 1;
                for (i, tab) in self.tabs.iter().enumerate() {
                    let selected = i == self.active;
                    if ui.selectable_label(selected, tab.label()).clicked() {
                        switch_to = Some(i);
                    }
                    // Only offer a close affordance when more than one tab.
                    if multiple && ui.small_button("×").clicked() {
                        close = Some(i);
                    }
                    ui.separator();
                }
                if ui.button("+").on_hover_text("New tab").clicked() {
                    self.tabs.push(Tab::new());
                    self.active = self.tabs.len() - 1;
                }
                if let Some(i) = switch_to {
                    self.active = i;
                }
                if let Some(i) = close {
                    self.tabs.remove(i);
                    if self.active >= self.tabs.len() {
                        self.active = self.tabs.len() - 1;
                    }
                }
            });
            ui.add_space(3.0);
        });

        // Toolbar (operates on the active tab).
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let width = self.last_width;
                let tab = &mut self.tabs[self.active];
                if ui
                    .add_enabled(tab.can_back(), egui::Button::new("◀"))
                    .clicked()
                {
                    tab.back(ctx, width, ppp);
                }
                if ui
                    .add_enabled(tab.can_forward(), egui::Button::new("▶"))
                    .clicked()
                {
                    tab.forward(ctx, width, ppp);
                }
                if ui.button("⟳").clicked() && !tab.url.is_empty() {
                    let url = tab.url.clone();
                    tab.load(ctx, &url, width, ppp);
                }
                let edit = egui::TextEdit::singleline(&mut tab.address)
                    .hint_text("Enter a URL and press Enter")
                    .desired_width(ui.available_width());
                let resp = ui.add(edit);
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let url = normalize_url(&tab.address);
                    tab.navigate(ctx, url, width, ppp);
                }
            });
            ui.add_space(4.0);
        });

        // Status bar (active tab).
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(&self.tabs[self.active].status);
        });

        // Page (active tab).
        egui::CentralPanel::default().show(ctx, |ui| {
            let width = ui.available_width();
            self.last_width = width;
            self.last_ppp = ppp;

            // First frame: load the initial URL into the first tab.
            if !self.started {
                self.started = true;
                let url = normalize_url(&self.initial_url);
                self.tabs[self.active].address = url.clone();
                self.tabs[self.active].navigate(ctx, url, width, ppp);
            }

            // Ensure the active tab's texture matches the current width/ppp
            // (re-render lazily on activation, resize, or DPI change).
            let tab = &mut self.tabs[self.active];
            if tab.page.is_some() && tab.rendered != Some((width, ppp)) {
                tab.reflow(ctx, width, ppp);
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let tab = &self.tabs[self.active];
                    if let Some(texture) = &tab.texture {
                        // The texture is rasterized at `ppp` device px per CSS px;
                        // display it back at logical points so it stays crisp 1:1.
                        let size =
                            egui::vec2(tab.tex_size.0 as f32 / ppp, tab.tex_size.1 as f32 / ppp);
                        let resp = ui.add(
                            egui::Image::new(texture)
                                .fit_to_exact_size(size)
                                .sense(egui::Sense::click()),
                        );
                        // Resolve any clicked link first (immutable borrow of the
                        // tab), then navigate (mutable) once that borrow is done.
                        let mut nav = None;
                        if resp.clicked() {
                            if let (Some(pos), Some(page)) =
                                (resp.interact_pointer_pos(), tab.page.as_ref())
                            {
                                let local = pos - resp.rect.min;
                                nav = page.link_at(local.x, local.y);
                            }
                        }
                        if let Some(url) = nav {
                            self.tabs[self.active].navigate(ctx, url, width, ppp);
                        }
                    } else {
                        ui.label("New tab — type a URL above and press Enter.");
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

/// The host portion of a URL, for a tab label fallback.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_string())
}
