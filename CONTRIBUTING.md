# Contributing to Rust Oxide

Thanks for your interest in pushing Rust Oxide forward. This document covers the contribution workflow, expectations, and the local checks every PR is expected to pass.

## Code of Conduct

By participating in this project you agree to abide by the [Code of Conduct](./CODE_OF_CONDUCT.md).

## Toolchain

* **Rust**: stable (pinned via `rust-toolchain.toml`, currently `>= 1.85` for edition 2024 transitive deps).
* **wasm-pack** *(optional, for `oxide-compress`)*: install via `curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh`.
* **Chromium / Chrome** *(optional, for `oxide-browser-sh` live tests)*: any modern build.

## Workspace layout

See the top-level `README.md` for the per-crate breakdown. Quick map:

```
crates/
├── oxide-k                  micro-kernel
├── oxide-compress           token compression (WASM target)
├── oxide-gen                spec-to-crate generator
├── oxide-browser-sh         self-healing browser
├── oxide-mirror             local data mirror
├── oxide-llm-orchestrator   LLM front-door
├── oxide-mcp-server         MCP stdio server
├── oxide-graph              knowledge graph
└── oxide-mesh               inter-agent comms
```

## Development loop

```bash
# Run from the repo root.
cargo fmt --all                                       # format
cargo clippy --workspace --all-targets -- -D warnings # lint
cargo test --workspace                                # all tests
```

For WASM:

```bash
rustup target add wasm32-unknown-unknown
cargo build -p oxide-compress --target wasm32-unknown-unknown --release
# Or via wasm-pack:
wasm-pack build crates/oxide-compress --target web --release
```

For real-browser tests:

```bash
cargo test -p oxide-browser-sh --features live-browser -- --ignored
```

## Commit + PR guidelines

* **Commits**: imperative mood, ≤ 70 chars subject, body explains *why* not *what*.
* **Branches**: short kebab-case (`feat-graph-traverse`, `fix-cli-bug`).
* **PRs**: link any related issue, include a summary of changes and a `Test plan` section listing what you ran locally.
* Every PR must pass the [`ci.yml`](./.github/workflows/ci.yml) workflow (check / test / clippy / fmt / wasm).
* **Tests are not optional.** New features ship with unit + (where useful) integration coverage. Tests run sub-second; there is no excuse to skip them.
* **No `unsafe`** without a written justification in the PR description.
* **No new external network dependencies** in tests — use mocks (the `MockLlmClient`, `MockBackend`, `StaticSource` patterns are there for a reason).

## Adding a new crate

1. Create `crates/<name>/` with `Cargo.toml` + `src/lib.rs`.
2. Add the path to the workspace `members` array.
3. Use `workspace = true` for shared dependencies (see `crates/oxide-k/Cargo.toml` for the pattern).
4. Add the crate to the table in `README.md`.

## Releasing

Releases are tag-driven via [`release.yml`](./.github/workflows/release.yml). To cut a release:

1. Bump the `version` field in the workspace `Cargo.toml`.
2. Update `CHANGELOG.md` (create one if missing).
3. `git tag vX.Y.Z` + `git push --tags`.
4. The release workflow builds cross-platform binaries, uploads them to the GitHub Release, and publishes every crate to `crates.io` (in dependency order).

## Security

Report security issues privately to the maintainers — do not file public issues. See `SECURITY.md` if present, otherwise email the repository owner directly.
