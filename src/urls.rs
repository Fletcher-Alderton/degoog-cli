//! URL normalisation shared by search dedupe and scraping.

use url::Url;

const TRACKING_PREFIXES: &[&str] = &["utm_", "ga_", "mc_", "pk_", "hsa_"];
const TRACKING_KEYS: &[&str] = &[
    "fbclid", "gclid", "dclid", "msclkid", "igshid", "mkt_tok", "ref", "ref_src", "referrer",
    "spm", "yclid", "_hsenc", "_hsmi",
];
const SECRET_HINTS: &[&str] = &["authorization", "api_key", "apikey", "token", "secret", "password"];

/// Parse `raw` and return it only when the scheme is http or https.
pub fn http_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

pub fn is_http_url(raw: &str) -> bool {
    http_url(raw).is_some()
}

fn is_tracking(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    TRACKING_KEYS.contains(&lower.as_str()) || TRACKING_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Canonical form used to merge duplicate results: no fragment, no `www.`,
/// no tracking params, sorted query, no trailing slash, no default port.
pub fn canonical_url(raw: &str) -> String {
    let Some(mut url) = http_url(raw) else {
        return raw.trim().to_string();
    };

    url.set_fragment(None);
    if let Some(host) = url.host_str() {
        let lowered = host.to_ascii_lowercase();
        let stripped = lowered.strip_prefix("www.").unwrap_or(&lowered).to_string();
        if stripped != host {
            let _ = url.set_host(Some(&stripped));
        }
    }

    if url.query().is_some() {
        let mut pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| !is_tracking(k))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        // Stable sort by key, same as URLSearchParams.sort().
        pairs.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        if pairs.is_empty() {
            url.set_query(None);
        } else {
            url.query_pairs_mut().clear().extend_pairs(pairs);
        }
    }

    let path = url.path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        let trimmed = path.trim_end_matches('/');
        url.set_path(if trimmed.is_empty() { "/" } else { trimmed });
    }

    // The url crate already drops default ports on parse.
    url.to_string()
}

/// Hostname without a leading `www.`.
pub fn domain_of(raw: &str) -> String {
    http_url(raw)
        .and_then(|u| u.host_str().map(|h| h.strip_prefix("www.").unwrap_or(h).to_string()))
        .unwrap_or_default()
}

/// Last two labels of the hostname. Crude, but it is only used to spread
/// results across sites.
pub fn root_domain(raw: &str) -> String {
    let host = domain_of(raw);
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() > 2 {
        parts[parts.len() - 2..].join(".")
    } else {
        host
    }
}

pub fn looks_secret(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_HINTS.iter().any(|hint| lower.contains(hint))
}

/// Remove credentials and secret-looking query values before logging.
pub fn redact_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_string();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    if url.query().is_some() {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| {
                let value = if looks_secret(&k) { "***".to_string() } else { v.into_owned() };
                (k.into_owned(), value)
            })
            .collect();
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalises() {
        assert_eq!(
            canonical_url("https://WWW.Example.com:443/a/b/?utm_source=x&z=1&a=2#frag"),
            "https://example.com/a/b?a=2&z=1"
        );
        assert_eq!(canonical_url("http://example.com/?fbclid=1"), "http://example.com/");
        assert_eq!(canonical_url("ftp://example.com/x"), "ftp://example.com/x");
    }

    #[test]
    fn domains() {
        assert_eq!(domain_of("https://www.docs.rust-lang.org/x"), "docs.rust-lang.org");
        assert_eq!(root_domain("https://docs.rust-lang.org/x"), "rust-lang.org");
        assert_eq!(domain_of("not a url"), "");
    }

    #[test]
    fn redacts() {
        assert_eq!(
            redact_url("https://user:pw@h.com/p?api_key=abc&q=1"),
            "https://h.com/p?api_key=***&q=1"
        );
    }
}
