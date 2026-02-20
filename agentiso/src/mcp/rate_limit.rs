use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::config::RateLimitConfig;

/// Token bucket rate limiter for MCP tool call categories.
///
/// Each category maintains its own token bucket with a configurable
/// burst size and refill rate. Tokens are refilled continuously based
/// on elapsed time since the last check.
pub struct RateLimiter {
    enabled: bool,
    buckets: Mutex<HashMap<String, TokenBucket>>,
}

struct TokenBucket {
    /// Current number of available tokens (can be fractional during refill).
    tokens: f64,
    /// Maximum tokens (burst capacity).
    max_tokens: f64,
    /// Tokens added per second.
    refill_rate: f64,
    /// Last time tokens were refilled.
    last_refill: Instant,
}

impl TokenBucket {
    fn new(max_tokens: u32, per_minute: u32) -> Self {
        Self {
            tokens: max_tokens as f64,
            max_tokens: max_tokens as f64,
            refill_rate: per_minute as f64 / 60.0,
            last_refill: Instant::now(),
        }
    }

    /// Refill tokens based on elapsed time and try to consume one.
    /// Returns true if a token was consumed, false if rate limited.
    fn try_acquire(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;

        // Refill tokens (capped at max).
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Tool call rate limit categories.
pub const CATEGORY_CREATE: &str = "create";
pub const CATEGORY_EXEC: &str = "exec";
pub const CATEGORY_DEFAULT: &str = "default";
pub const CATEGORY_TEAM_MESSAGE: &str = "team_message";

impl RateLimiter {
    /// Create a new rate limiter from configuration.
    pub fn new(config: &RateLimitConfig) -> Self {
        let mut buckets = HashMap::new();

        // Create: burst 3, refill at create_per_minute rate
        buckets.insert(
            CATEGORY_CREATE.to_string(),
            TokenBucket::new(3, config.create_per_minute),
        );

        // Exec: burst 10, refill at exec_per_minute rate
        buckets.insert(
            CATEGORY_EXEC.to_string(),
            TokenBucket::new(10, config.exec_per_minute),
        );

        // Default: burst 20, refill at default_per_minute rate
        buckets.insert(
            CATEGORY_DEFAULT.to_string(),
            TokenBucket::new(20, config.default_per_minute),
        );

        // Team message: burst 50, refill at team_message_per_minute rate
        buckets.insert(
            CATEGORY_TEAM_MESSAGE.to_string(),
            TokenBucket::new(50, config.team_message_per_minute),
        );

        Self {
            enabled: config.enabled,
            buckets: Mutex::new(buckets),
        }
    }

    /// Try to acquire a token for the given category.
    ///
    /// Returns `Ok(())` if the request is allowed, or `Err(message)` if rate limited.
    /// When rate limiting is disabled, always returns `Ok(())`.
    pub fn check(&self, category: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        let mut buckets = self.buckets.lock().unwrap();
        let bucket = match buckets.get_mut(category) {
            Some(b) => b,
            None => {
                // Unknown category falls back to default bucket.
                buckets.get_mut(CATEGORY_DEFAULT).unwrap()
            }
        };

        if bucket.try_acquire() {
            Ok(())
        } else {
            let limit = (bucket.refill_rate * 60.0).round() as u32;
            Err(format!(
                "Rate limit exceeded for {} operations. Maximum {} calls per minute. Please wait and retry.",
                category, limit
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn allows_requests_within_burst_limit() {
        let config = RateLimitConfig {
            enabled: true,
            create_per_minute: 5,
            exec_per_minute: 60,
            default_per_minute: 120,
            ..Default::default()
        };
        let limiter = RateLimiter::new(&config);

        // Create category has burst of 3 -- first 3 should succeed.
        assert!(limiter.check(CATEGORY_CREATE).is_ok());
        assert!(limiter.check(CATEGORY_CREATE).is_ok());
        assert!(limiter.check(CATEGORY_CREATE).is_ok());
    }

    #[test]
    fn blocks_requests_exceeding_burst() {
        let config = RateLimitConfig {
            enabled: true,
            create_per_minute: 5,
            exec_per_minute: 60,
            default_per_minute: 120,
            ..Default::default()
        };
        let limiter = RateLimiter::new(&config);

        // Exhaust burst of 3 for create category.
        for _ in 0..3 {
            assert!(limiter.check(CATEGORY_CREATE).is_ok());
        }

        // 4th request should be blocked.
        let result = limiter.check(CATEGORY_CREATE);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Rate limit exceeded for create operations"));
        assert!(err.contains("Maximum 5 calls per minute"));
    }

    #[test]
    fn refills_tokens_over_time() {
        let config = RateLimitConfig {
            enabled: true,
            // 60/min = 1/sec, so after 1 second we get 1 token back
            create_per_minute: 60,
            exec_per_minute: 60,
            default_per_minute: 120,
            ..Default::default()
        };
        let limiter = RateLimiter::new(&config);

        // Create category has burst 3 at 60/min = 1/sec refill.
        // Exhaust all 3 tokens.
        for _ in 0..3 {
            assert!(limiter.check(CATEGORY_CREATE).is_ok());
        }
        assert!(limiter.check(CATEGORY_CREATE).is_err());

        // Wait just over 1 second for a token to refill.
        thread::sleep(Duration::from_millis(1100));

        // Should have ~1 token now.
        assert!(limiter.check(CATEGORY_CREATE).is_ok());
    }

    #[test]
    fn disabled_limiter_allows_everything() {
        let config = RateLimitConfig {
            enabled: false,
            create_per_minute: 1,
            exec_per_minute: 1,
            default_per_minute: 1,
            ..Default::default()
        };
        let limiter = RateLimiter::new(&config);

        // Even with limits of 1/min, disabled limiter should allow unlimited calls.
        for _ in 0..100 {
            assert!(limiter.check(CATEGORY_CREATE).is_ok());
            assert!(limiter.check(CATEGORY_EXEC).is_ok());
            assert!(limiter.check(CATEGORY_DEFAULT).is_ok());
        }
    }

    #[test]
    fn exec_category_allows_higher_burst() {
        let config = RateLimitConfig::default();
        let limiter = RateLimiter::new(&config);

        // Exec has burst of 10.
        for _ in 0..10 {
            assert!(limiter.check(CATEGORY_EXEC).is_ok());
        }
        assert!(limiter.check(CATEGORY_EXEC).is_err());
    }

    #[test]
    fn default_category_allows_highest_burst() {
        let config = RateLimitConfig::default();
        let limiter = RateLimiter::new(&config);

        // Default has burst of 20.
        for _ in 0..20 {
            assert!(limiter.check(CATEGORY_DEFAULT).is_ok());
        }
        assert!(limiter.check(CATEGORY_DEFAULT).is_err());
    }

    #[test]
    fn unknown_category_falls_back_to_default() {
        let config = RateLimitConfig::default();
        let limiter = RateLimiter::new(&config);

        // Unknown category uses the default bucket.
        for _ in 0..20 {
            assert!(limiter.check("unknown_category").is_ok());
        }
        assert!(limiter.check("unknown_category").is_err());
    }

    #[test]
    fn categories_are_independent() {
        let config = RateLimitConfig::default();
        let limiter = RateLimiter::new(&config);

        // Exhaust create bucket (burst 3).
        for _ in 0..3 {
            assert!(limiter.check(CATEGORY_CREATE).is_ok());
        }
        assert!(limiter.check(CATEGORY_CREATE).is_err());

        // Exec and default should still work.
        assert!(limiter.check(CATEGORY_EXEC).is_ok());
        assert!(limiter.check(CATEGORY_DEFAULT).is_ok());
    }

    #[test]
    fn error_message_format() {
        let config = RateLimitConfig {
            enabled: true,
            create_per_minute: 10,
            exec_per_minute: 60,
            default_per_minute: 120,
            ..Default::default()
        };
        let limiter = RateLimiter::new(&config);

        // Exhaust create burst (3).
        for _ in 0..3 {
            let _ = limiter.check(CATEGORY_CREATE);
        }

        let err = limiter.check(CATEGORY_CREATE).unwrap_err();
        assert_eq!(
            err,
            "Rate limit exceeded for create operations. Maximum 10 calls per minute. Please wait and retry."
        );
    }

    #[test]
    fn config_defaults() {
        let config = RateLimitConfig::default();
        assert!(config.enabled);
        assert_eq!(config.create_per_minute, 5);
        assert_eq!(config.exec_per_minute, 60);
        assert_eq!(config.default_per_minute, 120);
        assert_eq!(config.team_message_per_minute, 300);
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = RateLimitConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: RateLimitConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.enabled, config.enabled);
        assert_eq!(deserialized.create_per_minute, config.create_per_minute);
        assert_eq!(deserialized.exec_per_minute, config.exec_per_minute);
        assert_eq!(deserialized.default_per_minute, config.default_per_minute);
        assert_eq!(
            deserialized.team_message_per_minute,
            config.team_message_per_minute
        );
    }

    #[test]
    fn team_message_category_burst_limit() {
        let config = RateLimitConfig {
            enabled: true,
            team_message_per_minute: 300,
            ..Default::default()
        };
        let limiter = RateLimiter::new(&config);

        // Team message has burst of 50
        for _ in 0..50 {
            assert!(limiter.check(CATEGORY_TEAM_MESSAGE).is_ok());
        }
        // 51st should be blocked
        assert!(limiter.check(CATEGORY_TEAM_MESSAGE).is_err());
    }

    #[test]
    fn team_message_category_error_message() {
        let config = RateLimitConfig {
            enabled: true,
            team_message_per_minute: 300,
            ..Default::default()
        };
        let limiter = RateLimiter::new(&config);

        // Exhaust burst of 50
        for _ in 0..50 {
            let _ = limiter.check(CATEGORY_TEAM_MESSAGE);
        }

        let err = limiter.check(CATEGORY_TEAM_MESSAGE).unwrap_err();
        assert!(err.contains("team_message"));
        assert!(err.contains("300"));
    }

    #[test]
    fn team_message_category_independent_from_others() {
        let config = RateLimitConfig::default();
        let limiter = RateLimiter::new(&config);

        // Exhaust team_message burst (50)
        for _ in 0..50 {
            assert!(limiter.check(CATEGORY_TEAM_MESSAGE).is_ok());
        }
        assert!(limiter.check(CATEGORY_TEAM_MESSAGE).is_err());

        // Other categories should still work
        assert!(limiter.check(CATEGORY_CREATE).is_ok());
        assert!(limiter.check(CATEGORY_EXEC).is_ok());
        assert!(limiter.check(CATEGORY_DEFAULT).is_ok());
    }
}
