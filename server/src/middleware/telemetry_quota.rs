//! 埋点上报防刷配额（REQ-202610-1174）。
//!
//! 按用户维度使用令牌桶：容量为每分钟事件数，按事件条数扣减。
//! 与全局请求限流相互独立 —— 即使全局限流关闭，埋点配额仍然生效。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 单桶最大数量（防止内存无限增长）。
const MAX_BUCKETS: usize = 100_000;
/// 空桶回收阈值。
const STALE_BUCKET_AGE: Duration = Duration::from_secs(600);

#[derive(Debug)]
struct QuotaBucket {
    tokens: f64,
    last_refill: Instant,
}

/// 埋点事件配额器。
#[derive(Debug)]
pub struct TelemetryQuota {
    enabled: bool,
    per_minute: f64,
    max_batch_size: usize,
    buckets: Mutex<HashMap<String, QuotaBucket>>,
}

impl TelemetryQuota {
    /// 构造配额器：`per_minute` 为每分钟允许的事件条数，`max_batch_size` 为单次上报上限。
    pub fn new(per_minute: u32, max_batch_size: usize, enabled: bool) -> Self {
        Self {
            enabled,
            per_minute: per_minute.max(1) as f64,
            max_batch_size: max_batch_size.max(1),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// 单次上报允许的最大事件条数。
    pub const fn max_batch_size(&self) -> usize {
        self.max_batch_size
    }

    /// 扣减 `cost` 个事件配额；额度不足时返回建议的 `Retry-After` 秒数。
    pub fn check(&self, user_id: &str, cost: usize) -> Result<(), u64> {
        if !self.enabled {
            return Ok(());
        }

        let now = Instant::now();
        let cost = cost.max(1) as f64;
        let mut buckets = match self.buckets.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        if buckets.len() >= MAX_BUCKETS && !buckets.contains_key(user_id) {
            buckets.retain(|_, bucket| {
                now.saturating_duration_since(bucket.last_refill) < STALE_BUCKET_AGE
            });
            if buckets.len() >= MAX_BUCKETS {
                if let Some(oldest_key) = buckets
                    .iter()
                    .min_by_key(|(_, bucket)| bucket.last_refill)
                    .map(|(key, _)| key.clone())
                {
                    buckets.remove(&oldest_key);
                }
            }
        }

        let refill_per_sec = self.per_minute / 60.0;
        let bucket = buckets
            .entry(user_id.to_string())
            .or_insert_with(|| QuotaBucket {
                tokens: self.per_minute,
                last_refill: now,
            });

        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(self.per_minute);
        bucket.last_refill = now;

        if bucket.tokens >= cost {
            bucket.tokens -= cost;
            Ok(())
        } else {
            let missing = cost - bucket.tokens;
            Err((missing / refill_per_sec).ceil().max(1.0) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_burst_then_rejects() {
        let quota = TelemetryQuota::new(2, 2, true);
        assert!(quota.check("u1", 2).is_ok());
        assert!(quota.check("u1", 1).is_err());
    }

    #[test]
    fn users_are_isolated() {
        let quota = TelemetryQuota::new(1, 1, true);
        assert!(quota.check("a", 1).is_ok());
        assert!(quota.check("b", 1).is_ok());
        assert!(quota.check("a", 1).is_err());
    }

    #[test]
    fn disabled_quota_allows_everything() {
        let quota = TelemetryQuota::new(1, 1, false);
        for _ in 0..10 {
            assert!(quota.check("u1", 100).is_ok());
        }
    }

    #[test]
    fn max_batch_size_is_exposed() {
        assert_eq!(TelemetryQuota::new(600, 100, true).max_batch_size(), 100);
    }
}
