use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::lease::Clock;

const MAX_BUCKETS: usize = 4096;

/// Per-key token buckets: `rate` requests per second with room for `burst`.
pub struct RateLimiter {
    clock: Arc<dyn Clock>,
    rate: f64,
    burst: f64,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn new(clock: Arc<dyn Clock>, rate: u32, burst: u32) -> Self {
        Self {
            clock,
            rate: f64::from(rate),
            burst: f64::from(burst),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token for `key`, or says how long to wait for the next one.
    pub fn check(&self, key: &str) -> Result<(), Duration> {
        let now = self.clock.now();
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if buckets.len() >= MAX_BUCKETS && !buckets.contains_key(key) {
            buckets.retain(|_, (tokens, at)| {
                *tokens + now.saturating_duration_since(*at).as_secs_f64() * self.rate < self.burst
            });
        }
        let (tokens, at) = buckets.entry(key.to_owned()).or_insert((self.burst, now));
        *tokens = (*tokens + now.saturating_duration_since(*at).as_secs_f64() * self.rate)
            .min(self.burst);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - *tokens) / self.rate))
        }
    }
}

#[cfg(test)]
mod tests;
