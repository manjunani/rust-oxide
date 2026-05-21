//! # Bus Bridge — bus-over-TLS for cross-process kernels (R-21)
//!
//! [`BusBridge`] relays `oxide_k::bus::Envelope` messages between two kernel
//! instances over a TLS-secured TCP connection using the JSON-line framing
//! already established in [`crate::tcp`].
//!
//! ## Protocol
//!
//! Each side sends one JSON-serialized [`oxide_k::bus::Envelope`] per line
//! (`\n` terminated). TLS provides confidentiality and peer authentication.
//!
//! ## Usage
//!
//! ```text
//! // Kernel A (server)
//! BusBridge::serve_tls(addr, bus_a, acceptor).await;
//!
//! // Kernel B (client)
//! let bridge = BusBridge::connect_tls(addr, connector, server_name).await?;
//! bridge.forward(&envelope).await?;
//! ```
//!
//! Requires the `tls` Cargo feature on `oxide-mesh`.

#[cfg(feature = "tls")]
use std::net::SocketAddr;
use std::sync::Arc;

use oxide_k::bus::{Envelope, MessageBus};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(feature = "tls")]
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::error::{MeshError, Result};

/// A handle to a TLS connection between two kernels' buses.
///
/// Obtain via [`BusBridge::connect_tls`]. Call [`BusBridge::forward`] to send
/// an envelope to the remote kernel.
#[derive(Clone)]
pub struct BusBridge {
    inner: Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>>,
}

impl BusBridge {
    /// Connect to a remote kernel's bus bridge over TLS.
    ///
    /// `server_name` must match the CN/SAN of the server certificate.
    /// `connector` is built from a [`rustls::ClientConfig`] that trusts the
    /// server's CA (see [`crate::tcp::tls_client_config`]).
    #[cfg(feature = "tls")]
    pub async fn connect_tls(
        addr: SocketAddr,
        connector: tokio_rustls::TlsConnector,
        server_name: tokio_rustls::rustls::pki_types::ServerName<'static>,
    ) -> Result<Self> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| MeshError::Other(anyhow::anyhow!(e)))?;
        let tls = connector
            .connect(server_name, stream)
            .await
            .map_err(|e| MeshError::Other(anyhow::anyhow!(e)))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(
                Box::new(tls) as Box<dyn tokio::io::AsyncWrite + Send + Unpin>
            )),
        })
    }

    /// Send an [`Envelope`] to the remote kernel.
    ///
    /// The envelope is serialized to a single JSON line and written over the
    /// TLS stream.
    pub async fn forward(&self, envelope: &Envelope) -> Result<()> {
        let json =
            serde_json::to_string(envelope).map_err(|e| MeshError::Other(anyhow::anyhow!(e)))?;
        let line = format!("{json}\n");
        self.inner
            .lock()
            .await
            .write_all(line.as_bytes())
            .await
            .map_err(|e| MeshError::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    /// Serve a TLS bus bridge: accept connections and relay each received
    /// [`Envelope`] into `bus`.
    ///
    /// Each inbound JSON line is deserialized as an `Envelope` and published
    /// on `bus`. Malformed lines are logged and skipped.
    ///
    /// This function runs indefinitely; cancel it with
    /// `tokio::task::JoinHandle::abort()`.
    #[cfg(feature = "tls")]
    pub async fn serve_tls(
        addr: SocketAddr,
        bus: MessageBus,
        acceptor: tokio_rustls::TlsAcceptor,
    ) -> Result<()> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| MeshError::Other(anyhow::anyhow!(e)))?;
        tracing::info!(%addr, "bus bridge listening (tls)");

        loop {
            let (stream, peer) = listener
                .accept()
                .await
                .map_err(|e| MeshError::Other(anyhow::anyhow!(e)))?;
            let acceptor = acceptor.clone();
            let bus = bus.clone();

            tokio::spawn(async move {
                match acceptor.accept(stream).await {
                    Ok(tls) => {
                        if let Err(e) = relay_inbound(tls, bus).await {
                            tracing::warn!(?peer, ?e, "bus bridge connection ended");
                        }
                    }
                    Err(e) => tracing::warn!(?peer, ?e, "bus bridge tls handshake failed"),
                }
            });
        }
    }
}

/// Read JSON-line envelopes from `stream` and publish each onto `bus`.
#[cfg(feature = "tls")]
async fn relay_inbound<S>(stream: S, bus: MessageBus) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| MeshError::Other(anyhow::anyhow!(e)))?;
        if n == 0 {
            break; // EOF — remote closed
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Envelope>(trimmed) {
            Ok(env) => {
                if let Err(e) = bus.publish(env).await {
                    tracing::warn!(?e, "bus bridge: failed to publish relay envelope");
                }
            }
            Err(e) => tracing::warn!(?e, "bus bridge: discarding malformed envelope line"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "tls")]
    use oxide_k::bus::{Command, Message};
    #[cfg(feature = "tls")]
    use std::net::Ipv4Addr;

    /// End-to-end: bridge two in-process buses over TLS using rcgen certs.
    #[cfg(feature = "tls")]
    #[tokio::test]
    async fn bus_bridge_tls_relays_envelope() {
        use crate::tcp::{tls_client_config, tls_server_config};
        use rcgen::generate_simple_self_signed;
        use std::sync::Arc;
        use tokio_rustls::{TlsAcceptor, TlsConnector};

        // Install ring provider (idempotent).
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Generate self-signed cert.
        let cert = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_pem = cert.cert.pem();
        let key_pem = cert.key_pair.serialize_pem();

        let server_cfg = tls_server_config(cert_pem.as_bytes(), key_pem.as_bytes()).unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_cfg));

        let client_cfg = tls_client_config(cert_pem.as_bytes()).unwrap();
        let connector = TlsConnector::from(Arc::new(client_cfg));

        // Server bus — receives relayed messages.
        let server_bus = MessageBus::new();
        let mut sub = server_bus.subscribe().await;

        // Bind server.
        let std_listener =
            std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let tls_listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
        let addr = tls_listener.local_addr().unwrap();

        let bus_srv = server_bus.clone();
        tokio::spawn(async move {
            let (stream, peer) = tls_listener.accept().await.unwrap();
            match acceptor.accept(stream).await {
                Ok(tls) => {
                    relay_inbound(tls, bus_srv).await.ok();
                }
                Err(e) => tracing::warn!(?peer, ?e, "test tls handshake failed"),
            }
        });

        // Client connects and forwards an envelope.
        let server_name =
            tokio_rustls::rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let bridge = BusBridge::connect_tls(addr, connector, server_name)
            .await
            .unwrap();

        let env = oxide_k::bus::Envelope::new("remote-kernel", Message::Command(Command::Ping));
        bridge.forward(&env).await.unwrap();

        // Server bus should receive the relayed Ping.
        let received =
            tokio::time::timeout(std::time::Duration::from_millis(500), sub.receiver.recv())
                .await
                .expect("timeout")
                .expect("channel closed");

        assert_eq!(received.source, "remote-kernel");
        assert!(matches!(received.message, Message::Command(Command::Ping)));
    }
}
