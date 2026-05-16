//! # Oxide Compress
//!
//! Token-compression primitives for the Rust Oxide Agent-Native OS. The crate
//! is designed to run as a WebAssembly (WASM) plugin inside the kernel's
//! Layer 2 sandbox, but it also compiles natively so that it can be unit-tested
//! and embedded directly inside other Rust crates.
//!
//! The crate exposes three primary functions:
//!
//! * [`select_fields`] — prune a JSON document down to a chosen set of fields.
//! * [`strip_metadata`] — remove HTML tags, common boilerplate, and excessive
//!   whitespace from arbitrary text.
//! * [`chunk_text`] — split text into bounded-length chunks while preserving
//!   semantic boundaries (paragraphs → sentences → words).
//!
//! ## WASM bindings
//!
//! When the crate is compiled for the `wasm32-unknown-unknown` target the same
//! three functions are also exposed through `wasm-bindgen` under JavaScript-
//! friendly names (`selectFields`, `stripMetadata`, `chunkText`). The native
//! Rust signatures match the technical specification verbatim; the wasm
//! wrappers adapt the inputs and outputs to types that cross the
//! JavaScript ↔ wasm ABI cleanly.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

use serde_json::Value;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// 1. Field selection
// ---------------------------------------------------------------------------

/// Return a new JSON string containing only the listed top-level fields.
///
/// * If the input is a JSON **object**, the returned object preserves only the
///   keys in `fields`. Missing keys are silently skipped.
/// * If the input is a JSON **array**, the function is applied element-wise to
///   every object in the array. Non-object elements are dropped.
/// * If the input is anything else (string/number/bool/null), or is not valid
///   JSON, the input is returned verbatim — there is nothing to prune.
///
/// The function never panics; malformed input simply passes through.
///
/// # Example
/// ```
/// use oxide_compress::select_fields;
/// let json = r#"{"id":1,"name":"x","secret":"shh"}"#;
/// let out = select_fields(json, &["id", "name"]);
/// assert!(out.contains("\"id\""));
/// assert!(out.contains("\"name\""));
/// assert!(!out.contains("secret"));
/// ```
pub fn select_fields(json_data: &str, fields: &[&str]) -> String {
    let Ok(value) = serde_json::from_str::<Value>(json_data) else {
        return json_data.to_string();
    };
    let pruned = prune_value(&value, fields);
    serde_json::to_string(&pruned).unwrap_or_else(|_| json_data.to_string())
}

fn prune_value(value: &Value, fields: &[&str]) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(fields.len().min(map.len()));
            for &key in fields {
                if let Some(v) = map.get(key) {
                    out.insert(key.to_string(), v.clone());
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            let pruned: Vec<Value> = items
                .iter()
                .filter(|v| v.is_object())
                .map(|v| prune_value(v, fields))
                .collect();
            Value::Array(pruned)
        }
        // Scalars pass through unchanged.
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// 2. Metadata stripping
// ---------------------------------------------------------------------------

/// Boilerplate phrases removed by [`strip_metadata`].
///
/// Comparisons are case-insensitive. The list is intentionally short and
/// conservative; agents that need richer rules should provide their own
/// post-processing.
const BOILERPLATE_PHRASES: &[&str] = &[
    "cookie policy",
    "privacy policy",
    "terms of service",
    "terms and conditions",
    "all rights reserved",
    "subscribe to our newsletter",
    "sign up for our newsletter",
    "click here to subscribe",
    "accept all cookies",
    "manage cookie preferences",
    "©",
    "(c)",
];

/// Strip HTML tags, decode common entities, remove boilerplate, and collapse
/// whitespace.
///
/// The algorithm is intentionally simple and dependency-free so that the
/// resulting WASM artifact stays small:
///
/// 1. Walk the input once and copy non-tag characters into the output, where a
///    tag is anything between `<` and `>` (incl. comments).
/// 2. Decode the most common HTML entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`,
///    `&apos;`, `&nbsp;`).
/// 3. Remove boilerplate phrases listed in [`BOILERPLATE_PHRASES`].
/// 4. Collapse any run of whitespace (spaces, tabs, newlines) into a single
///    space and trim leading / trailing whitespace.
///
/// # Example
/// ```
/// use oxide_compress::strip_metadata;
/// let html = "<p>Hello&nbsp;<b>World</b></p>   <!-- ad --> All Rights Reserved.";
/// assert_eq!(strip_metadata(html), "Hello World .");
/// ```
pub fn strip_metadata(text: &str) -> String {
    let de_tagged = strip_tags(text);
    let decoded = decode_entities(&de_tagged);
    let de_boiler = strip_boilerplate(&decoded);
    collapse_whitespace(&de_boiler)
}

fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                // Treat the closing `>` as a soft boundary so adjacent words
                // don't get glued together (e.g. `<b>foo</b>bar` → `foo bar`).
                out.push(' ');
            }
            _ if in_tag => {}
            _ => out.push(ch),
        }
    }
    out
}

fn decode_entities(input: &str) -> String {
    // A tiny table of the entities you actually hit in scraped content.
    // Anything else is left intact — better than a wrong substitution.
    const ENTITIES: &[(&str, &str)] = &[
        ("&nbsp;", " "),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&#39;", "'"),
        ("&#34;", "\""),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
    ];
    let mut out = input.to_string();
    for (from, to) in ENTITIES {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }
    out
}

fn strip_boilerplate(input: &str) -> String {
    let lower = input.to_lowercase();
    let mut keep = vec![true; input.len()];

    for phrase in BOILERPLATE_PHRASES {
        let phrase_lc = phrase.to_lowercase();
        let mut start = 0;
        while let Some(idx) = lower[start..].find(&phrase_lc) {
            let abs = start + idx;
            // Mark the byte range covered by this phrase for removal. Because
            // `input.to_lowercase()` produces a string of the same byte length
            // for the ASCII boilerplate we keep, the indices line up.
            for byte_idx in abs..abs + phrase_lc.len() {
                if byte_idx < keep.len() {
                    keep[byte_idx] = false;
                }
            }
            start = abs + phrase_lc.len();
        }
    }

    // Rebuild the string by filtering bytes. The phrases we remove are
    // ASCII or whole multi-byte UTF-8 sequences (e.g. `©`), so the byte mask
    // is always aligned to UTF-8 char boundaries. We still validate with
    // `from_utf8` and fall back to the input if anything goes wrong.
    let filtered: Vec<u8> = input
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| if keep[i] { Some(b) } else { None })
        .collect();
    String::from_utf8(filtered).unwrap_or_else(|_| input.to_string())
}

fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_was_space = true; // start `true` so leading whitespace is dropped
    for ch in input.chars() {
        if ch.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(ch);
            prev_was_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// 3. Semantic chunking
// ---------------------------------------------------------------------------

/// Split `text` into chunks no longer than `max_len` *characters* (Unicode
/// scalar values, not bytes), preserving semantic boundaries.
///
/// The algorithm walks three levels of granularity, falling through only when
/// a unit overflows `max_len`:
///
/// 1. **Paragraphs** — split on one or more blank lines (`\n\n`).
/// 2. **Sentences** — split on `.`, `!`, `?` followed by whitespace.
/// 3. **Words** — split on whitespace, packing as many as fit per chunk.
/// 4. **Hard fallback** — if a single word is longer than `max_len`, slice it
///    on character boundaries.
///
/// Each returned chunk is trimmed and non-empty. Whitespace-only inputs and
/// `max_len == 0` both return an empty vector.
///
/// # Example
/// ```
/// use oxide_compress::chunk_text;
/// let chunks = chunk_text("Hello world. Goodbye world.", 15);
/// assert!(chunks.iter().all(|c| c.chars().count() <= 15));
/// ```
pub fn chunk_text(text: &str, max_len: usize) -> Vec<String> {
    if max_len == 0 || text.trim().is_empty() {
        return Vec::new();
    }

    let mut chunks: Vec<String> = Vec::new();

    for paragraph in split_paragraphs(text) {
        let para = paragraph.trim();
        if para.is_empty() {
            continue;
        }
        if para.chars().count() <= max_len {
            chunks.push(para.to_string());
            continue;
        }
        // Paragraph too large — drop down to sentences.
        for sentence in split_sentences(para) {
            let sentence = sentence.trim();
            if sentence.is_empty() {
                continue;
            }
            if sentence.chars().count() <= max_len {
                push_or_merge(&mut chunks, sentence, max_len);
                continue;
            }
            // Sentence still too large — pack words.
            for piece in pack_words(sentence, max_len) {
                push_or_merge(&mut chunks, &piece, max_len);
            }
        }
    }

    chunks
}

/// Append `piece` as a new chunk, or merge with the previous chunk if both fit
/// within `max_len`. Keeps chunk counts low on inputs that have many tiny
/// sentences.
fn push_or_merge(chunks: &mut Vec<String>, piece: &str, max_len: usize) {
    if let Some(last) = chunks.last_mut() {
        // `+ 1` accounts for the joining space.
        if last.chars().count() + 1 + piece.chars().count() <= max_len {
            last.push(' ');
            last.push_str(piece);
            return;
        }
    }
    chunks.push(piece.to_string());
}

fn split_paragraphs(text: &str) -> Vec<&str> {
    // A paragraph is anything separated by a blank line. We split on `\n\n`
    // which is sufficient for already-normalized input (scraped content fed
    // through `strip_metadata` is normalized to single spaces, so paragraph
    // breaks survive only when the caller preserved `\n\n`).
    text.split("\n\n").collect()
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            if let Some(&next) = chars.peek() {
                if next.is_whitespace() {
                    out.push(std::mem::take(&mut current));
                }
            } else {
                out.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn pack_words(text: &str, max_len: usize) -> Vec<String> {
    let mut packed: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > max_len {
            // Flush whatever is accumulated, then hard-slice the giant word.
            if !current.is_empty() {
                packed.push(std::mem::take(&mut current));
            }
            packed.extend(hard_slice(word, max_len));
            continue;
        }
        let needed = if current.is_empty() { word_len } else { current.chars().count() + 1 + word_len };
        if needed > max_len {
            packed.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        packed.push(current);
    }
    packed
}

fn hard_slice(text: &str, max_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut count = 0;
    for ch in text.chars() {
        buf.push(ch);
        count += 1;
        if count == max_len {
            out.push(std::mem::take(&mut buf));
            count = 0;
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

// ---------------------------------------------------------------------------
// 4. WASM bindings
// ---------------------------------------------------------------------------
//
// These wrappers exist only when compiling to a wasm target. They adapt the
// idiomatic-Rust signatures from above to types that cross the JavaScript ↔
// wasm ABI cleanly. The native unit tests target the Rust functions directly.

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = selectFields)]
/// JavaScript-facing `selectFields(jsonData, fields)`.
///
/// `fields` is a `Vec<String>` because `wasm-bindgen` cannot marshal `&[&str]`.
pub fn select_fields_wasm(json_data: &str, fields: Vec<String>) -> String {
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    select_fields(json_data, &refs)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = stripMetadata)]
/// JavaScript-facing `stripMetadata(text)`.
pub fn strip_metadata_wasm(text: &str) -> String {
    strip_metadata(text)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = chunkText)]
/// JavaScript-facing `chunkText(text, maxLen)`.
pub fn chunk_text_wasm(text: &str, max_len: usize) -> Vec<String> {
    chunk_text(text, max_len)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- select_fields ----

    #[test]
    fn select_fields_keeps_only_listed_keys() {
        let input = r#"{"id":1,"name":"alice","secret":"hush","age":30}"#;
        let out = select_fields(input, &["id", "name"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], json!(1));
        assert_eq!(v["name"], json!("alice"));
        assert!(v.get("secret").is_none());
        assert!(v.get("age").is_none());
    }

    #[test]
    fn select_fields_missing_keys_are_silently_skipped() {
        let input = r#"{"id":1}"#;
        let out = select_fields(input, &["id", "ghost"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], json!(1));
        assert!(v.get("ghost").is_none());
    }

    #[test]
    fn select_fields_handles_array_of_objects() {
        let input = r#"[{"id":1,"x":"a"},{"id":2,"x":"b"}]"#;
        let out = select_fields(input, &["id"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v[0], json!({"id":1}));
        assert_eq!(v[1], json!({"id":2}));
    }

    #[test]
    fn select_fields_passes_through_invalid_json() {
        let input = "not json at all";
        assert_eq!(select_fields(input, &["id"]), "not json at all");
    }

    #[test]
    fn select_fields_passes_through_scalar_json() {
        assert_eq!(select_fields("42", &["id"]), "42");
        assert_eq!(select_fields("true", &["id"]), "true");
        assert_eq!(select_fields("\"hi\"", &["id"]), "\"hi\"");
    }

    // ---- strip_metadata ----

    #[test]
    fn strip_metadata_removes_html_tags() {
        let html = "<p>Hello <b>World</b></p>";
        assert_eq!(strip_metadata(html), "Hello World");
    }

    #[test]
    fn strip_metadata_decodes_common_entities() {
        let s = "Tom &amp; Jerry &nbsp; said &quot;hi&quot;";
        assert_eq!(strip_metadata(s), "Tom & Jerry said \"hi\"");
    }

    #[test]
    fn strip_metadata_removes_boilerplate_phrases() {
        let s = "Read more. Privacy Policy. All Rights Reserved.";
        let out = strip_metadata(s);
        let lc = out.to_lowercase();
        assert!(!lc.contains("privacy policy"));
        assert!(!lc.contains("all rights reserved"));
        assert!(out.contains("Read more"));
    }

    #[test]
    fn strip_metadata_collapses_whitespace() {
        let s = "  too\t\tmuch\n\n\nspace   here  ";
        assert_eq!(strip_metadata(s), "too much space here");
    }

    #[test]
    fn strip_metadata_keeps_unicode_intact() {
        let s = "<p>café — résumé</p>";
        assert_eq!(strip_metadata(s), "café — résumé");
    }

    // ---- chunk_text ----

    #[test]
    fn chunk_text_returns_empty_on_empty_input() {
        assert!(chunk_text("", 10).is_empty());
        assert!(chunk_text("   \n  ", 10).is_empty());
    }

    #[test]
    fn chunk_text_returns_empty_when_max_len_is_zero() {
        assert!(chunk_text("hello world", 0).is_empty());
    }

    #[test]
    fn chunk_text_respects_max_len() {
        let text = "First sentence. Second sentence. Third sentence. Fourth one.";
        let chunks = chunk_text(text, 20);
        for c in &chunks {
            assert!(
                c.chars().count() <= 20,
                "chunk too long ({}): {c:?}",
                c.chars().count()
            );
        }
        // Reassembled chunks contain everything (modulo whitespace
        // normalization at sentence boundaries).
        let joined: String = chunks.join(" ");
        assert!(joined.contains("First"));
        assert!(joined.contains("Fourth"));
    }

    #[test]
    fn chunk_text_prefers_paragraph_boundaries() {
        let text = "Para one.\n\nPara two.";
        let chunks = chunk_text(text, 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "Para one.");
        assert_eq!(chunks[1], "Para two.");
    }

    #[test]
    fn chunk_text_handles_oversized_word() {
        let word = "x".repeat(50);
        let text = format!("ok {word} done");
        let chunks = chunk_text(&text, 10);
        for c in &chunks {
            assert!(c.chars().count() <= 10, "chunk too long: {c:?}");
        }
        // All chars are preserved when reassembled.
        let total_x: usize = chunks.iter().map(|c| c.matches('x').count()).sum();
        assert_eq!(total_x, 50);
    }

    #[test]
    fn chunk_text_unicode_is_counted_by_chars_not_bytes() {
        // Each "é" is 2 bytes but 1 char. With max_len=4 we should get one
        // chunk of "café" not panic on a byte boundary.
        let chunks = chunk_text("café café", 4);
        for c in &chunks {
            assert!(c.chars().count() <= 4);
        }
    }
}
