//! `oxide-k` bus integration.
//!
//! [`MirrorModule`] wraps a [`Syncer`] in an [`oxide_k::module::Module`]. It
//! subscribes to the kernel bus and responds to `Command::Invoke` messages
//! targeted at its module id (`"mirror"` by default). Every result is emitted
//! back on the bus as a `Custom` event named `<method>.{ok,err}`.

use std::sync::Arc;

use async_trait::async_trait;
use oxide_k::bus::{Command, Event, Message, MessageBus};
use oxide_k::module::{Module, ModuleKind, ModuleMetadata};
use oxide_k::{KernelError, Result as KernelResult};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::sync::Syncer;

/// Default module id under which the mirror registers on the bus.
pub const DEFAULT_MODULE_ID: &str = "mirror";

/// Mirror module wrapping a [`Syncer`].
pub struct MirrorModule {
    id: String,
    syncer: Arc<Mutex<Syncer>>,
    listener: Option<JoinHandle<()>>,
}

impl MirrorModule {
    /// Build a module with the default id.
    pub fn new(syncer: Syncer) -> Self {
        Self::with_id(DEFAULT_MODULE_ID, syncer)
    }

    /// Build a module with a custom id.
    pub fn with_id(id: impl Into<String>, syncer: Syncer) -> Self {
        Self {
            id: id.into(),
            syncer: Arc::new(Mutex::new(syncer)),
            listener: None,
        }
    }

    /// Access the wrapped syncer for direct calls (e.g. `register_source`).
    pub fn syncer(&self) -> Arc<Mutex<Syncer>> {
        self.syncer.clone()
    }
}

#[async_trait]
impl Module for MirrorModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: self.id.clone(),
            name: "Oxide Mirror".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: ModuleKind::Native,
            description: Some(
                "Local event-sourced data mirror; pulls deltas from SyncSources, persists to SQLite, answers read-only SQL queries.".into(),
            ),
        }
    }

    async fn init(&mut self, bus: MessageBus) -> KernelResult<()> {
        let mut subscription = bus.subscribe().await;
        let syncer = self.syncer.clone();
        let id = self.id.clone();
        let bus_for_emit = bus.clone();
        let handle = tokio::spawn(async move {
            while let Some(envelope) = subscription.receiver.recv().await {
                let Message::Command(cmd) = envelope.message else {
                    continue;
                };
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
                let result = dispatch(&syncer, &method, payload).await;
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
        tracing::info!(module = %self.id, "mirror module started");
        Ok(())
    }

    async fn stop(&mut self) -> KernelResult<()> {
        if let Some(handle) = self.listener.take() {
            handle.abort();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SyncPayload {
    source: String,
}

#[derive(Debug, Deserialize)]
struct QueryPayload {
    sql: String,
}

#[derive(Debug, Deserialize)]
struct GetRecordPayload {
    resource: String,
    record_id: String,
}

#[derive(Debug, Deserialize)]
struct ListRecordsPayload {
    resource: String,
}

async fn dispatch(
    syncer: &Arc<Mutex<Syncer>>,
    method: &str,
    payload: serde_json::Value,
) -> KernelResult<serde_json::Value> {
    let to_kernel = |e: crate::error::MirrorError| KernelError::Other(anyhow::anyhow!(e));

    match method {
        "sync" => {
            let p: SyncPayload = serde_json::from_value(payload)?;
            let report = {
                let syncer = syncer.lock().await;
                syncer.sync_source(&p.source).await.map_err(to_kernel)?
            };
            Ok(serde_json::to_value(report)?)
        }
        "query" => {
            let p: QueryPayload = serde_json::from_value(payload)?;
            let rows = {
                let syncer = syncer.lock().await;
                syncer.store().query(&p.sql).await.map_err(to_kernel)?
            };
            Ok(serde_json::json!({ "rows": rows, "count": rows.len() }))
        }
        "get_record" => {
            let p: GetRecordPayload = serde_json::from_value(payload)?;
            let rec = {
                let syncer = syncer.lock().await;
                syncer
                    .store()
                    .get_record(&p.resource, &p.record_id)
                    .await
                    .map_err(to_kernel)?
            };
            Ok(serde_json::to_value(rec)?)
        }
        "list_records" => {
            let p: ListRecordsPayload = serde_json::from_value(payload)?;
            let recs = {
                let syncer = syncer.lock().await;
                syncer
                    .store()
                    .list_records(&p.resource)
                    .await
                    .map_err(to_kernel)?
            };
            Ok(serde_json::to_value(recs)?)
        }
        "resources" => {
            let resources = {
                let syncer = syncer.lock().await;
                syncer.store().list_resources().await.map_err(to_kernel)?
            };
            Ok(serde_json::json!({ "resources": resources }))
        }
        "counts" => {
            let counts = {
                let syncer = syncer.lock().await;
                syncer.store().record_counts().await.map_err(to_kernel)?
            };
            Ok(serde_json::to_value(counts)?)
        }
        other => Err(KernelError::Other(anyhow::anyhow!(
            "unknown mirror method `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Delta;
    use crate::source::StaticSource;
    use oxide_k::bus::{Event, Message};
    use serde_json::json;

    #[tokio::test]
    async fn bus_dispatches_sync_and_query() {
        let store = crate::store::MirrorStore::in_memory().await.unwrap();
        let mut syncer = Syncer::new(store);
        syncer.register_source(Arc::new(StaticSource::from_deltas(
            "static",
            vec![
                Delta::upsert("pets", "1", json!({"name": "Rex"}), "static"),
                Delta::upsert("pets", "2", json!({"name": "Buddy"}), "static"),
            ],
        )));
        let mut module = MirrorModule::new(syncer);

        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        Module::init(&mut module, bus.clone()).await.unwrap();
        Module::start(&mut module).await.unwrap();

        // Trigger a sync.
        bus.send_command(
            "test",
            Command::Invoke {
                module_id: DEFAULT_MODULE_ID.into(),
                method: "sync".into(),
                payload: json!({"source": "static"}),
            },
        )
        .await
        .unwrap();

        let mut saw_sync_ok = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), sub.receiver.recv())
                .await
            {
                Ok(Some(env)) => {
                    if let Message::Event(Event::Custom { kind, payload, .. }) = env.message {
                        if kind == "sync.ok" {
                            assert_eq!(payload["pulled"], json!(2));
                            assert_eq!(payload["applied"], json!(2));
                            saw_sync_ok = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(saw_sync_ok, "expected sync.ok event");

        // Now run a query through the bus.
        bus.send_command(
            "test",
            Command::Invoke {
                module_id: DEFAULT_MODULE_ID.into(),
                method: "query".into(),
                payload: json!({
                    "sql": "SELECT resource, COUNT(*) as n FROM mirror_records GROUP BY resource"
                }),
            },
        )
        .await
        .unwrap();

        let mut saw_query_ok = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), sub.receiver.recv())
                .await
            {
                Ok(Some(env)) => {
                    if let Message::Event(Event::Custom { kind, payload, .. }) = env.message {
                        if kind == "query.ok" {
                            assert_eq!(payload["count"], json!(1));
                            assert_eq!(payload["rows"][0]["resource"], json!("pets"));
                            assert_eq!(payload["rows"][0]["n"], json!(2));
                            saw_query_ok = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(saw_query_ok, "expected query.ok event");

        Module::stop(&mut module).await.unwrap();
    }
}
