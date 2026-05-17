//! High-level browser session with self-healing.
//!
//! [`BrowserSession`] is the API agents talk to. It holds a [`BrowserBackend`]
//! plus a [`HealingStrategy`] and provides ergonomic, healing-aware versions
//! of every primitive action ([`navigate`](BrowserSession::navigate),
//! [`click`](BrowserSession::click),
//! [`input`](BrowserSession::input),
//! [`scroll`](BrowserSession::scroll)).
//!
//! Every action is recorded into [`SessionStats`] so callers can audit what
//! actually happened on the page — including healing attempts.

use std::sync::Arc;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::action::{ScrollDirection, Selector};
use crate::backend::BrowserBackend;
use crate::error::{BrowserError, Result};
use crate::healing::{DefaultHealing, HealingContext, HealingDecision, HealingStrategy};

/// Record of a single action attempted by a [`BrowserSession`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRecord {
    /// Free-form name of the action (`navigate`, `click`, `input`, `scroll`).
    pub action: String,
    /// Selector involved, if any.
    pub selector: Option<Selector>,
    /// Whether the final attempt succeeded.
    pub ok: bool,
    /// Number of attempts (1 + number of healing iterations).
    pub attempts: usize,
}

/// Aggregate counters maintained by a [`BrowserSession`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStats {
    /// Every action attempted, in the order they happened.
    pub history: Vec<ActionRecord>,
    /// Total number of healing retries actually executed.
    pub healing_retries: usize,
}

/// Healing-aware session wrapper.
pub struct BrowserSession {
    backend: Arc<dyn BrowserBackend>,
    healing: Arc<dyn HealingStrategy>,
    stats: Mutex<SessionStats>,
}

impl BrowserSession {
    /// Build a session around an explicit healing strategy.
    pub fn new(backend: Arc<dyn BrowserBackend>, healing: Arc<dyn HealingStrategy>) -> Self {
        Self {
            backend,
            healing,
            stats: Mutex::new(SessionStats::default()),
        }
    }

    /// Build a session with the rule-based [`DefaultHealing`] strategy.
    pub fn with_default_healing(backend: Arc<dyn BrowserBackend>) -> Self {
        Self::new(backend, Arc::new(DefaultHealing::new()))
    }

    /// Snapshot the session stats.
    pub fn stats(&self) -> SessionStats {
        self.stats.lock().unwrap().clone()
    }

    /// Direct access to the backend, for read-only queries that do not need
    /// healing (`url`, `html`, `screenshot`, `accessibility_tree`).
    pub fn backend(&self) -> &dyn BrowserBackend {
        &*self.backend
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    /// Navigate to `url`. No healing — navigation failures bubble up.
    pub async fn navigate(&self, url: &str) -> Result<()> {
        let res = self.backend.navigate(url).await;
        self.record(ActionRecord {
            action: "navigate".into(),
            selector: None,
            ok: res.is_ok(),
            attempts: 1,
        });
        res
    }

    /// Click `selector`, healing on failure.
    pub async fn click(&self, selector: &Selector) -> Result<()> {
        self.heal_loop("click", selector.clone(), |s| {
            let backend = self.backend.clone();
            async move { backend.click(&s).await }
        })
        .await
    }

    /// Type `text` into `selector`, healing on failure.
    pub async fn input(&self, selector: &Selector, text: &str) -> Result<()> {
        let text_owned = text.to_string();
        self.heal_loop("input", selector.clone(), move |s| {
            let backend = self.backend.clone();
            let text = text_owned.clone();
            async move { backend.input(&s, &text).await }
        })
        .await
    }

    /// Scroll the viewport. Pure motor action — no healing applied.
    pub async fn scroll(&self, direction: ScrollDirection, amount: i32) -> Result<()> {
        let res = self.backend.scroll(direction, amount).await;
        self.record(ActionRecord {
            action: "scroll".into(),
            selector: None,
            ok: res.is_ok(),
            attempts: 1,
        });
        res
    }

    // -----------------------------------------------------------------------
    // Healing loop
    // -----------------------------------------------------------------------

    async fn heal_loop<F, Fut>(
        &self,
        action_name: &'static str,
        initial: Selector,
        operation: F,
    ) -> Result<()>
    where
        F: Fn(Selector) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let max_attempts = self.healing.max_attempts().max(1);
        let mut selector = initial.clone();
        let mut last_error: Option<BrowserError> = None;

        for attempt in 1..=max_attempts {
            match operation(selector.clone()).await {
                Ok(()) => {
                    self.record(ActionRecord {
                        action: action_name.into(),
                        selector: Some(selector),
                        ok: true,
                        attempts: attempt,
                    });
                    return Ok(());
                }
                Err(err) => {
                    tracing::warn!(
                        action = action_name,
                        attempt,
                        selector = %selector.label(),
                        "{err}"
                    );
                    // Last attempt? Give up.
                    if attempt == max_attempts {
                        last_error = Some(err);
                        break;
                    }
                    // Otherwise: ask the healing strategy for an alternative.
                    let ctx = self.capture_context(action_name, &selector, attempt, &err).await;
                    let decision = self.healing.heal(&ctx).await;
                    match decision {
                        HealingDecision::Retry(new_sel) => {
                            self.stats.lock().unwrap().healing_retries += 1;
                            tracing::info!(
                                action = action_name,
                                from = %selector.label(),
                                to = %new_sel.label(),
                                strategy = self.healing.label(),
                                "healing retry"
                            );
                            selector = new_sel;
                        }
                        HealingDecision::GiveUp => {
                            last_error = Some(err);
                            break;
                        }
                    }
                }
            }
        }

        let err = last_error.unwrap_or(BrowserError::HealingExhausted {
            attempts: max_attempts,
            last_error: "<none>".into(),
        });
        self.record(ActionRecord {
            action: action_name.into(),
            selector: Some(selector),
            ok: false,
            attempts: max_attempts,
        });
        Err(err)
    }

    async fn capture_context(
        &self,
        action_name: &'static str,
        selector: &Selector,
        attempt: usize,
        err: &BrowserError,
    ) -> HealingContext {
        let url = self.backend.url().await.ok();
        let html = self.backend.html().await.unwrap_or_default();
        let screenshot_available = matches!(self.backend.screenshot().await, Ok(b) if !b.is_empty());
        HealingContext {
            action: action_name.to_string(),
            selector: selector.clone(),
            url,
            html,
            screenshot_available,
            attempt,
            error: err.to_string(),
        }
    }

    fn record(&self, record: ActionRecord) {
        self.stats.lock().unwrap().history.push(record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::healing::LlmStubHealing;
    use crate::mock::MockBackend;

    const HTML: &str = r#"<!doctype html>
        <html><body>
          <button id="cta" class="primary">Get started</button>
          <a id="learn-more" href="/docs">Learn more</a>
        </body></html>"#;

    fn session_with(backend: Arc<MockBackend>) -> BrowserSession {
        BrowserSession::with_default_healing(backend)
    }

    #[tokio::test]
    async fn click_succeeds_first_time() {
        let mock = Arc::new(MockBackend::new());
        mock.register_page("/", HTML);
        let session = session_with(mock.clone());
        session.navigate("/").await.unwrap();
        session
            .click(&Selector::css("#cta"))
            .await
            .unwrap();
        let stats = session.stats();
        let click = stats
            .history
            .iter()
            .find(|r| r.action == "click")
            .unwrap();
        assert!(click.ok);
        assert_eq!(click.attempts, 1);
        assert_eq!(stats.healing_retries, 0);
    }

    #[tokio::test]
    async fn healing_recovers_after_one_failure() {
        let mock = Arc::new(MockBackend::new());
        mock.register_page("/", HTML);
        let bad = Selector::css(".primary"); // unique among AX nodes → heal-able
        mock.fail_once(bad.clone());

        let session = session_with(mock.clone());
        session.navigate("/").await.unwrap();
        session.click(&bad).await.expect("heals");

        let stats = session.stats();
        assert_eq!(stats.healing_retries, 1);
        let click = stats
            .history
            .iter()
            .find(|r| r.action == "click")
            .unwrap();
        assert!(click.ok);
        assert!(click.attempts >= 2);
    }

    #[tokio::test]
    async fn healing_exhausts_on_unrecoverable_failure() {
        let mock = Arc::new(MockBackend::new());
        mock.register_page("/", HTML);
        let session = session_with(mock);
        session.navigate("/").await.unwrap();
        let err = session
            .click(&Selector::css(".does-not-exist"))
            .await
            .unwrap_err();
        // Either GiveUp surfaces NotFound, or HealingExhausted wraps it; both
        // outcomes are acceptable failure modes for the public API.
        assert!(matches!(
            err,
            BrowserError::NotFound(_) | BrowserError::HealingExhausted { .. }
        ));
    }

    #[tokio::test]
    async fn llm_stub_strategy_is_pluggable() {
        let mock = Arc::new(MockBackend::new());
        mock.register_page("/", HTML);
        let session =
            BrowserSession::new(mock.clone(), Arc::new(LlmStubHealing::new()));
        session.navigate("/").await.unwrap();
        session
            .click(&Selector::role_named("button", "Get started"))
            .await
            .unwrap();
    }
}
