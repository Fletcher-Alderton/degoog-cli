use super::chunks::{pick_chunks, Chunk};
use super::extract::{extract, Extraction};
use super::fetch::{fetch_page, fetch_via, Fetchers};
use super::markdown::MarkdownOptions;
use crate::config::ScrapeConfig;
use crate::text::{cap_chars, char_len};
use futures::StreamExt;

const LOG_NS: &str = "scrape";
pub const BROWSER_NEEDED: &str = "page needs browser rendering, this scraper is static fetch only";
const BROWSER_STILL: &str =
    "page still needs browser rendering, the delegated fetcher did not return a rendered body";
const NO_CONTENT: &str =
    "no readable article content found in the static html, the page may render its body with JavaScript";
/// How much more text than the budget is scanned for chunks.
const SCAN_FACTOR: usize = 8;

#[derive(Clone, Debug, Default)]
pub struct ScrapeRow {
    pub url: String,
    pub final_url: String,
    pub ok: bool,
    pub title: String,
    pub canonical: String,
    pub published_at: Option<String>,
    pub site_name: Option<String>,
    pub chunks: Vec<Chunk>,
    pub chunks_omitted: usize,
    pub chars: usize,
    pub truncated: bool,
    pub redirects: Vec<String>,
    pub error: Option<String>,
}

pub struct ScrapeOptions<'a> {
    pub query: &'a str,
    pub config: &'a ScrapeConfig,
    pub max_chars_per_url: Option<usize>,
    pub max_chunks_per_url: Option<usize>,
}

fn failed_row(url: &str, error: impl Into<String>, final_url: &str) -> ScrapeRow {
    ScrapeRow { url: url.into(), final_url: final_url.into(), error: Some(error.into()), ..Default::default() }
}

/// HTML parsing is CPU work on a non-Send DOM, so it runs on the blocking
/// pool and any panic in it becomes a failed row instead of a crash.
async fn extract_blocking(html: String, url: String, options: MarkdownOptions) -> Option<Extraction> {
    tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract(&html, &url, options))).ok()
    })
    .await
    .ok()
    .flatten()
}

async fn scrape_one(fetchers: &Fetchers, url: String, options: &ScrapeOptions<'_>) -> ScrapeRow {
    let url = url.as_str();
    let config = options.config;
    let max_chars = options.max_chars_per_url.unwrap_or(config.max_chars_per_url);
    let max_chunks = options.max_chunks_per_url.unwrap_or(config.max_chunks_per_url);
    let delegated = !config.fetcher.url.is_empty();

    let outcome =
        if delegated { fetch_via(fetchers, url, config).await } else { fetch_page(fetchers, url, config).await };
    if !outcome.ok {
        return ScrapeRow {
            redirects: outcome.redirects,
            ..failed_row(url, outcome.error.unwrap_or_else(|| "fetch failed".into()), &outcome.url)
        };
    }

    let markdown = MarkdownOptions { hide_images: config.hide_images };
    let Some(extraction) = extract_blocking(outcome.html, outcome.url.clone(), markdown).await else {
        crate::warn!(LOG_NS, "extraction failed for {url}");
        return failed_row(url, "extraction failed", &outcome.url);
    };

    if extraction.needs_browser || extraction.text.trim().is_empty() {
        let reason = match (extraction.needs_browser, delegated) {
            (true, true) => BROWSER_STILL,
            (true, false) => BROWSER_NEEDED,
            (false, _) => NO_CONTENT,
        };
        return ScrapeRow {
            title: extraction.title,
            canonical: extraction.canonical,
            redirects: outcome.redirects,
            ..failed_row(url, reason, &outcome.url)
        };
    }

    let scanned = cap_chars(&extraction.text, max_chars.saturating_mul(SCAN_FACTOR));
    let picked = pick_chunks(&scanned, options.query, config.chunk_chars, max_chunks, max_chars);
    let chars: usize = picked.chunks.iter().map(|c| char_len(&c.text)).sum();
    let ok = !picked.chunks.is_empty();

    ScrapeRow {
        url: url.into(),
        final_url: outcome.url,
        ok,
        title: extraction.title,
        canonical: extraction.canonical,
        published_at: extraction.published_at,
        site_name: extraction.site_name,
        chunks: picked.chunks,
        chunks_omitted: picked.omitted,
        chars,
        truncated: outcome.truncated || char_len(&extraction.text) > chars,
        redirects: outcome.redirects,
        error: (!ok).then(|| "no usable evidence extracted".into()),
    }
}

/// Scrape every URL with bounded concurrency. Rows come back in input order,
/// one per URL, failures included.
pub async fn scrape_urls(fetchers: &Fetchers, urls: &[String], options: &ScrapeOptions<'_>) -> Vec<ScrapeRow> {
    futures::stream::iter(urls.iter().cloned())
        .map(|url| scrape_one(fetchers, url, options))
        .buffered(options.config.concurrency.max(1))
        .collect()
        .await
}
