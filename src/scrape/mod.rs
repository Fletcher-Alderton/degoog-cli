//! Fetch pages from this machine and turn them into scored evidence chunks.

pub mod chunks;
pub mod clean;
pub mod extract;
pub mod failures;
pub mod fetch;
pub mod markdown;
pub mod pipeline;
pub mod safety;

pub use pipeline::{scrape_urls, ScrapeOptions, ScrapeRow};
