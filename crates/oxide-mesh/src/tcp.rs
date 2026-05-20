//! JSON-line framed TCP transport — plain and TLS.
//!
//! `TcpMesh` is a thin server: it accepts connections, frames messages as one
//! JSON-encoded [`PeerMessage`] per line, and forwards each into the supplied
//! handler. Outbound dispatch happens through the same [`LocalMesh`] used by
//! in-process peers, so a TCP peer behaves identically to a local one once
//! its `Hello` has been processed.
//!
//! # TLS
//!
//! Enable the `tls` Cargo feature to unlock [`TcpMesh::serve_tls`] and
//! [`TcpMesh::connect_tls`]. Both sides authenticate with a certificate.
//! For mutual TLS (mTLS) supply a `ClientConfig` that includes a client
//! certificate — the server already requests client certs when built with
//! [`tls_server_config`].

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::error::Result;
use crate::local::LocalMesh;
use crate::message::PeerMessage;

/// TCP mesh server bound to a [`LocalMesh`] for routing.
pub struct TcpMesh {
    local: LocalMesh,
}

impl TcpMesh {
    /// Build a TCP wrapper around `local`.
    pub fn new(local: LocalMesh) -> Self {
        Self { local }
    }

    /// Bind to `addr` and serve **plain** TCP connections until cancellation.
    pub async fn serve(self, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await?;
        tracing::info!(%addr, "tcp mesh listening (plain)");
        loop {
            let (socket, peer) = listener.accept().await?;
            tracing::debug!(?peer, "tcp peer connected");
            let local = self.local.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(socket, local).await {
                    tracing::warn!(?peer, ?e, "tcp connection ended");
                }
            });
        }
    }

    /// Bind to `addr` and serve **TLS** connections.
    ///
    /// `acceptor` is a [`tokio_rustls::TlsAcceptor`] built from a
    /// [`rustls::ServerConfig`]. Use [`tls_server_config`] to build one from
    /// PEM bytes, or construct it manually for mTLS.
    #[cfg(feature = "tls")]
    pub async fn serve_tls(
        self,
        addr: SocketAddr,
        acceptor: tokio_rustls::TlsAcceptor,
    ) -> Result<()> {
        let listener = TcpListener::bind(addr).await?;
        tracing::info!(%addr, "tcp mesh listening (tls)");
        loop {
            let (socket, peer) = listener.accept().await?;
            let acceptor = acceptor.clone();
            let local = self.local.clone();
            tokio::spawn(async move {
                match acceptor.accept(socket).await {
                    Ok(tls) => {
                        if let Err(e) = handle_connection(tls, local).await {
                            tracing::warn!(?peer, ?e, "tls connection ended");
                        }
                    }
                    Err(e) => tracing::warn!(?peer, ?e, "tls handshake failed"),
                }
            });
        }
    }

    /// Connect to a remote `addr` over **plain** TCP, send `hello`, and
    /// return a [`TcpClient`].
    pub async fn connect(addr: SocketAddr, hello: PeerMessage) -> Result<TcpClient> {
        let mut socket = TcpStream::connect(addr).await?;
        let line = format!("{}\n", serde_json::to_string(&hello)?);
        socket.write_all(line.as_bytes()).await?;
        Ok(TcpClient {
            inner: Arc::new(Mutex::new(
                Box::new(socket) as Box<dyn AsyncWrite + Send + Unpin>
            )),
        })
    }

    /// Connect to a remote `addr` over **TLS**, send `hello`, and return a
    /// [`TcpClient`].
    ///
    /// `server_name` must match the CN / SAN in the server certificate.
    /// `connector` is a [`tokio_rustls::TlsConnector`] built from a
    /// [`rustls::ClientConfig`] that trusts the server's CA.
    #[cfg(feature = "tls")]
    pub async fn connect_tls(
        addr: SocketAddr,
        hello: PeerMessage,
        server_name: tokio_rustls::rustls::pki_types::ServerName<'static>,
        connector: tokio_rustls::TlsConnector,
    ) -> Result<TcpClient> {
        let stream = TcpStream::connect(addr).await?;
        let mut tls = connector.connect(server_name, stream).await?;
        let line = format!("{}\n", serde_json::to_string(&hello)?);
        tls.write_all(line.as_bytes()).await?;
        Ok(TcpClient {
            inner: Arc::new(Mutex::new(
                Box::new(tls) as Box<dyn AsyncWrite + Send + Unpin>
            )),
        })
    }
}

/// Client handle returned by [`TcpMesh::connect`] or [`TcpMesh::connect_tls`].
#[derive(Clone)]
pub struct TcpClient {
    inner: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
}

impl TcpClient {
    /// Send `msg` as a single JSON line.
    pub async fn send(&self, msg: &PeerMessage) -> Result<()> {
        let line = format!("{}\n", serde_json::to_string(msg)?);
        self.inner.lock().await.write_all(line.as_bytes()).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Generic connection handler — works over plain TCP and TLS streams.
// ---------------------------------------------------------------------------

async fn handle_connection<S>(socket: S, local: LocalMesh) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    let mut sender_id: Option<String> = None;
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: PeerMessage = match serde_json::from_str(trimmed) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(?e, "discarding malformed line");
                continue;
            }
        };
        if let PeerMessage::Hello { from, capabilities } = &msg {
            sender_id = Some(from.clone());
            let _ = local
                .join(from.clone(), capabilities.clone(), Vec::new())
                .await;
        }
        let sender = sender_id.clone().unwrap_or_else(|| msg.sender().clone());
        let (_p, handle) = local
            .join(format!("ephemeral:{sender}"), Vec::new(), Vec::new())
            .await?;
        handle.publish(msg).await?;
        local.leave(&handle.id).await?;
    }
    if let Some(id) = sender_id {
        let _ = local.leave(&id).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// TLS config helpers
// ---------------------------------------------------------------------------

/// Build a [`rustls::ServerConfig`] from PEM-encoded certificate + private key.
///
/// The returned config does **not** request client certificates; for mTLS
/// call `.with_client_cert_verifier(...)` on the builder instead.
#[cfg(feature = "tls")]
pub fn tls_server_config(cert_pem: &[u8], key_pem: &[u8]) -> anyhow::Result<rustls::ServerConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls_pemfile::{certs, private_key};
    use std::io::Cursor;

    let certs: Vec<CertificateDer<'static>> = certs(&mut Cursor::new(cert_pem))
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("cert parse: {e}"))?;

    let key: PrivateKeyDer<'static> = private_key(&mut Cursor::new(key_pem))
        .map_err(|e| anyhow::anyhow!("key parse: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("no private key found"))?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow::anyhow!("tls config: {e}"))?;
    Ok(config)
}

/// Build a [`rustls::ClientConfig`] that trusts a single CA certificate (PEM).
///
/// Suitable for connecting to servers that present a self-signed or
/// `rcgen`-generated certificate.
#[cfg(feature = "tls")]
pub fn tls_client_config(ca_cert_pem: &[u8]) -> anyhow::Result<rustls::ClientConfig> {
    use rustls::pki_types::CertificateDer;
    use rustls::RootCertStore;
    use rustls_pemfile::certs;
    use std::io::Cursor;

    let mut roots = RootCertStore::empty();
    for cert in certs(&mut Cursor::new(ca_cert_pem))
        .collect::<std::result::Result<Vec<CertificateDer<'static>>, _>>()
        .map_err(|e| anyhow::anyhow!("ca cert parse: {e}"))?
    {
        roots
            .add(cert)
            .map_err(|e| anyhow::anyhow!("add root: {e}"))?;
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::PeerCapability;
    use serde_json::json;
    use std::net::Ipv4Addr;

    fn caps(name: &str) -> Vec<PeerCapability> {
        vec![PeerCapability {
            name: name.into(),
            version: None,
        }]
    }

    #[tokio::test]
    async fn tcp_round_trip_delivers_broadcast() {
        let local = LocalMesh::new();
        let (mut listener_handle, _h) = local.join("listener", caps("x"), vec![]).await.unwrap();

        let server = TcpMesh::new(local.clone());
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let local_clone = local.clone();
        let accept_task = tokio::spawn(async move {
            let _ = server;
            while let Ok((socket, _)) = listener.accept().await {
                let local = local_clone.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(socket, local).await;
                });
            }
        });

        let hello = PeerMessage::Hello {
            from: "remote".into(),
            capabilities: caps("remote"),
        };
        let client = TcpMesh::connect(addr, hello).await.unwrap();
        client
            .send(&PeerMessage::broadcast("remote", "topic", json!({"v": 1})))
            .await
            .unwrap();

        let mut saw_broadcast = false;
        for _ in 0..6 {
            let recv = tokio::time::timeout(
                std::time::Duration::from_millis(400),
                listener_handle.receiver.recv(),
            )
            .await;
            match recv {
                Ok(Some(PeerMessage::Broadcast { from, topic, .. })) => {
                    assert_eq!(from, "remote");
                    assert_eq!(topic, "topic");
                    saw_broadcast = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(saw_broadcast, "expected a Broadcast to arrive");
        accept_task.abort();
    }

    /// End-to-end TLS round-trip using an rcgen self-signed certificate.
    #[cfg(feature = "tls")]
    #[tokio::test]
    async fn tls_round_trip_delivers_broadcast() {
        use rcgen::generate_simple_self_signed;
        use rustls::pki_types::ServerName;
        use std::sync::Arc;
        use tokio_rustls::{TlsAcceptor, TlsConnector};

        // Install ring as the default crypto provider (idempotent).
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Generate a self-signed cert for "localhost".
        let cert = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_pem = cert.cert.pem();
        let key_pem = cert.key_pair.serialize_pem();

        // Server config.
        let server_cfg = tls_server_config(cert_pem.as_bytes(), key_pem.as_bytes()).unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_cfg));

        // Client config — trusts the same cert as CA.
        let client_cfg = tls_client_config(cert_pem.as_bytes()).unwrap();
        let connector = TlsConnector::from(Arc::new(client_cfg));

        let local = LocalMesh::new();
        let (mut listener_handle, _h) = local.join("listener", caps("x"), vec![]).await.unwrap();

        // Bind the TLS server manually so we can pick an ephemeral port.
        let std_listener =
            std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let tls_listener = TcpListener::from_std(std_listener).unwrap();
        let addr = tls_listener.local_addr().unwrap();

        let local_clone = local.clone();
        let accept_task = tokio::spawn(async move {
            while let Ok((socket, peer)) = tls_listener.accept().await {
                let acceptor = acceptor.clone();
                let local = local_clone.clone();
                tokio::spawn(async move {
                    match acceptor.accept(socket).await {
                        Ok(tls) => {
                            let _ = handle_connection(tls, local).await;
                        }
                        Err(e) => tracing::warn!(?peer, ?e, "tls handshake failed in test"),
                    }
                });
            }
        });

        let hello = PeerMessage::Hello {
            from: "tls-remote".into(),
            capabilities: caps("tls-remote"),
        };
        let server_name = ServerName::try_from("localhost").unwrap();
        let client = TcpMesh::connect_tls(addr, hello, server_name, connector)
            .await
            .unwrap();
        client
            .send(&PeerMessage::broadcast(
                "tls-remote",
                "tls-topic",
                json!({"secure": true}),
            ))
            .await
            .unwrap();

        let mut saw_broadcast = false;
        for _ in 0..6 {
            let recv = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                listener_handle.receiver.recv(),
            )
            .await;
            match recv {
                Ok(Some(PeerMessage::Broadcast { from, topic, .. })) => {
                    assert_eq!(from, "tls-remote");
                    assert_eq!(topic, "tls-topic");
                    saw_broadcast = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(saw_broadcast, "expected a TLS Broadcast to arrive");
        accept_task.abort();
    }
}
