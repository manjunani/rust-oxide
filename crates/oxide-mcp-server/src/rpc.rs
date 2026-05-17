//! JSON-RPC 2.0 envelope types.

use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 request id. Either an integer, a string, or omitted (for
/// notifications).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RpcId {
    /// Numeric id (most common).
    Number(i64),
    /// Stringified id.
    Str(String),
    /// Null id — protocol calls this a "notification" with no response.
    Null,
}

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Must be exactly `"2.0"`.
    pub jsonrpc: String,
    /// Method name (e.g. `tools/list`).
    pub method: String,
    /// Optional parameters object.
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Correlation id; `None` for notifications.
    #[serde(default)]
    pub id: Option<RpcId>,
}

/// JSON-RPC 2.0 success / error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Must be exactly `"2.0"`.
    pub jsonrpc: String,
    /// Correlation id; mirrors the request.
    pub id: RpcId,
    /// Success payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Build a success response.
    pub fn ok(id: RpcId, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response.
    pub fn err(id: RpcId, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// JSON-RPC error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Numeric error code.
    pub code: i64,
    /// Short message.
    pub message: String,
    /// Optional payload with extra detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Standard JSON-RPC error codes plus MCP extensions.
pub mod codes {
    /// Invalid JSON received.
    pub const PARSE_ERROR: i64 = -32700;
    /// Request shape was invalid.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Method not found.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid params.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Tool execution failed.
    pub const TOOL_FAILED: i64 = -32000;
}
