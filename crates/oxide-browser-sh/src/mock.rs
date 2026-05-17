//! An in-memory [`BrowserBackend`] used for tests and offline simulation.
//!
//! The mock holds a map of URL → HTML, evaluates [`Selector`]s against the
//! currently-loaded document via [`scraper`], and records every action so
//! tests can assert what the session attempted.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use scraper::Html;

use crate::accessibility::{AxNode, AxQuery};
use crate::action::{ScrollDirection, Selector};
use crate::backend::BrowserBackend;
use crate::error::{BrowserError, Result};

/// A single action observed by a [`MockBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockAction {
    /// `navigate` invocation.
    Navigate(String),
    /// `click` invocation.
    Click(Selector),
    /// `input` invocation.
    Input(Selector, String),
    /// `scroll` invocation.
    Scroll(ScrollDirection, i32),
}

struct State {
    pages: HashMap<String, String>,
    current_url: Option<String>,
    actions: Vec<MockAction>,
    /// Selectors that should fail the *first* time they are queried (used to
    /// exercise the healing path in tests).
    one_shot_failures: Vec<Selector>,
}

/// In-memory backend.
pub struct MockBackend {
    state: Mutex<State>,
}

impl MockBackend {
    /// Build an empty backend with no registered pages.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                pages: HashMap::new(),
                current_url: None,
                actions: Vec::new(),
                one_shot_failures: Vec::new(),
            }),
        }
    }

    /// Register a page so a future `navigate(url)` succeeds.
    pub fn register_page(&self, url: impl Into<String>, html: impl Into<String>) {
        let mut state = self.state.lock().unwrap();
        state.pages.insert(url.into(), html.into());
    }

    /// Cause the next query for `selector` to fail (used to trigger healing).
    pub fn fail_once(&self, selector: Selector) {
        let mut state = self.state.lock().unwrap();
        state.one_shot_failures.push(selector);
    }

    /// Snapshot of every action observed so far.
    pub fn actions(&self) -> Vec<MockAction> {
        self.state.lock().unwrap().actions.clone()
    }

    /// Reset all recorded state.
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.current_url = None;
        state.actions.clear();
        state.one_shot_failures.clear();
    }

    fn current_html(&self) -> Result<String> {
        let state = self.state.lock().unwrap();
        let url = state
            .current_url
            .as_ref()
            .ok_or_else(|| BrowserError::Backend("no page loaded".into()))?;
        state
            .pages
            .get(url)
            .cloned()
            .ok_or_else(|| BrowserError::Backend(format!("unknown page `{url}`")))
    }

    fn consume_one_shot(&self, selector: &Selector) -> bool {
        let mut state = self.state.lock().unwrap();
        if let Some(pos) = state.one_shot_failures.iter().position(|s| s == selector) {
            state.one_shot_failures.remove(pos);
            true
        } else {
            false
        }
    }

    fn ensure_present(&self, selector: &Selector) -> Result<()> {
        if self.consume_one_shot(selector) {
            return Err(BrowserError::NotFound(selector.clone()));
        }
        let html = self.current_html()?;
        if locate(&html, selector) {
            Ok(())
        } else {
            Err(BrowserError::NotFound(selector.clone()))
        }
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BrowserBackend for MockBackend {
    async fn navigate(&self, url: &str) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap();
            if !state.pages.contains_key(url) {
                return Err(BrowserError::Navigation {
                    url: url.to_string(),
                    message: "page not registered with MockBackend".into(),
                });
            }
            state.current_url = Some(url.to_string());
            state.actions.push(MockAction::Navigate(url.to_string()));
        }
        Ok(())
    }

    async fn click(&self, selector: &Selector) -> Result<()> {
        self.ensure_present(selector)?;
        self.state
            .lock()
            .unwrap()
            .actions
            .push(MockAction::Click(selector.clone()));
        Ok(())
    }

    async fn input(&self, selector: &Selector, text: &str) -> Result<()> {
        self.ensure_present(selector)?;
        self.state
            .lock()
            .unwrap()
            .actions
            .push(MockAction::Input(selector.clone(), text.to_string()));
        Ok(())
    }

    async fn scroll(&self, direction: ScrollDirection, amount: i32) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .actions
            .push(MockAction::Scroll(direction, amount));
        Ok(())
    }

    async fn url(&self) -> Result<String> {
        self.state
            .lock()
            .unwrap()
            .current_url
            .clone()
            .ok_or_else(|| BrowserError::Backend("no page loaded".into()))
    }

    async fn html(&self) -> Result<String> {
        self.current_html()
    }

    async fn screenshot(&self) -> Result<Vec<u8>> {
        // Mock backends cannot render. Return a small PNG-ish stub so the
        // calling code does not need to special-case the mock.
        Ok(vec![])
    }

    async fn accessibility_tree(&self) -> Result<AxNode> {
        let html = self.current_html()?;
        Ok(AxNode::from_html(&html))
    }
}

fn locate(html: &str, selector: &Selector) -> bool {
    match selector {
        Selector::Css(css) => match scraper::Selector::parse(css) {
            Ok(sel) => Html::parse_document(html).select(&sel).next().is_some(),
            Err(_) => false,
        },
        Selector::XPath(_) => false,
        Selector::Role { .. } | Selector::Text(_) => {
            let tree = AxNode::from_html(html);
            let query = AxQuery::from_selector(selector).expect("semantic selector");
            query.find_in(&tree).is_some()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<!doctype html>
        <html><body>
          <h1>Welcome</h1>
          <button id="cta">Get started</button>
          <input type="text" name="email" placeholder="you@example.com">
        </body></html>"#;

    #[tokio::test]
    async fn navigate_records_url_and_state() {
        let mock = MockBackend::new();
        mock.register_page("https://example.com/", HTML);
        mock.navigate("https://example.com/").await.unwrap();
        assert_eq!(mock.url().await.unwrap(), "https://example.com/");
        assert_eq!(
            mock.actions(),
            vec![MockAction::Navigate("https://example.com/".into())]
        );
    }

    #[tokio::test]
    async fn navigate_unregistered_page_errors() {
        let mock = MockBackend::new();
        let err = mock.navigate("https://missing").await.unwrap_err();
        assert!(matches!(err, BrowserError::Navigation { .. }));
    }

    #[tokio::test]
    async fn click_resolves_role_selector() {
        let mock = MockBackend::new();
        mock.register_page("/", HTML);
        mock.navigate("/").await.unwrap();
        mock.click(&Selector::role_named("button", "Get started"))
            .await
            .unwrap();
        let actions = mock.actions();
        assert!(matches!(actions.last(), Some(MockAction::Click(_))));
    }

    #[tokio::test]
    async fn click_missing_selector_errors() {
        let mock = MockBackend::new();
        mock.register_page("/", HTML);
        mock.navigate("/").await.unwrap();
        let err = mock.click(&Selector::css(".nope")).await.unwrap_err();
        assert!(matches!(err, BrowserError::NotFound(_)));
    }

    #[tokio::test]
    async fn fail_once_drops_first_attempt_only() {
        let mock = MockBackend::new();
        mock.register_page("/", HTML);
        mock.navigate("/").await.unwrap();
        let sel = Selector::css("#cta");
        mock.fail_once(sel.clone());
        assert!(mock.click(&sel).await.is_err());
        mock.click(&sel).await.unwrap();
    }
}
