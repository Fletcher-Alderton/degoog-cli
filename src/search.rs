//! Turn raw Degoog responses into a deduped, capped list in Degoog order.
//! Nothing here reorders by authority or freshness; Degoog's own score is
//! passed through as `degoog_score` for the client model to weigh.

use crate::degoog::types::{DegoogResult, SearchResponse};
use crate::text::{cap_chars, char_len, clean_title, last_index_chars, squash_space, strip_markup, take_chars};
use crate::urls::{canonical_url, domain_of, is_http_url, root_domain};
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ranked {
    pub title: String,
    pub url: String,
    pub original_url: String,
    pub domain: String,
    pub root: String,
    pub snippet: String,
    pub engines: Vec<String>,
    pub degoog_score: f64,
    pub queries: Vec<String>,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OverlapRow {
    pub url: String,
    pub engines: Vec<String>,
    pub count: usize,
}

pub struct PipelineInput<'a> {
    pub responses: &'a [(String, SearchResponse)],
    pub max_results: usize,
    pub snippet_chars: usize,
    pub per_domain: usize,
}

pub struct PipelineOutput {
    pub results: Vec<Ranked>,
    pub omitted: usize,
    pub merged: usize,
    pub domains: usize,
    pub engines: usize,
    pub overlap: Vec<OverlapRow>,
    pub related: Vec<String>,
}

fn unique(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|s| seen.insert(s.clone())).collect()
}

pub fn normalize(result: &DegoogResult, query: &str) -> Option<Ranked> {
    if !is_http_url(&result.url) {
        return None;
    }
    let url = canonical_url(&result.url);
    let engines = if !result.sources.is_empty() {
        unique(result.sources.iter().filter(|s| !s.is_empty()).cloned())
    } else {
        result.source.iter().filter(|s| !s.is_empty()).cloned().collect()
    };
    let domain = domain_of(&url);
    let raw_snippet = result
        .snippet
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(result.content.as_deref())
        .unwrap_or("");

    Some(Ranked {
        title: clean_title(result.title.as_deref(), &domain),
        root: root_domain(&url),
        domain,
        original_url: result.url.clone(),
        snippet: squash_space(&strip_markup(raw_snippet)),
        engines,
        degoog_score: result.score.unwrap_or(0.0),
        queries: if query.is_empty() { Vec::new() } else { vec![query.to_string()] },
        reasons: Vec::new(),
        url,
    })
}

fn merge_pair(base: Ranked, next: Ranked) -> Ranked {
    Ranked {
        title: if char_len(&base.title) >= char_len(&next.title) { base.title } else { next.title },
        snippet: if char_len(&base.snippet) >= char_len(&next.snippet) { base.snippet } else { next.snippet },
        engines: unique(base.engines.into_iter().chain(next.engines)),
        degoog_score: base.degoog_score.max(next.degoog_score),
        queries: unique(base.queries.into_iter().chain(next.queries)),
        ..base
    }
}

/// Merge results that canonicalise to the same URL, keeping first position.
pub fn dedupe(results: Vec<Ranked>) -> Vec<Ranked> {
    let mut by_url: IndexMap<String, Ranked> = IndexMap::new();
    for result in results {
        match by_url.shift_remove_full(&result.url) {
            Some((index, _, existing)) => {
                let merged = merge_pair(existing, result);
                by_url.shift_insert(index, merged.url.clone(), merged);
            }
            None => {
                by_url.insert(result.url.clone(), result);
            }
        }
    }
    by_url.into_values().collect()
}

fn with_reasons(mut result: Ranked) -> Ranked {
    result.reasons =
        if result.engines.len() > 1 { vec![format!("{} engines agree", result.engines.len())] } else { Vec::new() };
    result
}

fn site_key(result: &Ranked) -> &str {
    if result.root.is_empty() {
        &result.domain
    } else {
        &result.root
    }
}

/// Keep at most `per_root` results per site, in order.
pub fn spread_domains(results: Vec<Ranked>, per_root: usize) -> Vec<Ranked> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    results
        .into_iter()
        .filter(|r| {
            let used = seen.entry(site_key(r).to_string()).or_insert(0);
            if *used >= per_root {
                return false;
            }
            *used += 1;
            true
        })
        .collect()
}

static BOILERPLATE: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"(?i)^\s*\d+\s+(hours?|days?|weeks?|months?|years?)\s+ago\s*[-–—]\s*").unwrap(),
        Regex::new(r"(?i)^\s*(read more|continue reading)\s*[-–—:]\s*").unwrap(),
    ]
});

/// Shorten a snippet, preferring to cut at a sentence end, then a word.
pub fn trim_snippet(snippet: &str, max: usize) -> String {
    let mut text = squash_space(snippet);
    for pattern in BOILERPLATE.iter() {
        text = pattern.replace(&text, "").into_owned();
    }
    if char_len(&text) <= max {
        return text;
    }

    let cut = take_chars(&text, max);
    let last_stop = [last_index_chars(cut, ". "), last_index_chars(cut, "? ")].into_iter().flatten().max();
    if let Some(stop) = last_stop {
        if stop as f64 > max as f64 * 0.6 {
            return take_chars(cut, stop + 1).to_string();
        }
    }
    match last_index_chars(cut, " ") {
        Some(space) if space as f64 > max as f64 * 0.5 => cap_chars(take_chars(cut, space), max),
        _ => cap_chars(cut, max),
    }
}

pub fn run_pipeline(input: PipelineInput<'_>) -> PipelineOutput {
    let flat: Vec<Ranked> = input
        .responses
        .iter()
        .flat_map(|(query, response)| response.results.iter().filter_map(|r| normalize(r, query)))
        .collect();
    let flat_len = flat.len();

    let shaped: Vec<Ranked> = dedupe(flat).into_iter().map(with_reasons).collect();
    let merged = flat_len.saturating_sub(shaped.len());
    let shaped_len = shaped.len();
    let spread = spread_domains(shaped, input.per_domain);
    let spread_len = spread.len();

    let omitted_by_cap = spread_len.saturating_sub(input.max_results);
    let results: Vec<Ranked> = spread
        .into_iter()
        .take(input.max_results)
        .map(|mut r| {
            r.snippet = trim_snippet(&r.snippet, input.snippet_chars);
            r
        })
        .collect();

    let related = unique(input.responses.iter().flat_map(|(_, r)| r.related_searches.iter().cloned()));
    let domains = results.iter().map(|r| r.domain.as_str()).collect::<HashSet<_>>().len();
    let engines = results.iter().flat_map(|r| r.engines.iter()).collect::<HashSet<_>>().len();

    let mut overlap: Vec<OverlapRow> = results
        .iter()
        .filter(|r| r.engines.len() > 1)
        .map(|r| OverlapRow { url: r.url.clone(), engines: r.engines.clone(), count: r.engines.len() })
        .collect();
    overlap.sort_by_key(|o| std::cmp::Reverse(o.count));

    PipelineOutput {
        results,
        omitted: omitted_by_cap + shaped_len.saturating_sub(spread_len),
        merged,
        domains,
        engines,
        overlap,
        related,
    }
}

/// First result from each site, up to `max`.
pub fn pick_scrapable(results: &[Ranked], max: usize) -> Vec<&Ranked> {
    let mut seen = HashSet::new();
    results.iter().filter(|r| seen.insert(site_key(r).to_string())).take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(url: &str, title: &str, source: &str) -> DegoogResult {
        DegoogResult {
            title: Some(title.into()),
            url: url.into(),
            snippet: Some(format!("<b>about</b> {title}")),
            source: Some(source.into()),
            ..Default::default()
        }
    }

    #[test]
    fn pipeline_dedupes_spreads_and_caps() {
        let response = SearchResponse {
            results: vec![
                result("https://www.a.com/x?utm_source=1", "A", "google"),
                result("https://a.com/x", "A longer title", "bing"),
                result("https://a.com/y", "A2", "bing"),
                result("https://a.com/z", "A3", "bing"),
                result("https://b.com/", "B", "google"),
                result("mailto:x@y.z", "bad", "google"),
            ],
            related_searches: vec!["r1".into(), "r1".into()],
            ..Default::default()
        };
        let responses = vec![("q".to_string(), response)];
        let out = run_pipeline(PipelineInput { responses: &responses, max_results: 2, snippet_chars: 220, per_domain: 2 });

        assert_eq!(out.merged, 1);
        assert_eq!(out.results.len(), 2);
        assert_eq!(out.results[0].url, "https://a.com/x");
        assert_eq!(out.results[0].title, "A longer title");
        assert_eq!(out.results[0].engines, vec!["google", "bing"]);
        assert_eq!(out.results[0].reasons, vec!["2 engines agree"]);
        assert_eq!(out.results[0].snippet, "about A longer title");
        // a.com/z dropped by the per-domain spread, b.com dropped by the cap.
        assert_eq!(out.omitted, 2);
        assert_eq!(out.related, vec!["r1"]);
        assert_eq!(out.overlap.len(), 1);
    }

    #[test]
    fn trims_snippets() {
        assert_eq!(trim_snippet("3 days ago - Hello there", 100), "Hello there");
        let long = "First sentence is here. Second one is a lot longer and keeps going";
        assert_eq!(trim_snippet(long, 30), "First sentence is here.");
        assert_eq!(trim_snippet("word ".repeat(20).trim(), 12), "word word");
    }
}
