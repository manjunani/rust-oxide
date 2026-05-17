//! Wrapper around [`PromptTemplate::Summarize`] that hides the request /
//! response plumbing.

use std::sync::Arc;

use crate::client::LlmClient;
use crate::error::Result;
use crate::prompts::{PromptTemplate, SummarizeInput};

/// Convenience helper for the summarisation prompt.
pub struct Summarizer {
    client: Arc<dyn LlmClient>,
    model: String,
}

impl Summarizer {
    /// Build a summariser that talks to `client` using `model`.
    pub fn new(client: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    /// Summarise `text` into roughly `target_words`. `focus`, if set, biases
    /// the LLM toward the supplied question / topic.
    pub async fn summarize(
        &self,
        text: impl Into<String>,
        target_words: u32,
        focus: Option<String>,
    ) -> Result<String> {
        let input = SummarizeInput {
            text: text.into(),
            target_words,
            focus,
        };
        let request = PromptTemplate::Summarize.build_request(&self.model, &input);
        let response = self.client.complete(request).await?;
        Ok(response.content.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockLlmClient;

    #[tokio::test]
    async fn summarize_returns_trimmed_content() {
        let client: Arc<dyn LlmClient> = Arc::new(MockLlmClient::single("  short summary  "));
        let s = Summarizer::new(client, "m");
        let out = s.summarize("Long text", 20, None).await.unwrap();
        assert_eq!(out, "short summary");
    }
}
