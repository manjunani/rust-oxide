//! Emitters convert an [`ApiSpec`] into on-disk artifacts.
//!
//! Each sub-module knows how to render exactly one kind of output file. The
//! top-level [`emit_crate`] orchestrator runs them all into a single
//! destination directory.

pub mod cargo;
pub mod manifest;
pub mod mcp;
pub mod rust_cli;
pub mod rust_lib;
pub mod skill;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{GenError, Result};
use crate::ir::ApiSpec;

/// Summary of artifacts produced by [`emit_crate`].
///
/// Returned so callers (CLI, tests, the kernel) know exactly which files were
/// written and where.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmitReport {
    /// The crate root.
    pub crate_dir: PathBuf,
    /// All files written, in the order they were created.
    pub files: Vec<PathBuf>,
}

impl EmitReport {
    /// Convenience: look up a path inside this report by file name.
    pub fn file_named(&self, name: &str) -> Option<&Path> {
        self.files
            .iter()
            .find(|p| p.file_name().and_then(|s| s.to_str()) == Some(name))
            .map(PathBuf::as_path)
    }
}

/// Emit a full generated crate under `output_dir`.
///
/// Layout:
///
/// ```text
/// {output_dir}/
/// ├── Cargo.toml
/// ├── module.json         # oxide-k discovery manifest
/// ├── SKILL.md            # Claude Code skill descriptor
/// ├── mcp.json            # MCP server config
/// └── src/
///     ├── lib.rs          # types + client
///     └── main.rs         # clap CLI
/// ```
pub fn emit_crate(spec: &ApiSpec, output_dir: &Path) -> Result<EmitReport> {
    ensure_dir(output_dir)?;
    let src_dir = output_dir.join("src");
    ensure_dir(&src_dir)?;

    let mut report = EmitReport {
        crate_dir: output_dir.to_path_buf(),
        files: Vec::new(),
    };

    write_file(
        &output_dir.join("Cargo.toml"),
        &cargo::render(spec),
        &mut report,
    )?;
    write_file(
        &src_dir.join("lib.rs"),
        &rust_lib::render(spec),
        &mut report,
    )?;
    write_file(
        &src_dir.join("main.rs"),
        &rust_cli::render(spec),
        &mut report,
    )?;
    write_file(
        &output_dir.join("SKILL.md"),
        &skill::render(spec),
        &mut report,
    )?;
    write_file(
        &output_dir.join("mcp.json"),
        &mcp::render(spec)?,
        &mut report,
    )?;
    write_file(
        &output_dir.join("module.json"),
        &manifest::render(spec)?,
        &mut report,
    )?;

    Ok(report)
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|source| GenError::WriteOutput {
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, contents: &str, report: &mut EmitReport) -> Result<()> {
    fs::write(path, contents).map_err(|source| GenError::WriteOutput {
        path: path.to_path_buf(),
        source,
    })?;
    report.files.push(path.to_path_buf());
    Ok(())
}
