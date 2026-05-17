//! LLM-driven [`HealingStrategy`] for `oxide-browser-sh`.
//!
//! [`LlmHealing`] receives a [`HealingContext`] from a `BrowserSession`,
//! builds a prompt with [`PromptTemplate::HealingSelector`], asks the
//! configured [`LlmClient`] for a JSON-encoded recovery, and parses the
//! response back into a [`Selector`]. If the LLM cannot recover (or the
//! response is malformed) the strategy falls back to the rule-based
//! [`DefaultHealing`] so end-to-end flows still make progress.

use std::sync::Arc;

use async_trait::async_trait;
use oxide_browser_sh::action::Selector;
use oxide_browser_sh::healing::{DefaultHealing, HealingContext, HealingDecision, HealingStrategy};
use serde::Deserialize;

use crate::client::{LlmClient, MockLlmClient};
use crate::prompts::{HealingPromptInput, PromptTemplate};

/// Maximum HTML snippet sent to the LLM. Larger payloads spend tokens on
/// boilerplate without improving recovery quality; if a healthier strategy
/// needs more context it can override this via [`LlmHealing::with_html_limit`].
pub const DEFAULT_HTML_LIMIT_CHARS: usize = 4_000;

/// LLM-driven healing strategy.
pub struct LlmHealing {
    client: Arc<dyn LlmClient>,
    model: String,
    html_limit: usize,
    max_attempts: usize,
    fallback: DefaultHealing,
}

impl LlmHealing {
    /// Build a strategy that uses `client` and asks `model` for a JSON-shaped
    /// recovery selector.
    pub fn new(client: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            html_limit: DEFAULT_HTML_LIMIT_CHARS,
            max_attempts: 3,
            fallback: DefaultHealing::new(),
        }
    }

    /// Override the HTML truncation budget passed to the LLM.
    #[must_use]
    pub fn with_html_limit(mut self, chars: usize) -> Self {
        self.html_limit = chars;
        self
    }

    /// Override the maximum number of retry attempts.
    #[must_use]
    pub fn with_max_attempts(mut self, attempts: usize) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Build a mock-backed strategy that returns a single canned LLM
    /// response. Convenient in tests.
    pub fn mock(content: impl Into<String>) -> Self {
        Self::new(Arc::new(MockLlmClient::single(content)), "mock")
    }

    fn truncate_html(&self, html: &str) -> String {
        if html.chars().count() <= self.html_limit {
            return html.to_string();
        }
        html.chars().take(self.html_limit).collect()
    }
}

#[async_trait]
impl HealingStrategy for LlmHealing {
    async fn heal(&self, ctx: &HealingContext) -> HealingDecision {
        let input = HealingPromptInput {
            action: ctx.action.clone(),
            failed_selector: ctx.selector.label(),
            url: ctx.url.clone().unwrap_or_default(),
            attempt: ctx.attempt,
            error: ctx.error.clone(),
            html_excerpt: self.truncate_html(&ctx.html),
        };
        let request = PromptTemplate::HealingSelector.build_request(&self.model, &input);

        match self.client.complete(request).await {
            Ok(response) => match parse_response(&response.content) {
                Some(selector) => {
                    tracing::info!(
                        target: "oxide_llm_orchestrator::healing",
                        new_selector = %selector.label(),
                        "llm proposed recovery"
                    );
                    HealingDecision::Retry(selector)
                }
                None => {
                    tracing::warn!(
                        target: "oxide_llm_orchestrator::healing",
                        content = %response.content,
                        "llm response could not be parsed; falling back to rules"
                    );
                    self.fallback.heal(ctx).await
                }
            },
            Err(err) => {
                tracing::warn!(
                    target: "oxide_llm_orchestrator::healing",
                    %err,
                    "llm call failed; falling back to rules"
                );
                self.fallback.heal(ctx).await
            }
        }
    }

    fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    fn label(&self) -> &'static str {
        "llm"
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireSelector {
    Role { role: String, name: Option<String> },
    Text { text: String },
    Css { css: String },
    XPath { xpath: String },
    GiveUp,
}

fn parse_response(content: &str) -> Option<Selector> {
    let trimmed = content.trim();
    let json = if let Some(start) = trimmed.find('{') {
        let end = trimmed.rfind('}')?;
        if end < start {
            return None;
        }
        &trimmed[start..=end]
    } else {
        return None;
    };
    let wire: WireSelector = serde_json::from_str(json).ok()?;
    Some(match wire {
        WireSelector::Role { role, name } => Selector::Role { role, name },
        WireSelector::Text { text } => Selector::Text(text),
        WireSelector::Css { css } => Selector::Css(css),
        WireSelector::XPath { xpath } => Selector::XPath(xpath),
        WireSelector::GiveUp => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_browser_sh::action::Selector;
    use oxide_browser_sh::healing::HealingContext;

    fn ctx() -> HealingContext {
        HealingContext {
            action: "click".into(),
            selector: Selector::css("#missing"),
            url: Some("https://example.com".into()),
            html: "<button id=\"cta\">Go</button>".into(),
            screenshot_available: false,
            attempt: 1,
            error: "element not found".into(),
        }
    }

    #[tokio::test]
    async fn applies_llm_role_recovery() {
        let heal = LlmHealing::mock(r#"{"kind": "role", "role": "button", "name": "Go"}"#);
        match heal.heal(&ctx()).await {
            HealingDecision::Retry(Selector::Role { role, name }) => {
                assert_eq!(role, "button");
                assert_eq!(name.as_deref(), Some("Go"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn applies_llm_css_recovery() {
        let heal = LlmHealing::mock(r##"prelude {"kind": "css", "css": "#cta"} trailing"##);
        match heal.heal(&ctx()).await {
            HealingDecision::Retry(Selector::Css(css)) => assert_eq!(css, "#cta"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn llm_give_up_falls_back_to_rules() {
        // give_up → falls back to DefaultHealing which CAN recover #cta via
        // scraper-based upgrade.
        let heal = LlmHealing::mock(r#"{"kind": "give_up"}"#);
        let decision = heal
            .heal(&HealingContext {
                selector: Selector::css("#cta"),
                ..ctx()
            })
            .await;
        assert!(matches!(decision, HealingDecision::Retry(_)));
    }

    #[tokio::test]
    async fn llm_failure_falls_back_to_rules() {
        // Mock with empty queue returns Err on first call → fallback path.
        let client: Arc<dyn LlmClient> = Arc::new(MockLlmClient::new(vec![]));
        let heal = LlmHealing::new(client, "mock");
        let decision = heal
            .heal(&HealingContext {
                selector: Selector::css("#cta"),
                ..ctx()
            })
            .await;
        assert!(matches!(decision, HealingDecision::Retry(_)));
    }

    #[test]
    fn truncates_long_html() {
        let heal = LlmHealing::mock("{}").with_html_limit(10);
        let long: String = "a".repeat(50);
        assert_eq!(heal.truncate_html(&long).chars().count(), 10);
    }
}
