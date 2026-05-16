# Rust Oxide

**The Agent-Native OS** — a high-performance Rust toolchain that gives AI agents a unified, intelligent, and secure interface to structured APIs (OpenAPI, GraphQL, gRPC) and unstructured web content.

## Status

Early bootstrap. The `oxide-k` micro-kernel and the `oxide-compress` WASM plugin are in place. Other modules (`oxide-gen`, `oxide-browser-sh`, `oxide-mirror`) are planned.

## Workspace layout

```
rust-oxide/
├── Cargo.toml                 # workspace manifest
├── rust-toolchain.toml        # pinned to stable (>= 1.85, edition 2024 capable)
├── crates/
│   ├── oxide-k/               # Layer 1: native micro-kernel
│   │   ├── src/lib.rs
│   │   ├── src/main.rs        # demo binary
│   │   ├── src/kernel.rs
│   │   ├── src/module.rs
│   │   ├── src/bus.rs
│   │   ├── src/registry.rs
│   │   └── src/error.rs
│   └── oxide-compress/        # Layer 2: WASM token-compression plugin
│       ├── Cargo.toml
│       └── src/lib.rs
└── README.md
```

## `oxide-k` — micro-kernel

Three cooperating subsystems:

| Subsystem | File | Responsibility |
|-----------|------|----------------|
| Module orchestration | `module.rs` | `Module` and `WasmModule` traits, `ModuleManager` lifecycle |
| Secure message bus | `bus.rs` | `tokio::sync::mpsc` fan-out, `Command`/`Event` envelopes |
| Global state registry | `registry.rs` | `sqlx` + SQLite (in-memory by default) for module metadata and config |

All three are tied together by the `Kernel` façade (`kernel.rs`).

## `oxide-compress` — token-compression plugin

Token-compression primitives that compile to both native Rust (for testing and in-process embedding) and to WebAssembly (for the kernel's Layer 2 sandbox).

| Function | Purpose |
|----------|---------|
| `select_fields(json_data, fields)` | Prune a JSON document down to a chosen set of top-level fields. Recurses into arrays of objects. |
| `strip_metadata(text)` | Remove HTML tags, decode common entities, strip boilerplate phrases (cookie/privacy/ToS), collapse whitespace. |
| `chunk_text(text, max_len)` | Split text into chunks of at most `max_len` *characters*, preferring paragraph → sentence → word boundaries. Hard-slices oversized words as a last resort. |

The wasm target additionally exposes `selectFields`, `stripMetadata`, and `chunkText` through `wasm-bindgen` for JavaScript callers.

## Build & run

### Native

```bash
cargo build              # everything
cargo test               # 38 tests across both crates
cargo run -p oxide-k     # boot the kernel demo
```

### `oxide-compress` as a WASM module

The library is `crate-type = ["cdylib", "rlib"]`, so it produces a `.wasm` artifact under the `wasm32-unknown-unknown` target.

#### Option A — plain `cargo build`

```bash
rustup target add wasm32-unknown-unknown
cargo build -p oxide-compress --target wasm32-unknown-unknown --release
# Artifact: target/wasm32-unknown-unknown/release/oxide_compress.wasm
```

This is enough for hosts that load raw `.wasm` and bind imports themselves (e.g. an embedded `wasmtime` runtime inside `oxide-k`).

#### Option B — `wasm-pack` (recommended for JavaScript hosts)

[`wasm-pack`](https://rustwasm.github.io/wasm-pack/) bundles the wasm together with auto-generated JavaScript glue and TypeScript types.

```bash
# Install once (https://rustwasm.github.io/wasm-pack/installer/)
curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh

# Build for a Node.js consumer
wasm-pack build crates/oxide-compress --target nodejs --release
#   -> crates/oxide-compress/pkg/oxide_compress.js
#   -> crates/oxide-compress/pkg/oxide_compress_bg.wasm
#   -> crates/oxide-compress/pkg/oxide_compress.d.ts

# Or for a browser bundler (webpack/vite/etc.)
wasm-pack build crates/oxide-compress --target bundler --release

# Or for direct browser <script type="module">
wasm-pack build crates/oxide-compress --target web --release
```

Quick smoke test from Node.js after a `--target nodejs` build:

```js
const { selectFields, stripMetadata, chunkText } = require(
  "./crates/oxide-compress/pkg/oxide_compress.js"
);
console.log(selectFields('{"id":1,"secret":"x"}', ["id"]));   // {"id":1}
console.log(stripMetadata("<p>Hello&nbsp;world</p>"));         // Hello world
console.log(chunkText("Para one.\n\nPara two.", 100));         // ["Para one.", "Para two."]
```

The release build of `oxide_compress.wasm` is ~125 KB; running it through `wasm-opt -Oz` (which `wasm-pack` does automatically when available) trims it further.

## License

MIT OR Apache-2.0
