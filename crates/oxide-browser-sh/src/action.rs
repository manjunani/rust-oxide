//! Action primitives and selectors.

use serde::{Deserialize, Serialize};

/// How to identify an element on the page.
///
/// The variants are roughly ordered from most-preferred (semantic, resilient)
/// to least-preferred (brittle, layout-dependent). Self-healing strategies use
/// this ordering to upgrade a failing CSS selector into a stable
/// role-and-name query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    /// ARIA role plus optional accessible name (e.g. `button "Submit"`).
    Role {
        /// ARIA role (`button`, `link`, `textbox`, …).
        role: String,
        /// Accessible name; matched case-insensitively against `name` or the
        /// element's visible text.
        name: Option<String>,
    },
    /// Match by visible text content (substring, case-insensitive).
    Text(String),
    /// CSS selector. Resilient backends should prefer the semantic variants.
    Css(String),
    /// XPath expression. Provided for parity with legacy automation tools.
    XPath(String),
}

impl Selector {
    /// Convenience: build a role-only selector.
    pub fn role(role: impl Into<String>) -> Self {
        Selector::Role {
            role: role.into(),
            name: None,
        }
    }

    /// Convenience: build a role+name selector.
    pub fn role_named(role: impl Into<String>, name: impl Into<String>) -> Self {
        Selector::Role {
            role: role.into(),
            name: Some(name.into()),
        }
    }

    /// Convenience: build a CSS selector.
    pub fn css(s: impl Into<String>) -> Self {
        Selector::Css(s.into())
    }

    /// Convenience: build a text selector.
    pub fn text(s: impl Into<String>) -> Self {
        Selector::Text(s.into())
    }

    /// Short human-readable label for logs.
    pub fn label(&self) -> String {
        match self {
            Selector::Role {
                role,
                name: Some(n),
            } => format!("role:{role}[name~={n:?}]"),
            Selector::Role { role, name: None } => format!("role:{role}"),
            Selector::Text(t) => format!("text:{t:?}"),
            Selector::Css(c) => format!("css:{c}"),
            Selector::XPath(x) => format!("xpath:{x}"),
        }
    }
}

/// Scroll direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    /// Scroll upward by the given amount.
    Up,
    /// Scroll downward by the given amount.
    Down,
    /// Scroll leftward by the given amount.
    Left,
    /// Scroll rightward by the given amount.
    Right,
}

impl ScrollDirection {
    /// Translate (direction, amount) into (dx, dy) pixel deltas.
    pub fn deltas(self, amount: i32) -> (i32, i32) {
        let amount = amount.max(0);
        match self {
            ScrollDirection::Up => (0, -amount),
            ScrollDirection::Down => (0, amount),
            ScrollDirection::Left => (-amount, 0),
            ScrollDirection::Right => (amount, 0),
        }
    }
}
