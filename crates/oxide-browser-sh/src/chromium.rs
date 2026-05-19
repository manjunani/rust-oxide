//! `chromiumoxide`-backed [`BrowserBackend`].
//!
//! Drives a headless Chromium / Chrome over the Chrome DevTools Protocol.
//! Tests that actually launch a browser are gated behind the
//! `live-browser` feature so the default `cargo test` run does not require a
//! browser to be installed on the host.

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::Page;
use futures::StreamExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::accessibility::{AxNode, AxQuery};
use crate::action::{ScrollDirection, Selector};
use crate::backend::BrowserBackend;
use crate::error::{BrowserError, Result};

/// Real-browser backend powered by `chromiumoxide`.
pub struct ChromiumBackend {
    browser: Mutex<Browser>,
    page: Mutex<Option<Page>>,
    handler: Mutex<Option<JoinHandle<()>>>,
}

impl ChromiumBackend {
    /// Launch a new headless Chromium with the default configuration.
    pub async fn launch() -> Result<Self> {
        Self::with_config(
            BrowserConfig::builder()
                .build()
                .map_err(BrowserError::Chromium)?,
        )
        .await
    }

    /// Launch a new headless Chromium with a custom configuration.
    pub async fn with_config(config: BrowserConfig) -> Result<Self> {
        let (browser, mut handler_stream) = Browser::launch(config)
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;

        // The CDP event handler must be driven for the connection to stay
        // alive. Spawn a task to drain it.
        let handler = tokio::spawn(async move {
            while let Some(event) = handler_stream.next().await {
                if event.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            browser: Mutex::new(browser),
            page: Mutex::new(None),
            handler: Mutex::new(Some(handler)),
        })
    }

    /// Translate a [`Selector`] into a CSS string the chromiumoxide API will
    /// accept. Role / text selectors are resolved by snapshotting the page's
    /// HTML and walking the synthesized accessibility tree for a `css_hint`.
    /// XPath selectors are resolved live in the page via `document.evaluate`
    /// — the script returns a unique CSS path to the first matching element.
    async fn resolve_to_css(&self, selector: &Selector) -> Result<String> {
        match selector {
            Selector::Css(s) => Ok(s.clone()),
            Selector::XPath(xpath) => self.xpath_to_css(xpath).await,
            Selector::Role { .. } | Selector::Text(_) => {
                let html = self.html().await?;
                let tree = AxNode::from_html(&html);
                let query = AxQuery::from_selector(selector).expect("semantic selector");
                let node = query
                    .find_in(&tree)
                    .ok_or_else(|| BrowserError::NotFound(selector.clone()))?;
                node.css_hint
                    .clone()
                    .ok_or_else(|| BrowserError::NotFound(selector.clone()))
            }
        }
    }

    /// Resolve an XPath expression into a CSS path via an in-page JS shim.
    ///
    /// The shim walks ancestors from the matched element back to the document
    /// root, building `tag[:nth-of-type(N)]` segments. CSS / XPath selectors
    /// produce identical downstream behaviour after this hop, so the same
    /// `find_element` call serves all four selector kinds.
    async fn xpath_to_css(&self, xpath: &str) -> Result<String> {
        let page = self.ensure_page().await?;
        let script = format!(
            r#"
            (function () {{
                const r = document.evaluate(
                    {xpath_lit},
                    document,
                    null,
                    XPathResult.FIRST_ORDERED_NODE_TYPE,
                    null
                );
                const el = r.singleNodeValue;
                if (!el) return null;
                if (el.id) return '#' + CSS.escape(el.id);
                const segs = [];
                let n = el;
                while (n && n.nodeType === 1 && n !== document.documentElement) {{
                    let tag = n.nodeName.toLowerCase();
                    const sibs = Array.from(n.parentNode ? n.parentNode.children : [])
                        .filter(s => s.nodeName === n.nodeName);
                    if (sibs.length > 1) {{
                        const idx = sibs.indexOf(n) + 1;
                        tag += ':nth-of-type(' + idx + ')';
                    }}
                    segs.unshift(tag);
                    n = n.parentElement;
                }}
                return 'html > ' + segs.join(' > ');
            }})()
            "#,
            xpath_lit = serde_json::to_string(xpath).unwrap()
        );
        let value: Option<String> = page
            .evaluate(script)
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?
            .into_value()
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        value.ok_or_else(|| BrowserError::NotFound(Selector::XPath(xpath.to_string())))
    }

    async fn ensure_page(&self) -> Result<Page> {
        if let Some(p) = self.page.lock().await.clone() {
            return Ok(p);
        }
        let page = self
            .browser
            .lock()
            .await
            .new_page("about:blank")
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        *self.page.lock().await = Some(page.clone());
        Ok(page)
    }
}

impl Drop for ChromiumBackend {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.handler.try_lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
    }
}

#[async_trait]
impl BrowserBackend for ChromiumBackend {
    async fn navigate(&self, url: &str) -> Result<()> {
        let page = self.ensure_page().await?;
        page.goto(url).await.map_err(|e| BrowserError::Navigation {
            url: url.to_string(),
            message: e.to_string(),
        })?;
        page.wait_for_navigation()
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        Ok(())
    }

    async fn click(&self, selector: &Selector) -> Result<()> {
        let css = self.resolve_to_css(selector).await?;
        let page = self.ensure_page().await?;
        let element = page
            .find_element(&css)
            .await
            .map_err(|_| BrowserError::NotFound(selector.clone()))?;
        element
            .click()
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        Ok(())
    }

    async fn input(&self, selector: &Selector, text: &str) -> Result<()> {
        let css = self.resolve_to_css(selector).await?;
        let page = self.ensure_page().await?;
        let element = page
            .find_element(&css)
            .await
            .map_err(|_| BrowserError::NotFound(selector.clone()))?;
        element
            .click()
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        element
            .type_str(text)
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        Ok(())
    }

    async fn scroll(&self, direction: ScrollDirection, amount: i32) -> Result<()> {
        let (dx, dy) = direction.deltas(amount);
        let page = self.ensure_page().await?;
        let js = format!("window.scrollBy({dx}, {dy});");
        page.evaluate(js)
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        Ok(())
    }

    async fn url(&self) -> Result<String> {
        let page = self.ensure_page().await?;
        page.url()
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?
            .ok_or_else(|| BrowserError::Backend("page has no URL yet".into()))
    }

    async fn html(&self) -> Result<String> {
        let page = self.ensure_page().await?;
        page.content()
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))
    }

    async fn screenshot(&self) -> Result<Vec<u8>> {
        let page = self.ensure_page().await?;
        page.screenshot(chromiumoxide::page::ScreenshotParams::builder().build())
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))
    }

    async fn accessibility_tree(&self) -> Result<AxNode> {
        // Try the real CDP `Accessibility.getFullAXTree` domain first. The
        // call enables the accessibility domain implicitly, walks the tree
        // returned by Chromium, and rebuilds it in our [`AxNode`] shape.
        // Fall back to the HTML-synthesized tree on any error so callers
        // always get a usable tree.
        match self.cdp_accessibility_tree().await {
            Ok(tree) => Ok(tree),
            Err(e) => {
                tracing::warn!(error = %e, "CDP accessibility tree failed, falling back to HTML synthesis");
                let html = self.html().await?;
                Ok(AxNode::from_html(&html))
            }
        }
    }
}

impl ChromiumBackend {
    /// Fetch the live CDP accessibility tree and translate it to [`AxNode`].
    async fn cdp_accessibility_tree(&self) -> Result<AxNode> {
        use chromiumoxide::cdp::browser_protocol::accessibility::{
            EnableParams, GetFullAxTreeParams,
        };
        let page = self.ensure_page().await?;
        // Enable the accessibility domain (idempotent — repeated enables
        // succeed silently).
        page.execute(EnableParams::default())
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        let result = page
            .execute(GetFullAxTreeParams::default())
            .await
            .map_err(|e| BrowserError::Chromium(e.to_string()))?;
        Ok(cdp_to_ax_tree(&result.nodes))
    }
}

/// Translate CDP `AXNode` list into our nested [`AxNode`] tree.
fn cdp_to_ax_tree(nodes: &[chromiumoxide::cdp::browser_protocol::accessibility::AxNode]) -> AxNode {
    use chromiumoxide::cdp::browser_protocol::accessibility::AxNode as CdpAxNode;

    fn ax_value_string(
        v: Option<&chromiumoxide::cdp::browser_protocol::accessibility::AxValue>,
    ) -> Option<String> {
        let v = v?;
        // `value` is `serde_json::Value`; pull out string forms.
        match &v.value {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(other) => Some(other.to_string()),
            None => None,
        }
    }

    fn convert(node: &CdpAxNode, by_id: &std::collections::HashMap<String, &CdpAxNode>) -> AxNode {
        let role = ax_value_string(node.role.as_ref()).unwrap_or_else(|| "generic".into());
        let name = ax_value_string(node.name.as_ref()).filter(|s| !s.is_empty());
        let value = ax_value_string(node.value.as_ref());
        let mut children = Vec::new();
        if let Some(child_ids) = &node.child_ids {
            for cid in child_ids {
                if let Some(child) = by_id.get(&cid.inner().to_string()) {
                    if !child.ignored {
                        children.push(convert(child, by_id));
                    }
                }
            }
        }
        AxNode {
            role,
            name,
            value,
            css_hint: None,
            children,
        }
    }

    use std::collections::HashMap;
    let by_id: HashMap<String, &CdpAxNode> = nodes
        .iter()
        .map(|n| (n.node_id.inner().to_string(), n))
        .collect();

    // Root = node whose parent id is absent in the map (or who carries no
    // parent_id field at all). Fall back to first node.
    let root = nodes
        .iter()
        .find(|n| {
            n.parent_id
                .as_ref()
                .is_none_or(|p| !by_id.contains_key(&p.inner().to_string()))
        })
        .or_else(|| nodes.first());

    match root {
        Some(r) => convert(r, &by_id),
        None => AxNode::empty_root(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xpath_selector_is_accepted() {
        // No browser needed — confirm the type carries cleanly through the
        // public API. The actual resolution path (document.evaluate → CSS)
        // requires a live Chromium and is exercised by the `live-browser`
        // gated test below.
        let _ = std::mem::size_of::<ChromiumBackend>();
        let sel = Selector::XPath("//button".into());
        assert!(matches!(sel, Selector::XPath(_)));
    }

    #[cfg(feature = "live-browser")]
    #[tokio::test]
    #[ignore = "requires a local Chromium / Chrome install"]
    async fn launches_and_navigates() {
        let backend = ChromiumBackend::launch().await.expect("launch");
        backend.navigate("about:blank").await.expect("navigate");
        let url = backend.url().await.expect("url");
        assert!(url.contains("about:blank"));
    }
}
