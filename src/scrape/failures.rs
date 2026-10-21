//! Remember recent scrape failures so `search` can flag them. 403 and 429
//! are remembered per host, everything else per URL.

use super::ScrapeRow;
use crate::cache::{cache_key, ns, Cache};
use crate::urls::domain_of;
use regex::Regex;
use serde_json::json;
use std::sync::LazyLock;

const FAILURE_TTL_MS: u64 = 600_000;
pub const RECENT_FAILURE: &str = "recent_failure";
static HOST_WIDE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(403|429)\b").unwrap());

fn domain_key(url: &str) -> String {
    cache_key(ns::SCRAPE_FAILURE, &json!({ "domain": domain_of(url) }))
}

fn url_key(url: &str) -> String {
    cache_key(ns::SCRAPE_FAILURE, &json!({ "url": url }))
}

pub fn note_failure(cache: &Cache, url: &str, reason: &str) {
    let key = if HOST_WIDE.is_match(reason) { domain_key(url) } else { url_key(url) };
    cache.set(&key, &reason, Some(FAILURE_TTL_MS));
}

pub fn note_rows(cache: &Cache, rows: &[ScrapeRow]) {
    for row in rows.iter().filter(|r| !r.ok) {
        if let Some(error) = &row.error {
            note_failure(cache, &row.url, error);
        }
    }
}

/// The recorded failure reason for this URL or its host, if any.
pub fn recent_failure(cache: &Cache, url: &str) -> Option<String> {
    cache.get::<String>(&url_key(url)).or_else(|| cache.get::<String>(&domain_key(url)))
}
