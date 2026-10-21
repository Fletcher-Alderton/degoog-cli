//! In-memory LRU cache with TTL and a byte budget, optionally saved to disk
//! so separate CLI invocations can share search results and scrape failures.

use crate::config::CacheConfig;
use indexmap::IndexMap;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod ns {
    pub const SEARCH: &str = "search";
    pub const SUGGEST: &str = "suggest";
    pub const DISCOVER: &str = "discover";
    pub const SEARCH_TYPES: &str = "search-types";
    pub const SCRAPE_FAILURE: &str = "scrape-failure";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    value: Value,
    bytes: usize,
    expires_at: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

#[derive(Default)]
struct Inner {
    store: IndexMap<String, Entry>,
    bytes: usize,
    stats: CacheStats,
    dirty: bool,
}

pub struct Cache {
    config: CacheConfig,
    inner: Mutex<Inner>,
    path: Option<PathBuf>,
    clock: fn() -> u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn stable(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => format!("[{}]", items.iter().map(stable).collect::<Vec<_>>().join(",")),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().filter(|k| !map[*k].is_null()).collect();
            keys.sort();
            let parts: Vec<String> = keys.iter().map(|k| format!("{k}={}", stable(&map[*k]))).collect();
            format!("{{{}}}", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// Order-independent key: `namespace:{a=1,b=[x,y]}`.
pub fn cache_key(namespace: &str, parts: &Value) -> String {
    format!("{namespace}:{}", stable(parts))
}

impl Inner {
    fn drop_key(&mut self, key: &str) {
        if let Some(entry) = self.store.shift_remove(key) {
            self.bytes -= entry.bytes;
        }
    }

    fn evict(&mut self, now: u64, config: &CacheConfig) {
        let expired: Vec<String> =
            self.store.iter().filter(|(_, e)| e.expires_at <= now).map(|(k, _)| k.clone()).collect();
        for key in expired {
            self.drop_key(&key);
            self.stats.evictions += 1;
        }
        while self.store.len() > config.max_entries || self.bytes > config.max_bytes {
            let Some(oldest) = self.store.keys().next().cloned() else { break };
            self.drop_key(&oldest);
            self.stats.evictions += 1;
        }
    }
}

impl Cache {
    pub fn new(config: CacheConfig) -> Self {
        Self::with_clock(config, now_ms)
    }

    pub fn with_clock(config: CacheConfig, clock: fn() -> u64) -> Self {
        Self { config, inner: Mutex::new(Inner::default()), path: None, clock }
    }

    /// Cache backed by a file, loaded now and written by [`Cache::save`].
    pub fn persistent(config: CacheConfig, path: PathBuf) -> Self {
        let mut cache = Self::new(config);
        cache.load_from(&path);
        cache.path = Some(path);
        cache
    }

    fn load_from(&mut self, path: &Path) {
        if !self.config.enabled {
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Ok(store) = serde_json::from_str::<IndexMap<String, Entry>>(&text) else {
            crate::debug!("cache", "ignoring unreadable cache file {}", path.display());
            return;
        };
        let now = (self.clock)();
        let inner = self.inner.get_mut().unwrap();
        for (key, entry) in store {
            if entry.expires_at > now {
                inner.bytes += entry.bytes;
                inner.store.insert(key, entry);
            }
        }
        inner.evict(now, &self.config);
    }

    /// Write the cache to disk when it is persistent and has changed.
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        let inner = self.inner.lock().unwrap();
        if !inner.dirty {
            return;
        }
        let now = (self.clock)();
        let live: IndexMap<&String, &Entry> = inner.store.iter().filter(|(_, e)| e.expires_at > now).collect();
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&live)?)?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(err) = result {
            crate::debug!("cache", "could not save cache to {}: {err}", path.display());
        }
    }

    pub fn get_value(&self, key: &str) -> Option<Value> {
        if !self.config.enabled {
            return None;
        }
        let now = (self.clock)();
        let mut inner = self.inner.lock().unwrap();
        let Some(index) = inner.store.get_index_of(key) else {
            inner.stats.misses += 1;
            return None;
        };
        if inner.store[index].expires_at <= now {
            inner.drop_key(key);
            inner.stats.evictions += 1;
            inner.stats.misses += 1;
            return None;
        }
        // Move to the back: most recently used.
        let last = inner.store.len() - 1;
        inner.store.move_index(index, last);
        inner.stats.hits += 1;
        Some(inner.store[last].value.clone())
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.get_value(key).and_then(|v| serde_json::from_value(v).ok())
    }

    pub fn set<T: Serialize>(&self, key: &str, value: &T, ttl_ms: Option<u64>) {
        if !self.config.enabled {
            return;
        }
        let Ok(value) = serde_json::to_value(value) else { return };
        let bytes = value.to_string().len();
        if bytes > self.config.max_bytes {
            return;
        }
        let now = (self.clock)();
        let mut inner = self.inner.lock().unwrap();
        inner.drop_key(key);
        inner.store.insert(
            key.to_string(),
            Entry { value, bytes, expires_at: now + ttl_ms.unwrap_or(self.config.ttl) },
        );
        inner.bytes += bytes;
        inner.dirty = true;
        inner.evict(now, &self.config);
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.store.clear();
        inner.bytes = 0;
        inner.dirty = true;
    }

    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.lock().unwrap();
        CacheStats { entries: inner.store.len(), bytes: inner.bytes, ..inner.stats }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NOW: AtomicU64 = AtomicU64::new(1_000);
    fn clock() -> u64 {
        NOW.load(Ordering::SeqCst)
    }

    fn config(max_entries: usize) -> CacheConfig {
        CacheConfig { max_entries, ..CacheConfig::default() }
    }

    #[test]
    fn stable_keys_ignore_order() {
        assert_eq!(cache_key("s", &json!({"b": 1, "a": [2, "x"]})), "s:{a=[2,x],b=1}");
        assert_eq!(cache_key("s", &json!({"a": 1, "b": null})), cache_key("s", &json!({"a": 1})));
    }

    #[test]
    fn ttl_lru_and_disabled() {
        let cache = Cache::with_clock(config(2), clock);
        cache.set("a", &1, Some(100));
        cache.set("b", &2, None);
        assert_eq!(cache.get::<i32>("a"), Some(1)); // a is now most recent
        cache.set("c", &3, None); // evicts b
        assert_eq!(cache.get::<i32>("b"), None);
        NOW.fetch_add(200, Ordering::SeqCst);
        assert_eq!(cache.get::<i32>("a"), None); // expired
        assert_eq!(cache.get::<i32>("c"), Some(3));

        let off = Cache::new(CacheConfig { enabled: false, ..CacheConfig::default() });
        off.set("x", &1, None);
        assert_eq!(off.get::<i32>("x"), None);
    }

    #[test]
    fn persists_round_trip() {
        let dir = std::env::temp_dir().join(format!("degoog-cache-test-{}", std::process::id()));
        let path = dir.join("cache.json");
        let first = Cache::persistent(CacheConfig::default(), path.clone());
        first.set("k", &json!({"v": 1}), None);
        first.save();
        let second = Cache::persistent(CacheConfig::default(), path);
        assert_eq!(second.get_value("k"), Some(json!({"v": 1})));
        let _ = std::fs::remove_dir_all(dir);
    }
}
