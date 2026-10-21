//! Local client for a remote Degoog instance.
//!
//! The same tool set is exposed two ways: as `degoog <subcommand>` for people,
//! and as a stdio MCP server (`degoog mcp`) for MCP clients running on the
//! same machine. Everything except the Degoog search itself (scraping, HTML
//! extraction, chunking, evidence packing) runs locally.

pub mod bundle;
pub mod cache;
pub mod config;
pub mod degoog;
pub mod log;
pub mod mcp;
pub mod output;
pub mod scrape;
pub mod search;
pub mod text;
pub mod tools;
pub mod urls;

pub const NAME: &str = "degoog-cli";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
