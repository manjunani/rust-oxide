//! Accessibility tree types + synthesis from raw HTML.
//!
//! When a backend exposes a real CDP `Accessibility.getFullAXTree` channel we
//! can deserialize directly into [`AxNode`]. When it does not (the mock
//! backend, or a backend that hasn't enabled the AX domain) we synthesize a
//! tree by walking the HTML and mapping tags / ARIA attributes to roles.
//!
//! The synthesized tree is intentionally lossy — it only knows about a
//! curated set of semantic tags — but it is the same shape callers see from
//! the real CDP path, which keeps the self-healing logic backend-agnostic.

use scraper::{ElementRef, Html, Node};
use serde::{Deserialize, Serialize};

use crate::action::Selector;

/// A single node in the accessibility tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxNode {
    /// ARIA role (`button`, `link`, `heading`, …).
    pub role: String,
    /// Accessible name (visible text or `aria-label` / `alt`).
    pub name: Option<String>,
    /// Editable value, if any (input value, contenteditable text).
    pub value: Option<String>,
    /// Optional CSS selector that uniquely identifies this node on the source
    /// page. Used by the healing strategy to translate role queries back into
    /// targetable selectors.
    pub css_hint: Option<String>,
    /// Child nodes.
    pub children: Vec<AxNode>,
}

impl AxNode {
    /// An empty root for documents we cannot parse.
    pub fn empty_root() -> Self {
        Self {
            role: "WebArea".into(),
            name: None,
            value: None,
            css_hint: None,
            children: Vec::new(),
        }
    }

    /// Walk the tree pre-order, yielding a reference to every node.
    pub fn walk(&self) -> AxIter<'_> {
        AxIter { stack: vec![self] }
    }

    /// Synthesize an accessibility tree from a raw HTML document.
    pub fn from_html(html: &str) -> Self {
        let doc = Html::parse_document(html);
        let mut root = AxNode {
            role: "WebArea".into(),
            name: None,
            value: None,
            css_hint: Some("html".into()),
            children: Vec::new(),
        };
        for node in doc.tree.root().children() {
            if let Some(el) = ElementRef::wrap(node) {
                walk_element(el, &mut root.children);
            }
        }
        root
    }
}

fn walk_element(el: ElementRef<'_>, out: &mut Vec<AxNode>) {
    let tag = el.value().name();
    if matches!(tag, "script" | "style" | "head" | "meta" | "link") {
        return;
    }

    if let Some(node) = synthesize(el) {
        let mut node = node;
        for child in el.children() {
            if let Some(child_el) = ElementRef::wrap(child) {
                walk_element(child_el, &mut node.children);
            }
        }
        out.push(node);
    } else {
        // Container without a semantic role (div/span/section/…): inline its
        // children into the parent so the tree stays compact.
        for child in el.children() {
            if let Some(child_el) = ElementRef::wrap(child) {
                walk_element(child_el, out);
            }
        }
    }
}

/// Synthesize an [`AxNode`] for a single HTML element. Visible to the
/// healing strategies so they can translate a brittle CSS selector into a
/// semantic role+name selector for the matching element.
pub(crate) fn synthesize_for_element(el: ElementRef<'_>) -> Option<AxNode> {
    synthesize(el)
}

fn synthesize(el: ElementRef<'_>) -> Option<AxNode> {
    let tag = el.value().name();
    let attrs = el.value();
    let text = collect_text(el);
    let aria_role = attrs.attr("role").map(|s| s.to_string());
    let aria_label = attrs.attr("aria-label").map(|s| s.to_string());

    let (role, name, value) = match tag {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => ("heading".to_string(), Some(text), None),
        "a" if attrs.attr("href").is_some() => ("link".to_string(), Some(text), None),
        "button" => ("button".to_string(), Some(text), None),
        "input" => match attrs.attr("type").unwrap_or("text") {
            "button" | "submit" | "reset" => (
                "button".to_string(),
                Some(
                    attrs
                        .attr("value")
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                ),
                None,
            ),
            "checkbox" => (
                "checkbox".to_string(),
                attrs.attr("name").map(|s| s.to_string()),
                None,
            ),
            "radio" => (
                "radio".to_string(),
                attrs.attr("name").map(|s| s.to_string()),
                None,
            ),
            _ => (
                "textbox".to_string(),
                attrs
                    .attr("aria-label")
                    .or_else(|| attrs.attr("placeholder"))
                    .or_else(|| attrs.attr("name"))
                    .map(|s| s.to_string()),
                attrs.attr("value").map(|s| s.to_string()),
            ),
        },
        "textarea" => (
            "textbox".to_string(),
            attrs
                .attr("aria-label")
                .or_else(|| attrs.attr("placeholder"))
                .or_else(|| attrs.attr("name"))
                .map(|s| s.to_string()),
            Some(text),
        ),
        "img" => (
            "img".to_string(),
            attrs.attr("alt").map(|s| s.to_string()),
            None,
        ),
        "nav" => ("navigation".to_string(), aria_label.clone(), None),
        "main" => ("main".to_string(), aria_label.clone(), None),
        "form" => ("form".to_string(), aria_label.clone(), None),
        "ul" | "ol" => ("list".to_string(), None, None),
        "li" => ("listitem".to_string(), Some(text), None),
        "table" => ("table".to_string(), aria_label.clone(), None),
        "p" => ("paragraph".to_string(), Some(text), None),
        // Honour an explicit ARIA role override even on otherwise-unknown
        // elements (e.g. `<div role="dialog">`).
        _ if aria_role.is_some() => (aria_role.clone().unwrap(), aria_label.or(Some(text)), None),
        _ => return None,
    };

    let role = aria_role.unwrap_or(role);
    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let css_hint = build_css_hint(el);

    Some(AxNode {
        role,
        name,
        value,
        css_hint,
        children: Vec::new(),
    })
}

fn collect_text(el: ElementRef<'_>) -> String {
    let mut buf = String::new();
    for node in el.descendants() {
        if let Node::Text(t) = node.value() {
            buf.push_str(t);
            buf.push(' ');
        }
    }
    buf.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_css_hint(el: ElementRef<'_>) -> Option<String> {
    let v = el.value();
    let tag = v.name();
    if let Some(id) = v.attr("id") {
        return Some(format!("#{}", id));
    }
    if let Some(name) = v.attr("name") {
        return Some(format!("{tag}[name=\"{name}\"]"));
    }
    if let Some(class) = v.attr("class") {
        if let Some(first) = class.split_whitespace().next() {
            return Some(format!("{tag}.{first}"));
        }
    }
    if let Some(href) = v.attr("href") {
        return Some(format!("{tag}[href=\"{href}\"]"));
    }
    if let Some(typ) = v.attr("type") {
        return Some(format!("{tag}[type=\"{typ}\"]"));
    }
    Some(tag.to_string())
}

/// Pre-order iterator over the accessibility tree.
pub struct AxIter<'a> {
    stack: Vec<&'a AxNode>,
}

impl<'a> Iterator for AxIter<'a> {
    type Item = &'a AxNode;
    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        // Push children in reverse so the iteration order is parent → first
        // child → grandchild …
        for child in node.children.iter().rev() {
            self.stack.push(child);
        }
        Some(node)
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// A pre-compiled query over an accessibility tree.
#[derive(Debug, Clone)]
pub enum AxQuery {
    /// Match by role and optional accessible name.
    Role {
        /// ARIA role.
        role: String,
        /// Optional accessible name. Substring, case-insensitive.
        name: Option<String>,
    },
    /// Match by accessible name only.
    Text(String),
}

impl AxQuery {
    /// Build a query from a [`Selector`], if the selector has a semantic
    /// representation. CSS / XPath selectors have no AX equivalent and return
    /// `None`.
    pub fn from_selector(sel: &Selector) -> Option<Self> {
        match sel {
            Selector::Role { role, name } => Some(AxQuery::Role {
                role: role.clone(),
                name: name.clone(),
            }),
            Selector::Text(t) => Some(AxQuery::Text(t.clone())),
            _ => None,
        }
    }

    /// Find the first node in `tree` that satisfies the query.
    pub fn find_in<'a>(&self, tree: &'a AxNode) -> Option<&'a AxNode> {
        tree.walk().find(|n| self.matches(n))
    }

    /// Find every node in `tree` that satisfies the query.
    pub fn find_all_in<'a>(&self, tree: &'a AxNode) -> Vec<&'a AxNode> {
        tree.walk().filter(|n| self.matches(n)).collect()
    }

    fn matches(&self, node: &AxNode) -> bool {
        match self {
            AxQuery::Role { role, name } => {
                if !node.role.eq_ignore_ascii_case(role) {
                    return false;
                }
                match name {
                    None => true,
                    Some(want) => match &node.name {
                        Some(n) => n.to_ascii_lowercase().contains(&want.to_ascii_lowercase()),
                        None => false,
                    },
                }
            }
            AxQuery::Text(t) => match &node.name {
                Some(n) => n.to_ascii_lowercase().contains(&t.to_ascii_lowercase()),
                None => false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<!doctype html>
        <html><body>
          <h1>Welcome</h1>
          <nav aria-label="Primary"><a href="/about">About us</a></nav>
          <form>
            <label for="email">Email</label>
            <input id="email" type="email" name="email" placeholder="you@example.com">
            <button id="submit">Sign up</button>
          </form>
        </body></html>"#;

    #[test]
    fn synthesizes_basic_roles() {
        let tree = AxNode::from_html(HTML);
        let roles: Vec<_> = tree.walk().map(|n| n.role.as_str()).collect();
        assert!(roles.contains(&"heading"));
        assert!(roles.contains(&"navigation"));
        assert!(roles.contains(&"link"));
        assert!(roles.contains(&"button"));
        assert!(roles.contains(&"textbox"));
    }

    #[test]
    fn query_finds_button_by_name() {
        let tree = AxNode::from_html(HTML);
        let q = AxQuery::Role {
            role: "button".into(),
            name: Some("sign up".into()),
        };
        let n = q.find_in(&tree).expect("found");
        assert_eq!(n.css_hint.as_deref(), Some("#submit"));
    }

    #[test]
    fn query_falls_back_to_text() {
        let tree = AxNode::from_html(HTML);
        let q = AxQuery::Text("about us".into());
        let n = q.find_in(&tree).expect("found");
        assert_eq!(n.role, "link");
    }
}
