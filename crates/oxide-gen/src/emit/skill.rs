//! Emit `SKILL.md` — a Claude Code skill descriptor with YAML frontmatter.

use std::fmt::Write;

use crate::ir::{ApiSpec, ParamLocation};

/// Render the SKILL.md contents for the given spec.
pub fn render(spec: &ApiSpec) -> String {
    let mut out = String::new();
    let skill_name = spec.name.replace('_', "-");
    let bin = format!("{skill_name}-cli");

    // YAML frontmatter — the form Claude Code expects.
    writeln!(out, "---").unwrap();
    writeln!(out, "name: {skill_name}").unwrap();
    writeln!(
        out,
        "description: {}",
        yaml_inline(spec.description.as_deref().unwrap_or(&spec.display_name))
    )
    .unwrap();
    writeln!(out, "version: {}", spec.version).unwrap();
    writeln!(out, "kind: {}", spec.kind.slug()).unwrap();
    writeln!(out, "binary: {bin}").unwrap();
    writeln!(out, "---").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "# {} — generated client", spec.display_name).unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "This skill wraps `{bin}`, an auto-generated client for the `{}` API.",
        spec.display_name
    )
    .unwrap();
    if let Some(base) = &spec.base_url {
        writeln!(out, "Default endpoint: `{base}`.").unwrap();
        writeln!(
            out,
            "Override with `--base-url <URL>` or the `OXIDE_BASE_URL` environment variable."
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "## Commands").unwrap();
    writeln!(out).unwrap();
    for op in &spec.operations {
        writeln!(out, "### `{bin} {}`", kebab(&op.original_id)).unwrap();
        if let Some(desc) = &op.description {
            writeln!(out).unwrap();
            for line in desc.lines() {
                writeln!(out, "{line}").unwrap();
            }
        }
        writeln!(out).unwrap();
        writeln!(out, "- **Endpoint:** `{}`", op.endpoint).unwrap();
        writeln!(out, "- **Returns:** `{}`", op.return_type).unwrap();
        if !op.params.is_empty() {
            writeln!(out, "- **Arguments:**").unwrap();
            for p in &op.params {
                let req = if p.required { "required" } else { "optional" };
                let loc = match p.location {
                    ParamLocation::Path => "path",
                    ParamLocation::Query => "query",
                    ParamLocation::Body => "body (JSON)",
                    ParamLocation::Header => "header",
                    ParamLocation::GrpcField => "grpc",
                    ParamLocation::GraphQlVariable => "graphql variable",
                };
                writeln!(
                    out,
                    "  - `--{}` ({req}, {loc}, `{}`)",
                    kebab(&p.name),
                    p.rust_type
                )
                .unwrap();
            }
        }
        writeln!(out).unwrap();
    }

    writeln!(out, "## Output").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "Every command writes a single JSON document (pretty-printed) to stdout. Errors are written to stderr and result in a non-zero exit code."
    )
    .unwrap();

    out
}

fn yaml_inline(s: &str) -> String {
    // A safe-enough YAML scalar: wrap in double quotes if it contains
    // characters that would otherwise need escaping; otherwise use the plain
    // form.
    let needs_quote = s
        .chars()
        .any(|c| matches!(c, ':' | '#' | '"' | '\'' | '\n' | '{' | '}' | '[' | ']'));
    if needs_quote {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

fn kebab(s: &str) -> String {
    heck::ToKebabCase::to_kebab_case(s)
}
