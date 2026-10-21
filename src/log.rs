//! Minimal stderr logger. Stdout is reserved for command output and for the
//! MCP JSON-RPC stream, so every diagnostic goes to stderr.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug = 10,
    Info = 20,
    Warn = 30,
    Error = 40,
    Silent = 99,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Warn as u8);

impl Level {
    pub fn parse(raw: &str) -> Option<Level> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            "silent" | "off" => Some(Level::Silent),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Silent => "",
        }
    }
}

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 >= LEVEL.load(Ordering::Relaxed)
}

pub fn emit(level: Level, ns: &str, message: std::fmt::Arguments<'_>) {
    if enabled(level) {
        eprintln!("{} [{}] {}", level.label(), ns, message);
    }
}

#[macro_export]
macro_rules! debug {
    ($ns:expr, $($arg:tt)*) => { $crate::log::emit($crate::log::Level::Debug, $ns, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! info {
    ($ns:expr, $($arg:tt)*) => { $crate::log::emit($crate::log::Level::Info, $ns, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! warn {
    ($ns:expr, $($arg:tt)*) => { $crate::log::emit($crate::log::Level::Warn, $ns, format_args!($($arg)*)) };
}
