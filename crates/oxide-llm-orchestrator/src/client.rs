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

/// One function definition the model may choose to invoke. Mirrors the
/// OpenAI `tools[].function` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Function name (must match `^[a-zA-Z0-9_-]+$`).
    pub name: String,
    /// Short, single-sentence description shown to the model.
    pub description: String,
    /// JSON Schema for the function's `arguments` object.
    pub parameters: serde_json::Value,
}

/// One tool invocation returned by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned call id (used when sending the tool result back).
    pub id: String,
    /// Function name the model wants to call.
    pub name: String,
    /// JSON-encoded arguments (the model serialises the args object as a
    /// string, even when it conceptually matches the schema in
    /// [`ToolSpec::parameters`]).
    pub arguments: String,
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
    /// Tool definitions the model may invoke. Empty = no tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
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
            tools: Vec::new(),
        }
    }

    /// Builder helper: attach a tool definition.
    #[must_use]
    pub fn with_tool(mut self, tool: ToolSpec) -> Self {
        self.tools.push(tool);
        self
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
    /// Completion content from the first choice. Empty when the model
    /// returned only tool calls.
    pub content: String,
    /// Tool calls the model wants to issue. Empty when the model returned
    /// only text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
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

/// A pinned, boxed stream of completion chunks. Each item is one piece of
/// the assistant message as it streams in (typically a few tokens).
pub type CompletionStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>;

/// LLM client abstraction.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a [`ChatRequest`] and await the full [`ChatResponse`].
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse>;

    /// Stream a [`ChatRequest`] response chunk-by-chunk.
    ///
    /// The default implementation calls [`Self::complete`] and yields the
    /// whole content as a single item — backends that don't natively
    /// stream still satisfy the trait. Real streaming implementations
    /// (see [`OpenAiClient`]) decode Server-Sent Events from the provider.
    async fn complete_stream(&self, request: ChatRequest) -> Result<CompletionStream> {
        let response = self.complete(request).await?;
        let stream = futures::stream::once(async move { Ok(response.content) });
        Ok(Box::pin(stream))
    }
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
        let tools_payload: Option<serde_json::Value> = if request.tools.is_empty() {
            None
        } else {
            Some(serde_json::Value::Array(
                request
                    .tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            ))
        };
        let mut builder = self.http.post(&url).json(&serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
            "response_format": request.response_format.map(|f| match f {
                ResponseFormat::Text => serde_json::json!({"type": "text"}),
                ResponseFormat::JsonObject => serde_json::json!({"type": "json_object"}),
            }),
            "tools": tools_payload,
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
            #[serde(default)]
            tool_calls: Vec<WireToolCall>,
        }
        #[derive(Deserialize)]
        struct WireToolCall {
            id: String,
            function: WireToolFunction,
        }
        #[derive(Deserialize)]
        struct WireToolFunction {
            name: String,
            arguments: String,
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

        let choice = wire
            .choices
            .into_iter()
            .next()
            .ok_or(LlmError::EmptyCompletion)?;

        let content = choice.message.content.unwrap_or_default();
        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect();

        if content.is_empty() && tool_calls.is_empty() {
            return Err(LlmError::EmptyCompletion);
        }

        Ok(ChatResponse {
            content,
            tool_calls,
            model: wire.model.unwrap_or_else(|| request.model.clone()),
            usage: wire.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        })
    }

    async fn complete_stream(&self, request: ChatRequest) -> Result<CompletionStream> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut builder = self.http.post(&url).json(&serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
            "stream": true,
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

        // OpenAI SSE format: `data: {…}\n\n`, terminated by `data: [DONE]`.
        // We accumulate bytes across chunks, drain complete events at each
        // step, and yield one `delta.content` string per emit. Malformed
        // events are silently skipped — provider quirks shouldn't tear
        // down the stream.
        let state = (resp.bytes_stream(), String::new());
        let stream = futures::stream::unfold(state, |(mut bytes, mut buffer)| async move {
            loop {
                if let Some(event) = take_event(&mut buffer) {
                    for line in event.lines() {
                        let Some(payload) = line.strip_prefix("data: ") else {
                            continue;
                        };
                        let payload = payload.trim();
                        if payload == "[DONE]" || payload.is_empty() {
                            continue;
                        }
                        let value: serde_json::Value = match serde_json::from_str(payload) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        if let Some(content) = value
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !content.is_empty() {
                                return Some((Ok(content.to_string()), (bytes, buffer)));
                            }
                        }
                    }
                    continue;
                }
                use futures::StreamExt;
                match bytes.next().await {
                    Some(Ok(b)) => buffer.push_str(&String::from_utf8_lossy(b.as_ref())),
                    Some(Err(e)) => {
                        return Some((Err(LlmError::Http(e)), (bytes, buffer)));
                    }
                    None => return None,
                }
            }
        });
        Ok(Box::pin(stream))
    }
}

fn take_event(buffer: &mut String) -> Option<String> {
    let idx = buffer.find("\n\n")?;
    Some(buffer.drain(..idx + 2).collect())
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
            tool_calls: Vec::new(),
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
                tool_calls: Vec::new(),
                model: "m".into(),
                usage: None,
            },
            ChatResponse {
                content: "second".into(),
                tool_calls: Vec::new(),
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
    async fn mock_default_complete_stream_yields_full_content_once() {
        use futures::StreamExt;
        let mock = MockLlmClient::single("hello world");
        let stream = mock
            .complete_stream(ChatRequest::simple("m", "hi"))
            .await
            .unwrap();
        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].as_ref().unwrap(), "hello world");
    }

    #[test]
    fn sse_take_event_splits_on_blank_line() {
        let mut buf = String::from("data: a\n\ndata: b\n\n");
        let first = super::take_event(&mut buf).unwrap();
        assert!(first.contains("data: a"));
        let second = super::take_event(&mut buf).unwrap();
        assert!(second.contains("data: b"));
        assert!(super::take_event(&mut buf).is_none());
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
        assert!(matches!(
            req.response_format,
            Some(ResponseFormat::JsonObject)
        ));
        assert_eq!(req.temperature, 0.0);
    }
}
