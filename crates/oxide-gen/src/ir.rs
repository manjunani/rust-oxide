//! # Intermediate Representation
//!
//! Parsers convert their respective spec formats into a common [`ApiSpec`].
//! Emitters then consume the IR to produce Rust code, CLI definitions,
//! `SKILL.md`, and MCP server configurations.
//!
//! The IR is intentionally simple — just enough to drive a basic generator.
//! Features like polymorphism (`oneOf`/`anyOf`), recursive `$ref`s, and rich
//! validation rules are deliberately out of scope.

use serde::{Deserialize, Serialize};

/// The full description of an API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiSpec {
    /// Crate name (snake_case) for the generated output.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Semantic version.
    pub version: String,
    /// Optional crate-level description.
    pub description: Option<String>,
    /// Source API kind.
    pub kind: ApiKind,
    /// Default base URL, if known.
    pub base_url: Option<String>,
    /// User-defined types (structs, enums, aliases).
    pub types: Vec<TypeDef>,
    /// Callable operations.
    pub operations: Vec<Operation>,
    /// The raw spec contents (useful for build.rs or schema files).
    #[serde(default)]
    pub raw_spec: Option<String>,
}

/// The format of the original API specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKind {
    /// OpenAPI 3.x.
    OpenApi,
    /// GraphQL schema definition language.
    GraphQl,
    /// Protocol Buffers / gRPC.
    Grpc,
}

impl ApiKind {
    /// Short stable identifier (used in MCP/SKILL.md generation).
    pub fn slug(self) -> &'static str {
        match self {
            ApiKind::OpenApi => "openapi",
            ApiKind::GraphQl => "graphql",
            ApiKind::Grpc => "grpc",
        }
    }
}

/// A user-defined type that becomes a Rust struct/enum/alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypeDef {
    /// Plain object → Rust struct.
    Struct {
        /// PascalCase Rust name.
        name: String,
        /// Doc comment.
        description: Option<String>,
        /// Field list.
        fields: Vec<Field>,
    },
    /// Closed string enum → Rust enum.
    Enum {
        /// PascalCase Rust name.
        name: String,
        /// Doc comment.
        description: Option<String>,
        /// Variants — each one tracks its Rust name plus an optional original
        /// spec name for serde renaming.
        variants: Vec<EnumVariant>,
    },
    /// Simple alias → `pub type Alias = Target;`.
    Alias {
        /// PascalCase Rust name.
        name: String,
        /// Target Rust type expression.
        target: String,
    },
}

impl TypeDef {
    /// Return the Rust identifier of the type.
    pub fn name(&self) -> &str {
        match self {
            TypeDef::Struct { name, .. }
            | TypeDef::Enum { name, .. }
            | TypeDef::Alias { name, .. } => name,
        }
    }
}

/// A single enum variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumVariant {
    /// PascalCase Rust identifier.
    pub name: String,
    /// Original spec value, used for serde rename if it differs from `name`.
    pub serde_rename: Option<String>,
}

/// A single struct field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    /// snake_case Rust identifier.
    pub name: String,
    /// Original spec name (used for serde rename when it differs).
    pub serde_rename: Option<String>,
    /// Pre-rendered Rust type (e.g. `Vec<String>`, `Option<i64>`).
    pub rust_type: String,
    /// Whether the field is optional (already accounted for in `rust_type`).
    pub optional: bool,
    /// Doc comment.
    pub description: Option<String>,
}

/// An invocable API operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    /// snake_case Rust method name.
    pub id: String,
    /// Original operation id from the spec (used for CLI subcommand name).
    pub original_id: String,
    /// Doc comment / summary.
    pub description: Option<String>,
    /// Protocol used by this operation.
    pub protocol: Protocol,
    /// For HTTP-based operations: METHOD + path. For gRPC: full method
    /// identifier (`service/method`).
    pub endpoint: String,
    /// HTTP method (only meaningful for OpenAPI/GraphQL).
    pub http_method: HttpMethod,
    /// Inputs.
    pub params: Vec<Param>,
    /// Pre-rendered Rust return type, e.g. `Pet` or `Vec<Pet>` or `()`.
    pub return_type: String,
    /// Streaming direction. `Unary` for plain request/response operations,
    /// `ServerStream` for GraphQL subscriptions and gRPC `stream` responses,
    /// `ClientStream` / `BidiStream` for gRPC stream-in / stream-both
    /// signatures.
    #[serde(default)]
    pub streaming: StreamingMode,
}

/// Streaming direction tag attached to an [`Operation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamingMode {
    /// Plain request/response.
    #[default]
    Unary,
    /// Server pushes multiple messages (GraphQL `Subscription`, gRPC
    /// `stream` response).
    ServerStream,
    /// Client pushes multiple messages (gRPC `stream` request).
    ClientStream,
    /// Both sides stream (gRPC `stream` request + `stream` response).
    BidiStream,
}

impl StreamingMode {
    /// `true` for any non-unary mode.
    pub fn is_streaming(self) -> bool {
        !matches!(self, StreamingMode::Unary)
    }

    /// Short human-readable label used in SKILL.md / docs.
    pub fn label(self) -> &'static str {
        match self {
            StreamingMode::Unary => "unary",
            StreamingMode::ServerStream => "server-stream",
            StreamingMode::ClientStream => "client-stream",
            StreamingMode::BidiStream => "bidi-stream",
        }
    }
}

/// The wire protocol used by an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// REST / HTTP+JSON.
    Rest,
    /// GraphQL over HTTP POST.
    GraphQl,
    /// gRPC.
    Grpc,
}

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `PATCH`.
    Patch,
    /// `DELETE`.
    Delete,
    /// `OPTIONS`.
    Options,
    /// `HEAD`.
    Head,
    /// Not applicable (gRPC, etc.).
    None,
}

impl HttpMethod {
    /// Reqwest method-builder name (`get`, `post`, …). Returns `None` for
    /// [`HttpMethod::None`].
    pub fn reqwest_fn(self) -> Option<&'static str> {
        Some(match self {
            HttpMethod::Get => "get",
            HttpMethod::Post => "post",
            HttpMethod::Put => "put",
            HttpMethod::Patch => "patch",
            HttpMethod::Delete => "delete",
            HttpMethod::Options => return None,
            HttpMethod::Head => "head",
            HttpMethod::None => return None,
        })
    }

    /// Uppercase HTTP verb.
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Options => "OPTIONS",
            HttpMethod::Head => "HEAD",
            HttpMethod::None => "NONE",
        }
    }
}

/// A single operation parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    /// snake_case Rust identifier.
    pub name: String,
    /// Original spec name (used for serde / query string keys).
    pub original_name: String,
    /// Pre-rendered Rust type.
    pub rust_type: String,
    /// Where the parameter lives in the request.
    pub location: ParamLocation,
    /// Whether the parameter is mandatory.
    pub required: bool,
    /// Doc comment.
    pub description: Option<String>,
}

/// Where a parameter appears in the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamLocation {
    /// URL path segment, e.g. `/pets/{petId}`.
    Path,
    /// Query string parameter.
    Query,
    /// Request body (typically JSON).
    Body,
    /// HTTP header.
    Header,
    /// gRPC field on the request message.
    GrpcField,
    /// GraphQL operation variable.
    GraphQlVariable,
}
