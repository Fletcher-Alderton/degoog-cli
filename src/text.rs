//! Small text helpers. Lengths are counted in chars, not bytes, so slicing
//! never lands inside a multi-byte character.

use regex::Regex;
use std::sync::LazyLock;

static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// First `n` chars of `s`.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Everything after the first `n` chars of `s`.
pub fn skip_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((idx, _)) => &s[idx..],
        None => "",
    }
}

/// Char index of the last occurrence of `needle`, if any.
pub fn last_index_chars(s: &str, needle: &str) -> Option<usize> {
    s.rfind(needle).map(|byte| s[..byte].chars().count())
}

/// Collapse every whitespace run to one space and trim.
pub fn squash_space(value: &str) -> String {
    WS.replace_all(value, " ").trim().to_string()
}

/// Strip HTML tags and decode entities, when the value looks like markup.
pub fn strip_markup(value: &str) -> String {
    if !value.contains('<') && !value.contains('&') {
        return value.to_string();
    }
    let fragment = scraper::Html::parse_fragment(value);
    fragment.root_element().text().collect()
}

pub fn clean_title(raw: Option<&str>, fallback: &str) -> String {
    let value = raw.map(str::trim).unwrap_or("");
    if value.is_empty() {
        return fallback.to_string();
    }
    let cleaned = squash_space(&strip_markup(value));
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// Cap `text` at `max` chars, ending with an ellipsis when cut.
pub fn cap_chars(text: &str, max: usize) -> String {
    if char_len(text) <= max {
        return text.to_string();
    }
    let head = take_chars(text, max.saturating_sub(1)).trim_end();
    format!("{head}…")
}

/// Short version of a query for log lines.
pub fn short_query(query: &str) -> String {
    if char_len(query) > 48 {
        format!("{}...", take_chars(query, 48))
    } else {
        query.to_string()
    }
}

pub fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let n = char_len(value);
    if n <= 8 {
        return "***".into();
    }
    format!("{}***{}", take_chars(value, 3), skip_chars(value, n - 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_and_slices_by_char() {
        assert_eq!(cap_chars("héllo world", 5), "héll…");
        assert_eq!(cap_chars("short", 10), "short");
        assert_eq!(take_chars("añb", 2), "añ");
        assert_eq!(skip_chars("añb", 2), "b");
    }

    #[test]
    fn strips_markup_and_entities() {
        assert_eq!(strip_markup("<b>Rust</b> &amp; Go"), "Rust & Go");
        assert_eq!(clean_title(Some("  <i>A</i>   title "), "x"), "A title");
        assert_eq!(clean_title(Some("   "), "fallback"), "fallback");
    }

    #[test]
    fn masks_secrets() {
        assert_eq!(mask_secret("short"), "***");
        assert_eq!(mask_secret("abcdefghijkl"), "abc***kl");
    }
}
