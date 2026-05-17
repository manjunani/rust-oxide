# Rust Oxide

**The Agent-Native OS** — a high-performance Rust toolchain that gives AI agents a unified, intelligent, and secure interface to structured APIs (OpenAPI, GraphQL, gRPC) and unstructured web content.

## Status

Bootstrap phases shipped:

| Phase | Crate | What it does |
|-------|-------|--------------|
| 1 | `oxide-k` | Micro-kernel: module orchestration, message bus, state registry, manifest discovery |
| 2 | `oxide-compress` | WASM-bound token compression: field selection, metadata stripping, semantic chunking |
| 3 | `oxide-gen` | Spec-to-crate generator for OpenAPI / GraphQL / gRPC |
| 4 | `oxide-browser-sh` | Self-healing browser automation built on `chromiumoxide` |
| 5 | `oxide-mirror` | Event-sourced local data mirror with conflict strategies and SQL query interface |
| 6 | `oxide-llm-orchestrator` | LLM front-door — chat client, prompt templates, LLM-driven self-healing |
| 6 | `oxide-mcp-server` | MCP (Model Context Protocol) stdio server exposing CLI + bus tools |
| 7 | `oxide-graph` | Semantic knowledge graph: in-memory typed nodes + labelled edges, pattern queries, BFS traversal, mirror-record ingestion |
| 7 | `oxide-mesh` | Inter-agent communication: in-process `tokio::mpsc` fabric + JSON-line TCP transport, capability advertising, broadcast / direct / task messages |
| 7 | `oxide-k::xai` | Append-only Explainable-AI decision log: actor, action, rationale, inputs, output, confidence, queryable by actor / recent |
| 7 | `oxide-gen` streaming | GraphQL `type Subscription` fields + gRPC `stream` request / response detected and tagged as `ServerStream` / `ClientStream` / `BidiStream`; emitter renders streaming stubs |

Still to come: wasmtime integration for the `WasmModule` trait, real CDP `Accessibility.getFullAXTree` ingestion, tonic-based gRPC dispatch in generated crates.

## Workspace layout

```
rust-oxide/
├── Cargo.toml
├── rust-toolchain.toml
├── crates/
│   ├── oxide-k/                 # Layer 1 native micro-kernel
│   │   └── src/{lib, main, kernel, module, bus, registry, manifest, error}.rs
│   ├── oxide-compress/          # Layer 2 WASM plugin
│   │   └── src/lib.rs
│   ├── oxide-gen/               # spec-to-crate generator
│   │   ├── src/{lib, main, ir, error}.rs
│   │   ├── src/parsers/{openapi, graphql, proto, naming}.rs
│   │   ├── src/emit/{cargo, rust_lib, rust_cli, skill, mcp, manifest}.rs
│   │   └── tests/{integration, fixtures/}
│   └── oxide-browser-sh/        # self-healing browser automation
│       └── src/{lib, backend, mock, chromium, accessibility,
│                  session, healing, extract, kernel, action, error}.rs
└── README.md
```

## `oxide-k` — micro-kernel

Three cooperating subsystems plus a manifest loader:

| Subsystem | File | Responsibility |
|-----------|------|----------------|
| Module orchestration | `module.rs` | `Module` and `WasmModule` traits, `ModuleManager` lifecycle |
| Secure message bus | `bus.rs` | `tokio::sync::mpsc` fan-out, `Command`/`Event` envelopes |
| Global state registry | `registry.rs` | `sqlx` + SQLite (in-memory by default) for module metadata and config |
| Manifest discovery | `manifest.rs` | `ModuleManifest` + `Kernel::register_module_from_manifest(path)` |

Tied together by the `Kernel` façade (`kernel.rs`).

## `oxide-compress` — token-compression plugin

| Function | Purpose |
|----------|---------|
| `select_fields(json_data, fields)` | Prune JSON to a chosen set of top-level fields |
| `strip_metadata(text)` | HTML tag stripping, entity decoding, boilerplate removal, whitespace collapsing |
| `chunk_text(text, max_len)` | Char-bounded chunks preferring paragraph → sentence → word boundaries |

Builds for both native (rlib) and `wasm32-unknown-unknown` (cdylib). WASM bindings via `wasm-bindgen` are gated on the wasm target.

## `oxide-gen` — spec-to-crate generator

Takes an OpenAPI 3.x JSON/YAML, a GraphQL SDL, or a `.proto` file and emits a self-contained Rust crate.

### Pipeline

```
spec file ──► parser ──► ApiSpec (IR) ──► emitters ──► crate dir
                                              │
                                              ├── Cargo.toml
                                              ├── src/lib.rs        (types + reqwest/tonic client)
                                              ├── src/main.rs       (clap CLI per operation)
                                              ├── SKILL.md          (Claude Code skill descriptor)
                                              ├── mcp.json          (MCP server config)
                                              └── module.json       (oxide-k discovery manifest)
```

### Parsers

| Format | Crate / approach | Notes |
|--------|------------------|-------|
| OpenAPI 3.x | [`openapiv3`] 2.x | JSON or YAML; handles primitives, refs, arrays, object schemas, path/query/body/header parameters |
| GraphQL SDL | [`apollo-parser`] 0.8 | Walks the CST; emits structs for object/input types, enums for enum types, ops for `Query` / `Mutation` fields |
| gRPC `.proto` | hand-rolled | proto3-subset parser; supports `message`, `service`, `rpc`, scalars, `repeated` |

All three normalize to a common `ApiSpec` IR (`crates/oxide-gen/src/ir.rs`).

### Emitters

| File | Purpose |
|------|---------|
| `Cargo.toml` | Generated crate manifest, with `reqwest` for OpenAPI/GraphQL or `tonic` scaffolding for gRPC |
| `src/lib.rs` | Type definitions + async `Client` struct with one method per operation |
| `src/main.rs` | `clap`-derived CLI with one subcommand per operation; pretty-prints JSON output |
| `SKILL.md` | YAML-frontmatter skill descriptor enumerating every command and its args |
| `mcp.json` | MCP stdio server config exposing each subcommand as a callable tool |
| `module.json` | Manifest consumed by `oxide-k`'s `Kernel::register_module_from_manifest` |

### Usage

```bash
cargo run -p oxide-gen -- \
  --spec crates/oxide-gen/tests/fixtures/petstore.yaml \
  --output /tmp/petstore

cd /tmp/petstore
cargo check          # generated crate compiles independently
cargo run -- list-pets --base-url https://petstore.example.com/v1
```

`--kind {openapi,graphql,grpc}` can override auto-detection (defaults to the file extension). `--name <crate_name>` overrides the inferred crate name.

### `oxide-k` integration

Once generated, the kernel can discover the crate via its `module.json`:

```rust
let kernel = oxide_k::Kernel::in_memory().await?;
let resolved = kernel
    .register_module_from_manifest(Path::new("/tmp/petstore"))
    .await?;
println!("{:?}", resolved.binary_path);   // /tmp/petstore/pet-store-cli
```

The module is recorded in the registry in `ModuleState::Loaded`. Spawning the binary as a sandboxed child process is the responsibility of a future process supervisor.

## `oxide-browser-sh` — self-healing browser automation

Provides headless-Chromium control with accessibility-tree-first targeting and a self-healing retry loop.

| Layer | Module | What it does |
|-------|--------|--------------|
| Trait | `backend.rs` | `BrowserBackend` — async trait for `navigate / click / input / scroll / html / url / screenshot / accessibility_tree` |
| Backends | `chromium.rs` / `mock.rs` | Real `chromiumoxide` driver and an in-memory `scraper`-backed mock for offline tests |
| Selectors | `action.rs` | `Selector::{Role{role,name}, Text, Css, XPath}` — semantic first, brittle last |
| AX layer | `accessibility.rs` | `AxNode` tree + `AxQuery`; synthesizes a tree from HTML when CDP is unavailable |
| Healing | `healing.rs` | `HealingStrategy` trait, `DefaultHealing` (rule-based AX→CSS rewrites), `LlmStubHealing` (placeholder for `oxide-llm-orchestrator`) |
| Session | `session.rs` | `BrowserSession` orchestrator: every action runs through `heal_loop` and is recorded in `SessionStats` |
| Extraction | `extract.rs` | HTML → clean Markdown, post-processed through `oxide_compress::strip_metadata` + `chunk_text` |
| Kernel | `kernel.rs` | `BrowserModule` implements `oxide_k::module::Module`; routes `Command::Invoke { module_id: "browser", method, payload }` to session methods |

### Bus methods

| Method | Payload | Action |
|--------|---------|--------|
| `navigate` | `{"url": "..."}` | Open the URL |
| `click` | `{"selector": <Selector>}` | Click element (with healing) |
| `input` | `{"selector": <Selector>, "text": "..."}` | Type into element |
| `scroll` | `{"direction": "down", "amount": 240}` | Scroll the viewport |
| `extract` | `{"max_chunk_chars": 800, "strip_boilerplate": true}` | Render the current page to Markdown + chunks |
| `stats` | `{}` | Return the session's action history |

Every invocation emits an `Event::Custom { kind: "<method>.ok" | "<method>.err", payload }` on the bus.

### Real-browser tests

Tests that actually launch Chromium are gated behind the `live-browser` feature and tagged `#[ignore]`:

```bash
cargo test -p oxide-browser-sh --features live-browser -- --ignored
```

## `oxide-mirror` — local event-sourced data mirror

Persistent SQLite store that pulls deltas from API sources, resolves conflicts, and answers read-only SQL queries with full data provenance.

| Component | File | Responsibility |
|-----------|------|----------------|
| Events | `event.rs` | `Delta`, `DeltaOp::{Upsert,Delete}`, `Provenance { source, confidence }`, `MirroredRecord { resource, record_id, payload, source, last_synced_at, confidence, version }` |
| Source trait | `source.rs` | `SyncSource::pull(cursor) → PullResult { deltas, next_cursor, has_more }`. `oxide-gen` generated clients implement this. `StaticSource` for tests. |
| Store | `store.rs` | `MirrorStore` (sqlx + SQLite). Tables: `mirror_resources`, `mirror_events` (append-only audit log), `mirror_records` (materialised state), `mirror_cursors` (per-source-per-resource resume points). |
| Conflict | `conflict.rs` | `ConflictStrategy` trait + `LastWriteWins`, `HighestConfidence`, `KeepLocal`, `MergeJson` (deep JSON merge). |
| Sync | `sync.rs` | `Syncer` orchestrator: paginates the source, applies through chosen strategy, advances cursors per-resource + global. Returns `SyncReport { pulled, applied, skipped, final_cursor, per_resource }`. |
| Query | `store.rs` | `MirrorStore::query(sql)` — accepts `SELECT` / `WITH` / `PRAGMA` only, rejects multi-statement input. Returns `Vec<serde_json::Map>` with proper SQLite-type → JSON mapping (handles computed columns with no declared type). |
| Kernel | `kernel.rs` | `MirrorModule` implements `oxide_k::module::Module`. Bus methods: `sync`, `query`, `get_record`, `list_records`, `resources`, `counts`. Emits `Event::Custom { kind: "<method>.{ok,err}" }`. |

### Bus methods

| Method | Payload | Returns |
|--------|---------|---------|
| `sync` | `{"source": "<id>"}` | `SyncReport` |
| `query` | `{"sql": "SELECT …"}` | `{"rows": [...], "count": N}` |
| `get_record` | `{"resource": "...", "record_id": "..."}` | `MirroredRecord` or `null` |
| `list_records` | `{"resource": "..."}` | `[MirroredRecord]` |
| `resources` | `{}` | `{"resources": [...]}` |
| `counts` | `{}` | `[[resource, count], ...]` |

### Provenance

Every `mirror_records` row carries:
- `source` — id of the writer (matches `Provenance::source` on the originating delta)
- `last_synced_at` — RFC3339 UTC timestamp of the latest applied delta
- `confidence` — float in `[0.0, 1.0]`
- `version` — monotonic per-record counter, incremented on every successful apply

`mirror_events` is the full append-only audit trail. Even skipped deltas (due to conflict strategy) are recorded with `applied = 0` and the strategy's decision label.

## `oxide-llm-orchestrator` — LLM front-door

Provider-agnostic chat client + prompt templates + LLM-driven healing for `oxide-browser-sh`.

| Component | File | Responsibility |
|-----------|------|----------------|
| Client | `client.rs` | `LlmClient` async trait; `OpenAiClient` HTTP impl for any OpenAI-compatible `/v1/chat/completions`; `MockLlmClient` with a canned-response queue + call log for tests |
| Prompts | `prompts.rs` | `PromptTemplate::{HealingSelector, ErrorAnalysis, Summarize}` with stable system messages and a `Renderable` trait for input substitution. Healing + error-analysis templates auto-request `response_format: json_object` |
| Healing | `healing.rs` | `LlmHealing` implements `oxide_browser_sh::HealingStrategy` — truncates HTML, builds a healing prompt, parses the LLM's JSON, returns a new `Selector`. Falls back to `DefaultHealing` on any LLM error or unparseable response |
| Summarisation | `summarize.rs` | `Summarizer` wraps the summarise prompt with optional focus question + word budget |
| Kernel | `kernel.rs` | `LlmModule` implements `oxide_k::module::Module`. Bus methods: `complete`, `summarize`, `analyze`. Emits `Event::Custom { kind: "<method>.{ok,err}" }` |

### Bus methods

| Method | Payload | Returns |
|--------|---------|---------|
| `complete` | `{"messages":[…], "model"?, "temperature"?, "json"?}` | `{"content": …, "model": …, "usage": …}` |
| `summarize` | `{"text":…, "target_words"?, "focus"?}` | `{"summary":…, "model":…, "usage":…}` |
| `analyze` | `{"operation":…, "error":…, "context"?}` | `{"analysis":…, "model":…, "usage":…}` |

The `HealingStrategy` trait in `oxide-browser-sh` is now async to support remote (LLM) healers.

## `oxide-mcp-server` — Model Context Protocol stdio server

Exposes generated `oxide-gen` CLIs and `oxide-k` bus actions to MCP-compatible agent runtimes.

| Component | File | Responsibility |
|-----------|------|----------------|
| RPC | `rpc.rs` | JSON-RPC 2.0 envelope (`JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError`, `RpcId`); standard + MCP-specific error codes |
| Tools | `tool.rs` | `Tool` async trait + `ToolDescriptor`/`ToolInputSchema`; `CliTool` spawns a subprocess (flag-mapped or stdin-JSON, with timeout); `BusTool` sends `Command::Invoke` on a `MessageBus` and awaits the matching `<method>.{ok,err}` event; `ToolRegistry` keeps them in a name-keyed map |
| Server | `server.rs` | `McpServer::handle_line` dispatches `initialize`, `tools/list`, `tools/call`, `ping`, and acknowledges MCP lifecycle notifications. `run_io` is the stdio loop (one JSON request per line) |
| Binary | `main.rs` | Starts an empty-registry stdio server; designed to be embedded by other Rust Oxide processes that build their own registry |

### MCP-protocol coverage

| Method | Returns |
|--------|---------|
| `initialize` | `{protocolVersion, serverInfo:{name,version}, capabilities:{tools:{listChanged:false}}}` |
| `tools/list` | `{tools:[ToolDescriptor]}` (sorted by name) |
| `tools/call` | `{content:[{type:"text", text:<json>}], structuredContent:<json>, isError:false}` |
| `notifications/initialized` `notifications/cancelled` | acknowledged, no response |
| `ping` | `{pong:true}` |

Logs go to **stderr only** — stdout is the JSON-RPC channel.

## `oxide-graph` — semantic knowledge graph

In-process property graph that fuses `oxide-mirror` rows, `oxide-browser-sh` extractions, and any other module's facts.

| Component | File | Responsibility |
|-----------|------|----------------|
| Graph | `graph.rs` | `Node{id,labels,properties}`, `Edge{id,from,to,label,properties}`, `GraphStore` async trait, `InMemoryGraph` with label / out-edge / in-edge indices |
| Query | `query.rs` | `NodeQuery::label(L).property_eq(K,V)`, `EdgeQuery::outbound/inbound/either`, `traverse(start, edge_label?, max_depth)` BFS |
| Ingest | `ingest.rs` | `ingest_record(RecordRef)` — maps a JSON object into a node; string fields shaped `"<resource>:<id>"` become reference edges (with placeholder nodes on dangling refs) |
| Kernel | `kernel.rs` | `GraphModule` bus methods: `ingest`, `upsert_node`, `add_edge`, `get_node`, `node_query`, `edge_query`, `traverse`, `stats` |

## `oxide-mesh` — inter-agent communication

Transport-agnostic peer fabric. Two backends ship: `tokio::mpsc` for in-process federation, JSON-line TCP for cross-host.

| Component | File | Responsibility |
|-----------|------|----------------|
| Protocol | `message.rs` | `PeerMessage::{Hello, Broadcast, Direct, Task, Result}` with `PeerCapability` advertising |
| Local | `local.rs` | `LocalMesh` channel registry; topic-filtered broadcast + direct routing |
| TCP | `tcp.rs` | `TcpMesh::serve(addr)` accepts JSON-line peers; `TcpMesh::connect(addr, hello)` returns a `TcpClient` |
| Kernel | `kernel.rs` | `MeshModule` registers `kernel` as a peer, forwards inbound peer traffic onto the bus as `peer.{hello,broadcast,direct,task,result}` Custom events; bus methods: `publish`, `directory`, `peer_count` |

## XAI — `oxide-k::xai`

Append-only decision log inside the kernel registry. Used by healers, mirrors, and any agent that wants to explain itself.

```rust
use oxide_k::xai::{Decision, DecisionLog};

let log = DecisionLog::open(kernel.registry()).await?;
log.log(
    &Decision::new("healer", "rewrite-selector", "AX role match found")
        .with_inputs(serde_json::json!({"css": ".primary"}))
        .with_output(serde_json::json!({"role": "button", "name": "Sign up"}))
        .with_confidence(0.85),
).await?;
```

Stored in a `xai_decisions` SQLite table next to `modules` / `config`. Queryable via `recent(n)` and `by_actor(id)`. Confidence is clamped to `[0.0, 1.0]`.

## `oxide-gen` streaming refinements

GraphQL fields under `type Subscription { ... }` are tagged with `StreamingMode::ServerStream`. Proto `rpc Method (stream X) returns (stream Y)` becomes `BidiStream`; the four combinations (`Unary` / `ServerStream` / `ClientStream` / `BidiStream`) map directly. Emitted client methods include a streaming-mode comment and a runtime `anyhow::bail!` stub so callers know to wire `tonic`'s server-streaming or a GraphQL subscription client before relying on them. `SKILL.md` notes the streaming mode per command.

## Build & run

```bash
# everything
cargo build
cargo test                       # 147 tests across the workspace

# kernel demo
cargo run -p oxide-k

# WASM compress build
cargo build -p oxide-compress --target wasm32-unknown-unknown --release
wasm-pack build crates/oxide-compress --target nodejs --release

# generate from a spec
cargo run -p oxide-gen -- --spec <SPEC> --output <DIR> [--kind <K>] [--name <CRATE>]
```

## License

MIT OR Apache-2.0
