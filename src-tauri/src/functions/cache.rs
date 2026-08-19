//! In-memory TTL cache for video/playlist metadata, keyed by a normalized
//! URL. Re-pasting or revisiting the same URL within the TTL window skips
//! the yt-dlp subprocess spawn entirely instead of re-fetching.
//!
//! Only successful fetches are cached — a transient failure (e.g. YouTube's
//! anti-bot captcha) must not "stick" for the TTL window, so callers only
//! call `insert` on the Ok path.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(5 * 60);

pub struct TtlCache<T: Clone> {
    entries: Mutex<HashMap<String, (Instant, T)>>,
}

impl<T: Clone> TtlCache<T> {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, key: &str) -> Option<T> {
        let mut map = self.entries.lock();
        match map.get(key) {
            Some((inserted, value)) if inserted.elapsed() < TTL => Some(value.clone()),
            Some(_) => {
                map.remove(key);
                None
            }
            None => None,
        }
    }

    pub fn insert(&self, key: String, value: T) {
        self.entries.lock().insert(key, (Instant::now(), value));
    }
}

/// Declares a lazily-initialized, process-wide `TtlCache<$ty>` static named
/// `$name`.
macro_rules! ttl_cache {
    ($name:ident, $ty:ty) => {
        pub static $name: LazyLock<TtlCache<$ty>> = LazyLock::new(TtlCache::new);
    };
}

ttl_cache!(VIDEO_INFO_CACHE, crate::functions::get_info::VideoInfo);
ttl_cache!(TWITCH_INFO_CACHE, crate::functions::twitch::TwitchVideoInfo);
ttl_cache!(PLAYLIST_INFO_CACHE, crate::functions::playlist::PlaylistInfo);

/// Normalizes a URL to a stable cache key by dropping every query
/// parameter except the ones callers explicitly keep. Falls back to the
/// original (trimmed) URL if it doesn't parse — a cache miss, never a
/// crash.
fn normalize(url: &str, keep: &[&str]) -> String {
    match reqwest::Url::parse(url.trim()) {
        Ok(mut parsed) => {
            let kept: Vec<(String, String)> = parsed
                .query_pairs()
                .filter(|(k, _)| keep.contains(&k.as_ref()))
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            if kept.is_empty() {
                parsed.set_query(None);
            } else {
                let qs = kept
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("&");
                parsed.set_query(Some(&qs));
            }
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.trim().to_string(),
    }
}

/// Cache key for a single video (YouTube `v=`/`youtu.be` path, or a Twitch
/// VOD/clip path) — strips tracking params (`si=`, `t=`, …) so re-pasting
/// a shared link still hits the cache.
pub fn video_key(url: &str) -> String {
    normalize(url, &["v"])
}

/// Cache key for a playlist (YouTube `list=`, or a Twitch channel's videos
/// page, which carries no relevant query params).
pub fn playlist_key(url: &str) -> String {
    normalize(url, &["list"])
}
