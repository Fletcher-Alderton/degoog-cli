//! Configuration: defaults, then `config.toml`, then environment variables,
//! then command-line flags. Later sources win.
//!
//! Loading is forgiving on purpose, like the upstream sidecar: a value that
//! does not parse falls back to its default with a warning, instead of
//! refusing to start.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const DEFAULT_DEGOOG_URL: &str = "http://localhost:4444";
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

pub mod env {
    pub const CONFIG: &str = "DEGOOG_CONFIG";
    pub const URL: &str = "DEGOOG_URL";
    pub const API_KEY: &str = "DEGOOG_API_KEY";
    pub const LOG_LEVEL: &str = "DEGOOG_LOG_LEVEL";
    pub const USER_AGENT: &str = "DEGOOG_USER_AGENT";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMode {
    Compact,
    Balanced,
    Full,
    StructuredOnly,
}

impl OutputMode {
    pub fn wants_text(self) -> bool {
        self != OutputMode::StructuredOnly
    }
    pub fn wants_detail(self) -> bool {
        matches!(self, OutputMode::Full | OutputMode::Balanced)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextMode {
    Compact,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryExpansion {
    Off,
    Suggestions,
    Related,
}

impl QueryExpansion {
    pub fn as_str(self) -> &'static str {
        match self {
            QueryExpansion::Off => "off",
            QueryExpansion::Suggestions => "suggestions",
            QueryExpansion::Related => "related",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DegoogConfig {
    /// Base URL of the remote Degoog instance.
    pub url: String,
    /// Sent as `Authorization: Bearer`. Prefer `DEGOOG_API_KEY` over the file.
    pub api_key: String,
    /// Per-request timeout, milliseconds.
    pub timeout: u64,
}

impl Default for DegoogConfig {
    fn default() -> Self {
        Self { url: DEFAULT_DEGOOG_URL.into(), api_key: String::new(), timeout: 15_000 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub mode: OutputMode,
    pub guidance: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self { mode: OutputMode::Compact, guidance: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    pub max_results: usize,
    pub snippet_chars: usize,
    pub text_mode: TextMode,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { max_results: 8, snippet_chars: 220, text_mode: TextMode::Compact }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FetcherConfig {
    /// Empty means the built-in static fetch. Supports `{{url}}` and `{{timeout}}`.
    pub url: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
    /// Dot path to the html inside a JSON reply. Empty when the body is the html.
    pub html: String,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: "GET".into(),
            headers: BTreeMap::new(),
            body: String::new(),
            html: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScrapeConfig {
    pub fetcher: FetcherConfig,
    pub max_urls: usize,
    pub concurrency: usize,
    pub timeout: u64,
    pub max_response_bytes: usize,
    pub max_chars_per_url: usize,
    pub max_chunks_per_url: usize,
    pub chunk_chars: usize,
    pub allow_private_ips: bool,
    pub hide_images: bool,
    pub max_evidence_chars: usize,
    pub text_mode: TextMode,
    /// Empty means the built-in browser-like user agent.
    pub user_agent: String,
}

impl Default for ScrapeConfig {
    fn default() -> Self {
        Self {
            fetcher: FetcherConfig::default(),
            max_urls: 4,
            concurrency: 4,
            timeout: 15_000,
            max_response_bytes: 2_097_152,
            max_chars_per_url: 3000,
            max_chunks_per_url: 4,
            chunk_chars: 700,
            allow_private_ips: false,
            hide_images: false,
            max_evidence_chars: 8000,
            text_mode: TextMode::Compact,
            user_agent: String::new(),
        }
    }
}

impl ScrapeConfig {
    pub fn renderer(&self) -> &'static str {
        if self.fetcher.url.is_empty() {
            "static"
        } else {
            "delegated"
        }
    }

    pub fn user_agent(&self) -> &str {
        if self.user_agent.is_empty() {
            DEFAULT_USER_AGENT
        } else {
            &self.user_agent
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BundleSearchConfig {
    pub max_queries: usize,
    pub query_expansion: QueryExpansion,
    pub max_expanded_queries: usize,
    pub max_search_results_per_query: usize,
    pub max_scrape_urls: usize,
    /// Extra sources attempted in the first wave to absorb scrape failures.
    pub scrape_overage: usize,
    /// Hard ceiling on scrape attempts per bundle call.
    pub max_scrape_attempts: usize,
    pub max_evidence_chars: usize,
    pub include_raw_results: bool,
    pub text_mode: TextMode,
    pub timeout: u64,
}

impl Default for BundleSearchConfig {
    fn default() -> Self {
        Self {
            max_queries: 8,
            query_expansion: QueryExpansion::Off,
            max_expanded_queries: 2,
            max_search_results_per_query: 8,
            max_scrape_urls: 4,
            scrape_overage: 3,
            max_scrape_attempts: 10,
            max_evidence_chars: 8000,
            include_raw_results: false,
            text_mode: TextMode::Compact,
            timeout: 90_000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub enabled: bool,
    pub ttl: u64,
    pub max_entries: usize,
    pub max_bytes: usize,
    /// Keep the cache on disk between CLI runs.
    pub persist: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self { enabled: true, ttl: 1_800_000, max_entries: 512, max_bytes: 67_108_864, persist: true }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub degoog: DegoogConfig,
    pub output: OutputConfig,
    pub search: SearchConfig,
    pub scrape: ScrapeConfig,
    pub bundle_search: BundleSearchConfig,
    pub cache: CacheConfig,
}

/// Values that override the file, usually from command-line flags.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub config_path: Option<PathBuf>,
    pub url: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub file_found: bool,
    pub warnings: Vec<String>,
}

fn read_env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// `$XDG_CONFIG_HOME/degoog/config.toml`, else `~/.config/degoog/config.toml`.
pub fn default_config_path() -> PathBuf {
    if let Some(xdg) = read_env("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("degoog").join("config.toml");
    }
    if cfg!(windows) {
        if let Some(dir) = dirs::config_dir() {
            return dir.join("degoog").join("config.toml");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("degoog")
        .join("config.toml")
}

pub fn default_cache_path() -> Option<PathBuf> {
    if let Some(xdg) = read_env("XDG_CACHE_HOME") {
        return Some(PathBuf::from(xdg).join("degoog").join("cache.json"));
    }
    if cfg!(windows) {
        return dirs::cache_dir().map(|d| d.join("degoog").join("cache.json"));
    }
    dirs::home_dir().map(|h| h.join(".cache").join("degoog").join("cache.json"))
}

pub fn resolve_path(overrides: &Overrides) -> PathBuf {
    overrides
        .config_path
        .clone()
        .or_else(|| read_env(env::CONFIG).map(PathBuf::from))
        .unwrap_or_else(default_config_path)
}

static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\$|\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}").unwrap());

/// Expand `${VAR}`, `${VAR:-default}` and `$$` in a config string.
pub fn expand_env_vars(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    TOKEN
        .replace_all(value, |caps: &regex::Captures<'_>| {
            if &caps[0] == "$$" {
                return "$".to_string();
            }
            match lookup(&caps[1]).filter(|v| !v.is_empty()) {
                Some(found) => found,
                None => caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default(),
            }
        })
        .into_owned()
}

fn expand_deep(value: Value, lookup: &dyn Fn(&str) -> Option<String>) -> Value {
    match value {
        Value::String(s) => Value::String(expand_env_vars(&s, lookup)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| expand_deep(v, lookup)).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, expand_deep(v, lookup))).collect()),
        other => other,
    }
}

/// Paths treated as one value instead of being walked into.
const OPAQUE: &[&[&str]] = &[&["scrape", "fetcher", "headers"]];

fn leaves(value: &Value, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, Value)>) {
    let opaque = OPAQUE.iter().any(|p| p.iter().eq(path.iter()));
    match value {
        Value::Object(map) if !opaque && !map.is_empty() => {
            for (key, nested) in map {
                path.push(key.clone());
                leaves(nested, path, out);
                path.pop();
            }
        }
        Value::Object(map) if opaque => {
            // Header values may be written as numbers, keep them as strings.
            let coerced: Map<String, Value> = map
                .iter()
                .map(|(k, v)| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), Value::String(s))
                })
                .collect();
            out.push((path.clone(), Value::Object(coerced)));
        }
        other => out.push((path.clone(), other.clone())),
    }
}

fn set_path(root: &mut Value, path: &[String], value: Value) {
    let mut node = root;
    for key in &path[..path.len() - 1] {
        if !node.get(key).is_some_and(Value::is_object) {
            node[key] = Value::Object(Map::new());
        }
        node = node.get_mut(key).unwrap();
    }
    node[path[path.len() - 1].as_str()] = value;
}

/// Strings coming out of `${VAR}` expansion may stand for numbers or bools.
fn coerced(value: &Value) -> Option<Value> {
    let s = value.as_str()?.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Some(Value::from(n));
    }
    match s.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(Value::Bool(true)),
        "false" | "no" | "off" | "0" => Some(Value::Bool(false)),
        _ => None,
    }
}

/// Merge a parsed file over the defaults one leaf at a time, keeping the
/// default for every leaf that does not fit the schema.
pub fn merge(raw: Value, lookup: &dyn Fn(&str) -> Option<String>) -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    let mut current = serde_json::to_value(Config::default()).expect("defaults serialize");

    let mut found = Vec::new();
    leaves(&expand_deep(raw, lookup), &mut Vec::new(), &mut found);

    for (path, value) in found {
        if path.is_empty() {
            continue;
        }
        let dotted = path.join(".");
        let mut attempt = |candidate: Value| -> bool {
            let mut next = current.clone();
            set_path(&mut next, &path, candidate);
            if serde_json::from_value::<Config>(next.clone()).is_ok() {
                current = next;
                true
            } else {
                false
            }
        };
        if attempt(value.clone()) {
            continue;
        }
        if let Some(alt) = coerced(&value) {
            if attempt(alt) {
                continue;
            }
        }
        warnings.push(format!("ignoring {dotted} = {value}: unknown key or invalid value, using the default"));
    }

    let config = serde_json::from_value::<Config>(current).unwrap_or_default();
    (sanitize(config, &mut warnings), warnings)
}

/// Zero is never a sensible cap or timeout, fall back to the default.
fn sanitize(mut c: Config, warnings: &mut Vec<String>) -> Config {
    let d = Config::default();
    macro_rules! positive {
        ($($field:ident).+) => {
            if c.$($field).+ == 0 {
                warnings.push(format!("{} must be positive, using the default", stringify!($($field).+).replace(' ', "")));
                c.$($field).+ = d.$($field).+;
            }
        };
    }
    positive!(degoog.timeout);
    positive!(search.max_results);
    positive!(search.snippet_chars);
    positive!(scrape.max_urls);
    positive!(scrape.concurrency);
    positive!(scrape.timeout);
    positive!(scrape.max_response_bytes);
    positive!(scrape.max_chars_per_url);
    positive!(scrape.max_chunks_per_url);
    positive!(scrape.chunk_chars);
    positive!(scrape.max_evidence_chars);
    positive!(bundle_search.max_queries);
    positive!(bundle_search.max_expanded_queries);
    positive!(bundle_search.max_search_results_per_query);
    positive!(bundle_search.max_scrape_urls);
    positive!(bundle_search.max_scrape_attempts);
    positive!(bundle_search.max_evidence_chars);
    positive!(bundle_search.timeout);
    positive!(cache.ttl);
    positive!(cache.max_entries);
    positive!(cache.max_bytes);

    c.degoog.url = c.degoog.url.trim().trim_end_matches('/').to_string();
    c.degoog.api_key = c.degoog.api_key.trim().to_string();
    c.scrape.fetcher.url = c.scrape.fetcher.url.trim().to_string();
    c.scrape.fetcher.html = c.scrape.fetcher.html.trim().to_string();
    c.scrape.fetcher.method = c.scrape.fetcher.method.trim().to_ascii_uppercase();
    if c.scrape.fetcher.method.is_empty() {
        c.scrape.fetcher.method = "GET".into();
    }
    c
}

fn toml_to_json(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect()),
    }
}

/// Parse TOML text into a config. Exposed for tests.
pub fn from_toml_str(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> (Config, Vec<String>) {
    match text.parse::<toml::Table>() {
        Ok(table) => merge(toml_to_json(toml::Value::Table(table)), lookup),
        Err(err) => (
            Config::default(),
            vec![format!("config is not valid TOML, using defaults: {}", err.message())],
        ),
    }
}

pub fn load(overrides: &Overrides) -> Loaded {
    let path = resolve_path(overrides);
    let lookup = |name: &str| std::env::var(name).ok();

    let (mut config, mut warnings, file_found) = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (config, warnings) = from_toml_str(&text, &lookup);
            (config, warnings, true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Config::default(), Vec::new(), false),
        Err(err) => (
            Config::default(),
            vec![format!("could not read {}: {err}, using defaults", path.display())],
            false,
        ),
    };

    if let Some(url) = read_env(env::URL) {
        config.degoog.url = url;
    }
    if let Some(key) = read_env(env::API_KEY) {
        config.degoog.api_key = key;
    }
    if let Some(ua) = read_env(env::USER_AGENT) {
        config.scrape.user_agent = ua;
    }
    if let Some(url) = overrides.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        config.degoog.url = url.to_string();
    }
    if let Some(key) = overrides.api_key.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        config.degoog.api_key = key.to_string();
    }
    config.degoog.url = config.degoog.url.trim().trim_end_matches('/').to_string();

    if url::Url::parse(&config.degoog.url).is_err() {
        warnings.push(format!("degoog.url {:?} is not a valid URL", config.degoog.url));
    }

    Loaded { config, path, file_found, warnings }
}

pub const TEMPLATE: &str = r#"# degoog-cli configuration.
# Precedence: built-in defaults < this file < environment < command-line flags.
# Any string accepts ${VAR} and ${VAR:-default}, with $$ for a literal $.
# Timeout and ttl values are milliseconds.

[degoog]
url = "{{url}}" # your remote Degoog instance, DEGOOG_URL or --url win over this
api_key = "${DEGOOG_API_KEY:-}" # sent as a bearer token, keep the real key in the environment
timeout = 15000

[output]
mode = "compact" # compact | balanced | full | structured-only (affects MCP visible text)
guidance = true # false drops the lines that tell a model what to do next

[search]
max_results = 8
snippet_chars = 220
text_mode = "compact" # compact | full, for MCP clients that ignore structuredContent

[scrape]
max_urls = 4
concurrency = 4
timeout = 15000
max_response_bytes = 2097152
max_chars_per_url = 3000
max_chunks_per_url = 4
chunk_chars = 700
allow_private_ips = false # pages are fetched from this machine, so this guards your LAN
hide_images = false # true drops image alt/aria-label/title text
max_evidence_chars = 8000
text_mode = "compact" # compact | full
user_agent = "" # empty uses a browser-like default, DEGOOG_USER_AGENT wins

[scrape.fetcher] # hand the page fetch to your own http service (e.g. FlareSolverr)
url = "" # empty means the built-in static fetch, supports {{url}} and {{timeout}}
method = "GET"
body = "" # supports {{url}} and {{timeout}}, e.g. '{"cmd":"request.get","url":"{{url}}","maxTimeout":{{timeout}}}'
html = "" # dot path to the html in a json reply, empty when the body is the html (e.g. "solution.response")

[scrape.fetcher.headers]
# "Content-Type" = "application/json"

[bundle_search]
max_queries = 8
query_expansion = "off" # off | suggestions | related
max_expanded_queries = 2
max_search_results_per_query = 8
max_scrape_urls = 4
scrape_overage = 3 # extra sources attempted in the first wave to absorb scrape failures
max_scrape_attempts = 10 # hard ceiling on scrape attempts per bundle call
max_evidence_chars = 8000
include_raw_results = false
text_mode = "compact" # compact | full
timeout = 90000

[cache]
enabled = true
ttl = 1800000
max_entries = 512
max_bytes = 67108864
persist = true # keep the cache on disk between runs
"#;

pub fn render_template(url: &str) -> String {
    TEMPLATE.replace("{{url}}", &url.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Write a fresh config file. Refuses to overwrite unless `force`.
pub fn write_template(path: &Path, url: &str, force: bool) -> anyhow::Result<()> {
    if path.exists() && !force {
        anyhow::bail!("{} already exists, pass --force to overwrite it", path.display());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, render_template(url))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn template_parses_cleanly() {
        let (config, warnings) = from_toml_str(&render_template("https://search.example.com/"), &no_env);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.degoog.url, "https://search.example.com");
        assert_eq!(config.search.max_results, 8);
        assert_eq!(config.bundle_search.query_expansion, QueryExpansion::Off);
    }

    #[test]
    fn bad_leaves_fall_back_individually() {
        let text = r#"
            [search]
            max_results = "lots"
            snippet_chars = 300
            [output]
            mode = "loud"
            [nope]
            x = 1
            [scrape]
            concurrency = 0
        "#;
        let (config, warnings) = from_toml_str(text, &no_env);
        assert_eq!(config.search.max_results, 8);
        assert_eq!(config.search.snippet_chars, 300);
        assert_eq!(config.output.mode, OutputMode::Compact);
        assert_eq!(config.scrape.concurrency, 4);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
    }

    #[test]
    fn expands_env_and_coerces() {
        let lookup = |name: &str| match name {
            "N" => Some("12".to_string()),
            "KEY" => Some("secret".to_string()),
            _ => None,
        };
        let text = r#"
            [degoog]
            api_key = "${KEY}"
            url = "${MISSING:-http://h:1}"
            [search]
            max_results = "${N}"
            [cache]
            enabled = "${NOPE:-off}"
            [scrape.fetcher.headers]
            X-Num = 5
        "#;
        let (config, warnings) = from_toml_str(text, &lookup);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.degoog.api_key, "secret");
        assert_eq!(config.degoog.url, "http://h:1");
        assert_eq!(config.search.max_results, 12);
        assert!(!config.cache.enabled);
        assert_eq!(config.scrape.fetcher.headers["X-Num"], "5");
    }

    #[test]
    fn dollar_escape() {
        assert_eq!(expand_env_vars("a$$b ${X:-y}", &no_env), "a$b y");
    }
}
