//! `oxide-k` bus integration.
//!
//! [`LlmModule`] wraps an [`LlmClient`] and exposes three methods on the
//! kernel bus:
//!
//! | method        | payload                                            | returns                 |
//! |---------------|----------------------------------------------------|-------------------------|
//! | `complete`    | `{"messages": [...], "temperature": f, ...}`       | `{"content": ..., ...}` |
//! | `summarize`   | `{"text": ..., "target_words": n, "focus": ...}`   | `{"summary": ...}`      |
//! | `analyze`     | `{"operation": ..., "error": ..., "context": ...}` | `{"analysis": ...}`     |

use std::sync::Arc;

use async_trait::async_trait;
use oxide_k::bus::{Command, Event, Message, MessageBus};
use oxide_k::module::{Module, ModuleKind, ModuleMetadata};
use oxide_k::{KernelError, Result as KernelResult};
use serde::Deserialize;
use tokio::task::JoinHandle;

use crate::client::{ChatMessage, ChatRequest, LlmClient, ResponseFormat};
use crate::prompts::{ErrorAnalysisInput, PromptTemplate, SummarizeInput};

/// Default module id under which the orchestrator registers on the bus.
pub const DEFAULT_MODULE_ID: &str = "llm";

/// LLM module that responds to bus invocations.
pub struct LlmModule {
    id: String,
    client: Arc<dyn LlmClient>,
    default_model: String,
    listener: Option<JoinHandle<()>>,
}

impl LlmModule {
    /// Build a module with [`DEFAULT_MODULE_ID`].
    pub fn new(client: Arc<dyn LlmClient>, default_model: impl Into<String>) -> Self {
        Self::with_id(DEFAULT_MODULE_ID, client, default_model)
    }

    /// Build a module with a custom id.
    pub fn with_id(
        id: impl Into<String>,
        client: Arc<dyn LlmClient>,
        default_model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            client,
            default_model: default_model.into(),
            listener: None,
        }
    }
}

#[async_trait]
impl Module for LlmModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: self.id.clone(),
            name: "Oxide LLM Orchestrator".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: ModuleKind::Native,
            description: Some(
                "Routes chat / summarisation / error-analysis requests to an LLM client.".into(),
            ),
        }
    }

    async fn init(&mut self, bus: MessageBus) -> KernelResult<()> {
        let mut subscription = bus.subscribe().await;
        let client = self.client.clone();
        let default_model = self.default_model.clone();
        let id = self.id.clone();
        let bus_for_emit = bus.clone();
        let handle = tokio::spawn(async move {
            while let Some(envelope) = subscription.receiver.recv().await {
                let Message::Command(Command::Invoke {
                    module_id,
                    method,
                    payload,
                }) = envelope.message
                else {
                    continue;
                };
                if module_id != id {
                    continue;
                }
                let result = dispatch(&client, &default_model, &method, payload).await;
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
        tracing::info!(module = %self.id, "llm module started");
        Ok(())
    }

    async fn stop(&mut self) -> KernelResult<()> {
        if let Some(h) = self.listener.take() {
            h.abort();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CompletePayload {
    #[serde(default)]
    model: Option<String>,
    messages: Vec<ChatMessage>,
    #[serde(default = "default_temperature")]
    temperature: f32,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    json: bool,
}

fn default_temperature() -> f32 {
    0.2
}

#[derive(Debug, Deserialize)]
struct SummarizePayload {
    text: String,
    #[serde(default = "default_target_words")]
    target_words: u32,
    #[serde(default)]
    focus: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

fn default_target_words() -> u32 {
    100
}

#[derive(Debug, Deserialize)]
struct AnalyzePayload {
    operation: String,
    error: String,
    #[serde(default)]
    context: serde_json::Value,
    #[serde(default)]
    model: Option<String>,
}

async fn dispatch(
    client: &Arc<dyn LlmClient>,
    default_model: &str,
    method: &str,
    payload: serde_json::Value,
) -> KernelResult<serde_json::Value> {
    let to_kernel = |e: crate::error::LlmError| KernelError::Other(anyhow::anyhow!(e));

    match method {
        "complete" => {
            let p: CompletePayload = serde_json::from_value(payload)?;
            let request = ChatRequest {
                model: p.model.unwrap_or_else(|| default_model.to_string()),
                messages: p.messages,
                temperature: p.temperature,
                max_tokens: p.max_tokens,
                response_format: p.json.then_some(ResponseFormat::JsonObject),
                tools: Vec::new(),
            };
            let response = client.complete(request).await.map_err(to_kernel)?;
            Ok(serde_json::to_value(response)?)
        }
        "summarize" => {
            let p: SummarizePayload = serde_json::from_value(payload)?;
            let model = p.model.unwrap_or_else(|| default_model.to_string());
            let request = PromptTemplate::Summarize.build_request(
                &model,
                &SummarizeInput {
                    text: p.text,
                    target_words: p.target_words,
                    focus: p.focus,
                },
            );
            let response = client.complete(request).await.map_err(to_kernel)?;
            Ok(serde_json::json!({
                "summary": response.content.trim(),
                "model": response.model,
                "usage": response.usage,
            }))
        }
        "analyze" => {
            let p: AnalyzePayload = serde_json::from_value(payload)?;
            let model = p.model.unwrap_or_else(|| default_model.to_string());
            let request = PromptTemplate::ErrorAnalysis.build_request(
                &model,
                &ErrorAnalysisInput {
                    operation: p.operation,
                    error: p.error,
                    context: p.context,
                },
            );
            let response = client.complete(request).await.map_err(to_kernel)?;
            let parsed: serde_json::Value = serde_json::from_str(&response.content)
                .unwrap_or_else(|_| serde_json::json!({ "raw": response.content }));
            Ok(serde_json::json!({
                "analysis": parsed,
                "model": response.model,
                "usage": response.usage,
            }))
        }
        other => Err(KernelError::Other(anyhow::anyhow!(
            "unknown llm method `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ChatResponse, MockLlmClient};
    use oxide_k::bus::{Event, Message};

    #[tokio::test]
    async fn bus_summarize_returns_summary() {
        let client: Arc<dyn LlmClient> = Arc::new(MockLlmClient::single("short."));
        let mut module = LlmModule::new(client, "mock-model");

        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        Module::init(&mut module, bus.clone()).await.unwrap();
        Module::start(&mut module).await.unwrap();

        bus.send_command(
            "test",
            Command::Invoke {
                module_id: DEFAULT_MODULE_ID.into(),
                method: "summarize".into(),
                payload: serde_json::json!({
                    "text": "Long story here",
                    "target_words": 5
                }),
            },
        )
        .await
        .unwrap();

        let mut saw = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), sub.receiver.recv())
                .await
            {
                Ok(Some(env)) => {
                    if let Message::Event(Event::Custom { kind, payload, .. }) = env.message {
                        if kind == "summarize.ok" {
                            assert_eq!(payload["summary"], serde_json::json!("short."));
                            saw = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(saw, "expected summarize.ok event");
        Module::stop(&mut module).await.unwrap();
    }

    #[tokio::test]
    async fn bus_complete_returns_chat_response() {
        let client: Arc<dyn LlmClient> = Arc::new(MockLlmClient::new(vec![ChatResponse {
            content: "hi back".into(),
            tool_calls: Vec::new(),
            model: "mock-model".into(),
            usage: None,
        }]));
        let mut module = LlmModule::new(client, "mock-model");
        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        Module::init(&mut module, bus.clone()).await.unwrap();
        Module::start(&mut module).await.unwrap();

        bus.send_command(
            "test",
            Command::Invoke {
                module_id: DEFAULT_MODULE_ID.into(),
                method: "complete".into(),
                payload: serde_json::json!({
                    "messages": [{"role": "user", "content": "hi"}]
                }),
            },
        )
        .await
        .unwrap();

        let mut saw = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), sub.receiver.recv())
                .await
            {
                Ok(Some(env)) => {
                    if let Message::Event(Event::Custom { kind, payload, .. }) = env.message {
                        if kind == "complete.ok" {
                            assert_eq!(payload["content"], serde_json::json!("hi back"));
                            saw = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(saw);
        Module::stop(&mut module).await.unwrap();
    }
}
