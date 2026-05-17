# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# build / test
cargo build                          # whole workspace
cargo test                           # all tests (~98 across workspace)
cargo test -p <crate>                # single crate
cargo test -p <crate> <test_name>    # single test

# real-browser tests (require Chrome/Chromium installed)
cargo test -p oxide-browser-sh --features live-browser -- --ignored

# WASM build for oxide-compress
cargo build -p oxide-compress --target wasm32-unknown-unknown --release
wasm-pack build crates/oxide-compress --target nodejs --release

# spec-to-crate generator
cargo run -p oxide-gen -- --spec <SPEC> --output <DIR> [--kind openapi|graphql|grpc] [--name <CRATE>]

# kernel demo binary
cargo run -p oxide-k

# lint
cargo clippy --workspace
cargo fmt --check
```

Rust toolchain: `stable` (see `rust-toolchain.toml`). Min version: 1.75.

## Architecture

"Agent-Native OS" — a kernel + plugin stack where every capability is a `Module` on a message bus.

### Layer model

```
oxide-k          ← micro-kernel (Layer 1): bus, module manager, state registry (SQLite)
oxide-compress   ← WASM plugin  (Layer 2): token compression / HTML stripping
oxide-gen        ← spec-to-crate generator: reads OpenAPI/GraphQL/proto → emits full Rust crate
oxide-browser-sh ← browser automation: AX-first selectors, self-healing, Markdown extraction
oxide-mirror     ← event-sourced data mirror: sync sources → SQLite, conflict strategies, SQL queries
oxide-llm-orchestrator ← LLM front-door: provider-agnostic chat client, healing loop, prompt templates
```

### oxide-k — micro-kernel

`Kernel` (`kernel.rs`) is a cheap-to-clone handle owning three subsystems:
- `MessageBus` (`bus.rs`) — `tokio::sync::mpsc` fan-out; envelopes are `Command::Invoke { module_id, method, payload: serde_json::Value }` and `Event::Custom { kind, payload }`
- `ModuleManager` (`module.rs`) — lifecycle for types implementing the `Module` async trait
- `StateRegistry` (`registry.rs`) — `sqlx` + SQLite (in-memory default, on-disk via `Kernel::with_registry`)

All non-kernel crates expose a `*Module` struct (`kernel.rs` in each crate) that implements `oxide_k::module::Module` and dispatches bus `Command`s to internal methods by string-matching `method`.

### oxide-gen — spec-to-crate pipeline

`spec file → parser → ApiSpec IR (ir.rs) → emitters → output dir`

Three parsers normalize to one IR: `openapiv3` (OpenAPI), `apollo-parser` (GraphQL SDL), hand-rolled proto3 subset. Emitters produce: `Cargo.toml`, `src/lib.rs` (types + async `Client`), `src/main.rs` (clap CLI), `SKILL.md`, `mcp.json`, `module.json` (consumed by `oxide-k`).

Test fixtures live in `crates/oxide-gen/tests/fixtures/` (`petstore.yaml`, `schema.graphql`, `echo.proto`).

### oxide-browser-sh — self-healing browser automation

Selector hierarchy (semantic first): `Selector::Role{role,name}` → `Text` → `Css` → `XPath`.

`BrowserSession` (`session.rs`) wraps every action in `heal_loop`, which calls the `HealingStrategy` trait on failure. `DefaultHealing` (`healing.rs`) does rule-based AX→CSS rewrites; `LlmStubHealing` is the placeholder until `oxide-llm-orchestrator` is wired in. Tests that need Chromium are `#[ignore]` and behind the `live-browser` feature.

`BrowserModule` (`kernel.rs`) routes bus methods: `navigate`, `click`, `input`, `scroll`, `extract`, `stats`.

### oxide-mirror — event-sourced mirror

Four SQLite tables: `mirror_resources`, `mirror_events` (append-only audit log), `mirror_records` (materialised state), `mirror_cursors` (per-source resume points). `Syncer` (`sync.rs`) paginates `SyncSource::pull(cursor)`, applies through a `ConflictStrategy` (`LastWriteWins`, `HighestConfidence`, `KeepLocal`, `MergeJson`), advances cursors. `MirrorStore::query(sql)` accepts only `SELECT`/`WITH`/`PRAGMA`.

`MirrorModule` bus methods: `sync`, `query`, `get_record`, `list_records`, `resources`, `counts`.

### oxide-llm-orchestrator — LLM client

`LlmClient` async trait with `OpenAiClient` (any OpenAI-compatible endpoint) and `MockLlmClient` for offline tests. `LlmHealing` implements `oxide_browser_sh::HealingStrategy`. Prompt templates in `prompts.rs` cover healing, error analysis, and summarisation.

### oxide-compress — dual-target

Builds as native `rlib` and `wasm32-unknown-unknown` cdylib. `wasm-bindgen` bindings are cfg-gated on the wasm target.
