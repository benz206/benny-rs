use dashmap::DashMap;
use std::time::{Duration, Instant};

pub struct CooldownBucket {
    cooldown: Duration,
    last_used: DashMap<u64, Instant>,
}

impl CooldownBucket {
    pub fn new(seconds: u64) -> Self {
        Self {
            cooldown: Duration::from_secs(seconds),
            last_used: DashMap::new(),
        }
    }

    /// Returns Some(remaining_secs) if on cooldown, None if allowed.
    pub fn check(&self, user_id: u64) -> Option<u64> {
        if let Some(last) = self.last_used.get(&user_id) {
            let elapsed = last.elapsed();
            if elapsed < self.cooldown {
                let remaining = (self.cooldown - elapsed).as_secs().max(1);
                return Some(remaining);
            }
        }
        None
    }

    /// Mark user as having just used this command.
    pub fn trigger(&self, user_id: u64) {
        self.last_used.insert(user_id, Instant::now());
    }
}
