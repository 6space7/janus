//! `janus-shot` — the headless entry point and the P0 acceptance test.
//!
//! Given a URL, local HTML file, inline HTML, or stdin, it runs the whole engine
//! once via `janus-host` — bytes → DOM → CSS → cascade → layout — then drives
//! **both painters**: a PNG (the human view) and the semantic snapshot (the
//! agent view). One layout pass, two outputs: the dual-painter thesis.
//!
//! Usage:
//!   janus-shot [SOURCE] [--width N] [--out FILE]
//!     SOURCE   an http(s) URL, a file path, `file://…`, inline HTML, or `-`
//!              for stdin (omitted → a built-in demo page)
//!
//! `http(s)` SOURCEs are fetched via the from-scratch `janus-net` client, and
//! their `<link>`ed stylesheets are fetched and applied too.

use std::io::Read;
use std::process::ExitCode;

use janus_host::Page;

const DEMO_HTML: &str = r#"<!DOCTYPE html>
<html>
  <head>
    <title>Janus demo</title>
    <style>
      body { background: #ffffff; color: #222222; }
      h1 { color: navy; }
      .card { background: #eef; border: 2px solid navy; padding: 8px; }
      .danger { color: red; }
    </style>
  </head>
  <body>
    <h1>Hello from Janus</h1>
    <p class="lead">A from-scratch, dual-painter browser engine in Rust.</p>
    <div class="card">
      <p>This box has a background and a border.</p>
      <a href="https://example.com/">A link</a>
    </div>
    <ul>
      <li>parse</li>
      <li>style</li>
      <li>layout</li>
    </ul>
  </body>
</html>
"#;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut width = 800.0_f32;
    let mut out = "janus-shot.png".to_string();
    let mut source: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--width" => {
                i += 1;
                if let Some(w) = args.get(i).and_then(|s| s.parse::<f32>().ok()) {
                    width = w;
                }
            }
            "--out" => {
                i += 1;
                if let Some(o) = args.get(i) {
                    out.clone_from(o);
                }
            }
            "--help" | "-h" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other => source = Some(other.to_string()),
        }
        i += 1;
    }

    let page = match build_page(source.as_deref(), width) {
        Ok(page) => page,
        Err(e) => {
            eprintln!("janus-shot: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Painter 1 — human pixels.
    match janus_paint::paint_png(&page.layout) {
        Some(png) => {
            if let Err(e) = std::fs::write(&out, &png) {
                eprintln!("janus-shot: writing {out}: {e}");
                return ExitCode::FAILURE;
            }
            let size = janus_paint::canvas_size(&page.layout);
            eprintln!(
                "janus-shot: wrote {out} ({}x{} px, {} bytes)",
                size.width,
                size.height,
                png.len()
            );
        }
        None => {
            eprintln!("janus-shot: failed to rasterize");
            return ExitCode::FAILURE;
        }
    }

    // Painter 2 — the agent's semantic snapshot, to stdout.
    print!("{}", page.snapshot());

    ExitCode::SUCCESS
}

fn build_page(source: Option<&str>, width: f32) -> Result<Page, String> {
    match source {
        None => janus_host::render_html(DEMO_HTML, None, width).ok_or_else(empty),
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("reading stdin: {e}"))?;
            janus_host::render_html(&buf, None, width).ok_or_else(empty)
        }
        Some(s) if s.starts_with("http://") || s.starts_with("https://") => {
            janus_host::render_url(s, width)
        }
        Some(s) => {
            let path = s.strip_prefix("file://").unwrap_or(s);
            let html = if std::path::Path::new(path).is_file() {
                std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?
            } else if s.contains('<') {
                s.to_string()
            } else {
                return Err(format!("no such file, and not inline HTML: {s}"));
            };
            janus_host::render_html(&html, None, width).ok_or_else(empty)
        }
    }
}

fn empty() -> String {
    "nothing to render".to_string()
}

fn print_usage() {
    eprintln!(
        "janus-shot [SOURCE] [--width N] [--out FILE]\n\
         \n\
         SOURCE   an http(s) URL, a file path, file://…, inline HTML, or - for stdin\n\
         --width  viewport width in CSS px (default 800)\n\
         --out    PNG output path (default janus-shot.png)\n\
         \n\
         With no SOURCE, renders a built-in demo page."
    );
}
