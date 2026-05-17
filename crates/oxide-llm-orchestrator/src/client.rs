//! Chat-completion client trait + concrete implementations.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{LlmError, Result};

/// The role of a [`ChatMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    /// System / instruction message.
    System,
    /// End-user message.
    User,
    /// Prior assistant turn.
    Assistant,
    /// Tool / function-call message.
    Tool,
}

/// A single chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role of the speaker.
    pub role: ChatRole,
    /// Message body.
    pub content: String,
}

impl ChatMessage {
    /// Convenience: build a `system` message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }
    /// Convenience: build a `user` message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }
    /// Convenience: build an `assistant` message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

/// Per-provider response-format hint. Matches OpenAI's `response_format`
/// shape: `{"type": "text"}` or `{"type": "json_object"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Plain text completion.
    Text,
    /// Provider should constrain output to a JSON object.
    JsonObject,
}

/// Request shape sent to the provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Model identifier (e.g. `gpt-4o-mini`, `claude-3-5-sonnet`).
    pub model: String,
    /// Conversation so far.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature (`0.0` ‒ `2.0`).
    pub temperature: f32,
    /// Hard cap on output tokens. `None` = provider default.
    pub max_tokens: Option<u32>,
    /// Optional response format constraint.
    pub response_format: Option<ResponseFormat>,
}

impl ChatRequest {
    /// Build a minimal request with a single user message.
    pub fn simple(model: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: vec![ChatMessage::user(user)],
            temperature: 0.2,
            max_tokens: None,
            response_format: None,
        }
    }

    /// Builder helper: prepend a system message.
    #[must_use]
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.messages.insert(0, ChatMessage::system(system));
        self
    }

    /// Builder helper: enable JSON-object response format.
    #[must_use]
    pub fn json(mut self) -> Self {
        self.response_format = Some(ResponseFormat::JsonObject);
        self
    }

    /// Builder helper: override the temperature.
    #[must_use]
    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = t;
        self
    }
}

/// Provider response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Completion content from the first choice.
    pub content: String,
    /// Model identifier the provider used (sometimes differs from the
    /// requested one, e.g. when a router substitutes).
    pub model: String,
    /// Approximate token usage, if the provider reported it.
    pub usage: Option<Usage>,
}

/// Token-usage breakdown.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens.
    pub prompt_tokens: u32,
    /// Completion tokens.
    pub completion_tokens: u32,
    /// Total tokens.
    pub total_tokens: u32,
}

/// LLM client abstraction.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a [`ChatRequest`] and await the [`ChatResponse`].
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse>;
}

// ---------------------------------------------------------------------------
// OpenAI-compatible HTTP client
// ---------------------------------------------------------------------------

/// HTTP client for any OpenAI-compatible `/v1/chat/completions` endpoint.
///
/// Tested only against the wire shape — has no allegiance to a particular
/// provider. Use it with OpenAI itself, an OpenRouter proxy, a self-hosted
/// vLLM / Ollama instance, or any other shim that speaks the same JSON.
pub struct OpenAiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
}

impl OpenAiClient {
    /// Build a client pointing at `base_url` (which should *not* include the
    /// `/v1/chat/completions` suffix) and authenticated with `api_key`.
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: impl Into<String>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            default_model: default_model.into(),
        })
    }

    /// Default model identifier baked in at construction time.
    pub fn default_model(&self) -> &str {
        &self.default_model
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut builder = self.http.post(&url).json(&serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
            "response_format": request.response_format.map(|f| match f {
                ResponseFormat::Text => serde_json::json!({"type": "text"}),
                ResponseFormat::JsonObject => serde_json::json!({"type": "json_object"}),
            }),
        }));
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        let resp = builder.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Provider {
                status: status.as_u16(),
                body: body.chars().take(4096).collect(),
            });
        }

        #[derive(Deserialize)]
        struct WireResponse {
            model: Option<String>,
            choices: Vec<WireChoice>,
            usage: Option<WireUsage>,
        }
        #[derive(Deserialize)]
        struct WireChoice {
            message: WireMessage,
        }
        #[derive(Deserialize)]
        struct WireMessage {
            content: Option<String>,
        }
        #[derive(Deserialize)]
        struct WireUsage {
            prompt_tokens: u32,
            completion_tokens: u32,
            total_tokens: u32,
        }

        let wire: WireResponse = resp.json().await.map_err(|e| {
            LlmError::InvalidResponse(format!("failed to decode chat response: {e}"))
        })?;

        let content = wire
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or(LlmError::EmptyCompletion)?;

        Ok(ChatResponse {
            content,
            model: wire.model.unwrap_or_else(|| request.model.clone()),
            usage: wire.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        })
    }
}

// ---------------------------------------------------------------------------
// Mock client (tests)
// ---------------------------------------------------------------------------

/// Pre-baked test client. Pops the next response from an internal queue on
/// every call; returns an error once exhausted.
pub struct MockLlmClient {
    responses: Mutex<Vec<ChatResponse>>,
    calls: Mutex<Vec<ChatRequest>>,
}

impl MockLlmClient {
    /// Build a mock with the given response queue.
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Build a mock that returns `content` exactly once, then errors.
    pub fn single(content: impl Into<String>) -> Self {
        Self::new(vec![ChatResponse {
            content: content.into(),
            model: "mock".into(),
            usage: None,
        }])
    }

    /// Snapshot of every request the mock observed, in order.
    pub fn calls(&self) -> Vec<ChatRequest> {
        self.calls.lock().unwrap().clone()
    }

    /// Push another canned response onto the queue.
    pub fn push(&self, response: ChatResponse) {
        self.responses.lock().unwrap().push(response);
    }
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.calls.lock().unwrap().push(request);
        let mut q = self.responses.lock().unwrap();
        if q.is_empty() {
            return Err(LlmError::EmptyCompletion);
        }
        Ok(q.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_returns_queued_responses() {
        let mock = MockLlmClient::new(vec![
            ChatResponse {
                content: "first".into(),
                model: "m".into(),
                usage: None,
            },
            ChatResponse {
                content: "second".into(),
                model: "m".into(),
                usage: None,
            },
        ]);
        let r1 = mock.complete(ChatRequest::simple("m", "a")).await.unwrap();
        assert_eq!(r1.content, "first");
        let r2 = mock.complete(ChatRequest::simple("m", "b")).await.unwrap();
        assert_eq!(r2.content, "second");

        // Exhausted.
        assert!(mock.complete(ChatRequest::simple("m", "c")).await.is_err());

        let calls = mock.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].messages[0].content, "a");
    }

    #[tokio::test]
    async fn chat_request_builder_helpers() {
        let req = ChatRequest::simple("m", "hi")
            .with_system("you are short")
            .json()
            .temperature(0.0);
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, ChatRole::System);
        assert_eq!(req.messages[1].role, ChatRole::User);
        assert!(matches!(req.response_format, Some(ResponseFormat::JsonObject)));
        assert_eq!(req.temperature, 0.0);
    }
}
