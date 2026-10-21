//! Degoog endpoints, with the cached variants the tools use.

use super::client::Request;
use super::search_types::{self, ResolvedType, TypeCatalog, TypeKind};
use super::types::{route, Command, CommandResult, Extension, SearchQuery, SearchResponse, Suggestion};
use super::{Client, DegoogError};
use crate::cache::{cache_key, ns, Cache};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

const LOG_NS: &str = "degoog-api";

fn search_params<'a>(mut req: Request<'a>, q: &'a SearchQuery) -> Request<'a> {
    req = req
        .param("q", Some(&q.query))
        .param("type", q.kind.as_ref())
        .param("page", q.page)
        .param("time", q.time.as_ref())
        .param("lang", q.lang.as_ref())
        .param("dateFrom", q.date_from.as_ref())
        .param("dateTo", q.date_to.as_ref())
        .param("safeMode", q.safe_mode.as_ref());
    req
}

fn search_body(q: &SearchQuery) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("query".into(), json!(q.query));
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(v) = value {
            body.insert(key.into(), v);
        }
    };
    put("type", q.kind.clone().map(Value::from));
    put("page", q.page.map(Value::from));
    put("time", q.time.clone().map(Value::from));
    put("lang", q.lang.clone().map(Value::from));
    put("dateFrom", q.date_from.clone().map(Value::from));
    put("dateTo", q.date_to.clone().map(Value::from));
    put("safeMode", q.safe_mode.clone().map(Value::from));
    if !q.engines.is_empty() {
        body.insert("engines".into(), json!(q.engines));
    }
    body
}

/// GET with query params, or POST when specific engines are requested.
pub async fn search(client: &Client, q: &SearchQuery) -> Result<SearchResponse, DegoogError> {
    if q.engines.is_empty() {
        client.json(search_params(Request::get(route::SEARCH), q)).await
    } else {
        client.json(Request::post(route::SEARCH, Value::Object(search_body(q)))).await
    }
}

pub async fn tab_search(client: &Client, tab: &str, query: &str, page: u32) -> Result<SearchResponse, DegoogError> {
    client
        .json(Request::get(route::TAB_SEARCH).param("tab", Some(tab)).param("q", Some(query)).param("page", Some(page)))
        .await
}

pub async fn retry_engine(client: &Client, q: &SearchQuery, engine: &str) -> Result<SearchResponse, DegoogError> {
    if q.engines.is_empty() {
        client
            .json(search_params(Request::get(route::SEARCH_RETRY), q).param("engine", Some(engine)))
            .await
    } else {
        let mut body = search_body(q);
        body.insert("engine".into(), json!(engine));
        client.json(Request::post(route::SEARCH_RETRY, Value::Object(body))).await
    }
}

/// Suggestions never fail the caller: problems come back as an empty list.
pub async fn suggest(client: &Client, query: &str) -> Vec<Suggestion> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let raw: Value = match client.json(Request::get(route::SUGGEST).param("q", Some(query))).await {
        Ok(raw) => raw,
        Err(err) => {
            crate::warn!(LOG_NS, "suggestion lookup failed: {err}");
            return Vec::new();
        }
    };
    let Value::Array(items) = raw else { return Vec::new() };
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::String(text) => Some(Suggestion { text, source: None }),
            Value::Object(map) => Some(Suggestion {
                text: map.get("text")?.as_str()?.to_string(),
                source: map.get("source").and_then(Value::as_str).map(String::from),
            }),
            _ => None,
        })
        .collect()
}

pub async fn list_commands(client: &Client) -> Result<Vec<Command>, DegoogError> {
    let raw: Value = client.json(Request::get(route::COMMANDS)).await?;
    let source = match raw {
        Value::Array(items) => items,
        Value::Object(mut map) => match map.remove("commands") {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    Ok(source.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

pub async fn run_command(client: &Client, query: &str, page: Option<u32>) -> Result<CommandResult, DegoogError> {
    client.json(Request::get(route::COMMAND).param("q", Some(query)).param("page", page)).await
}

pub async fn extensions(client: &Client, kind: &str) -> Result<Vec<Extension>, DegoogError> {
    let raw: Value = client.json(Request::get(route::EXTENSIONS).param("type", Some(kind))).await?;
    let group = match kind {
        "engine" => "engines",
        other => other,
    };
    Ok(raw
        .get(group)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect())
        .unwrap_or_default())
}

pub struct SearchOutcome {
    pub response: SearchResponse,
    pub search_type: ResolvedType,
}

pub async fn search_cached(client: &Client, cache: &Cache, input: &SearchQuery) -> Result<SearchOutcome, DegoogError> {
    let search_type = search_types::resolve(client, cache, input.kind.as_deref()).await;
    let tab_id = search_type.info.as_ref().and_then(|i| i.tab_id.clone());
    let mut engines = input.engines.clone();
    engines.sort();

    let key = cache_key(
        ns::SEARCH,
        &json!({
            "base": client.base_url(),
            "query": input.query,
            "type": search_type.value,
            "tab": tab_id.clone().unwrap_or_default(),
            "page": input.page.unwrap_or(1),
            "time": input.time.clone().unwrap_or_else(|| "any".into()),
            "lang": input.lang.clone().unwrap_or_default(),
            "dateFrom": input.date_from.clone().unwrap_or_default(),
            "dateTo": input.date_to.clone().unwrap_or_default(),
            "safeMode": input.safe_mode.clone().unwrap_or_default(),
            "engines": engines,
        }),
    );

    if let Some(hit) = cache.get::<SearchResponse>(&key) {
        return Ok(SearchOutcome { response: hit, search_type });
    }

    let is_tab = search_type.info.as_ref().is_some_and(|i| i.kind == TypeKind::Tab);
    let response = match (is_tab, tab_id) {
        (true, Some(tab)) => tab_search(client, &tab, &input.query, input.page.unwrap_or(1)).await?,
        _ => {
            let mut q = input.clone();
            q.kind = Some(search_type.value.clone());
            search(client, &q).await?
        }
    };

    cache.set(&key, &response, None);
    Ok(SearchOutcome { response, search_type })
}

pub async fn suggest_cached(client: &Client, cache: &Cache, query: &str) -> Vec<Suggestion> {
    let key = cache_key(ns::SUGGEST, &json!({ "base": client.base_url(), "query": query }));
    if let Some(hit) = cache.get::<Vec<Suggestion>>(&key) {
        return hit;
    }
    let suggestions = suggest(client, query).await;
    cache.set(&key, &suggestions, None);
    suggestions
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub search_types: Vec<search_types::TypeInfo>,
    pub type_aliases: Vec<String>,
    pub engines: Vec<Extension>,
    pub autocomplete: Vec<Extension>,
    pub commands: Vec<Command>,
    pub reachable: bool,
    pub problems: Vec<String>,
}

fn settle<T>(label: &str, problems: &mut Vec<String>, fallback: T, result: Result<T, DegoogError>) -> T {
    result.unwrap_or_else(|err| {
        crate::warn!(LOG_NS, "{label} lookup failed: {err}");
        problems.push(format!("{label} unavailable"));
        fallback
    })
}

/// Everything `discover` reports, fetched in parallel. Individual lookups
/// may fail without failing the whole call.
pub async fn capabilities(client: &Client, cache: &Cache) -> Capabilities {
    let (catalog, engines, autocomplete, commands) = tokio::join!(
        search_types::list_catalog(client, cache),
        extensions(client, "engine"),
        extensions(client, "autocomplete"),
        list_commands(client),
    );
    let mut problems = Vec::new();
    let fallback = TypeCatalog { types: vec![search_types::web_type()], aliases: Vec::new() };
    let catalog = settle("search-tabs", &mut problems, fallback, catalog);
    let engines = settle("engines", &mut problems, Vec::new(), engines);
    let autocomplete = settle("autocomplete", &mut problems, Vec::new(), autocomplete);
    let commands = settle("commands", &mut problems, Vec::new(), commands);

    Capabilities {
        search_types: catalog.types,
        type_aliases: catalog.aliases,
        engines,
        autocomplete,
        commands,
        reachable: problems.len() < 4,
        problems,
    }
}
