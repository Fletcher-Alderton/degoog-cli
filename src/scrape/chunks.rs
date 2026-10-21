//! Split page text into chunks under headings and score them against the query.

use crate::text::{char_len, skip_chars, take_chars};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static TERM_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}+#.]+").unwrap());
static BLOCK_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{2,}").unwrap());
static HEADING_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#+\s*").unwrap());

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub text: String,
    pub heading: Option<String>,
    pub index: usize,
    pub score: i64,
    #[serde(rename = "match")]
    pub match_pct: i64,
}

pub fn query_terms(query: &str) -> Vec<String> {
    TERM_SPLIT.split(&query.to_lowercase()).filter(|t| char_len(t) > 1).map(String::from).collect()
}

/// Split after `.`, `!` or `?` followed by whitespace.
fn sentences(block: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = block.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let ws_start = i + c.len_utf8();
        let mut end = ws_start;
        while let Some(&(j, next)) = iter.peek() {
            if !next.is_whitespace() {
                break;
            }
            end = j + next.len_utf8();
            iter.next();
        }
        if end > ws_start {
            out.push(&block[start..ws_start]);
            start = end;
        }
    }
    out.push(&block[start..]);
    out
}

fn split_long(block: &str, chunk_chars: usize) -> Vec<String> {
    if char_len(block) <= chunk_chars {
        return vec![block.to_string()];
    }
    let mut pieces = Vec::new();
    let mut current = String::new();
    for sentence in sentences(block) {
        if !current.is_empty() && char_len(&current) + char_len(sentence) + 1 > chunk_chars {
            pieces.push(std::mem::take(&mut current));
        }
        if current.is_empty() {
            current = sentence.to_string();
        } else {
            current.push(' ');
            current.push_str(sentence);
        }
        while char_len(&current) > chunk_chars {
            pieces.push(take_chars(&current, chunk_chars).to_string());
            current = skip_chars(&current, chunk_chars).trim_start().to_string();
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

pub fn chunk_text(text: &str, chunk_chars: usize) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut heading: Option<String> = None;
    let mut buffer: Vec<String> = Vec::new();
    let mut size = 0usize;

    let flush = |chunks: &mut Vec<Chunk>, buffer: &mut Vec<String>, size: &mut usize, heading: &Option<String>| {
        let body = buffer.join("\n").trim().to_string();
        buffer.clear();
        *size = 0;
        if char_len(&body) < 40 {
            return;
        }
        let index = chunks.len();
        chunks.push(Chunk { text: body, heading: heading.clone(), index, score: 0, match_pct: 0 });
    };

    for block in BLOCK_SPLIT.split(text) {
        let line = block.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            flush(&mut chunks, &mut buffer, &mut size, &heading);
            heading = Some(HEADING_MARK.replace(line, "").into_owned());
            continue;
        }
        for piece in split_long(line, chunk_chars) {
            let len = char_len(&piece);
            if size + len > chunk_chars && size > 0 {
                flush(&mut chunks, &mut buffer, &mut size, &heading);
            }
            buffer.push(piece);
            size += len + 1;
        }
    }
    flush(&mut chunks, &mut buffer, &mut size, &heading);
    chunks
}

pub fn score_chunks(chunks: &mut [Chunk], query: &str) {
    let terms = query_terms(query);
    for chunk in chunks.iter_mut() {
        if terms.is_empty() {
            chunk.score = 10i64.saturating_sub(chunk.index as i64).max(0);
            chunk.match_pct = 0;
            continue;
        }
        let haystack = format!("{} {}", chunk.heading.as_deref().unwrap_or(""), chunk.text).to_lowercase();
        let hits = terms.iter().filter(|t| haystack.contains(t.as_str())).count();
        let density = hits as f64 / terms.len() as f64;
        let position = (4.0 - chunk.index as f64 * 0.5).max(0.0);
        chunk.score = (density * 100.0 + position + 0.5).floor() as i64;
        chunk.match_pct = (density * 100.0 + 0.5).floor() as i64;
    }
}

pub struct Picked {
    pub chunks: Vec<Chunk>,
    pub omitted: usize,
}

/// Best-scoring chunks within the count and char budget, back in page order.
pub fn pick_chunks(text: &str, query: &str, chunk_chars: usize, max_chunks: usize, max_chars: usize) -> Picked {
    let mut scored = chunk_text(text, chunk_chars);
    score_chunks(&mut scored, query);
    scored.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));

    let total = scored.len();
    let mut kept: Vec<Chunk> = Vec::new();
    let mut used = 0;
    for chunk in scored {
        if kept.len() >= max_chunks {
            break;
        }
        let len = char_len(&chunk.text);
        if used + len > max_chars && !kept.is_empty() {
            continue;
        }
        used += len;
        kept.push(chunk);
    }
    kept.sort_by_key(|c| c.index);
    Picked { omitted: total.saturating_sub(kept.len()), chunks: kept }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms() {
        assert_eq!(query_terms("C++ vs C# in node.js, a test"), vec!["c++", "vs", "c#", "in", "node.js", "test"]);
    }

    #[test]
    fn splits_sentences() {
        assert_eq!(sentences("One. Two!  Three? x.y end"), vec!["One.", "Two!", "Three?", "x.y end"]);
    }

    #[test]
    fn chunks_under_headings_and_scores() {
        let text = "# Intro\n\nThis paragraph talks about cats and nothing else at all really.\n\n## Rust\n\nThis paragraph is about the rust borrow checker and lifetimes in depth.\n\ntiny";
        let picked = pick_chunks(text, "rust borrow", 700, 1, 3000);
        assert_eq!(picked.chunks.len(), 1);
        assert_eq!(picked.omitted, 1);
        assert_eq!(picked.chunks[0].heading.as_deref(), Some("Rust"));
        assert_eq!(picked.chunks[0].match_pct, 100);
    }

    #[test]
    fn splits_long_blocks() {
        let block = format!("{} {}", "a".repeat(50) + ".", "b".repeat(120));
        let pieces = split_long(&block, 60);
        assert!(pieces.iter().all(|p| char_len(p) <= 60), "{pieces:?}");
        assert_eq!(pieces.concat().replace(' ', ""), block.replace(' ', ""));
    }
}
