//! `bundle_search`: search, read sources in Degoog order, return one
//! compact evidence pack. Does not write the answer.

use super::search::{source_row, unknown_type_note};
use super::{check_range, non_empty, Ctx};
use crate::bundle::{build_pack, expand_queries, gather_in_order, next_actions, per_source_budget, pick_candidates};
use crate::config::QueryExpansion;
use crate::degoog::types::{SearchQuery, SearchResponse};
use crate::degoog::{api, DegoogError};
use crate::output::{bundle_text, error_result, from_degoog, timeout_error, BundleVisible, ErrorKindTag, ToolResult};
use crate::scrape::{failures, scrape_urls, ScrapeOptions};
use crate::search::{run_pipeline, PipelineInput};
use crate::text::short_query;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const LOG_NS: &str = "tool-bundle";
const CANDIDATE_POOL_FACTOR: usize = 2;
const TIMEOUT_FIX: &str = "Raise bundle_search.timeout in config.toml, or lower max_scrape_urls, max_queries and \
     scrape.timeout. All of them are milliseconds.";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleArgs {
    pub query: Option<String>,
    pub queries: Option<Vec<String>>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub lang: Option<String>,
    pub time: Option<String>,
    pub expand_queries: Option<bool>,
    pub max_scrape_urls: Option<usize>,
    pub max_evidence_chars: Option<usize>,
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Single research query." },
            "queries": { "type": "array", "items": { "type": "string" }, "description": "Explicit query list, used instead of query." },
            "type": { "type": "string", "description": "Search tab/type, defaults to web." },
            "lang": { "type": "string", "description": "Language code, e.g. en." },
            "time": { "type": "string", "description": "Time filter, e.g. week." },
            "expandQueries": { "type": "boolean", "description": "Allow suggestion-based query expansion for this call." },
            "maxScrapeUrls": { "type": "integer", "minimum": 1, "maximum": 12, "description": "How many sources to read." },
            "maxEvidenceChars": { "type": "integer", "minimum": 500, "maximum": 40000, "description": "Total evidence character budget." }
        }
    })
}

fn asked_queries(args: &BundleArgs) -> Vec<String> {
    let list = match (&args.queries, &args.query) {
        (Some(q), _) if !q.is_empty() => q.clone(),
        (_, Some(q)) => vec![q.clone()],
        _ => Vec::new(),
    };
    list.into_iter().map(|q| q.trim().to_string()).filter(|q| !q.is_empty()).collect()
}

async fn search_one(ctx: &Ctx, input: SearchQuery) -> Result<(String, api::SearchOutcome), DegoogError> {
    let outcome = api::search_cached(&ctx.client, &ctx.cache, &input).await?;
    Ok((input.query, outcome))
}

async fn gather(ctx: &Ctx, args: &BundleArgs, asked: Vec<String>) -> Result<ToolResult, DegoogError> {
    let config = &ctx.config;
    let settings = &config.bundle_search;

    let limit = args.queries.as_ref().map_or(1, Vec::len).min(settings.max_queries.max(1));
    let base: Vec<String> = asked.iter().take(limit.max(1)).cloned().collect();
    let rank_query = base[0].clone();

    let input_for = |query: &str| SearchQuery {
        query: query.to_string(),
        kind: non_empty(args.kind.clone()),
        lang: non_empty(args.lang.clone()),
        time: non_empty(args.time.clone()),
        ..Default::default()
    };

    let primary = futures::future::try_join_all(base.iter().map(|q| search_one(ctx, input_for(q))))
    .await?;
    let type_warning = primary.last().and_then(|(_, o)| unknown_type_note(&o.search_type));

    let mode = match args.expand_queries {
        Some(false) => QueryExpansion::Off,
        Some(true) if settings.query_expansion == QueryExpansion::Off => QueryExpansion::Suggestions,
        _ => settings.query_expansion,
    };
    let related: Vec<String> = primary.iter().flat_map(|(_, o)| o.response.related_searches.clone()).collect();
    let added = expand_queries(&ctx.client, &ctx.cache, mode, &base, &related, settings.max_expanded_queries).await;
    let source = if mode == QueryExpansion::Off || settings.max_expanded_queries == 0 { QueryExpansion::Off } else { mode };

    let expanded = futures::future::try_join_all(added.iter().map(|q| search_one(ctx, input_for(q))))
    .await?;

    let responses: Vec<(String, SearchResponse)> =
        primary.into_iter().chain(expanded).map(|(q, o)| (q, o.response)).collect();

    let target = args.max_scrape_urls.unwrap_or(settings.max_scrape_urls).max(1);
    let attempt_cap = target.max(settings.max_scrape_attempts);
    let pipeline = run_pipeline(PipelineInput {
        responses: &responses,
        max_results: settings.max_search_results_per_query.max(attempt_cap * CANDIDATE_POOL_FACTOR),
        snippet_chars: config.search.snippet_chars,
        per_domain: 2,
    });

    let max_chars = args.max_evidence_chars.unwrap_or(settings.max_evidence_chars);
    let per_source = per_source_budget(max_chars, target).min(config.scrape.max_chars_per_url);
    let candidates = pick_candidates(&pipeline.results, attempt_cap);

    let options = ScrapeOptions {
        query: &rank_query,
        config: &config.scrape,
        max_chars_per_url: Some(per_source),
        max_chunks_per_url: None,
    };
    let first_wave = candidates.len().min(target + settings.scrape_overage);
    let gathered =
        gather_in_order(&candidates, target, first_wave, |urls| {
            let options = &options;
            async move { scrape_urls(&ctx.fetchers, &urls, options).await }
        })
        .await;
    failures::note_rows(&ctx.cache, &gathered.rows);

    let pack = build_pack(&gathered.rows, &candidates, &pipeline.results, max_chars);
    let searched: Vec<&String> = responses.iter().map(|(q, _)| q).collect();

    crate::debug!(
        LOG_NS,
        "query={:?} sources={} chunks={} failures={}",
        short_query(&rank_query),
        pack.sources.len(),
        pack.chunks.len(),
        pack.failures.len()
    );

    let mut structured = json!({
        "originalQueries": asked,
        "searchedQueries": searched,
        "queriesOmitted": asked.len().saturating_sub(base.len()),
        "expansion": { "used": !added.is_empty(), "source": source.as_str(), "added": added },
        "counts": {
            "sources": pack.sources.len(),
            "chunks": pack.chunks.len(),
            "failures": pack.failures.len(),
            "resultsOmitted": pipeline.omitted,
            "chunksOmitted": pack.chunks_omitted,
            "evidenceChars": pack.chars_used,
        },
        "selectionPolicy": "degoog_order_readable_sources",
        "textMode": settings.text_mode,
        "selection": {
            "target": target,
            "attempted": gathered.attempted,
            "waves": gathered.waves,
            "continuedAfterFailure": gathered.continued,
            "surplusDiscarded": gathered.surplus,
            "candidatesExhausted": gathered.exhausted,
        },
        "sources": pack.sources,
        "evidence": pack.chunks,
        "failedSources": pack.failures,
        "nextActions": next_actions(&pack, pipeline.omitted),
    });
    if settings.include_raw_results || config.output.mode.wants_detail() {
        structured["rankedResults"] =
            json!(pipeline.results.iter().enumerate().map(|(i, r)| source_row(r, i)).collect::<Vec<_>>());
    }
    if let Some(w) = &type_warning {
        structured["typeWarning"] = json!(w);
    }

    let mut text = bundle_text(
        ctx.visible(settings.text_mode),
        &BundleVisible {
            sources: &pack.sources,
            chunks: &pack.chunks,
            failures: &pack.failures,
            continued: gathered.continued,
            exhausted: gathered.exhausted,
        },
    );
    if let Some(w) = type_warning {
        text.push('\n');
        text.push_str(&w);
    }
    Ok(ToolResult::ok(text, structured))
}

pub async fn run(ctx: &Ctx, args: BundleArgs) -> ToolResult {
    let asked = asked_queries(&args);
    if asked.is_empty() {
        return error_result(ErrorKindTag::Input, "provide query or queries", Some("Example: { \"query\": \"rust async runtime\" }"));
    }
    if let Err(e) = check_range("maxScrapeUrls", args.max_scrape_urls, 1, 12)
        .and(check_range("maxEvidenceChars", args.max_evidence_chars, 500, 40_000))
    {
        return e;
    }

    let limit = ctx.config.bundle_search.timeout;
    match tokio::time::timeout(Duration::from_millis(limit), gather(ctx, &args, asked)).await {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => {
            crate::warn!(LOG_NS, "bundle run failed: {err}");
            from_degoog(&err, Some("Check Degoog reachability with the health tool."))
        }
        Err(_) => timeout_error(format!("bundle_search timed out after {limit}ms"), TIMEOUT_FIX),
    }
}
