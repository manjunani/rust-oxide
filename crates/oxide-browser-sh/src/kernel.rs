//! Integration with the `oxide-k` micro-kernel.
//!
//! [`BrowserModule`] wraps a [`BrowserSession`] in an
//! [`oxide_k::module::Module`] so the browser can be managed alongside other
//! kernel modules. While running, the module subscribes to the kernel's
//! message bus and dispatches every [`oxide_k::bus::Command::Invoke`] aimed at
//! its module id to the matching browser action.

use std::sync::Arc;

use async_trait::async_trait;
use oxide_k::bus::{Command, Event, Message, MessageBus};
use oxide_k::module::{Module, ModuleKind, ModuleMetadata};
use oxide_k::{KernelError, Result as KernelResult};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::action::{ScrollDirection, Selector};
use crate::extract::{extract_markdown, ExtractOptions};
use crate::session::BrowserSession;

/// Default module id under which the browser registers on the bus.
pub const DEFAULT_MODULE_ID: &str = "browser";

/// `oxide-k` [`Module`] wrapping a [`BrowserSession`].
pub struct BrowserModule {
    id: String,
    session: Arc<BrowserSession>,
    listener: Option<JoinHandle<()>>,
}

impl BrowserModule {
    /// Build a module with the [`DEFAULT_MODULE_ID`].
    pub fn new(session: Arc<BrowserSession>) -> Self {
        Self::with_id(DEFAULT_MODULE_ID, session)
    }

    /// Build a module with a custom id.
    pub fn with_id(id: impl Into<String>, session: Arc<BrowserSession>) -> Self {
        Self {
            id: id.into(),
            session,
            listener: None,
        }
    }

    /// Access the underlying session.
    pub fn session(&self) -> &Arc<BrowserSession> {
        &self.session
    }
}

#[async_trait]
impl Module for BrowserModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: self.id.clone(),
            name: "Oxide Browser (self-healing)".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: ModuleKind::Native,
            description: Some(
                "Self-healing browser automation; routes bus commands to a BrowserSession."
                    .into(),
            ),
        }
    }

    async fn init(&mut self, bus: MessageBus) -> KernelResult<()> {
        // Subscribe to the bus and spawn a listener that dispatches commands
        // targeted at this module id to the wrapped session.
        let mut subscription = bus.subscribe().await;
        let session = self.session.clone();
        let bus_for_emit = bus.clone();
        let id = self.id.clone();
        let handle = tokio::spawn(async move {
            while let Some(envelope) = subscription.receiver.recv().await {
                let Message::Command(cmd) = envelope.message else { continue };
                let Command::Invoke {
                    module_id,
                    method,
                    payload,
                } = cmd
                else {
                    continue;
                };
                if module_id != id {
                    continue;
                }
                let result = dispatch(&session, &method, payload).await;
                let event = match result {
                    Ok(value) => Event::Custom {
                        module_id: id.clone(),
                        kind: format!("{method}.ok"),
                        payload: value,
                    },
                    Err(err) => Event::Custom {
                        module_id: id.clone(),
                        kind: format!("{method}.err"),
                        payload: serde_json::json!({ "error": err.to_string() }),
                    },
                };
                let _ = bus_for_emit.emit_event(id.clone(), event).await;
            }
        });
        self.listener = Some(handle);
        Ok(())
    }

    async fn start(&mut self) -> KernelResult<()> {
        tracing::info!(module = %self.id, "browser module started");
        Ok(())
    }

    async fn stop(&mut self) -> KernelResult<()> {
        if let Some(handle) = self.listener.take() {
            handle.abort();
        }
        tracing::info!(module = %self.id, "browser module stopped");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Method dispatch
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct NavigatePayload {
    url: String,
}

#[derive(Debug, Deserialize)]
struct ClickPayload {
    selector: Selector,
}

#[derive(Debug, Deserialize)]
struct InputPayload {
    selector: Selector,
    text: String,
}

#[derive(Debug, Deserialize)]
struct ScrollPayload {
    direction: ScrollDirection,
    amount: i32,
}

#[derive(Debug, Deserialize, Default)]
struct ExtractPayload {
    #[serde(default)]
    max_chunk_chars: Option<usize>,
    #[serde(default = "default_strip")]
    strip_boilerplate: bool,
}

fn default_strip() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct EmptyResult {
    ok: bool,
}

async fn dispatch(
    session: &BrowserSession,
    method: &str,
    payload: serde_json::Value,
) -> KernelResult<serde_json::Value> {
    let to_kernel = |e: crate::error::BrowserError| KernelError::Other(anyhow::anyhow!(e));
    match method {
        "navigate" => {
            let p: NavigatePayload = serde_json::from_value(payload)?;
            session.navigate(&p.url).await.map_err(to_kernel)?;
            Ok(serde_json::to_value(EmptyResult { ok: true })?)
        }
        "click" => {
            let p: ClickPayload = serde_json::from_value(payload)?;
            session.click(&p.selector).await.map_err(to_kernel)?;
            Ok(serde_json::to_value(EmptyResult { ok: true })?)
        }
        "input" => {
            let p: InputPayload = serde_json::from_value(payload)?;
            session
                .input(&p.selector, &p.text)
                .await
                .map_err(to_kernel)?;
            Ok(serde_json::to_value(EmptyResult { ok: true })?)
        }
        "scroll" => {
            let p: ScrollPayload = serde_json::from_value(payload)?;
            session
                .scroll(p.direction, p.amount)
                .await
                .map_err(to_kernel)?;
            Ok(serde_json::to_value(EmptyResult { ok: true })?)
        }
        "extract" => {
            let p: ExtractPayload = serde_json::from_value(payload).unwrap_or_default();
            let html = session.backend().html().await.map_err(to_kernel)?;
            let opts = ExtractOptions {
                max_chunk_chars: p.max_chunk_chars,
                strip_boilerplate: p.strip_boilerplate,
            };
            let extracted = extract_markdown(&html, &opts);
            Ok(serde_json::to_value(extracted)?)
        }
        "stats" => Ok(serde_json::to_value(session.stats())?),
        other => Err(KernelError::Other(anyhow::anyhow!(
            "unknown browser method `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::healing::DefaultHealing;
    use crate::mock::MockBackend;
    use oxide_k::bus::{Event, Message};

    const HTML: &str = r#"<!doctype html>
        <html>
          <head><title>Bus test</title></head>
          <body>
            <h1>Hi</h1>
            <button id="cta">Go</button>
          </body>
        </html>"#;

    #[tokio::test]
    async fn dispatches_navigate_and_emits_event() {
        let backend = Arc::new(MockBackend::new());
        backend.register_page("/", HTML);
        let session = Arc::new(BrowserSession::new(
            backend.clone(),
            Arc::new(DefaultHealing::new()),
        ));
        let mut module = BrowserModule::new(session.clone());

        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        Module::init(&mut module, bus.clone()).await.unwrap();
        Module::start(&mut module).await.unwrap();

        bus.send_command(
            "test",
            Command::Invoke {
                module_id: DEFAULT_MODULE_ID.into(),
                method: "navigate".into(),
                payload: serde_json::json!({"url": "/"}),
            },
        )
        .await
        .unwrap();

        // Drain envelopes until we hit our navigate.ok event.
        let mut saw_ok = false;
        for _ in 0..10 {
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                sub.receiver.recv(),
            )
            .await
            {
                Ok(Some(env)) => {
                    if let Message::Event(Event::Custom { kind, .. }) = env.message {
                        if kind == "navigate.ok" {
                            saw_ok = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        Module::stop(&mut module).await.unwrap();
        assert!(saw_ok, "expected to see navigate.ok event");
        assert_eq!(session.backend().url().await.unwrap(), "/");
    }
}
