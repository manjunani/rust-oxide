# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- CI workflow (`.github/workflows/ci.yml`): fmt, clippy, check, test, WASM build.
- Release workflow (`.github/workflows/release.yml`): tag-triggered cross-platform binary builds + crates.io publish.
- `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, dual MIT/Apache-2.0 license files.
- `oxide-k`: `wasmtime`-backed `WasmExecutor` (feature `wasmtime-runtime`) for executing real `.wasm` plugins.
- `oxide-browser-sh`: real CDP `Accessibility.getFullAXTree` ingestion path; synthesised tree retained as fallback.
- `oxide-mesh`: CRDT primitives (`GSet`, `LwwRegister`) for eventual-consistency federation.
- `oxide-graph`: Neo4j adapter stub (feature `neo4j`) sketching the `GraphStore` trait against a Bolt-protocol client.
- `oxide-mesh`: libp2p adapter stub (feature `libp2p`) sketching a swarm-backed transport.

### Changed
- `oxide-gen`: GraphQL `Subscription` and gRPC `stream` requests/responses now propagate through `StreamingMode` enum into the emitted Rust client.

## [0.1.0] — bootstrap series

Initial seven-phase bootstrap. See commit history `c63be9e..1abfba1` for the per-phase split.
