//! Tool abstraction + the two built-in flavours (CLI subprocess + kernel bus).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use oxide_k::bus::{Command, Event, Message, MessageBus};
use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

use crate::error::{McpError, Result};

/// Lightweight JSON-Schema fragment describing a tool's input.
///
/// The MCP spec calls for a JSON Schema in `inputSchema`. For most generated
/// tools the schema is small enough that a hand-rolled struct beats pulling
/// in a full schema crate.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolInputSchema {
    /// `"object"` for all the tools we emit; included for spec conformance.
    #[serde(rename = "type")]
    pub kind: String,
    /// Property name → schema fragment (free-form `serde_json::Value`).
    pub properties: serde_json::Map<String, serde_json::Value>,
    /// Required property names.
    #[serde(default)]
    pub required: Vec<String>,
}

impl ToolInputSchema {
    /// Build an empty-input schema.
    pub fn empty() -> Self {
        Self {
            kind: "object".into(),
            properties: serde_json::Map::new(),
            required: Vec::new(),
        }
    }
}

/// Public-facing tool descriptor returned by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// Tool name (stable, machine-readable identifier).
    pub name: String,
    /// Human-readable summary.
    pub description: String,
    /// JSON-Schema description of the `arguments` field expected by
    /// `tools/call`.
    #[serde(rename = "inputSchema")]
    pub input_schema: ToolInputSchema,
}

/// Anything that can be invoked through MCP `tools/call`.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Return the descriptor the server publishes to clients.
    fn descriptor(&self) -> ToolDescriptor;
    /// Execute the tool with `arguments` and return a JSON result.
    async fn invoke(&self, arguments: serde_json::Value) -> Result<serde_json::Value>;
}

// ---------------------------------------------------------------------------
// CliTool — spawn a subprocess
// ---------------------------------------------------------------------------

/// MCP tool backed by a CLI subprocess (typically an `oxide-gen` generated
/// binary).
///
/// `invoke` spawns `command [args...] [extra_args_from_call]`. If
/// `flag_map_arguments` is true, every key in the `arguments` object becomes
/// `--key value` on the command line (booleans yield bare flags). Otherwise
/// the raw `arguments` JSON is forwarded on stdin.
pub struct CliTool {
    descriptor: ToolDescriptor,
    command: String,
    base_args: Vec<String>,
    flag_map_arguments: bool,
    timeout: Duration,
}

impl CliTool {
    /// Build a CLI tool that forwards arguments as `--key value` flags.
    pub fn new(
        descriptor: ToolDescriptor,
        command: impl Into<String>,
        base_args: Vec<String>,
    ) -> Self {
        Self {
            descriptor,
            command: command.into(),
            base_args,
            flag_map_arguments: true,
            timeout: Duration::from_secs(60),
        }
    }

    /// Tell the tool to send `arguments` as a JSON document on stdin instead
    /// of as `--flag` pairs.
    #[must_use]
    pub fn arguments_on_stdin(mut self) -> Self {
        self.flag_map_arguments = false;
        self
    }

    /// Override the subprocess wall-clock timeout.
    #[must_use]
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

#[async_trait]
impl Tool for CliTool {
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }

    async fn invoke(&self, arguments: serde_json::Value) -> Result<serde_json::Value> {
        let mut cmd = TokioCommand::new(&self.command);
        cmd.args(&self.base_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if self.flag_map_arguments {
            if let Some(map) = arguments.as_object() {
                for (k, v) in map {
                    cmd.arg(format!("--{k}"));
                    let str_val = match v {
                        serde_json::Value::Bool(_) => continue,
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    cmd.arg(str_val);
                }
            }
        }

        let mut child = cmd.spawn().map_err(McpError::Io)?;
        if !self.flag_map_arguments {
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                let body = serde_json::to_vec(&arguments)?;
                stdin.write_all(&body).await.map_err(McpError::Io)?;
            }
        }

        let waiter = child.wait_with_output();
        let output = timeout(self.timeout, waiter)
            .await
            .map_err(|_| McpError::ToolFailed {
                tool: self.descriptor.name.clone(),
                message: format!("timed out after {:?}", self.timeout),
            })?
            .map_err(McpError::Io)?;

        if !output.status.success() {
            return Err(McpError::ToolFailed {
                tool: self.descriptor.name.clone(),
                message: format!(
                    "exit {}: {}",
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        // If stdout is valid JSON, return it as-is; otherwise wrap in a
        // `{"output": "..."}` envelope so MCP clients always get JSON.
        match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Ok(v) => Ok(v),
            Err(_) => Ok(serde_json::json!({ "output": stdout })),
        }
    }
}

// ---------------------------------------------------------------------------
// BusTool — dispatch via the oxide-k message bus
// ---------------------------------------------------------------------------

/// MCP tool backed by the kernel message bus. Sends a `Command::Invoke` to
/// the named module and awaits the matching `Custom` event response.
pub struct BusTool {
    descriptor: ToolDescriptor,
    bus: MessageBus,
    module_id: String,
    method: String,
    timeout: Duration,
}

impl BusTool {
    /// Build a bus tool.
    pub fn new(
        descriptor: ToolDescriptor,
        bus: MessageBus,
        module_id: impl Into<String>,
        method: impl Into<String>,
    ) -> Self {
        Self {
            descriptor,
            bus,
            module_id: module_id.into(),
            method: method.into(),
            timeout: Duration::from_secs(30),
        }
    }

    /// Override the await-response timeout.
    #[must_use]
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

#[async_trait]
impl Tool for BusTool {
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }

    async fn invoke(&self, arguments: serde_json::Value) -> Result<serde_json::Value> {
        let mut sub = self.bus.subscribe().await;
        let ok_kind = format!("{}.ok", self.method);
        let err_kind = format!("{}.err", self.method);
        self.bus
            .send_command(
                "mcp-server",
                Command::Invoke {
                    module_id: self.module_id.clone(),
                    method: self.method.clone(),
                    payload: arguments,
                },
            )
            .await
            .map_err(McpError::Kernel)?;

        let deadline = timeout(self.timeout, async {
            loop {
                match sub.receiver.recv().await {
                    Some(env) => {
                        if let Message::Event(Event::Custom { kind, payload, .. }) = env.message {
                            if kind == ok_kind {
                                return Ok::<serde_json::Value, McpError>(payload);
                            }
                            if kind == err_kind {
                                let msg = payload
                                    .get("error")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                return Err(McpError::ToolFailed {
                                    tool: self.descriptor.name.clone(),
                                    message: msg,
                                });
                            }
                        }
                    }
                    None => {
                        return Err(McpError::ToolFailed {
                            tool: self.descriptor.name.clone(),
                            message: "bus closed before response arrived".into(),
                        })
                    }
                }
            }
        })
        .await;

        match deadline {
            Ok(r) => r,
            Err(_) => Err(McpError::ToolFailed {
                tool: self.descriptor.name.clone(),
                message: format!("timed out after {:?}", self.timeout),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Tool registry the [`crate::server::McpServer`] consults during
/// `tools/list` / `tools/call`.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    inner: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Build an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a tool.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.descriptor().name.clone();
        self.inner.insert(name, tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.inner.get(name).cloned()
    }

    /// Iterate descriptors in name order.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        let mut out: Vec<_> = self.inner.values().map(|t| t.descriptor()).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// True when no tool is registered.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_desc(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.into(),
            description: format!("Tool {name}"),
            input_schema: ToolInputSchema::empty(),
        }
    }

    #[tokio::test]
    async fn registry_holds_and_lists_tools() {
        let mut reg = ToolRegistry::new();
        struct Stub(ToolDescriptor);
        #[async_trait]
        impl Tool for Stub {
            fn descriptor(&self) -> ToolDescriptor {
                self.0.clone()
            }
            async fn invoke(&self, _args: serde_json::Value) -> Result<serde_json::Value> {
                Ok(serde_json::json!({"ok": true}))
            }
        }
        reg.register(Arc::new(Stub(sample_desc("a"))));
        reg.register(Arc::new(Stub(sample_desc("b"))));
        assert_eq!(reg.len(), 2);
        let descs = reg.descriptors();
        assert_eq!(descs[0].name, "a");
        assert_eq!(descs[1].name, "b");
    }

    #[tokio::test]
    async fn cli_tool_spawns_and_returns_stdout_json() {
        // `echo` returns the JSON we feed it via --output flag.
        // Use a tiny shell-out: `printf` always exists.
        let tool = CliTool::new(
            sample_desc("printer"),
            "printf",
            vec!["%s".into(), "{\"hello\": 1}".into()],
        );
        let out = tool.invoke(serde_json::json!({})).await.unwrap();
        assert_eq!(out, serde_json::json!({"hello": 1}));
    }

    #[tokio::test]
    async fn cli_tool_wraps_non_json_stdout() {
        let tool = CliTool::new(
            sample_desc("plain"),
            "printf",
            vec!["%s".into(), "not json".into()],
        );
        let out = tool.invoke(serde_json::json!({})).await.unwrap();
        assert_eq!(out["output"], serde_json::json!("not json"));
    }

    #[tokio::test]
    async fn bus_tool_round_trips_command_and_event() {
        let bus = MessageBus::new();

        // Spawn a fake module that responds to bus invocations.
        let bus_clone = bus.clone();
        let mut sub = bus.subscribe().await;
        let handler = tokio::spawn(async move {
            while let Some(env) = sub.receiver.recv().await {
                if let Message::Command(Command::Invoke {
                    module_id,
                    method,
                    payload,
                }) = env.message
                {
                    if module_id == "echo" && method == "say" {
                        let _ = bus_clone
                            .emit_event(
                                "echo",
                                Event::Custom {
                                    module_id: "echo".into(),
                                    kind: "say.ok".into(),
                                    payload: serde_json::json!({
                                        "received": payload
                                    }),
                                },
                            )
                            .await;
                    }
                }
            }
        });

        let tool = BusTool::new(sample_desc("echo.say"), bus.clone(), "echo", "say");
        let out = tool
            .invoke(serde_json::json!({"text": "hi"}))
            .await
            .unwrap();
        assert_eq!(out["received"]["text"], serde_json::json!("hi"));

        handler.abort();
    }
}
