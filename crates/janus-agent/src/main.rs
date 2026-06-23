//! `janus-mcp` — the Janus engine as an MCP server over stdio.
//!
//! Speaks newline-delimited JSON-RPC 2.0; exposes `navigate`, `snapshot`, and
//! `extract_text` tools so an LLM agent can drive the engine. Wire it into an
//! MCP client as a stdio server command.

use std::process::ExitCode;

fn main() -> ExitCode {
    match janus_agent::serve_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("janus-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
