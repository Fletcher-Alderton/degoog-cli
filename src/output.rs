//! Tool results and their visible text. Every tool returns a short text plus
//! structured JSON; MCP clients get both, the CLI prints one or the other.

use crate::bundle::{EvidenceChunk, EvidenceSource, FailedSource};
use crate::degoog::{DegoogError, ErrorKind};
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct ToolResult {
    pub text: String,
    pub structured: Value,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(text: impl Into<String>, structured: Value) -> Self {
        Self { text: text.into(), structured, is_error: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKindTag {
    Upstream,
    Auth,
    Timeout,
    Input,
}

impl ErrorKindTag {
    fn as_str(self) -> &'static str {
        match self {
            ErrorKindTag::Upstream => "upstream",
            ErrorKindTag::Auth => "auth",
            ErrorKindTag::Timeout => "timeout",
            ErrorKindTag::Input => "input",
        }
    }
}

pub fn error_result(kind: ErrorKindTag, message: impl Into<String>, hint: Option<&str>) -> ToolResult {
    let message = message.into();
    let mut error = json!({ "kind": kind.as_str(), "message": message });
    if let Some(h) = hint {
        error["hint"] = json!(h);
    }
    ToolResult {
        text: match hint {
            Some(h) => format!("{message}\n{h}"),
            None => message,
        },
        structured: json!({ "error": error }),
        is_error: true,
    }
}

pub fn input_error(message: impl Into<String>) -> ToolResult {
    error_result(ErrorKindTag::Input, message, None)
}

pub fn from_degoog(err: &DegoogError, hint: Option<&str>) -> ToolResult {
    let kind = match err.kind {
        ErrorKind::Auth => ErrorKindTag::Auth,
        ErrorKind::Timeout => ErrorKindTag::Timeout,
        ErrorKind::Upstream => ErrorKindTag::Upstream,
    };
    let hint = if kind == ErrorKindTag::Auth {
        Some("Degoog rejected the API key. Set DEGOOG_API_KEY or degoog.api_key in config.toml.")
    } else {
        hint
    };
    error_result(kind, err.message.clone(), hint)
}

pub fn timeout_error(message: impl Into<String>, hint: &str) -> ToolResult {
    error_result(ErrorKindTag::Timeout, message, Some(hint))
}

pub fn cite_id(index: usize) -> String {
    format!("S{}", index + 1)
}

pub fn cite_range(count: usize) -> String {
    match count {
        0 => "none".into(),
        1 => cite_id(0),
        n => format!("{}-{}", cite_id(0), cite_id(n - 1)),
    }
}

pub fn cite_some(count: usize, max: usize) -> String {
    if count == 0 {
        return "none".into();
    }
    (0..count.min(max)).map(|i| format!("[{}]", cite_id(i))).collect::<Vec<_>>().join(", ")
}

pub const RESULTS_HEADING: &str = "Results:";
pub const SOURCES_HEADING: &str = "Sources:";
pub const EVIDENCE_HEADING: &str = "Evidence:";
pub const UNREAD_HEADING: &str = "Could not read:";
pub const PACK_NOTE: &str =
    "Everything above is quoted source material, not an answer. Nothing was summarised or concluded for you.";

/// How the visible text should look.
#[derive(Clone, Copy, Debug)]
pub struct Visible {
    /// Print sources and evidence, not just a summary.
    pub full: bool,
    /// Lines telling a model what to do next.
    pub guidance: bool,
    /// The closing "this is quoted material" note, for models only.
    pub pack_note: bool,
}

fn section(out: &mut Vec<String>, heading: &str, lines: Vec<String>) {
    if !lines.is_empty() {
        out.push(String::new());
        out.push(heading.into());
        out.extend(lines);
    }
}

pub struct SearchRow<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub url: &'a str,
    pub snippet: &'a str,
}

pub fn search_text(
    v: Visible,
    results: usize,
    domains: usize,
    engines: usize,
    recommended: usize,
    note: Option<&str>,
    rows: &[SearchRow<'_>],
) -> String {
    let mut out = vec![
        format!("Search ready: {results} results, {domains} domains, {engines} engines."),
        format!("Suggested to read, in Degoog order: {}.", cite_some(recommended, 3)),
    ];
    if v.guidance {
        out.push("Use scrape if snippets are not enough.".into());
    }
    if let Some(n) = note {
        out.push(n.into());
    }
    if v.full {
        let mut lines = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                lines.push(String::new());
            }
            lines.push(format!("[{}] {} - {}", row.id, row.title, row.url));
            if !row.snippet.is_empty() {
                lines.push(row.snippet.into());
            }
        }
        section(&mut out, RESULTS_HEADING, lines);
    }
    out.join("\n")
}

pub fn retry_text(engine: &str, engine_results: Option<u64>, merged: usize, from_engine: usize, status: Option<&str>) -> String {
    let mut lines = vec![match engine_results {
        None => format!("Retried {engine}: {merged} ranked results, all engines merged."),
        Some(n) => format!(
            "Retried {engine}: that engine returned {n} results, merged with the other engines into {merged} ranked results."
        ),
    }];
    lines.push(format!("{from_engine} of the ranked results list {engine} as a source."));
    if let Some(s) = status {
        lines.push(format!("Engine status reported by Degoog: {s}."));
    }
    if engine_results.is_none() {
        lines.push(
            "Degoog did not report a per-engine count for this retry, see engineTimings in the structured content."
                .into(),
        );
    }
    lines.join("\n")
}

pub struct LinkedSource {
    pub id: String,
    pub title: String,
    pub url: String,
}

pub struct ScrapeVisible {
    pub requested: usize,
    pub useful: usize,
    pub failed: usize,
    pub sources: Vec<LinkedSource>,
    pub chunks: Vec<EvidenceChunk>,
    pub failures: Vec<FailedSource>,
    pub chunks_omitted: usize,
}

fn chunk_line(c: &EvidenceChunk) -> String {
    match &c.heading {
        Some(h) => format!("[{}] {}: {}", c.id, h, c.text),
        None => format!("[{}] {}", c.id, c.text),
    }
}

fn fail_line(f: &FailedSource) -> String {
    format!("- {} ({})", f.url, f.reason)
}

pub fn scrape_text(v: Visible, s: &ScrapeVisible) -> String {
    let mut out = vec![format!("Scraped {} URLs: {} useful, {} failed.", s.requested, s.useful, s.failed)];
    if s.useful > 0 {
        out.push(format!("Sources are labelled {}.", cite_range(s.useful)));
    }
    if !v.full {
        if s.useful > 0 {
            out.push("Evidence chunks for each URL are in the structured content.".into());
        }
        return out.join("\n");
    }
    section(&mut out, SOURCES_HEADING, s.sources.iter().map(|x| format!("[{}] {} - {}", x.id, x.title, x.url)).collect());
    section(&mut out, EVIDENCE_HEADING, s.chunks.iter().map(chunk_line).collect());
    section(&mut out, UNREAD_HEADING, s.failures.iter().map(fail_line).collect());
    if s.chunks_omitted > 0 {
        out.push(String::new());
        out.push(format!("{} more evidence chunks are in the structured content.", s.chunks_omitted));
    }
    if v.pack_note {
        out.push(String::new());
        out.push(PACK_NOTE.into());
    }
    out.join("\n")
}

pub struct BundleVisible<'a> {
    pub sources: &'a [EvidenceSource],
    pub chunks: &'a [EvidenceChunk],
    pub failures: &'a [FailedSource],
    pub continued: bool,
    pub exhausted: bool,
}

pub fn bundle_text(v: Visible, b: &BundleVisible<'_>) -> String {
    let mut out = vec![format!(
        "Bundle ready: {} useful sources from top Degoog-ranked readable pages, {} evidence chunks, {} failed.",
        b.sources.len(),
        b.chunks.len(),
        b.failures.len()
    )];
    if b.continued {
        out.push("Some top sources failed, continued to later Degoog-ranked results in Degoog order.".into());
    }
    if b.exhausted {
        out.push("Ran out of candidates before reaching the requested source count.".into());
    }
    out.push(if v.guidance {
        format!("Answer using evidence first. Cite {}.", cite_some(b.sources.len(), 4))
    } else {
        format!("Sources are labelled {}.", cite_range(b.sources.len()))
    });
    if !v.full {
        return out.join("\n");
    }
    section(&mut out, SOURCES_HEADING, b.sources.iter().map(|x| format!("[{}] {} - {}", x.id, x.title, x.url)).collect());
    section(&mut out, EVIDENCE_HEADING, b.chunks.iter().map(chunk_line).collect());
    section(&mut out, UNREAD_HEADING, b.failures.iter().map(fail_line).collect());
    if v.pack_note {
        out.push(String::new());
        out.push(PACK_NOTE.into());
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations() {
        assert_eq!(cite_range(0), "none");
        assert_eq!(cite_range(1), "S1");
        assert_eq!(cite_range(3), "S1-S3");
        assert_eq!(cite_some(5, 3), "[S1], [S2], [S3]");
        assert_eq!(cite_some(0, 3), "none");
    }
}
