# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — backlog burn-down (roadmap items R-02, R-04..R-10)

- **R-02**: crate-name availability verified — every `oxide-*` workspace
  crate is unclaimed on crates.io; no renames needed.
- **R-04**: `SECURITY.md` now points at GitHub's private vulnerability
  reporting flow with a documented SLA (72h ack → 7d triage → 30d patch).
- **R-05**: `oxide-browser-sh` gains a `chromium` feature flag.
  `chromiumoxide` (+ `futures`) are now optional dependencies; default
  builds drop the full CDP / async-tungstenite tree.
- **R-06**: `LlmClient::complete_stream` for token streaming.
  `OpenAiClient` implements real SSE decoding (`data: {…}\n\n` chunked
  events, JSON `delta.content` extraction). Default trait impl synthesises
  a single-item stream over `complete()` for backends that don't natively
  stream.
- **R-07**: tool-use passthrough. `ChatRequest.tools: Vec<ToolSpec>` flows
  into `tools[].function` payloads; `ChatResponse.tool_calls` carries the
  model's function-call requests back. Mock and `OpenAiClient` both
  honour the round trip.
- **R-08**: `BudgetGuard` middleware. Enforces three orthogonal caps —
  dollars (via per-model `Pricing` table), tokens (cumulative), and RPM
  (sliding 60s window). Pre-flight rejection emits a clear
  `LlmError::Other("budget exceeded …")` and the rejected counter ticks
  even when the inner call would have succeeded.
- **R-09**: real XPath selector support in `ChromiumBackend`. `Selector::XPath`
  now resolves via an in-page `document.evaluate` shim that walks
  ancestors to construct a unique CSS path; the rest of the click /
  input / scroll machinery is unchanged.
- **R-10**: `ToolRegistry::load_dir(path)` for MCP auto-discovery. Walks
  a tree (skipping `target/`, `node_modules/`, etc.), parses every
  `mcp.json` file emitted by `oxide-gen`, registers each declared tool
  as a `CliTool` resolved against the sibling binary
  (`target/release/<bin>` → `target/debug/<bin>` → `$PATH`).

Plus the pre-existing items from the previous bootstrap:

- CI workflow (`.github/workflows/ci.yml`): fmt, clippy, check, test, WASM build.
- Release workflow (`.github/workflows/release.yml`): tag-triggered cross-platform binary builds + crates.io publish.
- `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, dual MIT/Apache-2.0 license files.
- `oxide-k`: `wasmtime`-backed `WasmExecutor` (feature `wasmtime-runtime`) for executing real `.wasm` plugins.
- `oxide-browser-sh`: real CDP `Accessibility.getFullAXTree` ingestion path; synthesised tree retained as fallback.
- `oxide-mesh`: CRDT primitives (`GSet`, `LwwRegister`, `PnCounter`) for eventual-consistency federation.

### Changed
- `oxide-gen`: GraphQL `Subscription` and gRPC `stream` requests/responses now propagate through `StreamingMode` enum into the emitted Rust client.
- Workspace `reqwest` feature set widened to include `stream` (needed by
  `OpenAiClient::complete_stream`).

### Verification

- `cargo test --workspace`: **155/155 pass** across 22 suites (was 150 prior
  to R-06/07/08/09/10).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.

### Roadmap

See [`docs/ROADMAP.md`](./docs/ROADMAP.md). Phase 0 (R-01..R-04) is fully
closed once `v0.1.0` is tagged (R-01) and GitHub Code/Secret Scanning is
enabled in the repo settings (R-03, manual step). Phase 1 (R-05..R-11) is
closed except for R-11 (SSE/WebSocket MCP transports), which remains
pending — see ROADMAP for sequencing.

## [0.1.0] — bootstrap series

Initial seven-phase bootstrap. See commit history `c63be9e..1abfba1` for the per-phase split.
