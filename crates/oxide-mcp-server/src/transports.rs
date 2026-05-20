//! Optional network transports for [`McpServer`]: SSE and WebSocket.
//!
//! Enable via Cargo features:
//! * `sse` — HTTP + Server-Sent Events transport (MCP 2024-11-05 §4.2).
//! * `websocket` — WebSocket transport.
//!
//! Both transports share the purely-functional [`McpServer::handle_line`] core.

#[cfg(any(feature = "sse", feature = "websocket"))]
use std::sync::Arc;

#[cfg(any(feature = "sse", feature = "websocket"))]
use crate::{error::Result, server::McpServer};

// ---------------------------------------------------------------------------
// SSE transport
// ---------------------------------------------------------------------------

/// Serve MCP over HTTP / Server-Sent Events on `addr`.
///
/// Exposes two HTTP routes:
/// * `GET /sse` — client opens a persistent SSE connection; server immediately
///   emits `event: endpoint` with the POST URL for this session.
/// * `POST /message?session_id=<id>` — client sends a JSON-RPC request;
///   server replies via the matching SSE stream.
#[cfg(feature = "sse")]
pub async fn run_sse(server: Arc<McpServer>, addr: &str) -> Result<()> {
    use std::collections::HashMap;
    use std::convert::Infallible;

    use axum::{
        body::Bytes,
        extract::{Query, State},
        http::StatusCode,
        response::sse::{Event, KeepAlive, Sse},
        routing::{get, post},
        Router,
    };
    use tokio::sync::{mpsc, RwLock};

    type Sessions = Arc<RwLock<HashMap<String, mpsc::Sender<String>>>>;

    #[derive(Clone)]
    struct SseState {
        server: Arc<McpServer>,
        sessions: Sessions,
    }

    #[derive(serde::Deserialize)]
    struct SessionParam {
        session_id: String,
    }

    async fn sse_endpoint(
        State(state): State<SseState>,
    ) -> Sse<impl futures::Stream<Item = std::result::Result<Event, Infallible>> + Send> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = mpsc::channel::<String>(64);
        state.sessions.write().await.insert(session_id.clone(), tx);

        // Build a stream: first event tells the client where to POST; then
        // relay everything written to the channel.
        let stream = futures::stream::unfold(
            (Some(session_id.clone()), rx),
            |(maybe_init, mut rx)| async move {
                if let Some(sid) = maybe_init {
                    let event = Ok(Event::default()
                        .event("endpoint")
                        .data(format!("/message?session_id={sid}")));
                    return Some((event, (None, rx)));
                }
                rx.recv().await.map(|data| {
                    let event = Ok(Event::default().event("message").data(data));
                    (event, (None, rx))
                })
            },
        );

        Sse::new(stream).keep_alive(KeepAlive::default())
    }

    async fn post_message(
        State(state): State<SseState>,
        Query(params): Query<SessionParam>,
        body: Bytes,
    ) -> StatusCode {
        let line = String::from_utf8_lossy(&body).into_owned();
        if let Some(resp) = state.server.handle_line(&line).await {
            let sessions = state.sessions.read().await;
            if let Some(tx) = sessions.get(&params.session_id) {
                let _ = tx.try_send(resp);
            }
        }
        StatusCode::ACCEPTED
    }

    let state = SseState {
        server,
        sessions: Arc::new(RwLock::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/sse", get(sse_endpoint))
        .route("/message", post(post_message))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// WebSocket transport
// ---------------------------------------------------------------------------

/// Serve MCP over WebSocket on `addr`.
///
/// Each text frame is treated as one JSON-RPC request line. The server writes
/// the JSON-RPC response back as a text frame. Notifications (no `id`) produce
/// no frame. Binary / Ping / Pong frames are silently ignored.
#[cfg(feature = "websocket")]
pub async fn run_websocket(server: Arc<McpServer>, addr: &str) -> Result<()> {
    use futures::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            match tokio_tungstenite::accept_async(stream).await {
                Ok(ws_stream) => {
                    let (mut sink, mut source) = ws_stream.split();
                    while let Some(msg) = source.next().await {
                        match msg {
                            Ok(Message::Text(text)) => {
                                let line: &str = &text;
                                if let Some(response) = server.handle_line(line).await {
                                    if let Err(e) = sink.send(Message::Text(response)).await {
                                        tracing::warn!(%peer_addr, "ws send error: {e}");
                                        break;
                                    }
                                }
                            }
                            Ok(Message::Close(_)) | Err(_) => break,
                            _ => {}
                        }
                    }
                }
                Err(e) => tracing::warn!(%peer_addr, "ws handshake error: {e}"),
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "sse", feature = "websocket"))]
    use std::sync::Arc;

    #[cfg(any(feature = "sse", feature = "websocket"))]
    use crate::server::McpServer;
    #[cfg(any(feature = "sse", feature = "websocket"))]
    use crate::tool::{Tool, ToolDescriptor, ToolInputSchema, ToolRegistry};
    #[cfg(any(feature = "sse", feature = "websocket"))]
    use async_trait::async_trait;

    #[cfg(any(feature = "sse", feature = "websocket"))]
    struct EchoTool;

    #[cfg(any(feature = "sse", feature = "websocket"))]
    #[async_trait]
    impl Tool for EchoTool {
        fn descriptor(&self) -> ToolDescriptor {
            ToolDescriptor {
                name: "echo".into(),
                description: "echo".into(),
                input_schema: ToolInputSchema::empty(),
            }
        }
        async fn invoke(&self, args: serde_json::Value) -> crate::error::Result<serde_json::Value> {
            Ok(serde_json::json!({ "echoed": args }))
        }
    }

    #[cfg(any(feature = "sse", feature = "websocket"))]
    fn test_server() -> Arc<McpServer> {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        Arc::new(McpServer::new(reg))
    }

    // -----------------------------------------------------------------------
    // WebSocket roundtrip
    // -----------------------------------------------------------------------

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn websocket_initialize_roundtrip() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let server = test_server();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener); // run_websocket will rebind

        let addr_str = addr.to_string();
        let server_clone = Arc::clone(&server);
        tokio::spawn(async move {
            server_clone.run_websocket(&addr_str).await.ok();
        });

        // let server start
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("ws connect");
        let (mut sink, mut source) = ws.split();

        sink.send(Message::Text(
            r#"{"jsonrpc":"2.0","method":"initialize","id":1}"#.to_owned(),
        ))
        .await
        .unwrap();

        let msg = source.next().await.unwrap().unwrap();
        if let Message::Text(text) = msg {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["result"]["serverInfo"]["name"], "oxide-mcp-server");
        } else {
            panic!("expected text frame, got {msg:?}");
        }
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn websocket_tools_call_roundtrip() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let server = test_server();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let addr_str = addr.to_string();
        let server_clone = Arc::clone(&server);
        tokio::spawn(async move {
            server_clone.run_websocket(&addr_str).await.ok();
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        let (mut sink, mut source) = ws.split();

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 2,
            "params": { "name": "echo", "arguments": { "ping": "pong" } }
        });
        sink.send(Message::Text(req.to_string())).await.unwrap();

        let msg = source.next().await.unwrap().unwrap();
        if let Message::Text(text) = msg {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["result"]["structuredContent"]["echoed"]["ping"], "pong");
        } else {
            panic!("expected text frame");
        }
    }

    // -----------------------------------------------------------------------
    // SSE: verify HTTP routes exist and return correct content-type
    // -----------------------------------------------------------------------

    #[cfg(feature = "sse")]
    #[tokio::test]
    async fn sse_get_returns_event_stream_content_type() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let server = test_server();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let addr_str = addr.to_string();
        let server_clone = Arc::clone(&server);
        tokio::spawn(async move {
            server_clone.run_sse(&addr_str).await.ok();
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /sse HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();

        // Read enough to see the HTTP response headers.
        let mut buf = vec![0u8; 2048];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        )
        .await
        .expect("read timeout")
        .unwrap();

        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(
            response.contains("text/event-stream"),
            "expected text/event-stream, got:\n{response}"
        );
        // First SSE event must carry the endpoint path.
        // (May arrive in same read or need another tick — use contains on full buffer)
        // Just verify headers for now; E2E POST→SSE tested manually / integration.
        assert!(response.starts_with("HTTP/1.1 200"), "got:\n{response}");
    }
}
