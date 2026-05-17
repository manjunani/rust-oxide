//! [`McpServer`] — JSON-RPC method dispatcher that drives the
//! [`ToolRegistry`].

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::rpc::{codes, JsonRpcRequest, JsonRpcResponse, RpcId};
use crate::tool::ToolRegistry;

/// Server identity returned by `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Human-readable name.
    pub name: String,
    /// Semantic version.
    pub version: String,
    /// MCP protocol version this server speaks.
    pub protocol_version: String,
}

impl Default for ServerInfo {
    fn default() -> Self {
        Self {
            name: "oxide-mcp-server".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: "2024-11-05".into(),
        }
    }
}

/// MCP server core. Holds the [`ToolRegistry`] and translates JSON-RPC
/// requests into tool invocations.
pub struct McpServer {
    info: ServerInfo,
    registry: ToolRegistry,
}

impl McpServer {
    /// Build a server with the given registry and default [`ServerInfo`].
    pub fn new(registry: ToolRegistry) -> Self {
        Self {
            info: ServerInfo::default(),
            registry,
        }
    }

    /// Build a server with a custom [`ServerInfo`].
    pub fn with_info(info: ServerInfo, registry: ToolRegistry) -> Self {
        Self { info, registry }
    }

    /// Access the underlying registry — handy for tests and dynamic
    /// registration after construction.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Mutable access to the registry.
    pub fn registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.registry
    }

    /// Handle one JSON-RPC request line and return the response line.
    ///
    /// Notifications (requests with `id = null` / absent) return `None`.
    pub async fn handle_line(&self, line: &str) -> Option<String> {
        let request: JsonRpcRequest = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::err(
                    RpcId::Null,
                    codes::PARSE_ERROR,
                    format!("invalid JSON: {e}"),
                );
                return Some(serde_json::to_string(&resp).unwrap_or_default());
            }
        };

        let id = request.id.clone().unwrap_or(RpcId::Null);
        // Notifications: id absent / Null. JSON-RPC 2.0: no response.
        let is_notification = request.id.is_none() || matches!(id, RpcId::Null);

        let response = match self.dispatch(&request).await {
            Ok(value) => JsonRpcResponse::ok(id, value),
            Err((code, message)) => JsonRpcResponse::err(id, code, message),
        };

        if is_notification {
            None
        } else {
            Some(serde_json::to_string(&response).unwrap_or_default())
        }
    }

    async fn dispatch(
        &self,
        request: &JsonRpcRequest,
    ) -> std::result::Result<serde_json::Value, (i64, String)> {
        match request.method.as_str() {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": self.info.protocol_version,
                "serverInfo": {
                    "name": self.info.name,
                    "version": self.info.version,
                },
                "capabilities": {
                    "tools": { "listChanged": false }
                }
            })),

            "tools/list" => Ok(serde_json::json!({
                "tools": self.registry.descriptors(),
            })),

            "tools/call" => {
                #[derive(Deserialize)]
                struct CallParams {
                    name: String,
                    #[serde(default)]
                    arguments: Option<serde_json::Value>,
                }
                let params: CallParams = match request.params.as_ref() {
                    Some(p) => serde_json::from_value(p.clone())
                        .map_err(|e| (codes::INVALID_PARAMS, format!("bad call params: {e}")))?,
                    None => {
                        return Err((
                            codes::INVALID_PARAMS,
                            "tools/call requires `name` and `arguments`".into(),
                        ));
                    }
                };
                let tool = self.registry.get(&params.name).ok_or_else(|| {
                    (
                        codes::METHOD_NOT_FOUND,
                        format!("unknown tool `{}`", params.name),
                    )
                })?;
                match tool
                    .invoke(params.arguments.unwrap_or_else(|| serde_json::json!({})))
                    .await
                {
                    Ok(value) => Ok(serde_json::json!({
                        "content": [
                            {
                                "type": "text",
                                "text": serde_json::to_string(&value).unwrap_or_default()
                            }
                        ],
                        "structuredContent": value,
                        "isError": false
                    })),
                    Err(e) => Err((codes::TOOL_FAILED, e.to_string())),
                }
            }

            // Acknowledge MCP lifecycle notifications without doing anything.
            "notifications/initialized" | "notifications/cancelled" => Ok(serde_json::json!(null)),

            "ping" => Ok(serde_json::json!({"pong": true})),

            other => Err((codes::METHOD_NOT_FOUND, format!("unknown method `{other}`"))),
        }
    }

    /// Synchronous entry point used by the stdio loop in `main.rs`.
    pub async fn run_io<R, W>(&self, reader: R, mut writer: W) -> Result<()>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(&line).await {
                writer.write_all(response.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolDescriptor, ToolInputSchema, ToolRegistry};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn descriptor(&self) -> ToolDescriptor {
            ToolDescriptor {
                name: "echo".into(),
                description: "Echoes its arguments".into(),
                input_schema: ToolInputSchema::empty(),
            }
        }
        async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value> {
            Ok(serde_json::json!({"echoed": args}))
        }
    }

    fn server_with_echo() -> McpServer {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        McpServer::new(reg)
    }

    #[tokio::test]
    async fn initialize_returns_server_info() {
        let server = server_with_echo();
        let req = r#"{"jsonrpc":"2.0","method":"initialize","id":1}"#;
        let resp = server.handle_line(req).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["id"], serde_json::json!(1));
        assert_eq!(v["result"]["serverInfo"]["name"], "oxide-mcp-server");
        assert_eq!(
            v["result"]["capabilities"]["tools"]["listChanged"],
            serde_json::json!(false)
        );
    }

    #[tokio::test]
    async fn tools_list_returns_registered_tools() {
        let server = server_with_echo();
        let req = r#"{"jsonrpc":"2.0","method":"tools/list","id":"x"}"#;
        let resp = server.handle_line(req).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["id"], serde_json::json!("x"));
        let tools = v["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "echo");
    }

    #[tokio::test]
    async fn tools_call_dispatches_and_wraps_content() {
        let server = server_with_echo();
        let req = r#"{
            "jsonrpc":"2.0","method":"tools/call","id":2,
            "params":{"name":"echo","arguments":{"hi":"there"}}
        }"#;
        let resp = server.handle_line(req).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["id"], serde_json::json!(2));
        assert_eq!(
            v["result"]["structuredContent"]["echoed"]["hi"],
            serde_json::json!("there")
        );
        assert_eq!(v["result"]["isError"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_returns_error() {
        let server = server_with_echo();
        let req = r#"{
            "jsonrpc":"2.0","method":"tools/call","id":3,
            "params":{"name":"missing","arguments":{}}
        }"#;
        let resp = server.handle_line(req).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            v["error"]["code"],
            serde_json::json!(codes::METHOD_NOT_FOUND)
        );
    }

    #[tokio::test]
    async fn invalid_json_returns_parse_error() {
        let server = server_with_echo();
        let resp = server.handle_line("not json at all").await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], serde_json::json!(codes::PARSE_ERROR));
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        let server = server_with_echo();
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(server.handle_line(req).await.is_none());
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let server = server_with_echo();
        let req = r#"{"jsonrpc":"2.0","method":"bogus","id":7}"#;
        let resp = server.handle_line(req).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            v["error"]["code"],
            serde_json::json!(codes::METHOD_NOT_FOUND)
        );
    }
}
