# Rust Oxide

**The Agent-Native OS** — a high-performance Rust toolchain that gives AI agents a unified, intelligent, and secure interface to structured APIs (OpenAPI, GraphQL, gRPC) and unstructured web content.

## Status

Early bootstrap. The `oxide-k` micro-kernel is in place; other modules (`oxide-gen`, `oxide-browser-sh`, `oxide-mirror`, `oxide-compress`) are planned.

## Workspace layout

```
rust-oxide/
├── Cargo.toml            # workspace manifest
├── crates/
│   └── oxide-k/          # Oxide Kernel (this commit)
│       ├── src/lib.rs
│       ├── src/main.rs   # demo binary
│       ├── src/kernel.rs
│       ├── src/module.rs
│       ├── src/bus.rs
│       ├── src/registry.rs
│       └── src/error.rs
└── README.md
```

## `oxide-k` — the micro-kernel

Three cooperating subsystems:

| Subsystem | File | Responsibility |
|-----------|------|----------------|
| Module orchestration | `module.rs` | `Module` and `WasmModule` traits, `ModuleManager` lifecycle |
| Secure message bus | `bus.rs` | `tokio::sync::mpsc` fan-out, `Command`/`Event` envelopes |
| Global state registry | `registry.rs` | `sqlx` + SQLite (in-memory by default) for module metadata and config |

All three are tied together by the `Kernel` façade (`kernel.rs`).

## Build & run

```bash
cargo build
cargo test
cargo run -p oxide-k
```

## License

MIT OR Apache-2.0
