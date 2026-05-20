//! Minimal Protocol Buffers / gRPC parser.
//!
//! Rather than pulling a full `protoc` toolchain into the workspace, we
//! hand-roll a permissive parser that recognises the subset of proto3 syntax
//! used by typical service definitions:
//!
//! * `syntax = "proto3";`
//! * `package foo.bar;`
//! * `message Name { TYPE field = NUMBER; … }`
//! * `service Name { rpc Method (Request) returns (Response); … }`
//!
//! Imports, nested messages, `oneof`, `map`, `repeated`, options, and reserved
//! ranges are recognised but emit no IR (`repeated` is honoured and produces a
//! `Vec<T>`). Anything else is silently skipped — the goal is to emit *some*
//! useful scaffold for the common case, not to be a conformant compiler.

use crate::error::Result;
use crate::ir::{
    ApiKind, ApiSpec, Field, HttpMethod, Operation, Param, ParamLocation, Protocol, StreamingMode,
    TypeDef,
};
use crate::parsers::naming::{crate_name, pascal_ident, snake_ident};

/// Parse a `.proto` source string into an [`ApiSpec`].
pub fn parse(raw: &str) -> Result<ApiSpec> {
    let cleaned = strip_comments(raw);
    let mut package: Option<String> = None;
    let mut types: Vec<TypeDef> = Vec::new();
    let mut operations: Vec<Operation> = Vec::new();
    let mut services: Vec<String> = Vec::new();

    // package line.
    for line in cleaned.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("package") {
            let pkg = rest.trim().trim_end_matches(';').trim();
            if !pkg.is_empty() {
                package = Some(pkg.to_string());
            }
        }
    }

    // message and service blocks.
    let mut idx = 0;
    while let Some(start) = next_keyword(&cleaned, idx, &["message", "service"]) {
        let (keyword_end, kind) = start;
        idx = keyword_end;
        // Parse name.
        let (name, after_name) = match read_ident(&cleaned, idx) {
            Some(v) => v,
            None => break,
        };
        idx = after_name;
        // Find opening `{` and matching `}`.
        let Some(open) = cleaned[idx..].find('{') else {
            break;
        };
        let body_start = idx + open + 1;
        let Some(body_len) = matching_brace(&cleaned[body_start..]) else {
            break;
        };
        let body = &cleaned[body_start..body_start + body_len];
        idx = body_start + body_len + 1;

        match kind {
            BlockKind::Message => {
                types.push(TypeDef::Struct {
                    name: pascal_ident(&name),
                    description: None,
                    fields: parse_message_body(body),
                });
            }
            BlockKind::Service => {
                services.push(name.clone());
                for op in parse_service_body(body, &name) {
                    operations.push(op);
                }
            }
        }
    }

    let display = package
        .clone()
        .unwrap_or_else(|| "grpc_service".to_string());
    let name = crate_name(&display);

    Ok(ApiSpec {
        name,
        display_name: display.clone(),
        version: "0.1.0".to_string(),
        description: services.first().map(|s| format!("gRPC service: {s}")),
        kind: ApiKind::Grpc,
        base_url: None,
        types,
        operations,
        raw_spec: None,
    })
}

// ---------------------------------------------------------------------------
// Low-level helpers
// ---------------------------------------------------------------------------

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '/' if matches!(chars.peek(), Some('/')) => {
                // line comment
                chars.next();
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if matches!(chars.peek(), Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

#[derive(Copy, Clone)]
enum BlockKind {
    Message,
    Service,
}

fn next_keyword(src: &str, from: usize, keywords: &[&str]) -> Option<(usize, BlockKind)> {
    let mut earliest: Option<(usize, &str)> = None;
    for kw in keywords {
        if let Some(pos) = find_word(src, from, kw) {
            if earliest.is_none_or(|(p, _)| pos < p) {
                earliest = Some((pos, kw));
            }
        }
    }
    let (pos, kw) = earliest?;
    let kind = if kw == "message" {
        BlockKind::Message
    } else {
        BlockKind::Service
    };
    Some((pos + kw.len(), kind))
}

fn find_word(src: &str, from: usize, needle: &str) -> Option<usize> {
    let mut start = from;
    while let Some(rel) = src[start..].find(needle) {
        let abs = start + rel;
        let before_ok = abs == 0
            || !src.as_bytes()[abs - 1].is_ascii_alphanumeric() && src.as_bytes()[abs - 1] != b'_';
        let after_idx = abs + needle.len();
        let after_ok = after_idx >= src.len()
            || !src.as_bytes()[after_idx].is_ascii_alphanumeric()
                && src.as_bytes()[after_idx] != b'_';
        if before_ok && after_ok {
            return Some(abs);
        }
        start = abs + needle.len();
    }
    None
}

fn read_ident(src: &str, from: usize) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    let mut i = from;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if start == i {
        None
    } else {
        Some((src[start..i].to_string(), i))
    }
}

fn matching_brace(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 1usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Message body → Vec<Field>
// ---------------------------------------------------------------------------

fn parse_message_body(body: &str) -> Vec<Field> {
    let mut fields = Vec::new();
    for raw_line in body.split(';') {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("oneof")
            || line.starts_with("map")
            || line.starts_with("option")
            || line.starts_with("reserved")
            || line.starts_with("enum")
            || line.starts_with("message")
            || line.starts_with("//")
        {
            continue;
        }
        if let Some(field) = parse_field_line(line) {
            fields.push(field);
        }
    }
    fields
}

fn parse_field_line(line: &str) -> Option<Field> {
    let mut tokens = line.split_whitespace();
    let first = tokens.next()?;
    let (repeated, ty_token) = if first == "repeated" {
        (true, tokens.next()?)
    } else {
        (false, first)
    };
    let name_token = tokens.next()?;
    let rust_inner = proto_type_to_rust(ty_token);
    let rust_type = if repeated {
        format!("Vec<{rust_inner}>")
    } else {
        rust_inner
    };
    let snake = snake_ident(name_token);
    let serde_rename = if snake.trim_start_matches("r#") != name_token {
        Some(name_token.to_string())
    } else {
        None
    };
    Some(Field {
        name: snake,
        serde_rename,
        rust_type,
        // proto3 fields are all technically optional on the wire, but for
        // codegen ergonomics we treat scalars and repeated fields as
        // required (proto3 default values are populated automatically).
        // The `repeated` distinction is kept by the `Vec<T>` wrap above.
        optional: false,
        description: None,
        // Touch `repeated` so the compiler keeps the let-binding in sync
        // if a future change wants per-field optionality.
        // (no-op)
    })
}

fn proto_type_to_rust(t: &str) -> String {
    match t {
        "double" => "f64".into(),
        "float" => "f32".into(),
        "int32" | "sint32" | "sfixed32" => "i32".into(),
        "int64" | "sint64" | "sfixed64" => "i64".into(),
        "uint32" | "fixed32" => "u32".into(),
        "uint64" | "fixed64" => "u64".into(),
        "bool" => "bool".into(),
        "string" => "String".into(),
        "bytes" => "Vec<u8>".into(),
        other => pascal_ident(other.rsplit('.').next().unwrap_or(other)),
    }
}

// ---------------------------------------------------------------------------
// Service body → Vec<Operation>
// ---------------------------------------------------------------------------

fn parse_service_body(body: &str, service_name: &str) -> Vec<Operation> {
    let mut out = Vec::new();
    for raw_line in body.split(';') {
        let line = raw_line.trim();
        if line.is_empty() || !line.starts_with("rpc") {
            continue;
        }
        if let Some(op) = parse_rpc_line(line, service_name) {
            out.push(op);
        }
    }
    out
}

fn parse_rpc_line(line: &str, service_name: &str) -> Option<Operation> {
    // `rpc Method (Request) returns (Response)` — possibly with `stream` keywords.
    let after_rpc = line.strip_prefix("rpc")?.trim();
    let name_end = after_rpc.find('(')?;
    let method_name = after_rpc[..name_end].trim();
    let rest = &after_rpc[name_end..];

    let req_inner = extract_paren(rest)?;
    let after_req = rest.find(')').map(|i| &rest[i + 1..]).unwrap_or("");
    let returns_idx = after_req.find("returns")?;
    let after_returns = &after_req[returns_idx + "returns".len()..];
    let res_inner = extract_paren(after_returns)?;

    let req_stream = req_inner.trim().starts_with("stream");
    let res_stream = res_inner.trim().starts_with("stream");
    let request_type = strip_stream(req_inner);
    let response_type = strip_stream(res_inner);

    let streaming = match (req_stream, res_stream) {
        (false, false) => StreamingMode::Unary,
        (false, true) => StreamingMode::ServerStream,
        (true, false) => StreamingMode::ClientStream,
        (true, true) => StreamingMode::BidiStream,
    };

    let id = snake_ident(method_name);
    let original_id = method_name.to_string();
    let endpoint = format!("/{}/{}", service_name, method_name);
    let request_rust = pascal_ident(request_type);
    let response_rust = pascal_ident(response_type);

    Some(Operation {
        id,
        original_id,
        description: None,
        protocol: Protocol::Grpc,
        endpoint,
        http_method: HttpMethod::None,
        params: vec![Param {
            name: "request".into(),
            original_name: "request".into(),
            rust_type: request_rust,
            location: ParamLocation::GrpcField,
            required: true,
            description: None,
        }],
        return_type: response_rust,
        streaming,
    })
}

fn extract_paren(s: &str) -> Option<&str> {
    let open = s.find('(')?;
    let close = s[open + 1..].find(')')?;
    Some(s[open + 1..open + 1 + close].trim())
}

fn strip_stream(s: &str) -> &str {
    s.trim().trim_start_matches("stream").trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECHO: &str = include_str!("../../tests/fixtures/echo.proto");

    #[test]
    fn parses_messages_and_fields() {
        let spec = parse(ECHO).unwrap();
        let req = spec
            .types
            .iter()
            .find(|t| t.name() == "SayRequest")
            .unwrap();
        match req {
            TypeDef::Struct { fields, .. } => {
                let text = fields.iter().find(|f| f.name == "text").unwrap();
                assert_eq!(text.rust_type, "String");
            }
            _ => panic!("expected struct"),
        }
    }

    #[test]
    fn parses_service_rpcs() {
        let spec = parse(ECHO).unwrap();
        let op = spec
            .operations
            .iter()
            .find(|o| o.original_id == "Say")
            .expect("Say rpc");
        assert_eq!(op.protocol, Protocol::Grpc);
        assert_eq!(op.endpoint, "/Echo/Say");
        assert_eq!(op.params[0].rust_type, "SayRequest");
        assert_eq!(op.return_type, "SayResponse");
        assert_eq!(op.streaming, StreamingMode::Unary);
    }

    #[test]
    fn detects_streaming_modes() {
        let spec = parse(ECHO).unwrap();
        let stream_back = spec
            .operations
            .iter()
            .find(|o| o.original_id == "StreamBack")
            .unwrap();
        assert_eq!(stream_back.streaming, StreamingMode::ServerStream);
        let chat = spec
            .operations
            .iter()
            .find(|o| o.original_id == "Chat")
            .unwrap();
        assert_eq!(chat.streaming, StreamingMode::BidiStream);
    }

    #[test]
    fn repeated_fields_become_vec() {
        let proto = r#"
            syntax = "proto3";
            message Listing {
              repeated string tags = 1;
              int32 count = 2;
            }
        "#;
        let spec = parse(proto).unwrap();
        let listing = spec.types.iter().find(|t| t.name() == "Listing").unwrap();
        let TypeDef::Struct { fields, .. } = listing else {
            panic!("expected struct")
        };
        let tags = fields.iter().find(|f| f.name == "tags").unwrap();
        assert_eq!(tags.rust_type, "Vec<String>");
        let count = fields.iter().find(|f| f.name == "count").unwrap();
        assert_eq!(count.rust_type, "i32");
    }
}
