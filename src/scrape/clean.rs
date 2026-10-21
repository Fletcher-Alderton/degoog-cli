//! Line-level cleanup of extracted text: drop UI boilerplate and repeats.

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

static NOISE_LINES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)^(accept|manage|allow) (all )?cookies?$",
        r"(?i)^(subscribe|sign up|log ?in|sign in|register)\b",
        r"(?i)^share (this|on)\b",
        r"(?i)^advertisement$",
        r"(?i)^skip to (main )?content$",
        r"(?i)^cookie (policy|settings|preferences)$",
        r"(?i)^(follow us|newsletter)\b",
        r"(?i)^\[\s*edit\s*\]$",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

static INLINE_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+").unwrap());
static MANY_BREAKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());

fn is_noise(line: &str) -> bool {
    let trimmed = line.trim();
    NOISE_LINES.iter().any(|p| p.is_match(trimmed))
}

pub fn clean_text(text: &str) -> String {
    let mut seen = HashSet::new();
    let mut kept: Vec<String> = Vec::new();

    for raw in text.split('\n') {
        let line = INLINE_WS.replace_all(raw, " ").trim_end().to_string();
        if line.trim().is_empty() {
            if kept.last().is_some_and(|l| !l.is_empty()) {
                kept.push(String::new());
            }
            continue;
        }
        if is_noise(&line) {
            continue;
        }
        let key = line.trim().to_lowercase();
        if key.chars().count() > 12 && !seen.insert(key) {
            continue;
        }
        kept.push(line);
    }

    MANY_BREAKS.replace_all(&kept.join("\n"), "\n\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_noise_and_repeats() {
        let text = "Accept all cookies\nA real paragraph of text\n\n\n\nA real paragraph of text\nshort\nshort\n[edit]";
        assert_eq!(clean_text(text), "A real paragraph of text\n\nshort\nshort");
    }
}
