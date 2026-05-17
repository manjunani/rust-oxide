# How to Use Rust Oxide

A practical, no-marketing tour of what each crate does, how to drive it, and what it deliberately does **not** do. If `README.md` is the spec, this is the operator's manual.

---

## 1. What it is, in one paragraph

Rust Oxide is a set of nine cooperating Rust crates that give an AI agent a Unix-like toolbox for the digital world: turn API specs into typed clients, drive real or simulated browsers with self-healing, mirror remote data into local SQLite for cross-service SQL, talk to LLMs, link facts into a knowledge graph, gossip between peers, and expose any of it to MCP-compatible runtimes. There is no daemon and no GUI. You compose the pieces in Rust code (or drive the generated CLIs from any language).

---

## 2. What works today

| Crate | You can | You cannot (yet) |
|-------|---------|------------------|
| `oxide-k` | Boot a kernel, register modules, send `Command`/`Event` envelopes on a bus, store config + module records in SQLite, append XAI decisions, execute `.wasm` (`wasmtime-runtime` feature) | Capability-based access control, encrypted-at-rest registry, on-disk WASM sandbox cache |
| `oxide-compress` | Prune JSON fields, strip HTML/boilerplate, chunk text — both native Rust and WebAssembly (`wasm-pack build --target web`) | Auto-detect "irrelevant" content beyond the static phrase list |
| `oxide-gen` | Take an OpenAPI 3.x YAML/JSON, a GraphQL SDL, or a `.proto` file → emit a self-contained Rust crate with types, `reqwest` client, `clap` CLI, `SKILL.md`, `mcp.json`, `module.json` | gRPC method bodies are scaffolds that `anyhow::bail!` at runtime (no tonic wiring yet); GraphQL `Subscription` and gRPC `stream` operations are tagged but their bodies also bail; complex schemas (`oneOf`/`anyOf`, deep refs) fall back to `serde_json::Value` |
| `oxide-browser-sh` | Navigate, click, type, scroll, screenshot, extract Markdown — using real headless Chromium **or** a scraper-backed `MockBackend`. Resolve `Selector::Role` / `Selector::Text` via CDP `Accessibility.getFullAXTree` (with HTML synthesis fallback). Self-heal via rules or via an LLM | Drive iframes deeply (basic synthesis only), fill complex SPA flows that depend on timing, run XPath selectors (returns `Unsupported`) |
| `oxide-mirror` | Pull deltas from anything implementing `SyncSource`, persist them to SQLite with full provenance, run **read-only** SQL across all mirrored tables, resolve conflicts with `LastWriteWins` / `HighestConfidence` / `KeepLocal` / `MergeJson` | Schema-evolve mirrored tables without re-ingest; trigger live subscriptions back to sources |
| `oxide-llm-orchestrator` | Call any OpenAI-compatible `/v1/chat/completions` endpoint (OpenAI, OpenRouter, Ollama, vLLM, …); summarise text; analyse errors; act as a healing strategy for the browser | Stream tokens (returns the full completion), call tool-use APIs natively, manage cost / rate-limit budgets |
| `oxide-mcp-server` | Speak MCP JSON-RPC over stdio. Expose subprocess CLIs (`CliTool`) and kernel bus methods (`BusTool`) as MCP tools | Auto-load tools from generated `mcp.json` files (the registry must be built programmatically today); SSE / WebSocket transports |
| `oxide-graph` | Build an in-memory knowledge graph of typed nodes + labelled edges; ingest mirrored records (foreign-key strings auto-become edges); query by label + property; traverse BFS | Persist to disk; back onto Neo4j / Dgraph (trait seam is there, adapter is future work) |
| `oxide-mesh` | Spin up an in-process peer fabric over `tokio::mpsc` or a cross-host JSON-line TCP transport. Send `Hello` / `Broadcast` / `Direct` / `Task` / `Result` messages. Maintain `GSet`, `LwwRegister`, `PnCounter` CRDTs | Auth peers, encrypt traffic (TLS), route over libp2p / QUIC, federate actual ML weights |

There are **150 tests** that exercise every crate. If a feature has no test it is not in the table above.

---

## 3. The first 10 minutes

```bash
git clone https://github.com/manjunani/rust-oxide
cd rust-oxide
cargo test         # ~30s, expect: 150 pass across 22 suites
cargo run -p oxide-k   # boots a kernel demo, prints lifecycle events
```

That's it. Nothing depends on Chrome, an LLM key, or a network — every test backend is in-process.

---

## 4. Concrete recipes

### A. Generate a typed client from an OpenAPI spec

```bash
cargo run -p oxide-gen -- \
    --spec crates/oxide-gen/tests/fixtures/petstore.yaml \
    --output ./generated/petstore
```

You get a working crate:

```
generated/petstore/
├── Cargo.toml
├── module.json     # oxide-k discovery manifest
├── SKILL.md        # Claude Code skill descriptor (YAML frontmatter)
├── mcp.json        # MCP tool descriptor
└── src/
    ├── lib.rs      # Pet struct, PetStatus enum, async Client
    └── main.rs     # pet-store-cli with subcommands list-pets / get-pet / create-pet
```

Build + run it:

```bash
cd generated/petstore
cargo run --bin pet-store-cli -- list-pets --limit 5
cargo run --bin pet-store-cli -- get-pet --pet-id 42
```

Override the base URL via `--base-url https://my.api/` or the `OXIDE_BASE_URL` environment variable. Use `--kind graphql` or `--kind grpc` for the other spec formats.

> **Gotcha:** the gRPC generated body returns `anyhow::bail!("gRPC method ... not yet wired")`. Treat it as a scaffold and wire `tonic` in by hand for now.

### B. Drive a website (mock, no Chrome required)

```rust
use std::sync::Arc;
use oxide_browser_sh::{BrowserSession, MockBackend, Selector};

let backend = Arc::new(MockBackend::new());
backend.register_page(
    "https://shop.example/",
    r#"<button id="cta" class="primary">Buy now</button>"#,
);
let session = BrowserSession::with_default_healing(backend);
session.navigate("https://shop.example/").await?;
session
    .click(&Selector::role_named("button", "Buy now"))
    .await?;
println!("{:?}", session.stats());
```

For a real browser, swap `MockBackend` for `oxide_browser_sh::chromium::ChromiumBackend::launch().await?`. Chrome / Chromium must be on `$PATH`.

### C. Mirror a remote API and query it locally

```rust
use std::sync::Arc;
use oxide_mirror::{Delta, MirrorStore, StaticSource, Syncer};

let store = MirrorStore::in_memory().await?;
let source = Arc::new(StaticSource::from_deltas(
    "petstore",
    vec![
        Delta::upsert("pets", "1", serde_json::json!({"name": "Rex"}), "petstore"),
        Delta::upsert("pets", "2", serde_json::json!({"name": "Buddy"}), "petstore"),
    ],
));
let mut syncer = Syncer::new(store.clone());
syncer.register_source(source);

let report = syncer.sync_source("petstore").await?;
println!("pulled {} applied {}", report.pulled, report.applied);

let rows = store
    .query("SELECT record_id, payload FROM mirror_records WHERE resource = 'pets'")
    .await?;
for row in rows {
    println!("{:?}", row);
}
```

Replace `StaticSource` with anything that implements `SyncSource` — typically the `Client` you got out of `oxide-gen`, wrapped to translate `list_pets()` into a `Delta` batch.

### D. Ask an LLM (no key required for tests)

```rust
use std::sync::Arc;
use oxide_llm_orchestrator::{
    ChatRequest, LlmClient, MockLlmClient, OpenAiClient, Summarizer,
};

// Test / offline:
let client: Arc<dyn LlmClient> = Arc::new(MockLlmClient::single("short."));

// Production: any OpenAI-compatible endpoint
// let client: Arc<dyn LlmClient> = Arc::new(
//     OpenAiClient::new("https://api.openai.com", Some(std::env::var("OPENAI_API_KEY")?), "gpt-4o-mini")?
// );

let summarizer = Summarizer::new(client.clone(), "gpt-4o-mini");
let summary = summarizer
    .summarize("a very long article about ...", 60, None)
    .await?;
println!("{summary}");
```

### E. Expose tools to Claude Code via MCP

```rust
use std::sync::Arc;
use oxide_mcp_server::{CliTool, McpServer, ToolDescriptor, ToolInputSchema, ToolRegistry};

let descriptor = ToolDescriptor {
    name: "list-pets".into(),
    description: "List all pets in the petstore".into(),
    input_schema: ToolInputSchema::empty(),
};
let mut registry = ToolRegistry::new();
registry.register(Arc::new(CliTool::new(
    descriptor,
    "./target/debug/pet-store-cli",
    vec!["list-pets".into()],
)));

let server = McpServer::new(registry);
let stdin = tokio::io::BufReader::new(tokio::io::stdin());
let stdout = tokio::io::stdout();
server.run_io(stdin, stdout).await?;
```

Add it to Claude Code's `mcp.json`:

```json
{
  "servers": {
    "rust-oxide": {
      "command": "/path/to/rust-oxide/target/debug/oxide-mcp-server-launcher",
      "transport": "stdio"
    }
  }
}
```

(The shipped `oxide-mcp-server` binary starts with an empty registry; embed it in a small launcher binary that registers your tools first. Auto-discovery from generated `mcp.json` files is on the backlog.)

### F. Knowledge graph from mirror records

```rust
use oxide_graph::{ingest_record, InMemoryGraph, NodeQuery, RecordRef};
use serde_json::json;

let g = InMemoryGraph::new();
for rec in store.list_records("pets").await? {
    ingest_record(&g, RecordRef {
        resource: &rec.resource,
        record_id: &rec.record_id,
        payload: &rec.payload,
        source: &rec.source,
    }).await?;
}

let available = NodeQuery::label("pets")
    .property_eq("status", json!("available"))
    .run(&g)
    .await?;
println!("{} pets available", available.len());
```

### G. Multi-agent fabric (in process)

```rust
use oxide_mesh::{LocalMesh, PeerCapability, PeerMessage};
use serde_json::json;

let mesh = LocalMesh::new();
let (mut peer_a, handle_a) = mesh.join("alpha", vec![PeerCapability { name: "browser".into(), version: None }], vec![]).await?;
let (mut peer_b, _handle_b) = mesh.join("beta", vec![], vec![]).await?;

handle_a.publish(PeerMessage::broadcast("alpha", "events", json!({"hello": 1}))).await?;
let received = peer_b.receiver.recv().await.unwrap();
```

For cross-host, swap `LocalMesh::new()` for `TcpMesh::serve(addr)` on the listener side and `TcpMesh::connect(addr, hello)` on the client side. Same wire format.

---

## 5. What it deliberately does not do

* **It is not a runtime / daemon.** Nothing listens by default. You build a Rust binary, you call the APIs.
* **It does not provide a web UI.** All output is JSON, Markdown, or stdout.
* **It does not ship a managed LLM.** Bring your own OpenAI-compatible endpoint.
* **It does not enforce auth/authz.** The kernel bus, the mesh, and the MCP server all assume a trusted local context. Add TLS / mTLS / token checks at your transport.
* **It does not promise zero-downtime upgrades**, hot module reload, or graceful crash recovery — modules are registered programmatically and restart by re-running the host binary.
* **It is not yet versioned for crates.io.** `0.1.0` is bootstrap quality. APIs will move.

---

## 6. Mental model

```
                    ┌────────────────────────────┐
                    │  Your binary (you write)   │
                    └──────────────┬─────────────┘
                                   │
                                   ▼
   ┌───────────────────────  oxide-k  ───────────────────────┐
   │  MessageBus  ·  ModuleManager  ·  StateRegistry         │
   │  ModuleManifest discovery  ·  XAI decision log          │
   │  wasm_exec (feature-gated)                              │
   └─────────────┬─────────────────────┬─────────────────────┘
                 │                     │
   ┌─────────────┴───────┐ ┌───────────┴────────────┐
   │ Module trait impls  │ │ External transports    │
   │ • oxide-browser-sh  │ │ • oxide-mcp-server     │
   │ • oxide-mirror      │ │ • oxide-mesh (TCP/mpsc)│
   │ • oxide-llm-orch.   │ │ • oxide-gen CLIs       │
   │ • oxide-graph       │ └────────────────────────┘
   └─────────────────────┘
              │
              ▼
   oxide-compress (WASM) — token reducer plumbed where useful
```

Everything is async-first, `tokio` everywhere, no globals.

---

## 7. When you hit a wall

* **Tests fail** — `cargo test --workspace -- --nocapture` shows stdout/stderr. The failing assertion usually points at a wire-format change.
* **Generated client won't compile** — your spec triggered a fallback to `serde_json::Value`. Open the emitted `src/lib.rs`, replace the alias with the concrete struct you want, and re-run.
* **Browser session loops** — check `session.stats().history` for the healing trail. The `LlmStubHealing` and the real `LlmHealing` both log the prompt they would send.
* **MCP client can't see tools** — confirm logs go to stderr (default) so they don't pollute the JSON-RPC channel on stdout.
* **Mirror query returns `Null`** — your column is computed (`COUNT(*)`, `1+1`). The store handles this via `try_get_unchecked`; if you wrote a custom adapter, mirror the same pattern.

---

## 8. Where to read next

* Per-crate docs: `cargo doc --workspace --no-deps --open`
* End-to-end examples: each crate's `src/.../kernel.rs` shows the bus integration shape.
* CI workflows: `.github/workflows/ci.yml` is also a runnable spec — every step there should pass locally.
* Tests as documentation: `crates/<x>/src/**/tests` and `crates/oxide-gen/tests/integration.rs` are the authoritative usage examples.
