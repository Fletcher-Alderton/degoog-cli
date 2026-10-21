//! SSRF guard. Scraping runs on the user's own machine, so by default it
//! refuses to reach loopback, LAN and other non-public addresses.

use crate::urls::http_url;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::Host;

pub const BAD_SCHEME: &str = "url must use http or https";
pub const BAD_URL: &str = "url could not be parsed";

#[derive(Debug)]
pub struct HostCheck {
    pub ok: bool,
    pub detail: Option<String>,
    pub addresses: Vec<IpAddr>,
}

impl HostCheck {
    fn pass(addresses: Vec<IpAddr>) -> Self {
        Self { ok: true, detail: None, addresses }
    }
    fn block(detail: String, addresses: Vec<IpAddr>) -> Self {
        Self { ok: false, detail: Some(detail), addresses }
    }
}

pub fn private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    a == 0
        || a == 10
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || (a == 100 && (64..=127).contains(&b))
        || a >= 224
}

pub fn private_v6(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() {
        return true;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return private_v4(v4);
    }
    let first = ip.segments()[0];
    (first & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (first & 0xff00) == 0xff00 // multicast ff00::/8
}

pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => private_v4(v4),
        IpAddr::V6(v6) => private_v6(v6),
    }
}

async fn resolve(host: &Host<&str>, port: u16) -> std::io::Result<Vec<IpAddr>> {
    match host {
        Host::Ipv4(ip) => Ok(vec![IpAddr::V4(*ip)]),
        Host::Ipv6(ip) => Ok(vec![IpAddr::V6(*ip)]),
        Host::Domain(name) => Ok(tokio::net::lookup_host((*name, port)).await?.map(|a| a.ip()).collect()),
    }
}

pub async fn check_host(raw: &str, allow_private: bool) -> HostCheck {
    let Some(url) = http_url(raw) else {
        let reason = if raw.contains("://") { BAD_SCHEME } else { BAD_URL };
        return HostCheck::block(reason.into(), Vec::new());
    };
    if allow_private {
        return HostCheck::pass(Vec::new());
    }
    let Some(host) = url.host() else {
        return HostCheck::block(BAD_URL.into(), Vec::new());
    };
    let name = url.host_str().unwrap_or_default().to_string();
    let no_dns = || format!("{name} did not resolve, DNS lookup failed");

    let addresses = match resolve(&host, url.port_or_known_default().unwrap_or(80)).await {
        Ok(found) if !found.is_empty() => found,
        _ => return HostCheck::block(no_dns(), Vec::new()),
    };

    let blocked: Vec<String> = addresses.iter().filter(|ip| is_private_ip(**ip)).map(|ip| ip.to_string()).collect();
    if !blocked.is_empty() {
        return HostCheck::block(
            format!(
                "{name} resolves to a private or local address ({}), blocked by the SSRF guard. \
                 Set scrape.allow_private_ips = true in config.toml only if you meant to reach an internal host.",
                blocked.join(", ")
            ),
            addresses,
        );
    }
    HostCheck::pass(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> bool {
        is_private_ip(s.parse().unwrap())
    }

    #[test]
    fn classifies_addresses() {
        for private in ["127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.1", "169.254.1.1", "100.64.0.1", "0.0.0.0",
            "224.0.0.1", "::1", "::", "fe80::1", "fd00::1", "ff02::1", "::ffff:127.0.0.1"] {
            assert!(ip(private), "{private} should be private");
        }
        for public in ["8.8.8.8", "172.32.0.1", "1.1.1.1", "2606:4700::1111", "::ffff:8.8.8.8"] {
            assert!(!ip(public), "{public} should be public");
        }
    }

    #[tokio::test]
    async fn blocks_literals_and_bad_urls() {
        assert!(!check_host("http://127.0.0.1/", false).await.ok);
        assert!(!check_host("http://[::1]:8080/", false).await.ok);
        assert!(check_host("http://127.0.0.1/", true).await.ok);
        assert_eq!(check_host("ftp://x.com", false).await.detail.as_deref(), Some(BAD_SCHEME));
        assert_eq!(check_host("nope", false).await.detail.as_deref(), Some(BAD_URL));
        assert!(check_host("http://8.8.8.8/", false).await.ok);
    }
}
