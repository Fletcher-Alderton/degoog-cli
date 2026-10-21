//! bundle_search building blocks: candidate selection in Degoog order,
//! scrape waves that walk further down the list when pages fail, and the
//! evidence pack with stable `[S1]` ids.

use crate::cache::Cache;
use crate::config::QueryExpansion;
use crate::degoog::{api, Client};
use crate::output::cite_id;
use crate::scrape::ScrapeRow;
use crate::search::Ranked;
use crate::text::{char_len, clean_title};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::future::Future;

pub fn per_source_budget(total_chars: usize, sources: usize) -> usize {
    (total_chars / sources.max(1)).max(400)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub url: String,
    pub title: String,
    pub domain: String,
    pub reason: String,
    pub degoog_score: f64,
}

/// One candidate per site, in Degoog order, up to `max`.
pub fn pick_candidates(results: &[Ranked], max: usize) -> Vec<Candidate> {
    let mut used = HashSet::new();
    let mut picked = Vec::new();
    for result in results {
        if picked.len() >= max {
            break;
        }
        let key = if result.root.is_empty() { &result.domain } else { &result.root };
        if !used.insert(key.clone()) {
            continue;
        }
        let reason = if result.engines.len() > 1 {
            format!("{} engines agree", result.engines.len())
        } else {
            result.reasons.first().cloned().unwrap_or_else(|| "next in Degoog order".into())
        };
        picked.push(Candidate {
            url: result.url.clone(),
            title: result.title.clone(),
            domain: result.domain.clone(),
            reason,
            degoog_score: result.degoog_score,
        });
    }
    picked
}

#[derive(Debug)]
pub struct Gathered {
    /// Useful rows (at most `target`) in Degoog order, then failures.
    pub rows: Vec<ScrapeRow>,
    pub attempted: usize,
    pub useful: usize,
    pub failed: usize,
    pub surplus: usize,
    pub waves: usize,
    pub continued: bool,
    pub exhausted: bool,
}

/// Scrape a first wave of `first_wave` candidates; if fewer than `target`
/// came back useful, run one more wave further down the list.
pub async fn gather_in_order<F, Fut>(candidates: &[Candidate], target: usize, first_wave: usize, scrape: F) -> Gathered
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = Vec<ScrapeRow>>,
{
    let order: HashMap<&str, usize> = candidates.iter().enumerate().map(|(i, c)| (c.url.as_str(), i)).collect();
    let mut collected: Vec<ScrapeRow> = Vec::new();
    let mut cursor = 0;
    let mut waves = 0;

    let first_end = first_wave.max(1).min(candidates.len());
    if first_end > 0 {
        collected.extend(scrape(candidates[..first_end].iter().map(|c| c.url.clone()).collect()).await);
        cursor = first_end;
        waves += 1;
    }

    let useful_now = collected.iter().filter(|r| r.ok).count();
    let short_by = target.saturating_sub(useful_now);
    let continued = short_by > 0 && cursor < candidates.len();
    if continued {
        let end = (cursor + short_by + first_wave.saturating_sub(target)).min(candidates.len());
        collected.extend(scrape(candidates[cursor..end].iter().map(|c| c.url.clone()).collect()).await);
        cursor = end;
        waves += 1;
    }

    collected.sort_by_key(|r| order.get(r.url.as_str()).copied().unwrap_or(usize::MAX));
    let (usable, failures): (Vec<ScrapeRow>, Vec<ScrapeRow>) = collected.into_iter().partition(|r| r.ok);
    let surplus = usable.len().saturating_sub(target);
    let kept: Vec<ScrapeRow> = usable.into_iter().take(target).collect();
    let useful = kept.len();
    let failed = failures.len();

    Gathered {
        rows: kept.into_iter().chain(failures).collect(),
        attempted: cursor,
        useful,
        failed,
        surplus,
        waves,
        continued,
        exhausted: useful < target && cursor >= candidates.len(),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EvidenceChunk {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    pub text: String,
    #[serde(rename = "match")]
    pub match_pct: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceSource {
    pub id: String,
    pub title: String,
    pub url: String,
    pub domain: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(rename = "match")]
    pub match_pct: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct FailedSource {
    pub url: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct EvidencePack {
    pub sources: Vec<EvidenceSource>,
    pub chunks: Vec<EvidenceChunk>,
    pub failures: Vec<FailedSource>,
    pub chars_used: usize,
    pub chunks_omitted: usize,
}

pub fn build_pack(rows: &[ScrapeRow], candidates: &[Candidate], ranked: &[Ranked], max_chars: usize) -> EvidencePack {
    let mut pack = EvidencePack::default();

    for row in rows {
        if !row.ok {
            pack.failures.push(FailedSource {
                url: row.url.clone(),
                reason: row.error.clone().unwrap_or_else(|| "scrape failed".into()),
            });
            continue;
        }
        let candidate = candidates.iter().find(|c| c.url == row.url);
        let id = cite_id(pack.sources.len());
        let fallback = candidate
            .map(|c| if c.title.is_empty() { c.domain.clone() } else { c.title.clone() })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| row.url.clone());

        pack.sources.push(EvidenceSource {
            id: id.clone(),
            title: clean_title(Some(&row.title), &fallback),
            url: if row.canonical.is_empty() { row.url.clone() } else { row.canonical.clone() },
            domain: candidate.map(|c| c.domain.clone()).unwrap_or_default(),
            reason: candidate.map(|c| c.reason.clone()).unwrap_or_else(|| "selected in Degoog order".into()),
            published_at: row.published_at.clone(),
            snippet: ranked.iter().find(|r| r.url == row.url).map(|r| r.snippet.clone()),
            match_pct: row.chunks.iter().map(|c| c.match_pct).max().unwrap_or(0),
        });

        pack.chunks_omitted += row.chunks_omitted;
        for chunk in &row.chunks {
            let len = char_len(&chunk.text);
            if pack.chars_used + len > max_chars {
                pack.chunks_omitted += 1;
                continue;
            }
            pack.chars_used += len;
            pack.chunks.push(EvidenceChunk {
                id: id.clone(),
                heading: chunk.heading.clone(),
                text: chunk.text.clone(),
                match_pct: chunk.match_pct,
            });
        }
    }
    pack
}

pub fn next_actions(pack: &EvidencePack, omitted: usize) -> Vec<&'static str> {
    let mut actions = Vec::new();
    if pack.chunks.is_empty() {
        actions.push("no evidence extracted, try scrape on specific URLs");
    }
    if !pack.failures.is_empty() {
        actions.push("some sources failed, consider scrape on alternates");
    }
    if omitted > 0 {
        actions.push("more ranked results exist, narrow the query or raise caps");
    }
    if pack.chunks_omitted > 0 {
        actions.push("evidence was capped, scrape a single URL for more depth");
    }
    actions
}

fn unique_new(candidates: impl IntoIterator<Item = String>, existing: &[String], max: usize) -> Vec<String> {
    let mut seen: HashSet<String> = existing.iter().map(|e| e.to_lowercase()).collect();
    let mut picked = Vec::new();
    for candidate in candidates {
        if picked.len() >= max {
            break;
        }
        let trimmed = candidate.trim().to_string();
        if trimmed.is_empty() || !seen.insert(trimmed.to_lowercase()) {
            continue;
        }
        picked.push(trimmed);
    }
    picked
}

/// Extra queries from Degoog suggestions or related searches.
pub async fn expand_queries(
    client: &Client,
    cache: &Cache,
    mode: QueryExpansion,
    base: &[String],
    related: &[String],
    max: usize,
) -> Vec<String> {
    match mode {
        _ if max == 0 => Vec::new(),
        QueryExpansion::Off => Vec::new(),
        QueryExpansion::Related => unique_new(related.iter().cloned(), base, max),
        QueryExpansion::Suggestions => {
            let seed = base.first().map(String::as_str).unwrap_or("");
            let suggestions = api::suggest_cached(client, cache, seed).await;
            unique_new(suggestions.into_iter().map(|s| s.text), base, max)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn candidate(url: &str) -> Candidate {
        Candidate { url: url.into(), title: String::new(), domain: String::new(), reason: String::new(), degoog_score: 0.0 }
    }

    fn row(url: &str, ok: bool) -> ScrapeRow {
        ScrapeRow { url: url.into(), ok, error: (!ok).then(|| "HTTP 403".into()), ..Default::default() }
    }

    #[tokio::test]
    async fn second_wave_fills_failures() {
        let candidates: Vec<Candidate> = (0..8).map(|i| candidate(&format!("https://s{i}.com/"))).collect();
        let calls = Mutex::new(Vec::new());
        let out = gather_in_order(&candidates, 3, 4, |urls: Vec<String>| {
            calls.lock().unwrap().push(urls.len());
            async move {
                // s0, s2, s3 fail; everything else works.
                urls.iter().map(|u| row(u, !(u.contains("s0") || u.contains("s2") || u.contains("s3")))).collect()
            }
        })
        .await;
        // First wave 4 (1 useful), short by 2, so 2 + (4 - 3) = 3 more.
        assert_eq!(*calls.lock().unwrap(), vec![4, 3]);
        assert!(out.continued);
        assert_eq!(out.useful, 3);
        assert_eq!(out.failed, 3);
        assert_eq!(out.attempted, 7);
        assert!(!out.exhausted);
        let urls: Vec<&str> = out.rows.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["https://s1.com/", "https://s4.com/", "https://s5.com/", "https://s0.com/", "https://s2.com/", "https://s3.com/"]);
    }

    #[test]
    fn expansion_dedupes() {
        let base = vec!["Rust".to_string()];
        let out = unique_new(["rust".into(), " rust async ".into(), "".into(), "tokio".into(), "x".into()], &base, 2);
        assert_eq!(out, vec!["rust async", "tokio"]);
    }
}
