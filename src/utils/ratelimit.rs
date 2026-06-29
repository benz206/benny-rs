//! Shared throttle for the paths poise's command cooldowns do not cover:
//! component/modal interactions and passive `on_message` / `on_member_join`
//! handlers. Keyed map of last-allowed times, size-capped so it cannot grow
//! without bound over a long uptime (eviction is arbitrary, like
//! [`crate::utils::cache::bounded_insert`], which every caller tolerates).

use dashmap::DashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

pub struct RateLimiter<K: Eq + Hash + Clone> {
    last: DashMap<K, Instant>,
    cap: usize,
}

impl<K: Eq + Hash + Clone> RateLimiter<K> {
    pub fn new(cap: usize) -> Self {
        Self {
            last: DashMap::new(),
            cap,
        }
    }

    /// Returns `Some(remaining)` if `key` is still within `window` of its last
    /// allowed time (caller should reject/skip). Otherwise records "now" and
    /// returns `None` (allowed). The read guard is dropped before the insert, so
    /// there is no shard self-deadlock.
    pub fn check(&self, key: K, window: Duration) -> Option<Duration> {
        if let Some(prev) = self.last.get(&key) {
            let elapsed = prev.elapsed();
            if elapsed < window {
                return Some(window - elapsed);
            }
        }
        crate::utils::cache::bounded_insert(&self.last, key, Instant::now(), self.cap);
        None
    }
}
