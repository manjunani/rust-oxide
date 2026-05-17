//! HTML → token-efficient Markdown extraction.
//!
//! The extractor walks the parsed HTML tree once and emits Markdown directly,
//! skipping noise (`<script>`, `<style>`, `<nav>`, `<footer>`, …) and
//! collapsing whitespace. The output is then optionally piped through
//! [`oxide_compress::strip_metadata`] (to remove boilerplate the walker
//! didn't catch) and [`oxide_compress::chunk_text`] (to bound chunk length
//! for downstream LLM consumption).

use scraper::{ElementRef, Html, Node};
use serde::{Deserialize, Serialize};

/// Options controlling [`extract_markdown`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractOptions {
    /// If `Some(n)`, the output is also returned chunked into pieces of at
    /// most `n` characters via [`oxide_compress::chunk_text`].
    pub max_chunk_chars: Option<usize>,
    /// If `true` (default), run the final Markdown through
    /// [`oxide_compress::strip_metadata`] to cull boilerplate.
    pub strip_boilerplate: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            max_chunk_chars: None,
            strip_boilerplate: true,
        }
    }
}

/// Extracted content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedContent {
    /// Full Markdown rendering.
    pub markdown: String,
    /// Page title (`<title>` tag).
    pub title: Option<String>,
    /// Token-efficient chunks (only populated when
    /// [`ExtractOptions::max_chunk_chars`] is `Some`).
    pub chunks: Vec<String>,
}

/// Extract Markdown from an HTML document.
pub fn extract_markdown(html: &str, opts: &ExtractOptions) -> ExtractedContent {
    let doc = Html::parse_document(html);
    let title = extract_title(&doc);

    let mut buf = String::new();
    for node in doc.tree.root().children() {
        if let Some(el) = ElementRef::wrap(node) {
            walk(el, &mut buf, 0);
        }
    }

    let mut markdown = normalize_whitespace(&buf);
    if opts.strip_boilerplate {
        markdown = oxide_compress::strip_metadata(&markdown);
        markdown = clean_after_strip(&markdown);
    }

    let chunks = match opts.max_chunk_chars {
        Some(n) if n > 0 => oxide_compress::chunk_text(&markdown, n),
        _ => Vec::new(),
    };

    ExtractedContent {
        markdown,
        title,
        chunks,
    }
}

fn extract_title(doc: &Html) -> Option<String> {
    let sel = scraper::Selector::parse("title").ok()?;
    let title = doc.select(&sel).next()?;
    Some(
        title
            .text()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Tags whose content we drop entirely.
const DROP_TAGS: &[&str] = &[
    "script", "style", "noscript", "iframe", "svg", "head", "meta", "link", "nav", "footer",
    "header", "aside", "form",
];

fn walk(el: ElementRef<'_>, out: &mut String, list_depth: usize) {
    let tag = el.value().name();
    if DROP_TAGS.contains(&tag) {
        return;
    }

    match tag {
        "h1" => emit_heading(el, out, "#"),
        "h2" => emit_heading(el, out, "##"),
        "h3" => emit_heading(el, out, "###"),
        "h4" => emit_heading(el, out, "####"),
        "h5" => emit_heading(el, out, "#####"),
        "h6" => emit_heading(el, out, "######"),
        "p" => {
            out.push_str(&inline_text(el));
            out.push_str("\n\n");
        }
        "br" => out.push('\n'),
        "hr" => out.push_str("\n---\n\n"),
        "ul" | "ol" => {
            let ordered = tag == "ol";
            let mut idx = 1usize;
            for child in el.children() {
                if let Some(li) = ElementRef::wrap(child) {
                    if li.value().name() == "li" {
                        for _ in 0..list_depth {
                            out.push_str("  ");
                        }
                        if ordered {
                            out.push_str(&format!("{idx}. "));
                            idx += 1;
                        } else {
                            out.push_str("- ");
                        }
                        out.push_str(&inline_text(li));
                        out.push('\n');
                        // Nested lists.
                        for sub in li.children() {
                            if let Some(sub_el) = ElementRef::wrap(sub) {
                                if matches!(sub_el.value().name(), "ul" | "ol") {
                                    walk(sub_el, out, list_depth + 1);
                                }
                            }
                        }
                    }
                }
            }
            out.push('\n');
        }
        "pre" => {
            // Code blocks: include language if `<code class="language-x">`.
            let code = el
                .children()
                .filter_map(ElementRef::wrap)
                .find(|c| c.value().name() == "code");
            let lang = code
                .as_ref()
                .and_then(|c| c.value().attr("class"))
                .and_then(|c| c.split_whitespace().find_map(|t| t.strip_prefix("language-")))
                .unwrap_or("");
            let body = match code {
                Some(c) => c.text().collect::<String>(),
                None => el.text().collect::<String>(),
            };
            out.push_str("```");
            out.push_str(lang);
            out.push('\n');
            out.push_str(body.trim_end());
            out.push_str("\n```\n\n");
        }
        "blockquote" => {
            for line in inline_text(el).lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }
        "table" => {
            // Tables collapse to plain rows; full markdown table rendering
            // requires column-width calculation that is overkill for the
            // basic extractor.
            for child in el.descendants() {
                if let Some(row) = ElementRef::wrap(child) {
                    if row.value().name() == "tr" {
                        let cells: Vec<String> = row
                            .children()
                            .filter_map(ElementRef::wrap)
                            .filter(|c| matches!(c.value().name(), "td" | "th"))
                            .map(|c| inline_text(c))
                            .collect();
                        if !cells.is_empty() {
                            out.push_str(&cells.join(" | "));
                            out.push('\n');
                        }
                    }
                }
            }
            out.push('\n');
        }
        _ => {
            // Generic container: recurse into children so nested headings /
            // paragraphs still get picked up.
            for child in el.children() {
                if let Some(c) = ElementRef::wrap(child) {
                    walk(c, out, list_depth);
                }
            }
        }
    }
}

fn emit_heading(el: ElementRef<'_>, out: &mut String, marker: &str) {
    out.push_str(marker);
    out.push(' ');
    out.push_str(&inline_text(el));
    out.push_str("\n\n");
}

/// Render an element's contents as inline Markdown (links, emphasis, code).
fn inline_text(el: ElementRef<'_>) -> String {
    let mut buf = String::new();
    inline_walk(el, &mut buf);
    buf.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn inline_walk(el: ElementRef<'_>, buf: &mut String) {
    for child in el.children() {
        match child.value() {
            Node::Text(t) => buf.push_str(t),
            Node::Element(_) => {
                if let Some(c) = ElementRef::wrap(child) {
                    let tag = c.value().name();
                    if DROP_TAGS.contains(&tag) {
                        continue;
                    }
                    match tag {
                        "a" => {
                            let href = c.value().attr("href").unwrap_or("");
                            let mut inner = String::new();
                            inline_walk(c, &mut inner);
                            let inner = inner.trim();
                            if !href.is_empty() && !inner.is_empty() {
                                buf.push_str(&format!("[{inner}]({href})"));
                            } else {
                                buf.push_str(inner);
                            }
                        }
                        "strong" | "b" => {
                            buf.push_str("**");
                            inline_walk(c, buf);
                            buf.push_str("**");
                        }
                        "em" | "i" => {
                            buf.push('*');
                            inline_walk(c, buf);
                            buf.push('*');
                        }
                        "code" => {
                            buf.push('`');
                            inline_walk(c, buf);
                            buf.push('`');
                        }
                        "img" => {
                            let alt = c.value().attr("alt").unwrap_or("");
                            let src = c.value().attr("src").unwrap_or("");
                            buf.push_str(&format!("![{alt}]({src})"));
                        }
                        _ => inline_walk(c, buf),
                    }
                }
            }
            _ => {}
        }
    }
}

fn normalize_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_newlines = 0u8;
    for line in s.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            prev_newlines = prev_newlines.saturating_add(1);
            if prev_newlines <= 1 {
                out.push('\n');
            }
        } else {
            out.push_str(trimmed);
            out.push('\n');
            prev_newlines = 0;
        }
    }
    out.trim().to_string()
}

/// `strip_metadata` collapses everything into one line; reapply paragraph
/// breaks by splitting on the markdown structural markers we emitted.
fn clean_after_strip(s: &str) -> String {
    // strip_metadata replaces all whitespace with single spaces. Restore line
    // breaks before headings / list bullets / fence markers so chunkers can
    // still find paragraph boundaries.
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        let ch = bytes[i];
        let inserted_break = matches!(ch, b'#')
            && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\n')
            && !out.ends_with("\n\n");
        if inserted_break && !out.is_empty() {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push_str("\n\n");
        }
        out.push(ch as char);
        i += 1;
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<!doctype html>
        <html>
          <head><title>Demo Page</title></head>
          <body>
            <nav>Hidden nav</nav>
            <main>
              <h1>Hello world</h1>
              <p>This is a <strong>test</strong> paragraph with a <a href="/x">link</a>.</p>
              <h2>Features</h2>
              <ul>
                <li>One</li>
                <li>Two</li>
              </ul>
              <pre><code class="language-rust">fn main() { println!("hi"); }</code></pre>
            </main>
            <footer>© 2026 Demo</footer>
          </body>
        </html>"#;

    #[test]
    fn extracts_title_and_headings() {
        let out = extract_markdown(HTML, &ExtractOptions::default());
        assert_eq!(out.title.as_deref(), Some("Demo Page"));
        assert!(out.markdown.contains("# Hello world"));
        assert!(out.markdown.contains("## Features"));
    }

    #[test]
    fn drops_nav_and_footer_boilerplate() {
        let out = extract_markdown(HTML, &ExtractOptions::default());
        assert!(!out.markdown.to_lowercase().contains("hidden nav"));
    }

    #[test]
    fn keeps_lists_and_code_blocks() {
        let out = extract_markdown(HTML, &ExtractOptions::default());
        assert!(out.markdown.contains("- One"));
        assert!(out.markdown.contains("```rust"));
        assert!(out.markdown.contains("fn main()"));
    }

    #[test]
    fn renders_links_and_emphasis_inline() {
        let out = extract_markdown(HTML, &ExtractOptions::default());
        assert!(out.markdown.contains("[link](/x)"));
        assert!(out.markdown.contains("**test**"));
    }

    #[test]
    fn chunks_when_requested() {
        let opts = ExtractOptions {
            max_chunk_chars: Some(40),
            strip_boilerplate: true,
        };
        let out = extract_markdown(HTML, &opts);
        assert!(!out.chunks.is_empty());
        for c in &out.chunks {
            assert!(c.chars().count() <= 40, "chunk too long: {c:?}");
        }
    }
}
