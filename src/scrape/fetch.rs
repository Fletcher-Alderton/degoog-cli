//! Page fetching: the built-in static fetch, or a delegated HTTP service.

use super::safety::check_host;
use crate::config::{FetcherConfig, ScrapeConfig};
use crate::degoog::client::root_cause;
use futures::StreamExt;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::Value;
use std::time::Duration;

const LOG_NS: &str = "scrape-fetch";
const MAX_REDIRECTS: usize = 5;
const TEXT_TYPES: &[&str] = &["text/html", "application/xhtml", "text/plain", "application/xml", "text/xml"];
pub const URL_SLOT: &str = "{{url}}";
pub const TIMEOUT_SLOT: &str = "{{timeout}}";

/// encodeURIComponent: everything but A-Z a-z 0-9 - _ . ! ~ * ' ( )
const COMPONENT: &AsciiSet =
    &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'!').remove(b'~').remove(b'*').remove(b'\'').remove(b'(').remove(b')');

#[derive(Clone, Debug, Default)]
pub struct FetchOutcome {
    pub ok: bool,
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub html: String,
    pub bytes: usize,
    pub truncated: bool,
    pub redirects: Vec<String>,
    pub error: Option<String>,
}

pub fn failed(url: &str, error: impl Into<String>, status: u16) -> FetchOutcome {
    FetchOutcome { ok: false, url: url.to_string(), status, error: Some(error.into()), ..Default::default() }
}

/// HTTP clients for scraping. The direct one follows redirects by hand so
/// every hop goes through the SSRF check.
pub struct Fetchers {
    direct: reqwest::Client,
    delegated: reqwest::Client,
}

impl Fetchers {
    pub fn new(config: &ScrapeConfig) -> anyhow::Result<Self> {
        let direct = reqwest::Client::builder()
            .user_agent(config.user_agent())
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let delegated = reqwest::Client::builder()
            .user_agent(concat!("degoog-cli/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;
        Ok(Self { direct, delegated })
    }
}

pub fn brief_error(err: &reqwest::Error) -> String {
    let cause = root_cause(err);
    let cause = cause.split("For more information,").next().unwrap_or("").trim();
    if cause.is_empty() {
        "fetch failed".into()
    } else {
        format!("fetch failed: {cause}")
    }
}

struct Body {
    text: String,
    bytes: usize,
    truncated: bool,
}

async fn read_capped(response: reqwest::Response, max_bytes: usize) -> Result<Body, reqwest::Error> {
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let room = max_bytes.saturating_sub(buf.len());
        if chunk.len() > room {
            buf.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    let bytes = buf.len();
    Ok(Body { text: String::from_utf8_lossy(&buf).into_owned(), bytes, truncated })
}

fn is_text_type(content_type: &str) -> bool {
    content_type.is_empty() || TEXT_TYPES.iter().any(|t| content_type.contains(t))
}

async fn with_timeout(ms: u64, url: &str, fut: impl std::future::Future<Output = FetchOutcome>) -> FetchOutcome {
    match tokio::time::timeout(Duration::from_millis(ms), fut).await {
        Ok(outcome) => outcome,
        Err(_) => failed(url, format!("timed out after {ms}ms"), 0),
    }
}

pub async fn fetch_page(fetchers: &Fetchers, target: &str, config: &ScrapeConfig) -> FetchOutcome {
    let mut redirects = Vec::new();
    let mut current = target.to_string();

    for _ in 0..=MAX_REDIRECTS {
        let check = check_host(&current, config.allow_private_ips).await;
        if !check.ok {
            return FetchOutcome { redirects, ..failed(&current, check.detail.unwrap_or_else(|| "blocked".into()), 0) };
        }

        let hop = current.clone();
        let outcome = with_timeout(config.timeout, &hop, async {
            let response = match fetchers
                .direct
                .get(&hop)
                .header("Accept", "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5")
                .header("Accept-Language", "en;q=0.9")
                .send()
                .await
            {
                Ok(r) => r,
                Err(err) => {
                    crate::debug!(LOG_NS, "fetch failed for {hop}: {err}");
                    return failed(&hop, brief_error(&err), 0);
                }
            };
            let status = response.status().as_u16();

            if response.status().is_redirection() {
                if let Some(location) = response.headers().get("location").and_then(|v| v.to_str().ok()) {
                    return match url::Url::parse(&hop).and_then(|base| base.join(location)) {
                        // Signal the redirect with ok=false, status 3xx and the next URL.
                        Ok(next) => FetchOutcome { url: next.to_string(), status, ..Default::default() },
                        Err(_) => failed(&hop, "invalid redirect target", status),
                    };
                }
            }

            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !response.status().is_success() {
                return failed(&hop, format!("HTTP {status}"), status);
            }
            if !is_text_type(&content_type) {
                let shown = if content_type.is_empty() { "unknown" } else { &content_type };
                return failed(&hop, format!("unsupported content type {shown}"), status);
            }
            match read_capped(response, config.max_response_bytes).await {
                Ok(body) => FetchOutcome {
                    ok: true,
                    url: hop.clone(),
                    status,
                    content_type,
                    html: body.text,
                    bytes: body.bytes,
                    truncated: body.truncated,
                    ..Default::default()
                },
                Err(err) => failed(&hop, brief_error(&err), status),
            }
        })
        .await;

        let is_redirect = !outcome.ok && outcome.error.is_none() && (300..400).contains(&outcome.status);
        if is_redirect {
            redirects.push(outcome.url.clone());
            current = outcome.url;
            continue;
        }
        return FetchOutcome { redirects, ..outcome };
    }

    FetchOutcome { redirects, ..failed(&current, "too many redirects", 0) }
}

fn json_slot(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    quoted[1..quoted.len() - 1].to_string()
}

fn component(value: &str) -> String {
    utf8_percent_encode(value, COMPONENT).to_string()
}

fn wants_json(config: &FetcherConfig) -> bool {
    let declared = config
        .headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("content-type") && value.to_ascii_lowercase().contains("json"));
    let body = config.body.trim_start();
    declared || body.starts_with('{') || body.starts_with('[')
}

pub fn fill_slots(template: &str, target: &str, timeout_ms: u64, escape: fn(&str) -> String) -> String {
    template.replace(URL_SLOT, &escape(target)).replace(TIMEOUT_SLOT, &timeout_ms.to_string())
}

fn dig<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').filter(|p| !p.is_empty()).try_fold(value, |node, key| node.get(key))
}

pub const BAD_JSON: &str = "delegated fetcher did not return valid json";
pub const NO_HTML: &str = "delegated fetcher returned no html at the configured path";

pub fn read_html(payload: &str, path: &str) -> Result<String, &'static str> {
    if path.is_empty() {
        return Ok(payload.to_string());
    }
    let parsed: Value = serde_json::from_str(payload).map_err(|_| BAD_JSON)?;
    match dig(&parsed, path) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(NO_HTML),
    }
}

pub async fn fetch_via(fetchers: &Fetchers, target: &str, config: &ScrapeConfig) -> FetchOutcome {
    let fetcher = &config.fetcher;
    let check = check_host(target, config.allow_private_ips).await;
    if !check.ok {
        return failed(target, check.detail.unwrap_or_else(|| "blocked".into()), 0);
    }

    let endpoint = fill_slots(&fetcher.url, target, config.timeout, component);
    let escape: fn(&str) -> String = if wants_json(fetcher) { json_slot } else { component };
    let method = reqwest::Method::from_bytes(fetcher.method.as_bytes()).unwrap_or(reqwest::Method::GET);

    with_timeout(config.timeout, target, async {
        let mut request = fetchers.delegated.request(method, &endpoint);
        for (name, value) in &fetcher.headers {
            request = request.header(name, value);
        }
        if !fetcher.body.is_empty() {
            request = request.body(fill_slots(&fetcher.body, target, config.timeout, escape));
        }
        let response = match request.send().await {
            Ok(r) => r,
            Err(err) => {
                crate::warn!(LOG_NS, "delegated fetcher could not be reached for {target}: {err}");
                return failed(target, brief_error(&err), 0);
            }
        };
        let status = response.status();
        let body = match read_capped(response, config.max_response_bytes).await {
            Ok(b) => b,
            Err(err) => return failed(target, brief_error(&err), status.as_u16()),
        };
        if !status.is_success() {
            return failed(target, format!("delegated fetcher replied HTTP {}", status.as_u16()), status.as_u16());
        }
        match read_html(&body.text, &fetcher.html) {
            Ok(html) => FetchOutcome {
                ok: true,
                url: target.to_string(),
                status: status.as_u16(),
                content_type: "text/html".into(),
                html,
                bytes: body.bytes,
                truncated: body.truncated,
                ..Default::default()
            },
            Err(reason) => failed(target, reason, status.as_u16()),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_escaping() {
        let target = "https://x.com/a b?q=\"1\"&r=2";
        assert_eq!(
            fill_slots("http://f/?u={{url}}&t={{timeout}}", target, 900, component),
            "http://f/?u=https%3A%2F%2Fx.com%2Fa%20b%3Fq%3D%221%22%26r%3D2&t=900"
        );
        assert_eq!(
            fill_slots(r#"{"url":"{{url}}","maxTimeout":{{timeout}}}"#, target, 900, json_slot),
            r#"{"url":"https://x.com/a b?q=\"1\"&r=2","maxTimeout":900}"#
        );
    }

    #[test]
    fn reads_html_paths() {
        assert_eq!(read_html("<p>x</p>", "").unwrap(), "<p>x</p>");
        assert_eq!(read_html(r#"{"solution":{"response":"<p>y</p>"}}"#, "solution.response").unwrap(), "<p>y</p>");
        assert_eq!(read_html("nope", "a").unwrap_err(), BAD_JSON);
        assert_eq!(read_html(r#"{"a":1}"#, "a").unwrap_err(), NO_HTML);
    }
}
