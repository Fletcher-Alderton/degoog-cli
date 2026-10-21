//! Render an HTML subtree as light markdown: headings, lists, table rows,
//! code blocks and paragraphs, with image labels kept unless hidden.

use crate::text::take_chars;
use ego_tree::NodeRef;
use regex::Regex;
use scraper::node::Element;
use scraper::Node;
use std::sync::LazyLock;

const BLOCK_TAGS: &[&str] = &[
    "html", "body", "p", "div", "section", "article", "main", "header", "footer", "aside", "figure", "dl", "li", "tr",
    "blockquote", "pre", "figcaption", "dd", "dt",
];
const HEADING_TAGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
const LABEL_ATTRS: &[&str] = &["alt", "aria-label", "title"];
const MAX_LABEL_CHARS: usize = 80;

#[derive(Clone, Copy, Debug, Default)]
pub struct MarkdownOptions {
    pub hide_images: bool,
}

static INLINE_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+").unwrap());

fn squash(value: &str) -> String {
    INLINE_WS.replace_all(value, " ").trim().to_string()
}

/// All descendant text, like DOM `textContent`.
pub fn text_content(node: NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    for descendant in node.descendants() {
        if let Node::Text(text) = descendant.value() {
            out.push_str(text);
        }
    }
    out
}

fn tag(node: NodeRef<'_, Node>) -> Option<&str> {
    node.value().as_element().map(|e| e.name())
}

fn label_of(element: &Element) -> String {
    if element.attr("alt") == Some("") {
        return String::new();
    }
    for attr in LABEL_ATTRS {
        let label = squash(element.attr(attr).unwrap_or(""));
        if !label.is_empty() {
            return take_chars(&label, MAX_LABEL_CHARS).to_string();
        }
    }
    String::new()
}

/// Inline text of a node, with image labels spliced in.
fn inline_text(node: NodeRef<'_, Node>, options: MarkdownOptions) -> String {
    let mut out = String::new();
    for child in node.children() {
        match child.value() {
            Node::Text(text) => out.push_str(text),
            Node::Element(element) if element.name() == "img" => {
                if !options.hide_images {
                    let label = label_of(element);
                    if !label.is_empty() {
                        out.push(' ');
                        out.push_str(&label);
                        out.push(' ');
                    }
                }
            }
            Node::Element(_) => out.push_str(&inline_text(child, options)),
            _ => {}
        }
    }
    out
}

fn is_blockish(name: &str) -> bool {
    BLOCK_TAGS.contains(&name) || HEADING_TAGS.contains(&name) || matches!(name, "ul" | "ol" | "table")
}

fn render_node(node: NodeRef<'_, Node>, lines: &mut Vec<String>, options: MarkdownOptions) {
    let element = match node.value() {
        Node::Text(text) => {
            let text = squash(text);
            if !text.is_empty() {
                lines.push(text);
            }
            return;
        }
        Node::Element(element) => element,
        _ => return,
    };

    let name = element.name();
    match name {
        "br" => lines.push(String::new()),
        "img" => {
            if !options.hide_images {
                let label = label_of(element);
                if !label.is_empty() {
                    lines.push(label);
                }
            }
        }
        _ if HEADING_TAGS.contains(&name) => {
            let text = squash(&inline_text(node, options));
            if !text.is_empty() {
                let level: usize = name[1..].parse().unwrap_or(1);
                lines.extend([String::new(), format!("{} {text}", "#".repeat(level)), String::new()]);
            }
        }
        "pre" => {
            let code = text_content(node).trim().to_string();
            if !code.is_empty() {
                lines.extend([String::new(), "```".into(), code, "```".into(), String::new()]);
            }
        }
        "li" => {
            let text = squash(&inline_text(node, options));
            if !text.is_empty() {
                lines.push(format!("- {text}"));
            }
        }
        "tr" => {
            let cells: Vec<String> = node
                .descendants()
                .skip(1)
                .filter(|d| matches!(tag(*d), Some("th" | "td")))
                .map(|cell| squash(&inline_text(cell, options)))
                .filter(|c| !c.is_empty())
                .collect();
            if !cells.is_empty() {
                lines.push(format!("| {} |", cells.join(" | ")));
            }
        }
        "table" | "ul" | "ol" => {
            lines.push(String::new());
            for child in node.children() {
                render_node(child, lines, options);
            }
            lines.push(String::new());
        }
        _ => {
            let has_block_child = node.children().any(|c| tag(c).is_some_and(is_blockish));
            if has_block_child {
                for child in node.children() {
                    render_node(child, lines, options);
                }
            } else {
                let text = squash(&inline_text(node, options));
                if !text.is_empty() {
                    lines.push(if BLOCK_TAGS.contains(&name) { format!("{text}\n") } else { text });
                }
            }
        }
    }
}

/// Markdown for the children of `root`.
pub fn to_markdown(root: NodeRef<'_, Node>, options: MarkdownOptions) -> String {
    let mut lines = Vec::new();
    for child in root.children() {
        render_node(child, &mut lines, options);
    }
    lines.join("\n")
}
