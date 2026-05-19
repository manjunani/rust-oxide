//! # `oxide-browser-sh` — Self-Healing Browser Automation
//!
//! `oxide-browser-sh` treats any website as a programmable interface for AI
//! agents. It is built around three deliberate design choices:
//!
//! 1. **Accessibility-tree first targeting.** Rather than handing agents
//!    brittle CSS selectors, the module exposes a [`Selector`] enum that
//!    prefers semantic queries (role + name, text content) and falls back to
//!    CSS / XPath only when asked. The same selector translates to both the
//!    real Chromium backend and the in-memory mock backend used in tests.
//!
//! 2. **Pluggable backends.** [`BrowserBackend`] is a trait. The default
//!    [`chromium::ChromiumBackend`] drives a real headless Chromium through
//!    `chromiumoxide`; the [`mock::MockBackend`] holds an in-memory HTML
//!    document and evaluates queries through [`scraper`] so tests can exercise
//!    the full self-healing logic without launching a browser.
//!
//! 3. **Healing-by-default sessions.** [`session::BrowserSession`] wraps a
//!    backend with a [`healing::HealingStrategy`]. When an action fails the
//!    session captures the current URL, HTML, and accessibility tree, asks the
//!    strategy for an alternative selector, and retries. The default strategy
//!    derives candidates from the accessibility tree; an LLM-driven strategy
//!    will be wired up when `oxide-llm-orchestrator` lands.
//!
//! The crate also ships:
//!
//! * [`extract`] — HTML → clean Markdown, post-processed through
//!   [`oxide_compress`] so payloads stay token-efficient.
//! * [`accessibility`] — `AxNode` tree, synthesized from raw HTML when the
//!   backend does not expose a CDP `Accessibility.getFullAXTree` channel.
//! * [`kernel`] — a [`oxide_k::module::Module`] implementation that exposes
//!   browser commands over the kernel message bus.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod accessibility;
pub mod action;
pub mod backend;
#[cfg(feature = "chromium")]
pub mod chromium;
pub mod error;
pub mod extract;
pub mod healing;
pub mod kernel;
pub mod mock;
pub mod session;

pub use accessibility::{AxNode, AxQuery};
pub use action::{ScrollDirection, Selector};
pub use backend::BrowserBackend;
pub use error::{BrowserError, Result};
pub use extract::{extract_markdown, ExtractOptions};
pub use healing::{DefaultHealing, HealingDecision, HealingStrategy, LlmStubHealing};
pub use kernel::BrowserModule;
pub use mock::MockBackend;
pub use session::{ActionRecord, BrowserSession, SessionStats};
