//! # `oxide-mcp-server` — MCP server for Rust Oxide
//!
//! Implements a stdio JSON-RPC 2.0 server that adheres to the
//! [Model Context Protocol](https://modelcontextprotocol.io) and exposes two
//! flavours of tools:
//!
//! * [`CliTool`](tool::CliTool) — wraps an `oxide-gen` generated CLI binary
//!   (or any subprocess) and forwards arguments derived from the JSON-RPC
//!   request.
//! * [`BusTool`](tool::BusTool) — dispatches the call through an attached
//!   [`oxide_k::bus::MessageBus`], so browser actions ([`oxide-browser-sh`])
//!   and any other kernel-resident module can be invoked from MCP clients.
//!
//! The transport (stdio loop in `main.rs`) is a thin wrapper around the
//! purely-functional [`McpServer::handle_line`] which returns the JSON-RPC
//! response for a request line. Tests poke `handle_line` directly without
//! spawning a process or opening a socket.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod error;
pub mod rpc;
pub mod server;
pub mod tool;

pub use error::{McpError, Result};
pub use rpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, RpcId};
pub use server::{McpServer, ServerInfo};
pub use tool::{
    BusTool, CliTool, Tool, ToolDescriptor, ToolInputSchema, ToolRegistry,
};
