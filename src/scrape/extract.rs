//! Pick the main content of a page and return it as cleaned markdown, plus
//! title, canonical URL, date and site name.

use super::clean::clean_text;
use super::markdown::{text_content, to_markdown, MarkdownOptions};
use crate::text::{char_len, clean_title};
use crate::urls::{canonical_url, http_url};
use ego_tree::NodeId;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use std::sync::LazyLock;

const CONTENT_SELECTORS: &[&str] = &[
    "article", "main", "[role='main']", "[itemprop='articleBody']", "#content", "#main-content", ".post-content",
    ".post-body", ".article-body", ".article-content", ".story-body", ".entry-content", ".markdown-body",
];
const MIN_ROOT_CHARS: usize = 200;
const ROOT_KEEP_SHARE: f64 = 0.85;
const NOISE_MAX_SHARE: f64 = 0.5;
const APP_SHELL_HINTS: &[&str] =
    &["__NEXT_DATA__", "window.__NUXT__", "ng-app", "data-reactroot", "id=\"root\"", "id=\"app\""];
const META_DATE_KEYS: &[&str] = &[
    "article:published_time", "article:modified_time", "og:updated_time", "date", "pubdate", "publish-date", "dc.date",
];

pub const UI_CHROME: &str =
    "script, style, noscript, template, button, input, select, textarea, [role='button'], [aria-hidden='true']";
pub const PAGE_NOISE: &str = "iframe, svg, form, nav, header, footer, aside, [role='navigation'], [role='banner'], \
     [role='contentinfo'], [role='complementary']";

fn sel(s: &str) -> Selector {
    Selector::parse(s).unwrap_or_else(|e| panic!("bad selector {s}: {e:?}"))
}

static UI_CHROME_SEL: LazyLock<Selector> = LazyLock::new(|| sel(UI_CHROME));
static PAGE_NOISE_SEL: LazyLock<Selector> = LazyLock::new(|| sel(PAGE_NOISE));
static CLASS_OR_ID: LazyLock<Selector> = LazyLock::new(|| sel("[class], [id]"));
static BODY_TEXT: LazyLock<Selector> = LazyLock::new(|| sel("p, li, dd, blockquote"));
static CONTENT: LazyLock<Vec<Selector>> = LazyLock::new(|| CONTENT_SELECTORS.iter().map(|s| sel(s)).collect());
static NOISE_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[-_ ])(nav|menu|sidebar|cookie|consent|banner|advert|promo|share|social|comment|related|latest|trending|popular|recirc|recommend|newsletter|subscribe|breadcrumb|paywall|modal|widget|sponsor|taboola|outbrain)([-_ ]|$)",
    )
    .unwrap()
});

#[derive(Clone, Debug, Default)]
pub struct Extraction {
    pub title: String,
    pub canonical: String,
    pub published_at: Option<String>,
    pub site_name: Option<String>,
    pub text: String,
    pub needs_browser: bool,
}

fn first_attr(doc: &Html, selector: &str, attr: &str) -> Option<String> {
    let selector = Selector::parse(selector).ok()?;
    doc.select(&selector).next().and_then(|e| e.attr(attr)).map(String::from)
}

fn meta_value(doc: &Html, keys: &[&str]) -> Option<String> {
    for key in keys {
        let value = first_attr(doc, &format!("meta[property='{key}']"), "content")
            .or_else(|| first_attr(doc, &format!("meta[name='{key}']"), "content"));
        if let Some(v) = value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            return Some(v);
        }
    }
    None
}

fn first_text(doc: &Html, selector: &str) -> Option<String> {
    let selector = Selector::parse(selector).ok()?;
    doc.select(&selector).next().map(|e| text_content(*e))
}

fn pick_title(doc: &Html, fallback: &str) -> String {
    let candidates = [meta_value(doc, &["og:title", "twitter:title"]), first_text(doc, "h1"), first_text(doc, "title")];
    for candidate in candidates.iter().flatten() {
        let cleaned = clean_title(Some(candidate), "");
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    clean_title(Some(fallback), fallback)
}

fn pick_date(doc: &Html) -> Option<String> {
    meta_value(doc, META_DATE_KEYS).or_else(|| {
        first_attr(doc, "time[datetime]", "datetime").map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    })
}

fn pick_canonical(doc: &Html, source_url: &str) -> String {
    let href = first_attr(doc, "link[rel='canonical']", "href").or_else(|| meta_value(doc, &["og:url"])).unwrap_or_default();
    if href.is_empty() {
        return canonical_url(source_url);
    }
    match url::Url::parse(source_url).and_then(|base| base.join(&href)) {
        Ok(joined) => canonical_url(joined.as_str()),
        Err(_) => canonical_url(source_url),
    }
}

fn trimmed_len(s: &str) -> usize {
    char_len(s.trim())
}

/// The tightest content container that still holds most of the body text,
/// or `<body>` when no container is convincing.
fn pick_root(doc: &Html) -> NodeId {
    let body = doc.select(&sel("body")).next().map(|e| e.id()).unwrap_or_else(|| doc.root_element().id());

    struct Candidate {
        id: NodeId,
        body_chars: usize,
        total_chars: usize,
    }
    let mut found = Vec::new();
    for selector in CONTENT.iter() {
        for element in doc.select(selector) {
            let total_chars = trimmed_len(&text_content(*element));
            if total_chars < MIN_ROOT_CHARS {
                continue;
            }
            let body_text: String = element.select(&BODY_TEXT).map(|e| text_content(*e)).collect();
            found.push(Candidate { id: element.id(), body_chars: trimmed_len(&body_text), total_chars });
        }
    }

    let best = found.iter().map(|c| c.body_chars).max().unwrap_or(0);
    if best < MIN_ROOT_CHARS {
        return body;
    }
    let floor = best as f64 * ROOT_KEEP_SHARE;
    found
        .iter()
        .filter(|c| c.body_chars as f64 >= floor)
        .fold(None::<&Candidate>, |tightest, c| match tightest {
            Some(t) if c.total_chars >= t.total_chars => Some(t),
            _ => Some(c),
        })
        .map(|c| c.id)
        .unwrap_or(body)
}

fn detach_matching(doc: &mut Html, root: NodeId, selector: &Selector) {
    let ids: Vec<NodeId> = match doc.tree.get(root).and_then(ElementRef::wrap) {
        Some(el) => el.select(selector).map(|e| e.id()).collect(),
        None => return,
    };
    for id in ids {
        if let Some(mut node) = doc.tree.get_mut(id) {
            node.detach();
        }
    }
}

fn under(doc: &Html, id: NodeId, root: NodeId) -> bool {
    doc.tree.get(id).is_some_and(|n| n.ancestors().any(|a| a.id() == root))
}

/// Drop scripts, chrome, navigation, and small blocks whose class or id
/// looks like page furniture (menus, share bars, comment widgets...).
fn strip_noise(doc: &mut Html, root: NodeId) {
    detach_matching(doc, root, &UI_CHROME_SEL);
    detach_matching(doc, root, &PAGE_NOISE_SEL);

    let Some(root_ref) = doc.tree.get(root) else { return };
    let root_chars = trimmed_len(&text_content(root_ref)).max(1);
    let limit = root_chars as f64 * NOISE_MAX_SHARE;

    let marked: Vec<NodeId> = match ElementRef::wrap(root_ref) {
        Some(el) => el.select(&CLASS_OR_ID).map(|e| e.id()).collect(),
        None => return,
    };
    for id in marked {
        if !under(doc, id, root) {
            continue; // already removed with an ancestor
        }
        let node = doc.tree.get(id).unwrap();
        let element = node.value().as_element().unwrap();
        let marker = format!("{} {}", element.attr("class").unwrap_or(""), element.attr("id").unwrap_or(""));
        if !NOISE_ATTR.is_match(&marker) || trimmed_len(&text_content(node)) as f64 > limit {
            continue;
        }
        doc.tree.get_mut(id).unwrap().detach();
    }
}

pub fn extract(html: &str, source_url: &str, options: MarkdownOptions) -> Extraction {
    let mut doc = Html::parse_document(html);
    let fallback = http_url(source_url).and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| source_url.into());

    let title = pick_title(&doc, &fallback);
    let canonical = pick_canonical(&doc, source_url);
    let published_at = pick_date(&doc);
    let site_name = meta_value(&doc, &["og:site_name", "application-name"]);

    let root = pick_root(&doc);
    strip_noise(&mut doc, root);
    let text = clean_text(&to_markdown(doc.tree.get(root).unwrap(), options));
    let needs_browser = char_len(&text) < 200 && APP_SHELL_HINTS.iter().any(|h| html.contains(h));

    Extraction { title, canonical, published_at, site_name, text, needs_browser }
}

/// Plain text for an HTML fragment, used for bang command output.
pub fn html_to_text(html: &str) -> String {
    let mut doc = Html::parse_document(html);
    let root = doc.root_element().id();
    detach_matching(&mut doc, root, &UI_CHROME_SEL);
    let body = doc.select(&sel("body")).next().map(|e| e.id()).unwrap_or(root);
    clean_text(&to_markdown(doc.tree.get(body).unwrap(), MarkdownOptions::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = r#"<!doctype html><html><head>
      <title>Fallback title</title>
      <meta property="og:title" content="The &amp; Title">
      <meta property="og:site_name" content="Example">
      <meta name="date" content=" 2024-05-01 ">
      <link rel="canonical" href="/canon/?utm_source=x">
    </head><body>
      <nav><a href="/">Home</a> <a href="/a">About</a></nav>
      <header><h1>Site header</h1></header>
      <article>
        <h2>Section one</h2>
        <p>This is the first paragraph of the article and it has enough words in it to count as body text for the extractor heuristics.</p>
        <div class="share-bar">Share this on social</div>
        <p>Second paragraph, still going, with an <img src="x.png" alt="inline diagram"> and <b>bold</b> text that keeps on adding characters.</p>
        <ul><li>Point one</li><li>Point two</li></ul>
        <table><tr><th>Key</th><th>Value</th></tr><tr><td>a</td><td>1</td></tr></table>
        <pre>let x = 1;
let y = 2;</pre>
        <script>var tracking = true;</script>
        <button>Click me</button>
      </article>
      <footer>Copyright footer</footer>
    </body></html>"#;

    #[test]
    fn extracts_article() {
        let out = extract(ARTICLE, "https://www.example.com/post", MarkdownOptions::default());
        assert_eq!(out.title, "The & Title");
        assert_eq!(out.canonical, "https://example.com/canon");
        assert_eq!(out.published_at.as_deref(), Some("2024-05-01"));
        assert_eq!(out.site_name.as_deref(), Some("Example"));
        assert!(!out.needs_browser);

        let text = &out.text;
        assert!(text.contains("## Section one"), "{text}");
        assert!(text.contains("- Point one\n- Point two"), "{text}");
        assert!(text.contains("| Key | Value |\n| a | 1 |"), "{text}");
        assert!(text.contains("```\nlet x = 1;\nlet y = 2;\n```"), "{text}");
        assert!(text.contains("inline diagram"), "{text}");
        for absent in ["Share this", "tracking", "Click me", "Copyright", "Home", "Site header"] {
            assert!(!text.contains(absent), "{absent:?} leaked into {text}");
        }
    }

    #[test]
    fn hides_images_and_flags_app_shells() {
        let out = extract(ARTICLE, "https://example.com/post", MarkdownOptions { hide_images: true });
        assert!(!out.text.contains("inline diagram"));

        let shell = r#"<html><body><div id="root"></div><script>window.__NEXT_DATA__={}</script></body></html>"#;
        let out = extract(shell, "https://app.example.com/", MarkdownOptions::default());
        assert!(out.needs_browser);
        assert_eq!(out.title, "app.example.com");
    }

    #[test]
    fn command_html_to_text() {
        let text = html_to_text("<div><h3>Weather</h3><p>Sunny, 21C</p><script>x()</script></div>");
        assert_eq!(text, "### Weather\n\nSunny, 21C");
    }
}
