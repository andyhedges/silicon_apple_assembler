use dashmap::DashMap;
use std::time::{Duration, Instant};

/// Rate limiter configuration
struct RateWindow {
    max_requests: u64,
    window: Duration,
}

/// Per-key rate limiter with two windows: per-minute and per-hour
pub struct RateLimiter {
    /// Map from API key to list of request timestamps
    entries: DashMap<String, Vec<Instant>>,
    windows: Vec<RateWindow>,
}

/// Result of a rate limit check
pub struct RateLimitResult {
    pub allowed: bool,
    /// Seconds until the next request would be allowed (if rejected)
    pub retry_after_seconds: Option<u64>,
    /// Remaining requests in the most restrictive window
    pub remaining: u64,
    /// The limit that was applied
    pub limit: u64,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            windows: vec![
                RateWindow {
                    max_requests: 60,
                    window: Duration::from_secs(60),
                },
                RateWindow {
                    max_requests: 500,
                    window: Duration::from_secs(3600),
                },
            ],
        }
    }

    /// Check if a request is allowed for the given API key.
    /// If allowed, records the request. If not, returns retry info.
    pub fn check_and_record(&self, api_key: &str) -> RateLimitResult {
        let now = Instant::now();
        let mut entry = self.entries.entry(api_key.to_string()).or_default();
        let timestamps = entry.value_mut();

        // Clean up old entries (older than the largest window)
        let max_window = Duration::from_secs(3600);
        timestamps.retain(|t| now.duration_since(*t) < max_window);

        // Check each window
        for w in &self.windows {
            let cutoff = now - w.window;
            let count = timestamps.iter().filter(|t| **t >= cutoff).count() as u64;
            if count >= w.max_requests {
                // Find the oldest timestamp in this window to compute retry_after
                let oldest_in_window = timestamps
                    .iter()
                    .filter(|t| **t >= cutoff)
                    .min()
                    .cloned();
                let retry_after = oldest_in_window
                    .map(|oldest| {
                        let expires_at = oldest + w.window;
                        if expires_at > now {
                            expires_at.duration_since(now).as_secs() + 1
                        } else {
                            1
                        }
                    })
                    .unwrap_or(1);

                return RateLimitResult {
                    allowed: false,
                    retry_after_seconds: Some(retry_after),
                    remaining: 0,
                    limit: w.max_requests,
                };
            }
        }

        // All windows OK — record the request
        timestamps.push(now);

        // Compute remaining for the most restrictive (per-minute) window
        let minute_cutoff = now - Duration::from_secs(60);
        let minute_count = timestamps.iter().filter(|t| **t >= minute_cutoff).count() as u64;
        let remaining = 60u64.saturating_sub(minute_count);

        RateLimitResult {
            allowed: true,
            retry_after_seconds: None,
            remaining,
            limit: 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allows_under_limit() {
        let limiter = RateLimiter::new();
        for _ in 0..59 {
            let result = limiter.check_and_record("key1");
            assert!(result.allowed);
        }
    }

    #[test]
    fn test_blocks_at_minute_limit() {
        let limiter = RateLimiter::new();
        for _ in 0..60 {
            let result = limiter.check_and_record("key1");
            assert!(result.allowed);
        }
        let result = limiter.check_and_record("key1");
        assert!(!result.allowed);
        assert!(result.retry_after_seconds.is_some());
    }

    #[test]
    fn test_separate_keys() {
        let limiter = RateLimiter::new();
        for _ in 0..60 {
            limiter.check_and_record("key1");
        }
        // key2 should still be allowed
        let result = limiter.check_and_record("key2");
        assert!(result.allowed);
    }

    #[test]
    fn test_remaining_decreases() {
        let limiter = RateLimiter::new();
        let r1 = limiter.check_and_record("key1");
        let r2 = limiter.check_and_record("key1");
        assert!(r1.remaining > r2.remaining);
    }

    #[test]
    fn test_rate_limit_returns_429_info() {
        let limiter = RateLimiter::new();
        for _ in 0..60 {
            limiter.check_and_record("key1");
        }
        let result = limiter.check_and_record("key1");
        assert!(!result.allowed);
        assert_eq!(result.remaining, 0);
        assert_eq!(result.limit, 60);
    }
}