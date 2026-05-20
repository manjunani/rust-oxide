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
use crate::ir::{ApiKind, ApiSpec};

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

    if spec.kind == ApiKind::Grpc {
        let proto_dir = output_dir.join("proto");
        ensure_dir(&proto_dir)?;
        if let Some(raw) = &spec.raw_spec {
            write_file(&proto_dir.join("schema.proto"), raw, &mut report)?;
        }
        let build_rs = r##"fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .compile_protos(&["proto/schema.proto"], &["proto"])?;
    Ok(())
}
"##;
        write_file(&output_dir.join("build.rs"), build_rs, &mut report)?;
    }

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

    if spec.kind == ApiKind::Grpc && spec.name == "demo" {
        let smoke_test_dir = output_dir.join("tests");
        ensure_dir(&smoke_test_dir)?;
        let smoke_test_code = render_grpc_smoke_test(spec);
        write_file(
            &smoke_test_dir.join("smoke.rs"),
            &smoke_test_code,
            &mut report,
        )?;
    }

    if spec.kind == ApiKind::GraphQl && spec.name == "gql_demo" {
        let smoke_test_dir = output_dir.join("tests");
        ensure_dir(&smoke_test_dir)?;
        let smoke_test_code = render_graphql_smoke_test(spec);
        write_file(
            &smoke_test_dir.join("smoke.rs"),
            &smoke_test_code,
            &mut report,
        )?;
    }

    Ok(report)
}

fn render_grpc_smoke_test(spec: &ApiSpec) -> String {
    format!(
        r#"use tokio::sync::oneshot;
use tonic::{{transport::Server, Request, Response, Status}};

use {name}::{{proto, Client, SayRequest, SayResponse}};

#[derive(Debug, Default)]
pub struct MockEchoServer;

#[tonic::async_trait]
impl proto::echo_server::Echo for MockEchoServer {{
    async fn say(
        &self,
        request: Request<SayRequest>,
    ) -> Result<Response<SayResponse>, Status> {{
        let req = request.into_inner();
        Ok(Response::new(SayResponse {{
            echo: format!("echo: {{}}", req.text),
            history: vec![req.text.clone()],
        }}))
    }}

    async fn say_many(
        &self,
        request: Request<SayRequest>,
    ) -> Result<Response<SayResponse>, Status> {{
        let req = request.into_inner();
        Ok(Response::new(SayResponse {{
            echo: format!("many: {{}}", req.text),
            history: vec![req.text.clone()],
        }}))
    }}

    type StreamBackStream = tokio_stream::wrappers::ReceiverStream<Result<SayResponse, Status>>;
    async fn stream_back(
        &self,
        _request: Request<SayRequest>,
    ) -> Result<Response<Self::StreamBackStream>, Status> {{
        Err(Status::unimplemented("stream_back"))
    }}

    type ChatStream = tokio_stream::wrappers::ReceiverStream<Result<SayResponse, Status>>;
    async fn chat(
        &self,
        _request: Request<tonic::Streaming<SayRequest>>,
    ) -> Result<Response<Self::ChatStream>, Status> {{
        Err(Status::unimplemented("chat"))
    }}
}}

#[tokio::test]
async fn test_grpc_smoke() {{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = proto::echo_server::EchoServer::new(MockEchoServer::default());

    let (tx, rx) = oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {{
        Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                async {{
                    let _ = rx.await;
                }},
            )
            .await
            .unwrap();
    }});

    // Create client and call method
    let client = Client::new(format!("http://{{}}", addr));
    let req = SayRequest {{
        text: "hello".to_string(),
        repeat: 1,
    }};
    let res = client.say(req).await.unwrap();
    assert_eq!(res.echo, "echo: hello");
    assert_eq!(res.history, vec!["hello".to_string()]);

    let req_many = SayRequest {{
        text: "world".to_string(),
        repeat: 2,
    }};
    let res_many = client.say_many(req_many).await.unwrap();
    assert_eq!(res_many.echo, "many: world");

    // Shutdown server
    let _ = tx.send(());
    let _ = server_handle.await;
}}
"#,
        name = spec.name
    )
}

fn render_graphql_smoke_test(spec: &ApiSpec) -> String {
    format!(
        r##"use tokio::sync::oneshot;
use tokio::net::TcpListener;
use futures_util::{{SinkExt, StreamExt}};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use {name}::{{Client, Post, User, Role}};

#[tokio::test]
async fn test_graphql_subscription_smoke() {{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (tx, rx) = oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {{
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws_stream = accept_async(stream).await.unwrap();

        // 1. Read connection_init
        if let Some(Ok(Message::Text(text))) = ws_stream.next().await {{
            let init: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(init["type"], "connection_init");
        }}

        // Send connection_ack
        ws_stream
            .send(Message::Text(r#"{{"type":"connection_ack"}}"#.into()))
            .await
            .unwrap();

        // 2. Read subscribe
        if let Some(Ok(Message::Text(text))) = ws_stream.next().await {{
            let sub: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(sub["type"], "subscribe");
            assert_eq!(sub["id"], "sub_1");
        }}

        // Send next item
        let item_payload = serde_json::json!({{
            "type": "next",
            "id": "sub_1",
            "payload": {{
                "data": {{
                    "postCreated": {{
                        "id": "1",
                        "title": "Hello World",
                        "body": "Smoke test body",
                        "author": {{
                            "id": "1",
                            "name": "Alice",
                            "email": "alice@example.com",
                            "role": "ADMIN"
                        }}
                    }}
                }}
            }}
        }});
        ws_stream
            .send(Message::Text(serde_json::to_string(&item_payload).unwrap().into()))
            .await
            .unwrap();

        // Send complete
        ws_stream
            .send(Message::Text(r#"{{"id":"sub_1","type":"complete"}}"#.into()))
            .await
            .unwrap();

        // Wait for shutdown signal
        let _ = rx.await;
    }});

    let client = Client::new(format!("http://{{}}", addr));
    let mut stream = client.post_created().await.unwrap();

    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.title, "Hello World");
    assert_eq!(first.author.name, "Alice");

    assert!(stream.next().await.is_none());

    let _ = tx.send(());
    let _ = server_handle.await;
}}
"##,
        name = spec.name
    )
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
