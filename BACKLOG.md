# Rust Oxide — Open Backlog

Pending roadmap items not yet implemented. See [`docs/ROADMAP.md`](./docs/ROADMAP.md) for full context, DoD, and sequencing.

Items are ordered by phase (ascending). Each group can be tackled once its phase prerequisites are met.

---

## Phase 2 — Generated client maturity

Sequential: R-12 → R-13 → R-14.

| Id | Item | Effort | Why blocked |
|----|------|--------|-------------|
| R-12 | `tonic-build` invocation during oxide-gen gRPC generation | M | None — ready to start |
| R-13 | Real tonic-wired gRPC dispatch in generated client methods | M | Needs R-12 |
| R-14 | GraphQL subscription streaming runtime (`graphql-ws` over WS) | M | Needs R-13 |

**Crate affected:** `oxide-gen` only.

---

## Phase 3 — Persistence + live data

Parallelizable: R-15 ∥ R-16 ∥ R-17.

| Id | Item | Effort | Crate |
|----|------|--------|-------|
| R-15 | On-disk `PersistentGraph` (SQLite, `persist` feature) | M | `oxide-graph` |
| R-16 | Mirror schema evolution — `MirrorStore::migrate_to(version)` | M | `oxide-mirror` |
| R-17 | Live source subscriptions — `SyncSource::subscribe() -> Stream<Delta>` | M | `oxide-mirror` |

---

## Phase 4 — Security hardening

R-18 before R-19. R-20 and R-21 are independent.

| Id | Item | Effort | Crate |
|----|------|--------|-------|
| R-18 | Encrypted-at-rest kernel registry (SQLCipher + OS keyring) | M | `oxide-k` |
| R-19 | Capability-based bus ACL | L | `oxide-k` |
| R-20 | TLS / mTLS for `TcpMesh` | M | `oxide-mesh` |
| R-21 | Bus over TLS for cross-process kernels | M | `oxide-k`, `oxide-mesh` |

---

## Phase 5 — Real WASM ABI + browser depth

| Id | Item | Effort | Crate |
|----|------|--------|-------|
| R-22 | Full WASM host ABI (JSON in/out via linear memory) | L | `oxide-k`, `oxide-compress` |
| R-23 | iframe drilling + complex SPA flows | M | `oxide-browser-sh` |

---

## Phase 6 — Adapters (parallel)

| Id | Item | Effort | Crate |
|----|------|--------|-------|
| R-24 | Neo4j `GraphStore` adapter | L | `oxide-graph` |
| R-25 | libp2p transport for `oxide-mesh` | L | `oxide-mesh` |

---

## Phase 7 — Federated learning

| Id | Item | Effort | Crate |
|----|------|--------|-------|
| R-26 | New crate `oxide-fed` — federated learning with CRDT weight merge | XL | new crate |

---

## Manual / repo-settings tasks

| Id | Item | Who |
|----|------|-----|
| R-01 | Cut `v0.1.0` tag and confirm release.yml green | maintainer |
| R-03 | Enable GitHub Code Scanning + Secret Scanning in repo settings | maintainer |
| —   | Set `LIB_DEPS_PAT` secret in `rust-oxide-lib` repo for CI | maintainer |

---

## Release gates

| Tag | Prerequisite phases closed |
|-----|---------------------------|
| `v0.2.0` | Phase 1 fully closed (R-11 ✅ closes it) |
| `v0.3.0` | Phase 2 closed |
| `v0.4.0` | Phase 3 closed |
| `v0.5.0` | Phase 4 closed |
| `v0.6.0` | Phase 5 closed |
| `v0.7.0` | Phase 6 closed |
| `v1.0.0` | Phase 7 closed |

---

*Last updated: 2026-05-20. Cross-reference with [`docs/ROADMAP.md`](./docs/ROADMAP.md) checklist.*
