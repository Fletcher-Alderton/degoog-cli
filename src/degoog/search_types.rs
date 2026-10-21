//! Search types: `web`, engine-backed types, and plugin tabs. Degoog lists
//! them at `/api/search-tabs`; this folds legacy `engine:` / `engine-`
//! aliases onto their plain ids.

use super::types::{route, Tab};
use super::{Client, DegoogError};
use crate::cache::{cache_key, ns, Cache};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

const LOG_NS: &str = "search-types";
const ENGINE_PREFIX: &str = "engine:";
const ENGINE_DASH_PREFIX: &str = "engine-";
const TAB_PREFIX: &str = "tab:";
const TYPES_TTL_MS: u64 = 300_000;

pub const WEB_TYPE: &str = "web";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypeKind {
    Web,
    Engine,
    Tab,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeInfo {
    pub id: String,
    pub name: String,
    pub kind: TypeKind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tab_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TypeCatalog {
    pub types: Vec<TypeInfo>,
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedType {
    pub requested: String,
    pub value: String,
    pub info: Option<TypeInfo>,
    pub known: bool,
}

pub fn web_type() -> TypeInfo {
    TypeInfo { id: WEB_TYPE.into(), name: "Web".into(), kind: TypeKind::Web, tab_id: None }
}

/// Strip any number of `tab:` prefixes, then one `engine:` prefix.
pub fn plain_type(raw: &str) -> String {
    let mut value = raw.trim();
    while let Some(rest) = value.strip_prefix(TAB_PREFIX) {
        value = rest;
    }
    if let Some(rest) = value.strip_prefix(ENGINE_PREFIX) {
        value = rest;
    }
    value.trim().to_string()
}

struct Pending {
    raw: String,
    info: TypeInfo,
}

fn to_pending(tab: &Tab) -> Option<Pending> {
    let engine_backed = tab.id.starts_with(ENGINE_PREFIX);
    let id = plain_type(&tab.id);
    if id.is_empty() {
        return None;
    }
    let name = if tab.name.is_empty() { id.clone() } else { tab.name.clone() };
    Some(Pending {
        raw: tab.id.clone(),
        info: TypeInfo {
            id,
            name,
            kind: if engine_backed { TypeKind::Engine } else { TypeKind::Tab },
            tab_id: (!engine_backed).then(|| tab.id.clone()),
        },
    })
}

pub fn to_catalog(tabs: &[Tab]) -> TypeCatalog {
    let mut seen: HashSet<String> = HashSet::from([WEB_TYPE.to_string()]);
    let mut types = vec![web_type()];
    let mut aliases: Vec<String> = Vec::new();
    let pending: Vec<Pending> = tabs.iter().filter_map(to_pending).collect();

    for entry in &pending {
        if entry.raw.starts_with(ENGINE_PREFIX) {
            aliases.push(entry.raw.clone());
        }
        let key = entry.info.id.to_lowercase();
        if key.starts_with(ENGINE_DASH_PREFIX) || seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        types.push(entry.info.clone());
    }

    for entry in &pending {
        let key = entry.info.id.to_lowercase();
        let Some(target) = key.strip_prefix(ENGINE_DASH_PREFIX) else { continue };
        if seen.contains(target) {
            aliases.push(entry.info.id.clone());
            continue;
        }
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        types.push(entry.info.clone());
    }

    let mut unique = HashSet::new();
    aliases.retain(|a| unique.insert(a.clone()));
    TypeCatalog { types, aliases }
}

pub fn match_type<'a>(raw: &str, known: &'a [TypeInfo]) -> Option<&'a TypeInfo> {
    let wanted = plain_type(raw).to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    if let Some(exact) = known.iter().find(|t| t.id.to_lowercase() == wanted) {
        return Some(exact);
    }
    let target = wanted.strip_prefix(ENGINE_DASH_PREFIX)?;
    known.iter().find(|t| t.id.to_lowercase() == target)
}

pub async fn list_catalog(client: &Client, cache: &Cache) -> Result<TypeCatalog, DegoogError> {
    let key = cache_key(ns::SEARCH_TYPES, &json!({ "base": client.base_url() }));
    if let Some(hit) = cache.get::<TypeCatalog>(&key) {
        return Ok(hit);
    }
    let raw: Value = client.json(super::client::Request::get(route::SEARCH_TABS)).await?;
    let tabs: Vec<Tab> = raw
        .get("tabs")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect())
        .unwrap_or_default();
    let catalog = to_catalog(&tabs);
    cache.set(&key, &catalog, Some(TYPES_TTL_MS));
    Ok(catalog)
}

pub async fn resolve(client: &Client, cache: &Cache, raw: Option<&str>) -> ResolvedType {
    let requested = raw.map(str::trim).unwrap_or("").to_string();
    let stripped = plain_type(&requested);

    if stripped.is_empty() || stripped.eq_ignore_ascii_case(WEB_TYPE) {
        return ResolvedType { requested, value: WEB_TYPE.into(), info: Some(web_type()), known: true };
    }

    match list_catalog(client, cache).await {
        Ok(catalog) => {
            if let Some(found) = match_type(&requested, &catalog.types) {
                return ResolvedType { requested, value: found.id.clone(), info: Some(found.clone()), known: true };
            }
            crate::debug!(LOG_NS, "unknown search type {requested:?}, sending it as-is");
        }
        Err(err) => crate::warn!(LOG_NS, "search type lookup failed, sending the type as-is: {err}"),
    }

    ResolvedType { requested, value: stripped, info: None, known: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: &str, name: &str) -> Tab {
        Tab { id: id.into(), name: name.into() }
    }

    #[test]
    fn folds_aliases() {
        let catalog = to_catalog(&[
            tab("engine:images", "Images"),
            tab("engine-images", "Images (legacy)"),
            tab("news", "News"),
            tab("engine-videos", "Videos"),
            tab("tab:engine:", ""),
        ]);
        let ids: Vec<&str> = catalog.types.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["web", "images", "news", "engine-videos"]);
        assert_eq!(catalog.aliases, vec!["engine:images", "engine-images"]);
        assert_eq!(catalog.types[1].kind, TypeKind::Engine);
        assert_eq!(catalog.types[2].tab_id.as_deref(), Some("news"));
    }

    #[test]
    fn matches_types() {
        let catalog = to_catalog(&[tab("engine:images", "Images"), tab("news", "News")]);
        assert_eq!(match_type("Images", &catalog.types).unwrap().id, "images");
        assert_eq!(match_type("engine-images", &catalog.types).unwrap().id, "images");
        assert_eq!(match_type("tab:news", &catalog.types).unwrap().id, "news");
        assert!(match_type("maps", &catalog.types).is_none());
        assert_eq!(plain_type(" tab:tab:engine:x "), "x");
    }
}
