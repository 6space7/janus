//! The LLM interface — an MCP (JSON-RPC 2.0) server over the engine.
//!
//! This is the LLM-first product surface: a [`Session`] holds the current page
//! (DOM + styles + layout from one pass), and [`handle`] dispatches MCP requests
//! to tools that drive it:
//!
//! - `navigate { url | html, width? }` — load a page, return its semantic snapshot
//! - `snapshot` — the ref-tagged, box-grounded Semantic Surface of the page
//! - `extract_text` — the visible text (hidden/`display:none` content excluded)
//!
//! The request handler is pure and offline-unit-tested; [`serve_stdio`] runs the
//! newline-delimited JSON-RPC loop for the `janus-mcp` binary. The TOCTOU-safe
//! `act` tool (with nonce-bound stable-id revalidation) and semantic diff
//! streaming are the next additions.

use serde_json::{json, Value};

const DEFAULT_WIDTH: f32 = 800.0;

/// A browsing session: at most one loaded page, driven by the MCP tools.
#[derive(Debug, Default)]
pub struct Session {
    page: Option<janus_host::Page>,
}

impl Session {
    /// A fresh session with no page loaded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a page from inline HTML; returns its semantic snapshot text.
    ///
    /// # Errors
    /// If the document produces nothing renderable.
    pub fn load_html(&mut self, html: &str, width: f32) -> Result<String, String> {
        let page = janus_host::render_html(html, None, width).ok_or("nothing to render")?;
        let snapshot = page.snapshot();
        self.page = Some(page);
        Ok(snapshot)
    }

    /// Fetch a URL and load it (resolving external stylesheets).
    ///
    /// # Errors
    /// On a network/parse failure or an unrenderable document.
    pub fn load_url(&mut self, url: &str, width: f32) -> Result<String, String> {
        let page = janus_host::render_url(url, width)?;
        let snapshot = page.snapshot();
        self.page = Some(page);
        Ok(snapshot)
    }

    /// The semantic snapshot of the current page.
    ///
    /// # Errors
    /// If no page is loaded.
    pub fn snapshot(&self) -> Result<String, String> {
        Ok(self.page.as_ref().ok_or("no page loaded")?.snapshot())
    }

    /// The visible text content of the current page (excludes `display:none`).
    ///
    /// # Errors
    /// If no page is loaded.
    pub fn extract_text(&self) -> Result<String, String> {
        Ok(self.page.as_ref().ok_or("no page loaded")?.extract_text())
    }

    /// Follow the link at `ref_id`: resolve its target and load it, returning
    /// the new page's snapshot.
    ///
    /// # Errors
    /// If no page is loaded, the ref is not a link, or the load fails.
    pub fn click(&mut self, ref_id: &str, width: f32) -> Result<String, String> {
        let url = self
            .page
            .as_ref()
            .ok_or("no page loaded")?
            .resolve_link(ref_id)
            .ok_or_else(|| format!("ref '{ref_id}' is not a link"))?;
        self.load_url(&url, width)
    }

    /// Find nodes by optional role and/or name substring on the current page.
    ///
    /// # Errors
    /// If no page is loaded.
    pub fn find(&self, role: Option<&str>, name_contains: Option<&str>) -> Result<String, String> {
        Ok(self
            .page
            .as_ref()
            .ok_or("no page loaded")?
            .find(role, name_contains))
    }
}

/// Handle one MCP/JSON-RPC request. Returns the response, or `None` for a
/// notification (a message with no `id`).
#[must_use]
pub fn handle(session: &mut Session, request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match method {
        "initialize" => Some(ok(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "janus", "version": env!("CARGO_PKG_VERSION") },
            }),
        )),
        "ping" => Some(ok(id, json!({}))),
        "tools/list" => Some(ok(id, json!({ "tools": tool_specs() }))),
        "tools/call" => Some(call_tool(session, id, request.get("params"))),
        // Notifications (e.g. notifications/initialized) get no response.
        _ if id.is_none() => None,
        _ => Some(err(id, -32601, &format!("method not found: {method}"))),
    }
}

/// Run the newline-delimited JSON-RPC loop on stdin/stdout.
///
/// # Errors
/// On an unrecoverable IO error reading stdin or writing stdout.
pub fn serve_stdio() -> std::io::Result<()> {
    use std::io::{BufRead, Write};

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut session = Session::new();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(e) => {
                let response = err(Some(Value::Null), -32700, &format!("parse error: {e}"));
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
                continue;
            }
        };
        if let Some(response) = handle(&mut session, &request) {
            writeln!(
                stdout,
                "{}",
                serde_json::to_string(&response).unwrap_or_default()
            )?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn call_tool(session: &mut Session, id: Option<Value>, params: Option<&Value>) -> Value {
    let Some(params) = params else {
        return err(id, -32602, "missing params");
    };
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let width = args
        .get("width")
        .and_then(Value::as_f64)
        .map_or(DEFAULT_WIDTH, |w| w as f32);

    let outcome: Result<String, String> = match name {
        "navigate" => {
            if let Some(url) = args.get("url").and_then(Value::as_str) {
                session.load_url(url, width)
            } else if let Some(html) = args.get("html").and_then(Value::as_str) {
                session.load_html(html, width)
            } else {
                Err("navigate requires 'url' or 'html'".to_string())
            }
        }
        "snapshot" => session.snapshot(),
        "extract_text" => session.extract_text(),
        "click" => match args.get("ref").and_then(Value::as_str) {
            Some(ref_id) => session.click(ref_id, width),
            None => Err("click requires 'ref'".to_string()),
        },
        "find" => session.find(
            args.get("role").and_then(Value::as_str),
            args.get("name_contains").and_then(Value::as_str),
        ),
        other => Err(format!("unknown tool: {other}")),
    };

    // Per MCP, tool failures are a result with isError=true, not a protocol error.
    match outcome {
        Ok(text) => ok(id, tool_content(&text, false)),
        Err(message) => ok(id, tool_content(&message, true)),
    }
}

fn tool_content(text: &str, is_error: bool) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": is_error })
}

fn tool_specs() -> Value {
    json!([
        {
            "name": "navigate",
            "description": "Load a page by URL or inline HTML and return its semantic snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http(s) URL to fetch" },
                    "html": { "type": "string", "description": "inline HTML to render" },
                    "width": { "type": "number", "description": "viewport width in px (default 800)" }
                }
            }
        },
        {
            "name": "snapshot",
            "description": "Return the semantic snapshot of the current page: roles, accessible names, [ref=eN] handles, and box geometry.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "extract_text",
            "description": "Return the visible text content of the current page (hidden content excluded).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "click",
            "description": "Follow the link at the given [ref=eN]; loads its target and returns the new page's semantic snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": { "ref": { "type": "string", "description": "the element ref, e.g. e5" } },
                "required": ["ref"]
            }
        },
        {
            "name": "find",
            "description": "Find nodes on the current page by role and/or a name substring; returns one line per match.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "exact role, e.g. link, button, heading" },
                    "name_contains": { "type": "string", "description": "case-insensitive name substring" }
                }
            }
        }
    ])
}

fn ok(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result })
}

fn err(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(session: &mut Session, name: &str, args: Value) -> Value {
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        });
        handle(session, &request).expect("response")
    }

    #[test]
    fn initialize_reports_protocol_version() {
        let mut s = Session::new();
        let resp = handle(
            &mut s,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        )
        .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(resp["result"]["serverInfo"]["name"], "janus");
    }

    #[test]
    fn tools_list_advertises_tools() {
        let mut s = Session::new();
        let resp = handle(
            &mut s,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .unwrap();
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"navigate"));
        assert!(names.contains(&"snapshot"));
        assert!(names.contains(&"extract_text"));
    }

    #[test]
    fn navigate_html_returns_semantic_snapshot() {
        let mut s = Session::new();
        let resp = call(
            &mut s,
            "navigate",
            json!({ "html": "<html><body><h1>Hi</h1></body></html>" }),
        );
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("heading \"Hi\" [ref=e1]"), "{text}");
    }

    #[test]
    fn snapshot_without_page_is_tool_error() {
        let mut s = Session::new();
        let resp = call(&mut s, "snapshot", json!({}));
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn extract_text_excludes_hidden_content() {
        let mut s = Session::new();
        call(
            &mut s,
            "navigate",
            json!({ "html": "<html><body><p>visible</p><p style=\"display:none\">SECRET</p></body></html>" }),
        );
        let resp = call(&mut s, "extract_text", json!({}));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("visible"));
        assert!(
            !text.contains("SECRET"),
            "hidden text must not leak: {text}"
        );
    }

    #[test]
    fn unknown_method_is_protocol_error() {
        let mut s = Session::new();
        let resp = handle(&mut s, &json!({"jsonrpc":"2.0","id":9,"method":"bogus"})).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn notification_produces_no_response() {
        let mut s = Session::new();
        assert!(handle(
            &mut s,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .is_none());
    }
}
