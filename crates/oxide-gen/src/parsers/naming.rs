//! Shared naming helpers used by every parser.

use heck::{ToPascalCase, ToSnakeCase};

/// Reserved Rust keywords that need an `r#` prefix when used as identifiers.
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "Self", "self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "union",
];

/// Convert a free-form name into a snake_case Rust identifier, escaping
/// keywords with `r#`.
pub fn snake_ident(input: &str) -> String {
    let snake = input.to_snake_case();
    let snake = if snake.is_empty() {
        "field".to_string()
    } else if snake.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("_{snake}")
    } else {
        snake
    };
    if RUST_KEYWORDS.contains(&snake.as_str()) {
        format!("r#{snake}")
    } else {
        snake
    }
}

/// Convert a free-form name into a PascalCase Rust type identifier.
pub fn pascal_ident(input: &str) -> String {
    let pascal = input.to_pascal_case();
    if pascal.is_empty() {
        return "Type".to_string();
    }
    if pascal.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("T{pascal}")
    } else {
        pascal
    }
}

/// Sanitize a name for use as a crate identifier (snake_case, no leading
/// digits, dashes → underscores).
pub fn crate_name(input: &str) -> String {
    let mut out: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out = out.trim_matches('_').to_string();
    if out.is_empty() {
        out = "generated".to_string();
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_ident_handles_keywords() {
        assert_eq!(snake_ident("type"), "r#type");
        assert_eq!(snake_ident("Self"), "r#self");
        assert_eq!(snake_ident("petId"), "pet_id");
    }

    #[test]
    fn snake_ident_handles_leading_digits() {
        assert_eq!(snake_ident("1stPlace"), "_1st_place");
    }

    #[test]
    fn pascal_ident_basics() {
        assert_eq!(pascal_ident("pet"), "Pet");
        assert_eq!(pascal_ident("pet_store"), "PetStore");
        // Digit-prefixed names get a `T` prefix so the result is a valid ident;
        // the exact casing of trailing chars is delegated to `heck`.
        let starts_with_t = pascal_ident("123foo");
        assert!(starts_with_t.starts_with("T123"));
    }

    #[test]
    fn crate_name_sanitizes() {
        assert_eq!(crate_name("Pet Store"), "pet_store");
        assert_eq!(crate_name("foo-bar.baz"), "foo_bar_baz");
        assert_eq!(crate_name(""), "generated");
        assert_eq!(crate_name("1service"), "_1service");
    }
}
