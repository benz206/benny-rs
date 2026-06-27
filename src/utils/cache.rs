//! Small helper for keeping interactive-session `DashMap`s from growing without
//! bound. These maps gain an entry per command/component use and only some code
//! paths remove them, so over a long uptime they leak. `bounded_insert` caps the
//! entry count, evicting existing entries when the cap is reached. Eviction is
//! arbitrary (DashMap has no insertion order), which is fine here: every caller
//! already degrades gracefully when a session is missing.

use dashmap::DashMap;
use std::hash::Hash;

pub fn bounded_insert<K, V>(map: &DashMap<K, V>, key: K, value: V, cap: usize)
where
    K: Eq + Hash + Clone,
{
    if map.len() >= cap {
        // Collect victims first (this drops the iterator's shard locks) before
        // removing, so we never remove while holding an iteration guard.
        let excess = map.len() + 1 - cap;
        let victims: Vec<K> = map.iter().take(excess).map(|e| e.key().clone()).collect();
        for k in victims {
            map.remove(&k);
        }
    }
    map.insert(key, value);
}
