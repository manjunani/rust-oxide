//! Emit `mcp.json` — an MCP server configuration that exposes each generated
//! CLI subcommand as a callable tool.
//!
//! The output follows the shape expected by Claude Code's stdio MCP servers
//! (`command`/`args` plus a `tools` array describing each available tool).

use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::ir::{ApiSpec, Operation, ParamLocation};

/// Render the `mcp.json` contents as pretty-printed JSON.
pub fn render(spec: &ApiSpec) -> Result<String> {
    let bin = format!("{}-cli", spec.name.replace('_', "-"));

    let tools: Vec<Value> = spec.operations.iter().map(|op| tool_for(&bin, op)).collect();

    let cfg = json!({
        "name": spec.name.replace('_', "-"),
        "displayName": spec.display_name,
        "version": spec.version,
        "type": "stdio",
        "command": bin,
        "tools": tools,
    });

    Ok(serde_json::to_string_pretty(&cfg)?)
}

fn tool_for(bin: &str, op: &Operation) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for p in &op.params {
        let json_type = json_schema_type(&p.rust_type);
        let mut prop = Map::new();
        prop.insert("type".into(), Value::String(json_type.into()));
        if let Some(desc) = &p.description {
            prop.insert("description".into(), Value::String(desc.clone()));
        }
        prop.insert(
            "x-location".into(),
            Value::String(location_str(p.location).into()),
        );
        props.insert(p.name.clone(), Value::Object(prop));
        if p.required {
            required.push(Value::String(p.name.clone()));
        }
    }

    let cli_subcommand = heck::ToKebabCase::to_kebab_case(op.original_id.as_str());

    json!({
        "name": op.id,
        "description": op.description.clone().unwrap_or_else(|| format!("Invoke {} ({})", op.original_id, op.endpoint)),
        "endpoint": op.endpoint,
        "command": bin,
        "args": ["{subcommand}"],
        "subcommand": cli_subcommand,
        "inputSchema": {
            "type": "object",
            "properties": Value::Object(props),
            "required": Value::Array(required),
        }
    })
}

fn json_schema_type(rust_ty: &str) -> &'static str {
    if rust_ty.starts_with("Option<") || rust_ty.starts_with("Vec<") {
        return match rust_ty.trim_start_matches("Option<").trim_start_matches("Vec<") {
            t if t.starts_with("i") || t.starts_with("u") => "integer",
            t if t.starts_with("f") => "number",
            "bool>" | "bool" => "boolean",
            _ => "string",
        };
    }
    match rust_ty {
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" => "integer",
        "f32" | "f64" => "number",
        "bool" => "boolean",
        _ => "string",
    }
}

fn location_str(loc: ParamLocation) -> &'static str {
    match loc {
        ParamLocation::Path => "path",
        ParamLocation::Query => "query",
        ParamLocation::Body => "body",
        ParamLocation::Header => "header",
        ParamLocation::GrpcField => "grpc-field",
        ParamLocation::GraphQlVariable => "graphql-variable",
    }
}
