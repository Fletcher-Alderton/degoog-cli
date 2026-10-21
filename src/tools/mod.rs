//! The tool set shared by the CLI and the MCP server.

pub mod bundle;
pub mod info;
pub mod scrape;
pub mod search;

use crate::cache::Cache;
use crate::config::{Config, TextMode};
use crate::degoog::Client;
use crate::output::{input_error, ToolResult, Visible};
use crate::scrape::fetch::Fetchers;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Instant;

pub const SEARCH: &str = "search";
pub const SCRAPE: &str = "scrape";
pub const BUNDLE_SEARCH: &str = "bundle_search";
pub const DISCOVER: &str = "discover";
pub const RETRY_ENGINE: &str = "retry_engine";
pub const COMMAND: &str = "command";
pub const HEALTH: &str = "health";
pub const ALL: &[&str] = &[SEARCH, SCRAPE, BUNDLE_SEARCH, DISCOVER, RETRY_ENGINE, COMMAND, HEALTH];

pub struct Ctx {
    pub config: Config,
    pub client: Client,
    pub cache: Cache,
    pub fetchers: Fetchers,
    pub config_path: PathBuf,
    pub config_found: bool,
    pub started: Instant,
    /// Output is for a person at a terminal rather than a model.
    pub human: bool,
}

impl Ctx {
    pub fn new(config: Config, cache: Cache, config_path: PathBuf, config_found: bool, human: bool) -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::new(&config.degoog.url, &config.degoog.api_key, config.degoog.timeout)?,
            fetchers: Fetchers::new(&config.scrape)?,
            config,
            cache,
            config_path,
            config_found,
            started: Instant::now(),
            human,
        })
    }

    /// People get everything printed without model guidance; models get
    /// the configured text mode.
    pub fn visible(&self, mode: TextMode) -> Visible {
        if self.human {
            Visible { full: true, guidance: false, pack_note: false }
        } else {
            Visible { full: mode == TextMode::Full, guidance: self.config.output.guidance, pack_note: true }
        }
    }
}

const READ_ONLY: &str = r#"{"readOnlyHint": true, "openWorldHint": true}"#;

/// Tool definitions for MCP `tools/list`.
pub fn specs(ctx: &Ctx) -> Vec<Value> {
    let annotations: Value = serde_json::from_str(READ_ONLY).unwrap();
    let spec = |name: &str, title: &str, description: String, schema: Value| {
        json!({ "name": name, "title": title, "description": description, "inputSchema": schema, "annotations": annotations })
    };
    let empty = json!({ "type": "object", "properties": {} });
    vec![
        spec(SEARCH, "Degoog search",
            "Run a Degoog search and get compact ranked results with stable [S1] source ids and scrape recommendations.".into(),
            search::search_schema()),
        spec(SCRAPE, "Scrape URLs", scrape::about(ctx), scrape::schema()),
        spec(BUNDLE_SEARCH, "Bundle search",
            "Search, read sources in Degoog order, and return one compact evidence pack. Simplest single call for small models. Does not write the answer.".into(),
            bundle::schema()),
        spec(DISCOVER, "Discover capabilities",
            "List Degoog search types, engine ids, commands, suggestion availability and tool caps.".into(),
            empty.clone()),
        spec(RETRY_ENGINE, "Retry one engine",
            "Re-run a single Degoog engine for a query instead of repeating the whole search.".into(),
            search::retry_schema()),
        spec(COMMAND, "Run bang command",
            "Run a bang command installed on this Degoog instance and return its rendered output as text. Call discover first for the list of enabled commands, disabled ones return an error.".into(),
            info::command_schema()),
        spec(HEALTH, "Health",
            "Report Degoog reachability, API key status, cache stats and configured caps.".into(),
            empty),
    ]
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, ToolResult> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|err| input_error(format!("invalid arguments: {err}")))
}

/// Run a tool by name with JSON arguments. `None` when the name is unknown.
pub async fn call(ctx: &Ctx, name: &str, args: Value) -> Option<ToolResult> {
    let result = match name {
        SEARCH => match parse(args) {
            Ok(a) => search::run_search(ctx, a).await,
            Err(e) => e,
        },
        SCRAPE => match parse(args) {
            Ok(a) => scrape::run(ctx, a).await,
            Err(e) => e,
        },
        BUNDLE_SEARCH => match parse(args) {
            Ok(a) => bundle::run(ctx, a).await,
            Err(e) => e,
        },
        RETRY_ENGINE => match parse(args) {
            Ok(a) => search::run_retry(ctx, a).await,
            Err(e) => e,
        },
        COMMAND => match parse(args) {
            Ok(a) => info::run_command(ctx, a).await,
            Err(e) => e,
        },
        DISCOVER => info::run_discover(ctx).await,
        HEALTH => info::run_health(ctx).await,
        _ => return None,
    };
    Some(result)
}

pub fn caps(ctx: &Ctx) -> Value {
    let c = &ctx.config;
    json!({
        "searchMaxResults": c.search.max_results,
        "snippetChars": c.search.snippet_chars,
        "scrapeMaxUrls": c.scrape.max_urls,
        "scrapeMaxCharsPerUrl": c.scrape.max_chars_per_url,
        "scrapeRenderer": c.scrape.renderer(),
        "bundleMaxScrapeUrls": c.bundle_search.max_scrape_urls,
        "bundleMaxEvidenceChars": c.bundle_search.max_evidence_chars,
        "outputMode": c.output.mode,
    })
}

/// Check an optional integer argument against an inclusive range.
pub fn check_range<T: PartialOrd + std::fmt::Display + Copy>(name: &str, value: Option<T>, min: T, max: T) -> Result<(), ToolResult> {
    match value {
        Some(v) if v < min || v > max => Err(input_error(format!("{name} must be between {min} and {max}, got {v}"))),
        _ => Ok(()),
    }
}

pub fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}
