//! Backend abstraction.
//!
//! [`BrowserBackend`] is the trait every concrete driver implements. The
//! session layer and self-healing logic talk to this trait, never to a
//! specific backend, which keeps the public API testable without launching a
//! real browser.

use async_trait::async_trait;

use crate::accessibility::AxNode;
use crate::action::{ScrollDirection, Selector};
use crate::error::Result;

/// Async trait implemented by every browser backend.
#[async_trait]
pub trait BrowserBackend: Send + Sync {
    /// Navigate to `url`. Returns `Ok(())` once the navigation has been
    /// dispatched (backends are free to also wait for `load`).
    async fn navigate(&self, url: &str) -> Result<()>;

    /// Locate an element via `selector` and dispatch a click.
    async fn click(&self, selector: &Selector) -> Result<()>;

    /// Locate an element via `selector` and type `text` into it. The default
    /// expectation is that the field is editable (input/textarea or
    /// `contenteditable`).
    async fn input(&self, selector: &Selector, text: &str) -> Result<()>;

    /// Scroll the viewport by `amount` pixels in `direction`.
    async fn scroll(&self, direction: ScrollDirection, amount: i32) -> Result<()>;

    /// The current URL of the active page.
    async fn url(&self) -> Result<String>;

    /// The complete serialized HTML of the active page.
    async fn html(&self) -> Result<String>;

    /// PNG screenshot of the viewport. Backends that cannot render (mock,
    /// stubs) may return [`crate::error::BrowserError::Unsupported`] or an
    /// empty `Vec`.
    async fn screenshot(&self) -> Result<Vec<u8>>;

    /// Snapshot of the accessibility tree for the current document.
    async fn accessibility_tree(&self) -> Result<AxNode>;
}
