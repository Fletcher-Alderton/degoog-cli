use crate::urls::redact_url;
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::{Duration, Instant};

const LOG_NS: &str = "degoog";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Upstream,
    Auth,
    Timeout,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Upstream => "upstream",
            ErrorKind::Auth => "auth",
            ErrorKind::Timeout => "timeout",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DegoogError {
    pub message: String,
    pub status: Option<u16>,
    pub kind: ErrorKind,
}

impl std::fmt::Display for DegoogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DegoogError {}

impl DegoogError {
    pub fn http(path: &str, status: u16) -> Self {
        let detail = if status == 401 { "authentication rejected" } else { "request failed" };
        let kind = if status == 401 || status == 403 { ErrorKind::Auth } else { ErrorKind::Upstream };
        Self { message: format!("Degoog {path} {detail} (HTTP {status})"), status: Some(status), kind }
    }

    pub fn transport(path: &str, err: &reqwest::Error, timeout_ms: u64) -> Self {
        if err.is_timeout() {
            return Self {
                message: format!("Degoog {path} timed out after {timeout_ms}ms"),
                status: None,
                kind: ErrorKind::Timeout,
            };
        }
        Self {
            message: format!("Degoog {path} unreachable: {}", root_cause(err)),
            status: None,
            kind: ErrorKind::Upstream,
        }
    }
}

/// Deepest error in a chain, which is usually the useful one
/// ("connection refused", "dns error: ...").
pub fn root_cause(err: &(dyn std::error::Error + 'static)) -> String {
    let mut current = err;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}

pub struct Request<'a> {
    pub path: &'a str,
    pub query: Vec<(&'a str, String)>,
    pub body: Option<Value>,
    pub method: Method,
    pub timeout_ms: Option<u64>,
}

impl<'a> Request<'a> {
    pub fn get(path: &'a str) -> Self {
        Self { path, query: Vec::new(), body: None, method: Method::GET, timeout_ms: None }
    }

    pub fn post(path: &'a str, body: Value) -> Self {
        Self { path, query: Vec::new(), body: Some(body), method: Method::POST, timeout_ms: None }
    }

    /// Add a query parameter, skipping empty values.
    pub fn param(mut self, key: &'a str, value: Option<impl ToString>) -> Self {
        if let Some(value) = value.map(|v| v.to_string()).filter(|v| !v.is_empty()) {
            self.query.push((key, value));
        }
        self
    }

    pub fn timeout(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

pub struct Client {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
    timeout_ms: u64,
}

impl Client {
    pub fn new(base_url: &str, api_key: &str, timeout_ms: u64) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("degoog-cli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_millis(timeout_ms.min(10_000)))
            .build()?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            http,
            timeout_ms,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn has_api_key(&self) -> bool {
        !self.api_key.is_empty()
    }

    pub fn build_url(&self, req: &Request<'_>) -> Result<url::Url, DegoogError> {
        let mut url = url::Url::parse(&format!("{}{}", self.base_url, req.path)).map_err(|err| DegoogError {
            message: format!("degoog url {:?} is invalid: {err}", self.base_url),
            status: None,
            kind: ErrorKind::Upstream,
        })?;
        if !req.query.is_empty() {
            url.query_pairs_mut().extend_pairs(req.query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        Ok(url)
    }

    pub async fn raw(&self, req: &Request<'_>) -> Result<reqwest::Response, DegoogError> {
        let url = self.build_url(req)?;
        let timeout = req.timeout_ms.unwrap_or(self.timeout_ms);
        let mut builder = self
            .http
            .request(req.method.clone(), url.clone())
            .header("Accept", "application/json")
            .timeout(Duration::from_millis(timeout));
        if !self.api_key.is_empty() {
            builder = builder.bearer_auth(&self.api_key);
        }
        if let Some(body) = &req.body {
            builder = builder.json(body);
        }

        let started = Instant::now();
        let response = builder.send().await.map_err(|err| DegoogError::transport(req.path, &err, timeout))?;
        crate::debug!(
            LOG_NS,
            "{} {} status={} ms={}",
            req.method,
            redact_url(url.as_str()),
            response.status().as_u16(),
            started.elapsed().as_millis()
        );
        Ok(response)
    }

    pub async fn json<T: DeserializeOwned>(&self, req: Request<'_>) -> Result<T, DegoogError> {
        let response = self.raw(&req).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(DegoogError::http(req.path, status.as_u16()));
        }
        let timeout = req.timeout_ms.unwrap_or(self.timeout_ms);
        let bytes = response.bytes().await.map_err(|err| DegoogError::transport(req.path, &err, timeout))?;
        serde_json::from_slice(&bytes).map_err(|err| {
            crate::warn!(LOG_NS, "invalid JSON from {}: {err}", req.path);
            DegoogError {
                message: format!("Degoog {} returned invalid JSON", req.path),
                status: Some(502),
                kind: ErrorKind::Upstream,
            }
        })
    }
}
