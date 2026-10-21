//! Wire types for the Degoog API. Parsing is lenient: a field with an
//! unexpected type becomes its default instead of failing the whole response.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub mod route {
    pub const SEARCH: &str = "/api/search";
    pub const SEARCH_RETRY: &str = "/api/search/retry";
    pub const SUGGEST: &str = "/api/suggest";
    pub const SEARCH_TABS: &str = "/api/search-tabs";
    pub const TAB_SEARCH: &str = "/api/tab-search";
    pub const EXTENSIONS: &str = "/api/extensions";
    pub const COMMANDS: &str = "/api/commands";
    pub const COMMAND: &str = "/api/command";
    pub const HEALTH: &str = "/healthz";
}

pub const TIME_FILTERS: &[&str] = &["any", "hour", "day", "week", "month", "year", "custom"];

fn lenient<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(de)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Like [`lenient`] for lists, but drops only the bad items.
fn lenient_list<'de, D, T>(de: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Value::deserialize(de)?;
    Ok(match value {
        Value::Array(items) => items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect(),
        _ => Vec::new(),
    })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DegoogResult {
    #[serde(deserialize_with = "lenient")]
    pub title: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub url: String,
    #[serde(deserialize_with = "lenient")]
    pub snippet: Option<String>,
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub source: Option<String>,
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(deserialize_with = "lenient_list", skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DegoogTiming {
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "lenient")]
    pub time: f64,
    #[serde(deserialize_with = "lenient")]
    pub result_count: Option<u64>,
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchResponse {
    #[serde(deserialize_with = "lenient_list")]
    pub results: Vec<DegoogResult>,
    #[serde(deserialize_with = "lenient")]
    pub query: String,
    #[serde(deserialize_with = "lenient", rename = "type")]
    pub kind: String,
    #[serde(deserialize_with = "lenient")]
    pub total_time: f64,
    #[serde(deserialize_with = "lenient_list")]
    pub engine_timings: Vec<DegoogTiming>,
    #[serde(deserialize_with = "lenient_list")]
    pub related_searches: Vec<String>,
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub total_pages: Option<u64>,
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub timing: Option<DegoogTiming>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Suggestion {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    #[serde(default, deserialize_with = "lenient")]
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Extension {
    pub id: String,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Command {
    pub trigger: String,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CommandResult {
    #[serde(deserialize_with = "lenient", rename = "type")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub trigger: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub title: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub html: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub page: Option<u64>,
    #[serde(deserialize_with = "lenient")]
    pub total_pages: Option<u64>,
}

/// What to search for. Fields left `None` are not sent.
#[derive(Clone, Debug, Default)]
pub struct SearchQuery {
    pub query: String,
    pub kind: Option<String>,
    pub page: Option<u32>,
    pub time: Option<String>,
    pub lang: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub engines: Vec<String>,
    pub safe_mode: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_parsing() {
        let raw = serde_json::json!({
            "results": [
                {"title": "A", "url": "https://a.com", "snippet": null, "source": "x", "score": "high"},
                "garbage",
                {"title": 5, "url": "https://b.com", "sources": ["x", "y"]}
            ],
            "engineTimings": {"oops": true},
            "relatedSearches": ["r"],
            "totalTime": 12
        });
        let parsed: SearchResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].score, None);
        assert_eq!(parsed.results[1].title, None);
        assert_eq!(parsed.results[1].sources, vec!["x", "y"]);
        assert!(parsed.engine_timings.is_empty());
        assert_eq!(parsed.related_searches, vec!["r"]);
    }
}
