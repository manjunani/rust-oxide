//! `oxide-gen` CLI — generate a Rust crate from an API specification.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use oxide_gen::{generate_from_path, ApiKind};
use tracing::{info, Level};

#[derive(Parser, Debug)]
#[command(
    name = "oxide-gen",
    about = "Generate Rust clients, CLIs, SKILL.md, and MCP configs from API specs.",
    version
)]
struct Args {
    /// Path to the API specification file (`.yaml` / `.json` / `.graphql` /
    /// `.proto`).
    #[arg(long)]
    spec: PathBuf,

    /// Explicit spec kind. If omitted, `oxide-gen` infers it from the file
    /// extension.
    #[arg(long, value_enum)]
    kind: Option<CliKind>,

    /// Output directory for the generated crate. Created if it does not
    /// already exist.
    #[arg(long, short = 'o')]
    output: PathBuf,

    /// Override the snake_case crate name (default: derived from the spec
    /// title or package).
    #[arg(long)]
    name: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliKind {
    Openapi,
    Graphql,
    Grpc,
}

impl From<CliKind> for ApiKind {
    fn from(value: CliKind) -> Self {
        match value {
            CliKind::Openapi => ApiKind::OpenApi,
            CliKind::Graphql => ApiKind::GraphQl,
            CliKind::Grpc => ApiKind::Grpc,
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    let args = Args::parse();
    info!(spec = ?args.spec, output = ?args.output, "starting generation");

    let report =
        generate_from_path(&args.spec, args.kind.map(Into::into), &args.output, args.name.as_deref())?;

    info!(
        crate_dir = ?report.crate_dir,
        files = report.files.len(),
        "generation complete"
    );
    for f in &report.files {
        println!("{}", f.display());
    }
    Ok(())
}
