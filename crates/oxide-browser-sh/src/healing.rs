//! Self-healing strategies.
//!
//! When an action against the [`backend`](crate::backend) fails, the
//! [`session`](crate::session) layer captures contextual evidence (current
//! URL, accessibility tree, HTML, the original selector) and forwards it to a
//! [`HealingStrategy`]. The strategy responds with a [`HealingDecision`] —
//! either an alternative [`Selector`] to retry with, an instruction to give
//! up, or an alternative full action to execute.
//!
//! Two strategies ship today:
//!
//! * [`DefaultHealing`] — pure-Rust. Inspects the AX tree synthesized from
//!   the page's HTML, pulls candidate nodes that share role / text with the
//!   failing selector, and rewrites the selector against the best match's
//!   `css_hint`.
//! * [`LlmStubHealing`] — a placeholder for the eventual
//!   `oxide-llm-orchestrator` integration. Captures the failure context and
//!   logs it for the future LLM client; in the meantime it delegates to
//!   [`DefaultHealing`] so end-to-end flows still make progress.

use scraper::Html;
use serde::{Deserialize, Serialize};

use crate::accessibility::{synthesize_for_element, AxNode, AxQuery};
use crate::action::Selector;

/// What the healing strategy wants the session to do next.
#[derive(Debug, Clone)]
pub enum HealingDecision {
    /// Retry the same action against a new selector.
    Retry(Selector),
    /// Give up; the session will surface the original error.
    GiveUp,
}

/// Information handed to a healing strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealingContext {
    /// The action that was attempted (free-form descriptor for logs).
    pub action: String,
    /// The selector that failed.
    pub selector: Selector,
    /// The current URL.
    pub url: Option<String>,
    /// Raw HTML of the page at the moment of failure.
    pub html: String,
    /// Whether the backend was able to produce a screenshot. The bytes
    /// themselves are not embedded to keep contexts copy-cheap; an LLM-based
    /// strategy fetches them lazily.
    pub screenshot_available: bool,
    /// 1-based attempt counter (the original try is attempt 1).
    pub attempt: usize,
    /// Original failure message.
    pub error: String,
}

/// Healing strategies implement this trait.
///
/// Strategies are synchronous to keep call-sites simple; long-running LLM
/// invocations should run on a dedicated channel and have their results
/// applied via a [`HealingDecision::Retry`] returned by a cached future.
pub trait HealingStrategy: Send + Sync {
    /// Suggest a recovery for `ctx`.
    fn heal(&self, ctx: &HealingContext) -> HealingDecision;

    /// Maximum number of attempts to make per action. Default: 3.
    fn max_attempts(&self) -> usize {
        3
    }

    /// Human-readable label used in logs / events.
    fn label(&self) -> &'static str {
        "unnamed"
    }
}

// ---------------------------------------------------------------------------
// DefaultHealing
// ---------------------------------------------------------------------------

/// Rule-based healing that uses the accessibility tree to rewrite selectors.
#[derive(Debug, Default, Clone)]
pub struct DefaultHealing {
    /// Override for `max_attempts`. `None` keeps the trait default (3).
    pub max_attempts: Option<usize>,
}

impl DefaultHealing {
    /// Build the strategy with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the maximum attempts cap.
    pub fn with_max_attempts(mut self, max: usize) -> Self {
        self.max_attempts = Some(max);
        self
    }
}

impl HealingStrategy for DefaultHealing {
    fn heal(&self, ctx: &HealingContext) -> HealingDecision {
        let tree = AxNode::from_html(&ctx.html);
        if let Some(retry) = upgrade_via_scraper(&ctx.selector, &ctx.html)
            .or_else(|| upgrade_to_semantic(&ctx.selector, &tree))
            .or_else(|| rewrite_css_hint(&ctx.selector, &tree))
        {
            HealingDecision::Retry(retry)
        } else {
            HealingDecision::GiveUp
        }
    }

    fn max_attempts(&self) -> usize {
        self.max_attempts.unwrap_or(3)
    }

    fn label(&self) -> &'static str {
        "default-rules"
    }
}

/// Re-resolve the failed CSS selector against the page via [`scraper`] and
/// translate the single matching element (if any) into a semantic role+name
/// selector. This is the most reliable heuristic — a CSS selector that
/// previously failed at the backend usually still matches the DOM snapshot,
/// at which point we can simply name the element semantically.
fn upgrade_via_scraper(failed: &Selector, html: &str) -> Option<Selector> {
    let Selector::Css(css) = failed else { return None };
    let sel = scraper::Selector::parse(css).ok()?;
    let doc = Html::parse_document(html);
    let mut iter = doc.select(&sel);
    let el = iter.next()?;
    if iter.next().is_some() {
        return None;
    }
    let node = synthesize_for_element(el)?;
    Some(Selector::Role {
        role: node.role,
        name: node.name,
    })
}

/// If the failed selector is a brittle CSS selector that we have an AX
/// fallback for, return the semantic equivalent.
fn upgrade_to_semantic(failed: &Selector, tree: &AxNode) -> Option<Selector> {
    let Selector::Css(css) = failed else { return None };

    // Heuristic: look for an AX node whose css_hint contains the same final
    // selector token (id / class / tag). If exactly one node matches, return a
    // role+name selector for it; that is strictly more resilient than the
    // original CSS.
    let needle = css.trim_start_matches('.').trim_start_matches('#').to_string();
    let candidates: Vec<_> = tree
        .walk()
        .filter(|n| n.css_hint.as_deref().is_some_and(|h| h.contains(&needle)))
        .collect();
    if candidates.len() == 1 {
        let n = candidates[0];
        return Some(Selector::Role {
            role: n.role.clone(),
            name: n.name.clone(),
        });
    }
    None
}

/// If the failed selector is semantic but mis-spelled / case-shifted, look
/// for the nearest AX match and return its `css_hint` as a CSS selector.
fn rewrite_css_hint(failed: &Selector, tree: &AxNode) -> Option<Selector> {
    let query = AxQuery::from_selector(failed)?;
    let node = query.find_in(tree)?;
    node.css_hint.clone().map(Selector::Css)
}

// ---------------------------------------------------------------------------
// LlmStubHealing
// ---------------------------------------------------------------------------

/// Placeholder strategy that simulates calling out to an LLM-driven
/// orchestrator.
///
/// Logs the [`HealingContext`] (so the future `oxide-llm-orchestrator`
/// integration has a working hand-off shape), then delegates the actual
/// decision to [`DefaultHealing`]. Once the orchestrator lands this struct
/// can route the context to it and unwrap the suggestion verbatim.
pub struct LlmStubHealing {
    inner: DefaultHealing,
}

impl LlmStubHealing {
    /// Build a stub backed by the default rule engine.
    pub fn new() -> Self {
        Self {
            inner: DefaultHealing::new(),
        }
    }
}

impl Default for LlmStubHealing {
    fn default() -> Self {
        Self::new()
    }
}

impl HealingStrategy for LlmStubHealing {
    fn heal(&self, ctx: &HealingContext) -> HealingDecision {
        tracing::info!(
            target: "oxide_browser_sh::llm",
            action = %ctx.action,
            selector = %ctx.selector.label(),
            url = ?ctx.url,
            attempt = ctx.attempt,
            error = %ctx.error,
            "LLM placeholder: would forward this context to oxide-llm-orchestrator"
        );
        self.inner.heal(ctx)
    }

    fn max_attempts(&self) -> usize {
        self.inner.max_attempts()
    }

    fn label(&self) -> &'static str {
        "llm-stub"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<!doctype html>
        <html><body>
          <button id="cta" class="primary">Get started</button>
          <a id="learn-more" href="/docs">Learn more</a>
        </body></html>"#;

    fn ctx_for(selector: Selector) -> HealingContext {
        HealingContext {
            action: "click".into(),
            selector,
            url: Some("https://example.com".into()),
            html: HTML.into(),
            screenshot_available: false,
            attempt: 1,
            error: "element not found".into(),
        }
    }

    #[test]
    fn default_upgrades_unique_css_to_role_name() {
        let dh = DefaultHealing::new();
        let decision = dh.heal(&ctx_for(Selector::css("#cta")));
        match decision {
            HealingDecision::Retry(Selector::Role { role, name }) => {
                assert_eq!(role, "button");
                assert_eq!(name.as_deref(), Some("Get started"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn default_rewrites_role_to_css_hint() {
        let dh = DefaultHealing::new();
        let decision = dh.heal(&ctx_for(Selector::role_named("link", "Learn more")));
        match decision {
            HealingDecision::Retry(Selector::Css(css)) => {
                assert_eq!(css, "#learn-more");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn default_gives_up_on_unrecognisable_selector() {
        let dh = DefaultHealing::new();
        let decision = dh.heal(&ctx_for(Selector::css(".no-such-thing-here")));
        assert!(matches!(decision, HealingDecision::GiveUp));
    }

    #[test]
    fn llm_stub_delegates_to_default() {
        let stub = LlmStubHealing::new();
        let decision = stub.heal(&ctx_for(Selector::css("#cta")));
        assert!(matches!(decision, HealingDecision::Retry(_)));
        assert_eq!(stub.label(), "llm-stub");
    }
}
