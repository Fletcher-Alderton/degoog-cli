//! `search` and `retry_engine`.

use super::{check_range, non_empty, Ctx};
use crate::degoog::search_types::{plain_type, ResolvedType, TypeKind};
use crate::degoog::types::{DegoogTiming, SearchQuery, SearchResponse, TIME_FILTERS};
use crate::degoog::api;
use crate::output::{cite_id, from_degoog, input_error, retry_text, search_text, SearchRow, ToolResult};
use crate::scrape::failures::{recent_failure, RECENT_FAILURE};
use crate::search::{pick_scrapable, run_pipeline, PipelineInput, PipelineOutput, Ranked};
use crate::text::short_query;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const LOG_NS: &str = "tool-search";
const HEALTH_HINT: &str = "Check Degoog reachability with the health tool.";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchArgs {
    pub query: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub page: Option<u32>,
    pub time: Option<String>,
    pub lang: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    #[serde(default)]
    pub engines: Vec<String>,
    pub safe_mode: Option<String>,
    pub max_results: Option<usize>,
}

pub fn search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "minLength": 1, "description": "Search query to send to Degoog." },
            "type": { "type": "string", "description": "Search tab/type, defaults to web. Use discover for valid values." },
            "page": { "type": "integer", "minimum": 1, "maximum": 10, "description": "Result page, 1-10." },
            "time": { "type": "string", "enum": TIME_FILTERS, "description": "Time filter. Use custom with dateFrom/dateTo." },
            "lang": { "type": "string", "description": "Language code, e.g. en." },
            "dateFrom": { "type": "string", "description": "YYYY-MM-DD, only with time=custom." },
            "dateTo": { "type": "string", "description": "YYYY-MM-DD, only with time=custom." },
            "engines": { "type": "array", "items": { "type": "string" }, "description": "Engine ids to use. Omit for instance defaults." },
            "safeMode": { "type": "string", "description": "Safe search level." },
            "maxResults": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Cap on returned results." }
        },
        "required": ["query"]
    })
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRow {
    pub id: String,
    pub title: String,
    pub url: String,
    pub domain: String,
    pub snippet: String,
    pub engines: Vec<String>,
    pub degoog_score: f64,
    pub reasons: Vec<String>,
}

pub fn source_row(result: &Ranked, index: usize) -> SourceRow {
    SourceRow {
        id: cite_id(index),
        title: result.title.clone(),
        url: result.url.clone(),
        domain: result.domain.clone(),
        snippet: result.snippet.clone(),
        engines: result.engines.clone(),
        degoog_score: result.degoog_score,
        reasons: result.reasons.clone(),
    }
}

pub fn unknown_type_note(t: &ResolvedType) -> Option<String> {
    (!t.known).then(|| {
        format!(
            "{:?} is not an advertised search type, Degoog was asked for {:?}. Run discover for the values that work here.",
            t.requested, t.value
        )
    })
}

fn validate(args: &SearchArgs) -> Result<(), ToolResult> {
    if args.query.trim().is_empty() {
        return Err(input_error("query must not be empty"));
    }
    check_range("page", args.page, 1, 10)?;
    check_range("maxResults", args.max_results, 1, 50)?;
    if let Some(time) = &args.time {
        if !TIME_FILTERS.contains(&time.as_str()) {
            return Err(input_error(format!("time must be one of {}", TIME_FILTERS.join(", "))));
        }
    }
    Ok(())
}

pub fn pipeline(ctx: &Ctx, query: &str, response: SearchResponse, max_results: usize) -> PipelineOutput {
    let responses = [(query.to_string(), response)];
    run_pipeline(PipelineInput {
        responses: &responses,
        max_results,
        snippet_chars: ctx.config.search.snippet_chars,
        per_domain: 2,
    })
}

pub async fn run_search(ctx: &Ctx, args: SearchArgs) -> ToolResult {
    if let Err(e) = validate(&args) {
        return e;
    }
    let query = args.query.trim().to_string();
    let config = &ctx.config;
    let max_results = args.max_results.unwrap_or(config.search.max_results).min(50);

    let input = SearchQuery {
        query: query.clone(),
        kind: non_empty(args.kind),
        page: args.page,
        time: non_empty(args.time),
        lang: non_empty(args.lang),
        date_from: non_empty(args.date_from),
        date_to: non_empty(args.date_to),
        engines: args.engines.into_iter().filter(|e| !e.trim().is_empty()).collect(),
        safe_mode: non_empty(args.safe_mode),
    };

    let outcome = match api::search_cached(&ctx.client, &ctx.cache, &input).await {
        Ok(o) => o,
        Err(err) => {
            crate::warn!(LOG_NS, "search failed for {:?}: {err}", short_query(&query));
            return from_degoog(&err, Some(HEALTH_HINT));
        }
    };
    let total_time = outcome.response.total_time;
    let timings = outcome.response.engine_timings.clone();
    let raw_count = outcome.response.results.len();
    let out = pipeline(ctx, &query, outcome.response, max_results);

    let sources: Vec<SourceRow> = out.results.iter().enumerate().map(|(i, r)| source_row(r, i)).collect();
    let recommended: Vec<Value> = pick_scrapable(&out.results, config.scrape.max_urls.min(sources.len()))
        .into_iter()
        .map(|r| {
            let id = sources.iter().find(|s| s.url == r.url).map(|s| s.id.clone()).unwrap_or_default();
            let mut row = json!({
                "id": id,
                "url": r.url,
                "reason": r.reasons.first().cloned().unwrap_or_else(|| "top Degoog-ranked result".into()),
            });
            if let Some(reason) = recent_failure(&ctx.cache, &r.url) {
                row["scrapeStatus"] = json!(RECENT_FAILURE);
                row["scrapeFailure"] = json!(reason);
            }
            row
        })
        .collect();

    crate::debug!(LOG_NS, "query={:?} results={} omitted={}", short_query(&query), sources.len(), out.omitted);

    let search_type = &outcome.search_type;
    let type_warning = unknown_type_note(search_type);
    let mut structured = json!({
        "query": query,
        "type": search_type.value,
        "typeKind": search_type.info.as_ref().map(|i| i.kind).unwrap_or(TypeKind::Web),
        "page": input.page.unwrap_or(1),
        "counts": {
            "returned": sources.len(),
            "omitted": out.omitted,
            "duplicatesMerged": out.merged,
            "domains": out.domains,
            "engines": out.engines,
        },
        "results": sources,
        "scrapeRecommendations": recommended,
        "relatedSearches": out.related.iter().take(6).collect::<Vec<_>>(),
        "textMode": config.search.text_mode,
    });
    if let Some(w) = &type_warning {
        structured["typeWarning"] = json!(w);
    }
    if config.output.mode.wants_detail() {
        structured["sourceOverlap"] = json!(out.overlap);
        structured["timings"] = json!({ "totalMs": total_time, "engines": timings });
    }
    if config.output.mode == crate::config::OutputMode::Full {
        structured["rawResultCount"] = json!(raw_count);
    }

    let rows: Vec<SearchRow<'_>> = sources
        .iter()
        .map(|s| SearchRow { id: &s.id, title: &s.title, url: &s.url, snippet: &s.snippet })
        .collect();
    let text = search_text(
        ctx.visible(config.search.text_mode),
        sources.len(),
        out.domains,
        out.engines,
        recommended.len(),
        type_warning.as_deref(),
        &rows,
    );
    ToolResult::ok(text, structured)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryArgs {
    pub query: String,
    pub engine: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub page: Option<u32>,
    pub lang: Option<String>,
}

pub fn retry_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "minLength": 1, "description": "Query to re-run." },
            "engine": { "type": "string", "minLength": 1, "description": "Engine id or name to retry. Use discover for valid ids." },
            "type": { "type": "string", "description": "Search tab/type, defaults to web." },
            "page": { "type": "integer", "minimum": 1, "maximum": 10, "description": "Result page." },
            "lang": { "type": "string", "description": "Language code." }
        },
        "required": ["query", "engine"]
    })
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

pub fn retry_timing(response: &SearchResponse, engine: &str) -> Option<DegoogTiming> {
    response.timing.clone().or_else(|| response.engine_timings.iter().find(|t| same_name(&t.name, engine)).cloned())
}

pub async fn run_retry(ctx: &Ctx, args: RetryArgs) -> ToolResult {
    let query = args.query.trim().to_string();
    let engine = args.engine.trim().to_string();
    if query.is_empty() || engine.is_empty() {
        return input_error("query and engine must not be empty");
    }
    if let Err(e) = check_range("page", args.page, 1, 10) {
        return e;
    }

    let input = SearchQuery {
        query: query.clone(),
        kind: non_empty(args.kind).map(|k| plain_type(&k)),
        page: args.page,
        lang: non_empty(args.lang),
        ..Default::default()
    };
    let response = match api::retry_engine(&ctx.client, &input, &engine).await {
        Ok(r) => r,
        Err(err) => {
            crate::warn!(LOG_NS, "retry failed for engine {engine}: {err}");
            return from_degoog(&err, Some("Use discover to list valid engine ids."));
        }
    };

    let timing = retry_timing(&response, &engine);
    let timings = response.engine_timings.clone();
    let out = pipeline(ctx, &query, response, ctx.config.search.max_results);
    let engine_name = timing.as_ref().map(|t| t.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| engine.clone());
    let from_engine = out.results.iter().filter(|r| r.engines.iter().any(|e| same_name(e, &engine_name))).count();
    let engine_results = timing.as_ref().and_then(|t| t.result_count);

    let mut text = retry_text(&engine_name, engine_results, out.results.len(), from_engine, timing.as_ref().and_then(|t| t.status.as_deref()));
    let rows: Vec<SourceRow> = out.results.iter().enumerate().map(|(i, r)| source_row(r, i)).collect();
    if ctx.human && !rows.is_empty() {
        text.push_str("\n\nResults:");
        for row in &rows {
            text.push_str(&format!("\n[{}] {} - {} ({})", row.id, row.title, row.url, row.engines.join(", ")));
        }
    }

    ToolResult::ok(
        text,
        json!({
            "engine": engine,
            "engineName": engine_name,
            "query": query,
            "counts": { "engineResults": engine_results, "merged": out.results.len(), "fromEngine": from_engine },
            "results": rows,
            "engineTiming": timing,
            "engineTimings": timings,
        }),
    )
}
