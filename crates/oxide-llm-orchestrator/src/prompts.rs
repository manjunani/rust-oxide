//! Prompt templates.
//!
//! Each template is a [`PromptTemplate`] which exposes:
//!
//! * a stable `system` instruction string;
//! * a `render(input)` method that produces the corresponding user message.
//!
//! Templates intentionally avoid clever escaping — substitution uses
//! straightforward `replace` so the prompts read cleanly in source. If a value
//! happens to contain a placeholder name, it survives unchanged.

use serde::{Deserialize, Serialize};

use crate::client::{ChatMessage, ChatRequest, ChatRole, ResponseFormat};

/// Three built-in templates ship today; callers can add their own by
/// constructing [`ChatRequest`]s directly.
#[derive(Debug, Clone, Copy)]
pub enum PromptTemplate {
    /// Suggest an alternative selector for a failed browser action.
    HealingSelector,
    /// Explain an error message in actionable terms.
    ErrorAnalysis,
    /// Compress a long document into a focused summary.
    Summarize,
}

// ---------------------------------------------------------------------------
// Healing prompt
// ---------------------------------------------------------------------------

/// Input to the [`PromptTemplate::HealingSelector`] template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealingPromptInput {
    /// Action that failed (`click`, `input`, …).
    pub action: String,
    /// Stringified selector that failed.
    pub failed_selector: String,
    /// Current URL.
    pub url: String,
    /// Attempt counter (1-based).
    pub attempt: usize,
    /// Backend error message.
    pub error: String,
    /// HTML excerpt (truncated by the caller before being passed in).
    pub html_excerpt: String,
}

const HEAL_SYSTEM: &str = "You are a browser-automation healer. Given a failing selector and the HTML it ran against, return a single JSON object with the shape `{\"kind\": \"role|text|css\", \"role\": ..., \"name\": ..., \"text\": ..., \"css\": ...}` describing a new selector that should locate the intended element. Only fill the fields relevant to the chosen `kind`. If you cannot recover, respond with `{\"kind\": \"give_up\"}`. Reply with JSON only.";

const HEAL_USER_TEMPLATE: &str = r#"Action: {action}
Failed selector: {failed_selector}
URL: {url}
Attempt: {attempt}
Error: {error}

Relevant HTML:
```html
{html_excerpt}
```
"#;

// ---------------------------------------------------------------------------
// Error analysis prompt
// ---------------------------------------------------------------------------

/// Input to the [`PromptTemplate::ErrorAnalysis`] template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorAnalysisInput {
    /// What operation was being performed.
    pub operation: String,
    /// Error message (verbatim).
    pub error: String,
    /// Any extra context, JSON-encoded.
    pub context: serde_json::Value,
}

const ERROR_SYSTEM: &str = "You are an experienced Rust + distributed-systems engineer. Given an operation, an error, and free-form context, respond with a short JSON object `{\"root_cause\": <string>, \"remediation\": <string>, \"confidence\": <float 0..1>}`. Be terse. Reply with JSON only.";

const ERROR_USER_TEMPLATE: &str = r#"Operation: {operation}
Error: {error}
Context: {context}
"#;

// ---------------------------------------------------------------------------
// Summarisation prompt
// ---------------------------------------------------------------------------

/// Input to the [`PromptTemplate::Summarize`] template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeInput {
    /// Text to summarise.
    pub text: String,
    /// Target word budget for the summary.
    pub target_words: u32,
    /// Optional focus question to bias the summary.
    pub focus: Option<String>,
}

const SUMMARIZE_SYSTEM: &str = "You are a concise summariser. Compress the user's text to roughly the requested word budget while preserving every concrete fact, number, name, and decision. Drop adjectives, marketing copy, and repetition.";

const SUMMARIZE_USER_TEMPLATE: &str = r#"Target length: ~{target_words} words.
Focus question (may be empty): {focus}

Text:
{text}
"#;

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl PromptTemplate {
    /// The static system message for this template.
    pub fn system(&self) -> &'static str {
        match self {
            PromptTemplate::HealingSelector => HEAL_SYSTEM,
            PromptTemplate::ErrorAnalysis => ERROR_SYSTEM,
            PromptTemplate::Summarize => SUMMARIZE_SYSTEM,
        }
    }

    /// Build a [`ChatRequest`] from the input. JSON-output templates
    /// (`HealingSelector`, `ErrorAnalysis`) request the provider's JSON mode;
    /// summarisation defaults to plain text.
    pub fn build_request<I: Renderable>(&self, model: &str, input: &I) -> ChatRequest {
        let user_body = input.render(*self);
        let mut messages = vec![
            ChatMessage::system(self.system()),
            ChatMessage::user(user_body),
        ];
        let response_format = match self {
            PromptTemplate::HealingSelector | PromptTemplate::ErrorAnalysis => {
                Some(ResponseFormat::JsonObject)
            }
            PromptTemplate::Summarize => None,
        };
        // Make sure messages compile cleanly even when the consumer adds
        // more turns later.
        let _ = ChatRole::Assistant;
        messages.retain(|m| !m.content.is_empty());
        ChatRequest {
            model: model.to_string(),
            messages,
            temperature: 0.1,
            max_tokens: None,
            response_format,
        }
    }
}

/// Render a template input into its user-message body.
pub trait Renderable {
    /// Substitute fields into the template body.
    fn render(&self, template: PromptTemplate) -> String;
}

impl Renderable for HealingPromptInput {
    fn render(&self, _template: PromptTemplate) -> String {
        HEAL_USER_TEMPLATE
            .replace("{action}", &self.action)
            .replace("{failed_selector}", &self.failed_selector)
            .replace("{url}", &self.url)
            .replace("{attempt}", &self.attempt.to_string())
            .replace("{error}", &self.error)
            .replace("{html_excerpt}", &self.html_excerpt)
    }
}

impl Renderable for ErrorAnalysisInput {
    fn render(&self, _template: PromptTemplate) -> String {
        let context = serde_json::to_string(&self.context).unwrap_or_else(|_| "{}".into());
        ERROR_USER_TEMPLATE
            .replace("{operation}", &self.operation)
            .replace("{error}", &self.error)
            .replace("{context}", &context)
    }
}

impl Renderable for SummarizeInput {
    fn render(&self, _template: PromptTemplate) -> String {
        SUMMARIZE_USER_TEMPLATE
            .replace("{target_words}", &self.target_words.to_string())
            .replace("{focus}", self.focus.as_deref().unwrap_or(""))
            .replace("{text}", &self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn healing_request_includes_html_and_requests_json() {
        let input = HealingPromptInput {
            action: "click".into(),
            failed_selector: "css:.primary".into(),
            url: "https://example.com".into(),
            attempt: 1,
            error: "element not found".into(),
            html_excerpt: "<button id=\"cta\">Go</button>".into(),
        };
        let req = PromptTemplate::HealingSelector.build_request("m", &input);
        assert_eq!(req.messages.len(), 2);
        assert!(req.messages[1].content.contains("css:.primary"));
        assert!(req.messages[1].content.contains("<button id=\"cta\">"));
        assert!(matches!(
            req.response_format,
            Some(ResponseFormat::JsonObject)
        ));
    }

    #[test]
    fn error_analysis_substitutes_context() {
        let input = ErrorAnalysisInput {
            operation: "sync".into(),
            error: "timeout".into(),
            context: json!({"endpoint": "https://x.example"}),
        };
        let req = PromptTemplate::ErrorAnalysis.build_request("m", &input);
        assert!(req.messages[1]
            .content
            .contains("\"endpoint\":\"https://x.example\""));
    }

    #[test]
    fn summarize_supports_optional_focus() {
        let with = SummarizeInput {
            text: "Long story".into(),
            target_words: 30,
            focus: Some("dates".into()),
        };
        let without = SummarizeInput {
            text: "Long story".into(),
            target_words: 30,
            focus: None,
        };
        let r1 = with.render(PromptTemplate::Summarize);
        let r2 = without.render(PromptTemplate::Summarize);
        assert!(r1.contains("dates"));
        assert!(!r2.contains("Focus question (may be empty): None"));
        assert!(r2.contains("Focus question (may be empty): \n"));
    }
}
