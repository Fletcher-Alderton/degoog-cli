//! `discover`, `health` and `command`.

use super::{caps, check_range, Ctx, ALL};
use crate::cache::{cache_key, ns};
use crate::degoog::api::{self, Capabilities};
use crate::degoog::client::Request;
use crate::degoog::types::route;
use crate::output::{from_degoog, input_error, ToolResult};
use crate::scrape::extract::html_to_text;
use crate::text::cap_chars;
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_LISTED: usize = 40;
const DISCOVER_TTL_MS: u64 = 300_000;
const PROBE_TIMEOUT_MS: u64 = 4000;
const MAX_COMMAND_CHARS: usize = 4000;

pub async fn run_discover(ctx: &Ctx) -> ToolResult {
    let key = cache_key(ns::DISCOVER, &json!({ "base": ctx.client.base_url() }));
    let caps_found = match ctx.cache.get::<Capabilities>(&key) {
        Some(hit) => hit,
        None => {
            let fresh = api::capabilities(&ctx.client, &ctx.cache).await;
            // Do not cache a total failure, the server may just be starting.
            if fresh.reachable {
                ctx.cache.set(&key, &fresh, Some(DISCOVER_TTL_MS));
            }
            fresh
        }
    };

    let types: Vec<_> = caps_found.search_types.iter().take(MAX_LISTED).collect();
    let engines: Vec<_> = caps_found.engines.iter().take(MAX_LISTED).collect();
    let commands: Vec<_> = caps_found.commands.iter().take(MAX_LISTED).collect();

    let mut lines = vec![
        format!("Degoog: {} engines, {} search types, {} commands.", engines.len(), types.len(), commands.len()),
        format!(
            "Search types usable as search(type=...): {}.",
            types.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")
        ),
    ];
    if !caps_found.type_aliases.is_empty() {
        lines.push(format!(
            "Legacy aliases also accepted, see aliasesAccepted in the structured content: {}.",
            caps_found.type_aliases.len()
        ));
    }
    lines.push(format!(
        "Suggestions: {}.",
        if caps_found.autocomplete.is_empty() { "none installed" } else { "available" }
    ));
    if ctx.human {
        if !engines.is_empty() {
            lines.push(String::new());
            lines.push("Engines:".into());
            for e in &engines {
                let state = match e.enabled {
                    Some(false) => " (disabled)",
                    _ => "",
                };
                match &e.name {
                    Some(name) if name != &e.id => lines.push(format!("  {} - {name}{state}", e.id)),
                    _ => lines.push(format!("  {}{state}", e.id)),
                }
            }
        }
        if !commands.is_empty() {
            lines.push(String::new());
            lines.push("Commands:".into());
            for c in &commands {
                match &c.description {
                    Some(d) => lines.push(format!("  !{} - {d}", c.trigger.trim_start_matches('!'))),
                    None => lines.push(format!("  !{}", c.trigger.trim_start_matches('!'))),
                }
            }
        }
    } else {
        lines.push("Use bundle_search when you want search plus evidence in one call.".into());
    }
    if !caps_found.problems.is_empty() {
        lines.push(format!("Problems: {}.", caps_found.problems.join(", ")));
    }

    let structured = json!({
        "searchTypes": types.iter().map(|t| &t.id).collect::<Vec<_>>(),
        "aliasesAccepted": caps_found.type_aliases,
        "tabs": types.iter().map(|t| json!({ "id": t.id, "name": t.name, "kind": t.kind })).collect::<Vec<_>>(),
        "engines": engines.iter().map(|e| json!({ "id": e.id, "name": e.name, "enabled": e.enabled })).collect::<Vec<_>>(),
        "commands": commands.iter().map(|c| json!({ "trigger": c.trigger, "category": c.category })).collect::<Vec<_>>(),
        "suggestionsAvailable": !caps_found.autocomplete.is_empty(),
        "enabledTools": ALL,
        "caps": caps(ctx),
        "omitted": {
            "tabs": caps_found.search_types.len().saturating_sub(MAX_LISTED),
            "engines": caps_found.engines.len().saturating_sub(MAX_LISTED),
            "commands": caps_found.commands.len().saturating_sub(MAX_LISTED),
        },
        "problems": caps_found.problems,
    });
    let mut result = ToolResult::ok(lines.join("\n"), structured);
    result.is_error = !caps_found.reachable;
    result
}

pub async fn run_health(ctx: &Ctx) -> ToolResult {
    let mut degoog = json!({
        "url": ctx.client.base_url(),
        "apiKeyConfigured": ctx.client.has_api_key(),
    });
    match ctx.client.raw(&Request::get(route::HEALTH).timeout(PROBE_TIMEOUT_MS)).await {
        Ok(response) => {
            degoog["reachable"] = json!(response.status().is_success());
            degoog["status"] = json!(response.status().as_u16());
        }
        Err(err) => {
            degoog["reachable"] = json!(false);
            degoog["status"] = Value::Null;
            degoog["detail"] = json!(err.message);
        }
    }
    let reachable = degoog["reachable"].as_bool().unwrap_or(false);
    let stats = ctx.cache.stats();

    let mut lines = vec![
        format!("{} {} {}.", crate::NAME, crate::VERSION, if reachable { "ok" } else { "degraded" }),
        format!("Degoog {} at {}.", if reachable { "reachable" } else { "unreachable" }, ctx.client.base_url()),
    ];
    if let Some(detail) = degoog.get("detail").and_then(Value::as_str) {
        lines.push(format!("Problem: {detail}."));
    } else if let Some(status) = degoog.get("status").and_then(Value::as_u64).filter(|_| !reachable) {
        lines.push(format!("Problem: /healthz answered HTTP {status}."));
    }
    lines.push(format!("Degoog API key {}.", if ctx.client.has_api_key() { "configured" } else { "not configured" }));
    lines.push(format!(
        "Config: {}{}.",
        ctx.config_path.display(),
        if ctx.config_found { "" } else { " (not found, using defaults)" }
    ));
    if ctx.human {
        lines.push(format!("Cache: {} entries, {} bytes{}.", stats.entries, stats.bytes,
            ctx.cache.path().map(|p| format!(" at {}", p.display())).unwrap_or_default()));
    }

    let mut result = ToolResult::ok(
        lines.join("\n"),
        json!({
            "status": if reachable { "ok" } else { "degraded" },
            "healthy": reachable,
            "version": crate::VERSION,
            "uptimeSec": ctx.started.elapsed().as_secs(),
            "degoog": degoog,
            "cache": stats,
            "configPath": ctx.config_path,
            "configFound": ctx.config_found,
            "enabledTools": ALL,
            "caps": caps(ctx),
        }),
    );
    result.is_error = !reachable && ctx.human;
    result
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CommandArgs {
    pub query: String,
    pub page: Option<u32>,
}

pub fn command_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "minLength": 1, "description": "Bang command with arguments, using a trigger listed by discover." },
            "page": { "type": "integer", "minimum": 1, "maximum": 10, "description": "Result page." }
        },
        "required": ["query"]
    })
}

pub async fn run_command(ctx: &Ctx, args: CommandArgs) -> ToolResult {
    let query = args.query.trim();
    if query.is_empty() {
        return input_error("query must not be empty");
    }
    if let Err(e) = check_range("page", args.page, 1, 10) {
        return e;
    }
    let result = match api::run_command(&ctx.client, query, args.page).await {
        Ok(r) => r,
        Err(err) => {
            crate::warn!("tool-command", "command failed for {query:?}: {err}");
            return from_degoog(&err, Some("Use discover to list available bang commands."));
        }
    };
    let rendered = match result.html.as_deref().filter(|h| !h.is_empty()) {
        Some(html) => {
            let html = html.to_string();
            let text = tokio::task::spawn_blocking(move || html_to_text(&html)).await.unwrap_or_default();
            cap_chars(&text, MAX_COMMAND_CHARS)
        }
        None => String::new(),
    };
    let text = if rendered.is_empty() {
        format!("Command ran but produced no text output (type {}).", result.kind.as_deref().unwrap_or("unknown"))
    } else {
        let title = result.title.as_deref().or(result.trigger.as_deref()).unwrap_or("Command");
        format!("{title}:\n{rendered}")
    };
    ToolResult::ok(
        text,
        json!({
            "query": query,
            "type": result.kind,
            "trigger": result.trigger,
            "title": result.title,
            "text": rendered,
            "page": result.page,
            "totalPages": result.total_pages,
        }),
    )
}
