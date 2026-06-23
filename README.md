# Janus

> A from-scratch, **speed-first**, **LLM-first** (and human) browser engine, in Rust.

Janus is built on one idea: **parse once, style once, lay out once — then paint that
same geometry twice.** One painter produces pixels for humans; the other produces a
**Semantic Surface** for LLM agents. Both share a single stable element-identity space,
so the two views can never structurally disagree.

Owning the engine — rather than driving someone else's through CDP — is the only way to
guarantee what bolt-on agent frameworks cannot: deterministic snapshots, **element IDs
that survive re-render**, **engine-computed affordances** (not trusted author ARIA),
**visibility/provenance tagging** (a prompt-injection defense), and **semantic diff
streaming**.

> Status: **working P0 engine.** Janus fetches a real URL over its own HTTP/1.1 + TLS
> stack, parses the HTML and CSS from scratch, runs the cascade and layout, and emits
> **both** a PNG (human view) and a ref-tagged, box-grounded semantic snapshot (agent
> view) from one layout pass. An MCP server (`janus-mcp`) exposes `navigate` / `snapshot`
> / `extract_text` / `click` / `find` / `screenshot` so an LLM can drive it. The human PNG
> renders real glyphs (system fonts via cosmic-text). Still to come: JS (`janus-js` / V8),
> sandboxing (`janus-sandbox`), and the windowed shell (`janus-shell`). See the project
> plan for the full roadmap.

Try it:

```sh
# Fetch a live site → PNG + agent snapshot (one layout pass, two painters)
cargo run -p janus-cli -- https://example.com/ --out page.png

# Drive the engine as an MCP server over stdio
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"navigate","arguments":{"url":"https://example.com/"}}}' \
  | cargo run -q -p janus-agent --bin janus-mcp
```

## The from-scratch boundary

We **build** everything that encodes web-platform *semantics and policy*; we **reuse**
only an irreducible *primitives floor* (security-critical to get bit-exact, with zero
differentiation in rewriting); and we use a few mature crates as **offline test oracles**
only (never shipped).

| Build (owned) | Reuse (primitives floor) | Oracle only (test/CI) |
|---|---|---|
| URL parser, HTTP/1.1+2 state machines, HTML parser, CSS cascade, layout, paint pipeline, the DOM model, the Semantic Surface, the agent interface | TLS crypto (rustls), Unicode/encoding tables (encoding_rs/ICU4X), font byte-parse + glyph raster (skrifa/rustybuzz/swash), image codecs, the JS VM (**V8 via rusty_v8**) | html5ever, Stylo, Taffy, Chrome/CDP — to *calibrate* correctness, never linked into the product |

## Speed is a founding pillar

Data-oriented arenas + atom interning · deterministic `rayon` parallelism · incremental
recompute (O(changes), not O(page)) · streaming parse + preload scanner · rule-hashing +
Bloom-filter fast style · GPU-ready layerized paint · **a benchmark harness with CI
perf-budget gates from commit one.** All four perf axes (interaction latency, cold-start &
memory, raw throughput, agent-path latency/tokens) are first-class.

## Crate map

Status legend: ✅ working · 🟡 partial · ⬜ stub.

| Crate | Responsibility | |
|---|---|---|
| `janus-atom` | String interning → `O(1)` integer-equality atoms | ✅ |
| `janus-arena` | Generational arena for hot trees | ✅ |
| `janus-traits` | Dependency-isolating seams (rasterizer, JS engine) | ✅ |
| `janus-bytes` | WHATWG/RFC-3986 URL parser + reference resolution + MIME sniff | ✅ |
| `janus-net` | HTTP/1.1 client over rustls TLS, redirects, chunked | 🟡 |
| `janus-html` | HTML tokenizer (RAWTEXT/RCDATA/entities) + tree builder | 🟡 |
| `janus-dom` | Arena-backed DOM node store | ✅ |
| `janus-css` | CSS parser → selectors, specificity, declarations | 🟡 |
| `janus-style` | Selector matching, the cascade, inheritance → computed styles | 🟡 |
| `janus-layout` | Block + inline layout → positioned box tree (geometry) | 🟡 |
| `janus-paint` | Display list + `tiny-skia` → PNG, with real glyphs via `janus-text` | 🟡 |
| `janus-sem` | The Semantic Surface: roles, names, geometry, stable IDs, href | 🟡 |
| `janus-host` | Pipeline orchestrator: `render_html`/`render_url` + external CSS | ✅ |
| `janus-agent` | MCP server (`navigate`/`snapshot`/`extract_text`/`click`) | 🟡 |
| `janus-cli` | `janus-shot`: URL/HTML → PNG + semantic snapshot | ✅ |
| `janus-text` | Real text: system fonts + shaping + glyph raster (cosmic-text) | 🟡 |
| `janus-js` | V8 host: rooting bridge + WebIDL DOM bindings | ⬜ |
| `janus-sandbox` | Multi-process isolation + OS sandbox policy | ⬜ |
| `janus-shell` | Human shell: window, engine-drawn chrome, AccessKit graft | ⬜ |

## Quickstart

```sh
cargo build --workspace            # build everything
cargo test  --workspace            # run all tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo bench -p janus-arena -p janus-atom   # run the perf harness
```

## License

MIT OR Apache-2.0.
