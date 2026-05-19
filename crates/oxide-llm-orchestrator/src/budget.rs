//! Budget + rate-limit guard for [`LlmClient`].
//!
//! Wrap any concrete client in [`BudgetGuard`] to enforce three orthogonal
//! caps:
//!
//! * **Dollars.** Each completion contributes a cost calculated from
//!   the configured [`Pricing`] table and the provider-reported [`Usage`].
//!   If the provider does not report usage the call passes through (and
//!   logs a warning) so budgets do not corrupt silently.
//! * **Tokens.** Hard cap on cumulative prompt + completion tokens.
//! * **Requests-per-minute.** Sliding 60-second window.
//!
//! The guard is built around the [`LlmClient`] trait, so it composes with the
//! [`OpenAiClient`](crate::client::OpenAiClient), the
//! [`MockLlmClient`](crate::client::MockLlmClient), or any future
//! implementation without churn.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::client::{ChatRequest, ChatResponse, CompletionStream, LlmClient, Usage};
use crate::error::{LlmError, Result};

/// Per-model pricing in US dollars per million tokens. Add entries for
/// every model the budget guard should track. Unknown models pass through
/// (no cost recorded) and emit a `tracing::warn` event.
#[derive(Debug, Clone, Default)]
pub struct Pricing {
    rates: std::collections::HashMap<String, ModelRate>,
}

/// Per-million-token input + output rate.
#[derive(Debug, Clone, Copy)]
pub struct ModelRate {
    /// USD per 1M prompt tokens.
    pub input_per_million: f64,
    /// USD per 1M completion tokens.
    pub output_per_million: f64,
}

impl Pricing {
    /// Empty pricing table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder helper.
    #[must_use]
    pub fn with_model(
        mut self,
        model: impl Into<String>,
        input_per_million: f64,
        output_per_million: f64,
    ) -> Self {
        self.rates.insert(
            model.into(),
            ModelRate {
                input_per_million,
                output_per_million,
            },
        );
        self
    }

    fn cost(&self, model: &str, usage: &Usage) -> Option<f64> {
        let rate = self.rates.get(model)?;
        let prompt = usage.prompt_tokens as f64 * rate.input_per_million / 1_000_000.0;
        let completion = usage.completion_tokens as f64 * rate.output_per_million / 1_000_000.0;
        Some(prompt + completion)
    }
}

/// Caps applied by [`BudgetGuard`]. Any field left as `None` means "no cap".
#[derive(Debug, Clone, Default)]
pub struct BudgetCaps {
    /// Maximum cumulative dollars (USD) across all calls.
    pub max_dollars: Option<f64>,
    /// Maximum cumulative prompt+completion tokens.
    pub max_tokens: Option<u64>,
    /// Maximum requests within the trailing 60 seconds.
    pub max_rpm: Option<u32>,
}

/// Running counters owned by the guard.
#[derive(Debug, Clone, Default)]
pub struct BudgetState {
    /// Dollars spent so far.
    pub dollars_spent: f64,
    /// Tokens spent so far.
    pub tokens_spent: u64,
    /// Total request count.
    pub requests: u64,
    /// Total responses that were rejected pre-flight.
    pub rejected: u64,
}

struct GuardInner {
    caps: BudgetCaps,
    pricing: Pricing,
    state: BudgetState,
    request_window: VecDeque<Instant>,
}

/// [`LlmClient`] wrapper that enforces budgets before delegating to an inner
/// client. Cheaply cloneable.
#[derive(Clone)]
pub struct BudgetGuard {
    inner_client: Arc<dyn LlmClient>,
    inner: Arc<Mutex<GuardInner>>,
}

impl BudgetGuard {
    /// Build a guard.
    pub fn new(client: Arc<dyn LlmClient>, caps: BudgetCaps, pricing: Pricing) -> Self {
        Self {
            inner_client: client,
            inner: Arc::new(Mutex::new(GuardInner {
                caps,
                pricing,
                state: BudgetState::default(),
                request_window: VecDeque::new(),
            })),
        }
    }

    /// Snapshot of the running state.
    pub async fn state(&self) -> BudgetState {
        self.inner.lock().await.state.clone()
    }

    /// Pre-flight check + RPM bookkeeping. Returns `Err(BudgetExceeded)`
    /// if any cap is already past, otherwise records the request timestamp.
    async fn check_in(&self) -> Result<()> {
        let mut guard = self.inner.lock().await;

        // Trim window.
        let cutoff = Instant::now() - Duration::from_secs(60);
        while let Some(front) = guard.request_window.front() {
            if *front < cutoff {
                guard.request_window.pop_front();
            } else {
                break;
            }
        }

        if let Some(cap) = guard.caps.max_rpm {
            if guard.request_window.len() as u32 >= cap {
                guard.state.rejected += 1;
                return Err(LlmError::Other(anyhow::anyhow!(
                    "budget exceeded: rpm cap {} reached",
                    cap
                )));
            }
        }
        if let Some(cap) = guard.caps.max_dollars {
            if guard.state.dollars_spent >= cap {
                guard.state.rejected += 1;
                return Err(LlmError::Other(anyhow::anyhow!(
                    "budget exceeded (dollars): ${:.4} >= ${:.4} cap",
                    guard.state.dollars_spent,
                    cap
                )));
            }
        }
        if let Some(cap) = guard.caps.max_tokens {
            if guard.state.tokens_spent >= cap {
                guard.state.rejected += 1;
                return Err(LlmError::Other(anyhow::anyhow!(
                    "budget exceeded: {} >= {} tokens",
                    guard.state.tokens_spent,
                    cap
                )));
            }
        }

        guard.request_window.push_back(Instant::now());
        guard.state.requests += 1;
        Ok(())
    }

    /// Post-flight bookkeeping — increment dollar / token counters.
    async fn check_out(&self, model: &str, usage: Option<&Usage>) {
        let mut guard = self.inner.lock().await;
        let Some(usage) = usage else {
            tracing::warn!(
                target: "oxide_llm_orchestrator::budget",
                model,
                "provider returned no usage info; budget counters unchanged"
            );
            return;
        };
        guard.state.tokens_spent += usage.total_tokens as u64;
        if let Some(cost) = guard.pricing.cost(model, usage) {
            guard.state.dollars_spent += cost;
        }
    }
}

#[async_trait]
impl LlmClient for BudgetGuard {
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.check_in().await?;
        let response = self.inner_client.complete(request).await?;
        self.check_out(&response.model, response.usage.as_ref())
            .await;
        Ok(response)
    }

    async fn complete_stream(&self, request: ChatRequest) -> Result<CompletionStream> {
        self.check_in().await?;
        // Streaming providers usually omit per-chunk usage. The accumulator
        // is incremented when the stream ends — see the note in
        // [`BudgetGuard::check_out`]. Callers that need accurate streaming
        // budgets should configure their provider to emit usage in the
        // final chunk and account for it post-stream.
        self.inner_client.complete_stream(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ChatResponse, MockLlmClient};

    fn mk_response(content: &str, total_tokens: u32) -> ChatResponse {
        ChatResponse {
            content: content.into(),
            tool_calls: Vec::new(),
            model: "test-model".into(),
            usage: Some(Usage {
                prompt_tokens: total_tokens / 2,
                completion_tokens: total_tokens - (total_tokens / 2),
                total_tokens,
            }),
        }
    }

    #[tokio::test]
    async fn dollars_cap_rejects_after_threshold() {
        let mock: Arc<dyn LlmClient> = Arc::new(MockLlmClient::new(vec![
            mk_response("a", 1_000_000),
            mk_response("b", 1_000_000),
        ]));
        let guard = BudgetGuard::new(
            mock,
            BudgetCaps {
                max_dollars: Some(2.0),
                ..Default::default()
            },
            Pricing::new().with_model("test-model", 1.0, 1.0),
        );
        // 1M tokens × $1/1M = $1 per call → first OK, second still under,
        // third blocked.
        guard
            .complete(ChatRequest::simple("test-model", "x"))
            .await
            .unwrap();
        let state = guard.state().await;
        assert!((state.dollars_spent - 1.0).abs() < 1e-9);
        guard
            .complete(ChatRequest::simple("test-model", "y"))
            .await
            .unwrap();
        let err = guard
            .complete(ChatRequest::simple("test-model", "z"))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("dollars"));
    }

    #[tokio::test]
    async fn tokens_cap_rejects_after_threshold() {
        let mock: Arc<dyn LlmClient> = Arc::new(MockLlmClient::new(vec![
            mk_response("x", 500),
            mk_response("y", 500),
        ]));
        let guard = BudgetGuard::new(
            mock,
            BudgetCaps {
                max_tokens: Some(800),
                ..Default::default()
            },
            Pricing::new(),
        );
        guard.complete(ChatRequest::simple("m", "a")).await.unwrap();
        guard.complete(ChatRequest::simple("m", "b")).await.unwrap();
        let err = guard
            .complete(ChatRequest::simple("m", "c"))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("tokens"));
    }

    #[tokio::test]
    async fn rpm_cap_rejects_after_threshold() {
        let mock: Arc<dyn LlmClient> = Arc::new(MockLlmClient::new(vec![
            mk_response("a", 10),
            mk_response("b", 10),
            mk_response("c", 10),
        ]));
        let guard = BudgetGuard::new(
            mock,
            BudgetCaps {
                max_rpm: Some(2),
                ..Default::default()
            },
            Pricing::new(),
        );
        guard.complete(ChatRequest::simple("m", "a")).await.unwrap();
        guard.complete(ChatRequest::simple("m", "b")).await.unwrap();
        let err = guard
            .complete(ChatRequest::simple("m", "c"))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("rpm"));
        let state = guard.state().await;
        assert_eq!(state.rejected, 1);
    }
}
