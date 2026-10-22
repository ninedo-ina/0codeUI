//! 生成管线可观测指标。
//!
//! 指标只保存在进程内（与 REQ-202610-1181 的模型指标一致），通过
//! `GET /api/v1/ui/pipeline/metrics` 暴露快照；结构化日志另行输出，
//! 便于接入外部采集。所有计数使用原子量或单锁保护，读写均为 O(1) / O(window)。

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Mutex;

use serde::Serialize;

use super::orchestrator::PipelineOutcome;

/// 延迟分位窗口大小。
const LATENCY_WINDOW: usize = 1024;

/// 单个降级原因的计数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DegradationReasonCount {
    /// 原因标识。
    pub reason: String,
    /// 次数。
    pub count: u64,
}

/// 管线指标快照。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PipelineMetricsSnapshot {
    /// 总请求数。
    pub total_requests: u64,
    /// 动态生成成功数。
    pub dynamic_success: u64,
    /// 降级到静态模板数。
    pub degraded_requests: u64,
    /// 缓存命中数。
    pub cache_hits: u64,
    /// 缓存未命中数。
    pub cache_misses: u64,
    /// 兜底渲染失败数（无页面可返回）。
    pub fallback_failures: u64,
    /// 各降级原因计数。
    pub degradation_reasons: Vec<DegradationReasonCount>,
    /// 平均端到端耗时（毫秒）。
    pub avg_total_latency_ms: f64,
    /// P50 端到端耗时（毫秒）。
    pub p50_total_latency_ms: f64,
    /// P95 端到端耗时（毫秒）。
    pub p95_total_latency_ms: f64,
    /// 最大端到端耗时（毫秒）。
    pub max_total_latency_ms: f64,
    /// 时间预算（毫秒）。
    pub budget_ms: u64,
    /// 预算内完成的请求数。
    pub within_budget: u64,
    /// 预算内完成率（0.0–1.0）。
    pub within_budget_rate: f64,
}

#[derive(Debug, Default)]
struct MetricsInner {
    total_requests: u64,
    dynamic_success: u64,
    degraded_requests: u64,
    cache_hits: u64,
    cache_misses: u64,
    fallback_failures: u64,
    degradation_reasons: HashMap<&'static str, u64>,
    latency_samples: VecDeque<f64>,
    latency_sum: f64,
    max_latency_ms: f64,
    within_budget: u64,
}

/// 线程安全的管线指标收集器。
#[derive(Debug)]
pub struct PipelineMetrics {
    budget_ms: u64,
    inner: Mutex<MetricsInner>,
}

impl PipelineMetrics {
    /// 创建指标收集器。
    pub fn new(budget_ms: u64) -> Self {
        Self {
            budget_ms,
            inner: Mutex::new(MetricsInner::default()),
        }
    }

    /// 记录一次成功完成的请求。
    pub fn record(&self, outcome: &PipelineOutcome) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inner.total_requests += 1;
        if outcome.degraded {
            inner.degraded_requests += 1;
            if let Some(reason) = outcome.degradation_reason {
                *inner
                    .degradation_reasons
                    .entry(reason.as_str())
                    .or_insert(0) += 1;
            }
        } else {
            inner.dynamic_success += 1;
        }
        if outcome.cache_hit {
            inner.cache_hits += 1;
        } else {
            inner.cache_misses += 1;
        }
        let latency = outcome.timings.total_ms;
        inner.latency_sum += latency;
        inner.max_latency_ms = inner.max_latency_ms.max(latency);
        if latency <= self.budget_ms as f64 {
            inner.within_budget += 1;
        }
        if inner.latency_samples.len() >= LATENCY_WINDOW {
            inner.latency_samples.pop_front();
        }
        inner.latency_samples.push_back(latency);
    }

    /// 记录一次兜底渲染失败（此时没有可返回页面）。
    pub fn record_fallback_failure(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inner.fallback_failures += 1;
    }

    /// 生成快照。
    pub fn snapshot(&self) -> PipelineMetricsSnapshot {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut samples: Vec<f64> = inner.latency_samples.iter().copied().collect();
        samples.sort_by(|left, right| left.total_cmp(right));
        let total = inner.total_requests;
        let mut reasons: Vec<DegradationReasonCount> = inner
            .degradation_reasons
            .iter()
            .map(|(reason, count)| DegradationReasonCount {
                reason: (*reason).to_string(),
                count: *count,
            })
            .collect();
        reasons.sort_by_key(|left| std::cmp::Reverse(left.count));

        PipelineMetricsSnapshot {
            total_requests: total,
            dynamic_success: inner.dynamic_success,
            degraded_requests: inner.degraded_requests,
            cache_hits: inner.cache_hits,
            cache_misses: inner.cache_misses,
            fallback_failures: inner.fallback_failures,
            degradation_reasons: reasons,
            avg_total_latency_ms: if total == 0 {
                0.0
            } else {
                round3(inner.latency_sum / total as f64)
            },
            p50_total_latency_ms: percentile(&samples, 0.50),
            p95_total_latency_ms: percentile(&samples, 0.95),
            max_total_latency_ms: round3(inner.max_latency_ms),
            budget_ms: self.budget_ms,
            within_budget: inner.within_budget,
            within_budget_rate: if total == 0 {
                0.0
            } else {
                round4(inner.within_budget as f64 / total as f64)
            },
        }
    }
}

impl Default for PipelineMetrics {
    fn default() -> Self {
        Self::new(1500)
    }
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * quantile).round() as usize;
    round3(sorted[index.min(sorted.len() - 1)])
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::orchestrator::{DegradationReason, PipelineOutcome, PipelineTimings};

    fn outcome(
        degraded: bool,
        reason: Option<DegradationReason>,
        cache_hit: bool,
    ) -> PipelineOutcome {
        PipelineOutcome {
            render_type: "SINGLE_FILE_HTML".to_string(),
            strategy: if degraded { "template" } else { "dynamic" }.to_string(),
            version: "v1".to_string(),
            html_content: "<!DOCTYPE html><html></html>".to_string(),
            degraded,
            degradation_reason: reason,
            degradation_detail: None,
            cache_hit,
            model: None,
            prompt_hash: None,
            usage: None,
            estimated_cost_usd: 0.0,
            attempts: 0,
            timings: PipelineTimings {
                total_ms: 100.0,
                ..PipelineTimings::default()
            },
            obfuscated: false,
        }
    }

    #[test]
    fn snapshot_counts_success_and_degradation() {
        let metrics = PipelineMetrics::new(1500);
        metrics.record(&outcome(false, None, false));
        metrics.record(&outcome(true, Some(DegradationReason::ModelTimeout), true));
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.total_requests, 2);
        assert_eq!(snapshot.dynamic_success, 1);
        assert_eq!(snapshot.degraded_requests, 1);
        assert_eq!(snapshot.cache_hits, 1);
        assert_eq!(snapshot.cache_misses, 1);
        assert_eq!(snapshot.within_budget, 2);
        assert_eq!(snapshot.degradation_reasons[0].reason, "model_timeout");
    }
}
