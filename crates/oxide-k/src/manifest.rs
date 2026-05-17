//! # Module manifests
//!
//! `module.json` is the discovery file produced by `oxide-gen` for every
//! generated crate. It mirrors [`ModuleMetadata`] and adds the fields the
//! kernel needs to locate the generated binary, skill descriptor, and MCP
//! configuration.
//!
//! The [`Kernel::register_module_from_manifest`](crate::Kernel::register_module_from_manifest)
//! helper reads a manifest file and records the corresponding module in the
//! state registry, putting it into the `Loaded` lifecycle state. Actually
//! *running* the generated binary as a child process is out of scope for the
//! current bootstrap and will be wired up alongside a sandboxed process
//! supervisor in a later iteration.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{KernelError, Result};
use crate::module::{ModuleKind, ModuleMetadata, ModuleState};
use crate::registry::StateRegistry;
use crate::Kernel;

/// The on-disk shape of a `module.json` file emitted by `oxide-gen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleManifest {
    /// Stable module id (snake_case).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Semantic version.
    pub version: String,
    /// Module kind / execution layer.
    pub kind: ModuleKind,
    /// Free-form description.
    pub description: Option<String>,
    /// Source spec format (`openapi`, `graphql`, `grpc`).
    pub spec_kind: Option<String>,
    /// CLI binary name (relative to the crate root or on `$PATH`).
    pub binary: Option<String>,
    /// Path (relative) to the Claude Code skill descriptor.
    pub skill: Option<String>,
    /// Path (relative) to the MCP server configuration.
    pub mcp: Option<String>,
    /// Default base URL baked into the generated client.
    #[serde(default)]
    pub base_url: Option<String>,
    /// List of operation ids exposed by the module.
    #[serde(default)]
    pub operations: Vec<String>,
}

impl ModuleManifest {
    /// Parse a manifest from raw JSON.
    pub fn from_json(raw: &str) -> Result<Self> {
        Ok(serde_json::from_str(raw)?)
    }

    /// Load a manifest from disk. Accepts either the manifest file directly
    /// or a directory containing `module.json`.
    pub fn load(path: &Path) -> Result<Self> {
        let manifest_path = if path.is_dir() {
            path.join("module.json")
        } else {
            path.to_path_buf()
        };
        let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
            KernelError::Other(anyhow::anyhow!(
                "failed to read manifest {}: {e}",
                manifest_path.display()
            ))
        })?;
        Self::from_json(&raw)
    }

    /// Derive the [`ModuleMetadata`] the kernel uses for registry storage.
    pub fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            kind: self.kind,
            description: self.description.clone(),
        }
    }
}

/// Resolve the on-disk paths referenced by a manifest, anchored at
/// `base_dir`.
#[derive(Debug, Clone)]
pub struct ResolvedManifest {
    /// The parsed manifest.
    pub manifest: ModuleManifest,
    /// Absolute path to the binary, if specified.
    pub binary_path: Option<PathBuf>,
    /// Absolute path to the SKILL.md file, if specified.
    pub skill_path: Option<PathBuf>,
    /// Absolute path to the MCP config, if specified.
    pub mcp_path: Option<PathBuf>,
}

impl ResolvedManifest {
    /// Resolve every path field against `base_dir`.
    pub fn resolve(manifest: ModuleManifest, base_dir: &Path) -> Self {
        let join = |relative: &Option<String>| relative.as_deref().map(|s| base_dir.join(s));
        Self {
            binary_path: join(&manifest.binary),
            skill_path: join(&manifest.skill),
            mcp_path: join(&manifest.mcp),
            manifest,
        }
    }
}

impl Kernel {
    /// Register a module described by a `module.json` manifest with the
    /// kernel's state registry.
    ///
    /// `path` may be the manifest file or its parent directory. The module is
    /// recorded in [`ModuleState::Loaded`]. Starting the underlying binary is
    /// the responsibility of a future process supervisor.
    pub async fn register_module_from_manifest(&self, path: &Path) -> Result<ResolvedManifest> {
        let manifest = ModuleManifest::load(path)?;
        let base_dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        };
        let resolved = ResolvedManifest::resolve(manifest, &base_dir);
        let metadata = resolved.manifest.metadata();
        self.registry()
            .upsert_module(&metadata, ModuleState::Loaded)
            .await?;
        tracing::info!(
            id = %metadata.id,
            kind = ?metadata.kind,
            "registered module from manifest"
        );
        Ok(resolved)
    }
}

/// Convenience for callers that already own a [`StateRegistry`] but no full
/// [`Kernel`] (e.g. CLI tools, tests).
pub async fn register_in_registry(
    registry: &StateRegistry,
    manifest: &ModuleManifest,
) -> Result<()> {
    registry
        .upsert_module(&manifest.metadata(), ModuleState::Loaded)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
    {
      "id": "petstore",
      "name": "Pet Store",
      "version": "1.0.0",
      "kind": "native",
      "description": "Generated client for the Pet Store API.",
      "spec_kind": "openapi",
      "binary": "petstore-cli",
      "skill": "SKILL.md",
      "mcp": "mcp.json",
      "base_url": "https://petstore.example.com/v1",
      "operations": ["list_pets", "create_pet", "get_pet"]
    }"#;

    #[test]
    fn parses_sample_manifest() {
        let m = ModuleManifest::from_json(SAMPLE).unwrap();
        assert_eq!(m.id, "petstore");
        assert_eq!(m.kind, ModuleKind::Native);
        assert_eq!(m.operations.len(), 3);
    }

    #[tokio::test]
    async fn registers_manifest_in_registry() {
        let kernel = Kernel::in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("module.json");
        std::fs::write(&manifest_path, SAMPLE).unwrap();

        let resolved = kernel
            .register_module_from_manifest(&manifest_path)
            .await
            .unwrap();
        assert_eq!(resolved.manifest.id, "petstore");
        assert_eq!(
            resolved.binary_path.as_ref().unwrap(),
            &dir.path().join("petstore-cli")
        );

        let rec = kernel
            .registry()
            .get_module("petstore")
            .await
            .unwrap()
            .expect("record");
        assert_eq!(rec.state, ModuleState::Loaded);
        assert_eq!(rec.version, "1.0.0");
    }
}
