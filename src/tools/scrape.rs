//! `scrape`: fetch explicit URLs and return evidence chunks.

use super::{check_range, Ctx};
use crate::bundle::{EvidenceChunk, FailedSource};
use crate::output::{cite_id, input_error, scrape_text, LinkedSource, ScrapeVisible, ToolResult};
use crate::scrape::{failures, scrape_urls, ScrapeOptions, ScrapeRow};
use crate::text::char_len;
use serde::Deserialize;
use serde_json::{json, Value};

const LOG_NS: &str = "tool-scrape";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeArgs {
    pub urls: Vec<String>,
    pub query: Option<String>,
    pub max_chars: Option<usize>,
    pub max_chunks: Option<usize>,
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "urls": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Explicit http(s) URLs to fetch. Private addresses are blocked." },
            "query": { "type": "string", "description": "Optional query used to score evidence chunks within each page." },
            "maxChars": { "type": "integer", "minimum": 200, "maximum": 20000, "description": "Per-URL character budget." },
            "maxChunks": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Per-URL evidence chunk cap." }
        },
        "required": ["urls"]
    })
}

pub fn about(ctx: &Ctx) -> String {
    let lead = "Fetch explicit http(s) URLs and return cleaned evidence chunks.";
    let tail = "One row per URL, including failures.";
    if ctx.config.scrape.fetcher.url.is_empty() {
        format!("{lead} Static fetch only, no JavaScript rendering, so app-shell pages come back as failure rows. {tail}")
    } else {
        format!("{lead} {tail}")
    }
}

fn evidence_row(row: &ScrapeRow, index: usize) -> Value {
    let mut out = json!({
        "id": cite_id(index),
        "url": row.url,
        "ok": row.ok,
        "chunks": row.chunks.iter().map(|c| {
            let mut chunk = json!({ "text": c.text });
            if let Some(h) = &c.heading { chunk["heading"] = json!(h); }
            chunk
        }).collect::<Vec<_>>(),
        "chunksOmitted": row.chunks_omitted,
    });
    if row.final_url != row.url && !row.final_url.is_empty() {
        out["finalUrl"] = json!(row.final_url);
    }
    if !row.title.is_empty() {
        out["title"] = json!(row.title);
    }
    if let Some(p) = &row.published_at {
        out["publishedAt"] = json!(p);
    }
    if let Some(s) = &row.site_name {
        out["site"] = json!(s);
    }
    if row.truncated {
        out["truncated"] = json!(true);
    }
    if let Some(e) = &row.error {
        out["error"] = json!(e);
    }
    out
}

fn visible_pack(rows: &[ScrapeRow], max_chars: usize, useful: usize, failed: usize) -> ScrapeVisible {
    let mut pack = ScrapeVisible {
        requested: rows.len(),
        useful,
        failed,
        sources: Vec::new(),
        chunks: Vec::new(),
        failures: Vec::new(),
        chunks_omitted: 0,
    };
    let mut used = 0;
    for (index, row) in rows.iter().enumerate() {
        let id = cite_id(index);
        if !row.ok {
            pack.failures.push(FailedSource { url: row.url.clone(), reason: row.error.clone().unwrap_or_else(|| "scrape failed".into()) });
            continue;
        }
        pack.sources.push(LinkedSource {
            id: id.clone(),
            title: if row.title.is_empty() { row.url.clone() } else { row.title.clone() },
            url: row.final_url.clone(),
        });
        for chunk in &row.chunks {
            let len = char_len(&chunk.text);
            if used + len > max_chars {
                pack.chunks_omitted += 1;
                continue;
            }
            used += len;
            pack.chunks.push(EvidenceChunk { id: id.clone(), heading: chunk.heading.clone(), text: chunk.text.clone(), match_pct: chunk.match_pct });
        }
    }
    pack
}

pub async fn run(ctx: &Ctx, args: ScrapeArgs) -> ToolResult {
    let config = &ctx.config;
    if let Err(e) = check_range("maxChars", args.max_chars, 200, 20_000).and(check_range("maxChunks", args.max_chunks, 1, 20)) {
        return e;
    }
    let requested: Vec<String> = args.urls.iter().map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect();
    if requested.is_empty() {
        return input_error("at least one url is required");
    }
    let take = requested.len().min(config.scrape.max_urls);
    let skipped = requested.len() - take;
    let urls = &requested[..take];

    let options = ScrapeOptions {
        query: args.query.as_deref().unwrap_or(""),
        config: &config.scrape,
        max_chars_per_url: args.max_chars,
        max_chunks_per_url: args.max_chunks,
    };
    let rows = scrape_urls(&ctx.fetchers, urls, &options).await;
    failures::note_rows(&ctx.cache, &rows);

    let useful = rows.iter().filter(|r| r.ok).count();
    let failed = rows.len() - useful;
    crate::debug!(LOG_NS, "urls={} useful={useful} failed={failed} skipped={skipped}", rows.len());

    let mut structured = json!({
        "requested": requested.len(),
        "scraped": rows.len(),
        "skipped": skipped,
        "counts": { "useful": useful, "failed": failed },
        "sources": rows.iter().enumerate().map(|(i, r)| evidence_row(r, i)).collect::<Vec<_>>(),
        "renderer": config.scrape.renderer(),
        "textMode": config.scrape.text_mode,
    });
    if config.output.mode.wants_detail() {
        structured["redirects"] = json!(rows
            .iter()
            .filter(|r| !r.redirects.is_empty())
            .map(|r| json!({ "url": r.url, "hops": r.redirects }))
            .collect::<Vec<_>>());
    }

    let mut text = scrape_text(
        ctx.visible(config.scrape.text_mode),
        &visible_pack(&rows, config.scrape.max_evidence_chars, useful, failed),
    );
    if skipped > 0 {
        text.push_str(&format!("\n{skipped} URLs over the scrape.max_urls cap ({}) were skipped.", config.scrape.max_urls));
    }
    ToolResult::ok(text, structured)
}
