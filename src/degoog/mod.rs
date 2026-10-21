//! HTTP client for the remote Degoog API.

pub mod api;
pub mod client;
pub mod search_types;
pub mod types;

pub use client::{Client, DegoogError, ErrorKind};
