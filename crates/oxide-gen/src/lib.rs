//! # `oxide-gen` — Spec-to-Crate Generator
//!
//! `oxide-gen` ingests an API specification (OpenAPI 3.x, GraphQL SDL, or a
//! `.proto` file) and produces a self-contained Rust crate consisting of:
//!
//! * Type-safe Rust structs / enums for the spec's data models.
//! * A `reqwest`-based asynchronous client (for OpenAPI and GraphQL) or a
//!   `tonic`-shaped scaffold (for gRPC) with one method per operation.
//! * A `clap` CLI that exposes every operation as a subcommand.
//! * A `SKILL.md` file with YAML frontmatter for Claude Code.
//! * An `mcp.json` file describing the CLI as MCP-callable tools.
//! * A `module.json` manifest that the [`oxide_k`] kernel can discover and
//!   register as a module.
//!
//! The crate exposes three layers:
//!
//! * [`parsers`] — spec format ↔ [`ir::ApiSpec`].
//! * [`emit`] — [`ir::ApiSpec`] → on-disk artifacts.
//! * [`generate_from_path`] — convenience entry point that auto-detects the
//!   spec format from the file extension and runs the full pipeline.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

use std::path::Path;

pub mod emit;
pub mod error;
pub mod ir;
pub mod parsers;

pub use error::{GenError, Result};
pub use ir::{ApiKind, ApiSpec};

/// Top-level entry point: parse the spec at `spec_path`, optionally
/// overriding the auto-detected kind, then emit a complete generated crate
/// into `output_dir`.
///
/// `crate_name` overrides the snake_case crate name; if `None`, the parser's
/// inferred name is kept.
pub fn generate_from_path(
    spec_path: &Path,
    kind: Option<ApiKind>,
    output_dir: &Path,
    crate_name: Option<&str>,
) -> Result<emit::EmitReport> {
    let kind = kind
        .or_else(|| ApiKind::infer_from_path(spec_path))
        .ok_or_else(|| GenError::Parse {
            kind: "auto-detect",
            message: format!(
                "could not infer API kind from {:?}; pass --kind explicitly",
                spec_path
            ),
        })?;

    let raw = std::fs::read_to_string(spec_path).map_err(|source| GenError::ReadSpec {
        path: spec_path.to_path_buf(),
        source,
    })?;

    let mut spec = match kind {
        ApiKind::OpenApi => parsers::openapi::parse(&raw)?,
        ApiKind::GraphQl => parsers::graphql::parse(&raw)?,
        ApiKind::Grpc => parsers::proto::parse(&raw)?,
    };

    if let Some(name) = crate_name {
        spec.name = name.to_string();
    }

    emit::emit_crate(&spec, output_dir)
}

impl ApiKind {
    /// Infer the API kind from a spec file's extension.
    pub fn infer_from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "yaml" | "yml" | "json" => ApiKind::OpenApi,
            "graphql" | "graphqls" | "gql" => ApiKind::GraphQl,
            "proto" => ApiKind::Grpc,
            _ => return None,
        })
    }
}
