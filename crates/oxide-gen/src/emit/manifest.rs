//! Emit `module.json` — the manifest that the `oxide-k` kernel consumes when
//! discovering generated crates as modules.
//!
//! The shape mirrors [`oxide_k::module::ModuleMetadata`] plus extra fields that
//! help the kernel locate the generated CLI binary and its skill / MCP
//! descriptors.

use serde_json::{json, Value};

use crate::error::Result;
use crate::ir::{ApiKind, ApiSpec};

/// Build the `module.json` contents as pretty-printed JSON.
pub fn render(spec: &ApiSpec) -> Result<String> {
    let id = spec.name.replace('_', "-");
    let bin = format!("{id}-cli");

    let kind = match spec.kind {
        // Generated CLIs are native binaries from the kernel's point of view.
        ApiKind::OpenApi | ApiKind::GraphQl | ApiKind::Grpc => "native",
    };

    let mut manifest: Value = json!({
        "id": id,
        "name": spec.display_name,
        "version": spec.version,
        "kind": kind,
        "description": spec.description.clone().unwrap_or_else(|| spec.display_name.clone()),
        "spec_kind": spec.kind.slug(),
        "binary": bin,
        "skill": "SKILL.md",
        "mcp": "mcp.json",
        "operations": spec.operations.iter().map(|o| o.id.clone()).collect::<Vec<_>>(),
    });

    if let Some(base) = &spec.base_url {
        manifest["base_url"] = json!(base);
    }

    Ok(serde_json::to_string_pretty(&manifest)?)
}
