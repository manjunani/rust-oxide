# Rust Oxide

**The Agent-Native OS** — a high-performance Rust toolchain that gives AI agents a unified, intelligent, and secure interface to structured APIs (OpenAPI, GraphQL, gRPC) and unstructured web content.

## Status

Bootstrap phases shipped:

| Phase | Crate | What it does |
|-------|-------|--------------|
| 1 | `oxide-k` | Micro-kernel: module orchestration, message bus, state registry, manifest discovery |
| 2 | `oxide-compress` | WASM-bound token compression: field selection, metadata stripping, semantic chunking |
| 3 | `oxide-gen` | Spec-to-crate generator for OpenAPI / GraphQL / gRPC |

Still to come: `oxide-browser-sh`, `oxide-mirror`, `oxide-llm-orchestrator`, wasmtime integration for the `WasmModule` trait, and tonic-based gRPC dispatch in generated crates.

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
│   └── oxide-gen/               # spec-to-crate generator
│       ├── src/{lib, main, ir, error}.rs
│       ├── src/parsers/{openapi, graphql, proto, naming}.rs
│       ├── src/emit/{cargo, rust_lib, rust_cli, skill, mcp, manifest}.rs
│       └── tests/{integration, fixtures/}
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

## Build & run

```bash
# everything
cargo build
cargo test                       # 57 tests across the workspace

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
