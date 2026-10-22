//! 埋点事件领域模型（REQ-202610-1174）。
//!
//! 设计原则（PRD §3.2）：
//! - **数据最小化**：只接收交互指标与定位字段，事件结构 `deny_unknown_fields`，
//!   任何额外字段（例如页面 HTML、URL、文本内容）都会被拒绝，不会落库；
//! - **字段校验**：类型、长度、取值范围与字符集在进入仓储前统一校验；
//! - **批量上报**：支持单条事件与 `{ "events": [...] }` 两种载荷，客户端可先聚合；
//! - **幂等**：客户端可传 `event_id`，同一用户下重复上报会被识别为重复并跳过。

use chrono::{DateTime, Utc};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError, ValidationErrors};

/// 单条埋点事件上报。
///
/// 时间单位约定：`dwell_time` 为毫秒；三个比率字段均为 `0.0..=1.0` 的小数。
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct TelemetryEventInput {
    /// 客户端事件幂等键（可选）；重试同一事件时沿用，服务端据此去重。
    #[validate(length(min = 1, max = 64, message = "event_id must be 1..=64 characters"))]
    pub event_id: Option<String>,

    /// 用户标识；缺省时取当前鉴权用户。显式提供时必须与鉴权用户一致。
    #[validate(length(min = 1, max = 64, message = "user_id must be 1..=64 characters"))]
    pub user_id: Option<String>,

    /// 页面 / 场景标识，1~128 字符，仅允许 `[A-Za-z0-9._:-]`。
    #[validate(length(min = 1, max = 128, message = "page_id must be 1..=128 characters"))]
    pub page_id: String,

    /// 停留时长（毫秒），0 ~ 24 小时。
    #[validate(range(
        min = 0,
        max = 86_400_000,
        message = "dwell_time must be within 0..=86400000 milliseconds"
    ))]
    #[serde(alias = "dwell_time_ms")]
    pub dwell_time: i64,

    /// 控件点击率，`0.0..=1.0`。
    #[validate(range(
        min = 0.0,
        max = 1.0,
        message = "element_click_rate must be within 0.0..=1.0"
    ))]
    pub element_click_rate: f64,

    /// 误触率，`0.0..=1.0`。
    #[validate(range(
        min = 0.0,
        max = 1.0,
        message = "misclick_rate must be within 0.0..=1.0"
    ))]
    pub misclick_rate: f64,

    /// 滚动深度，`0.0..=1.0`。
    #[validate(range(
        min = 0.0,
        max = 1.0,
        message = "scroll_depth must be within 0.0..=1.0"
    ))]
    pub scroll_depth: f64,

    /// 事件发生时间（RFC3339）；缺省为服务端接收时间。
    pub timestamp: Option<DateTime<Utc>>,
}

/// 批量上报载荷（客户端聚合）。
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct TelemetryBatchInput {
    /// 事件列表，至少 1 条；上限由服务端配置 `TELEMETRY_MAX_BATCH_SIZE` 控制。
    #[validate(length(min = 1, message = "events must contain at least one event"))]
    #[validate(nested)]
    pub events: Vec<TelemetryEventInput>,
}

/// 上报载荷：兼容单条事件与批量事件。
#[derive(Debug, Clone)]
pub enum TelemetryIngestPayload {
    /// 批量：`{ "events": [...] }`。
    Batch(TelemetryBatchInput),
    /// 单条：事件对象本身。
    Single(TelemetryEventInput),
}

impl<'de> Deserialize<'de> for TelemetryIngestPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // 不用 `#[serde(untagged)]`：它与内部结构体的 `deny_unknown_fields`
        // 组合时，`serde_json::from_str` 会错误地拒绝合法单条事件。
        // 这里先读成 Value，再按是否存在 `events` 字段分派，语义等价且稳定。
        let value = serde_json::Value::deserialize(deserializer)?;
        let is_batch = value
            .as_object()
            .is_some_and(|object| object.contains_key("events"));
        if is_batch {
            serde_json::from_value(value)
                .map(Self::Batch)
                .map_err(de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Self::Single)
                .map_err(de::Error::custom)
        }
    }
}

impl Validate for TelemetryIngestPayload {
    fn validate(&self) -> Result<(), ValidationErrors> {
        match self {
            Self::Batch(batch) => batch.validate(),
            Self::Single(event) => event.validate(),
        }
    }
}

/// 已落库的埋点事件（查询明细返回结构）。
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TelemetryEvent {
    /// 服务端主键。
    pub id: String,
    /// 客户端幂等键（缺省时由服务端生成）。
    pub event_id: String,
    /// 用户标识。
    pub user_id: String,
    /// 页面 / 场景标识。
    pub page_id: String,
    /// 停留时长（毫秒）。
    pub dwell_time: i64,
    /// 控件点击率。
    pub element_click_rate: f64,
    /// 误触率。
    pub misclick_rate: f64,
    /// 滚动深度。
    pub scroll_depth: f64,
    /// 事件发生时间（客户端时间，缺省为接收时间）。
    pub timestamp: DateTime<Utc>,
    /// 服务端接收时间。
    pub received_at: DateTime<Utc>,
}

/// 上报结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelemetryIngestResult {
    /// 本次实际写入条数。
    pub accepted: usize,
    /// 因 `event_id` 重复而跳过的条数。
    pub duplicates: usize,
    /// 实际写入事件的服务端主键（按提交顺序）。
    pub event_ids: Vec<String>,
}

/// 明细查询过滤条件（用户维度在仓储方法参数中单独传入）。
#[derive(Debug, Clone, Default)]
pub struct TelemetryFilter {
    /// 页面标识。
    pub page_id: Option<String>,
    /// 起始时间（含）。
    pub from: Option<DateTime<Utc>>,
    /// 结束时间（含）。
    pub to: Option<DateTime<Utc>>,
}

/// 明细查询结果（分页）。
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryEventPage {
    /// 当前页事件。
    pub items: Vec<TelemetryEvent>,
    /// 满足过滤条件的总条数。
    pub total: i64,
    /// 本页大小。
    pub limit: i64,
    /// 本页偏移。
    pub offset: i64,
}

/// 指标均值。
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryAverages {
    /// 平均停留时长（毫秒）。
    pub avg_dwell_time: f64,
    /// 平均控件点击率。
    pub avg_element_click_rate: f64,
    /// 平均误触率。
    pub avg_misclick_rate: f64,
    /// 平均滚动深度。
    pub avg_scroll_depth: f64,
}

/// 单页面聚合。
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryPageAggregate {
    /// 页面标识。
    pub page_id: String,
    /// 该页面事件数。
    pub event_count: i64,
    /// 该页面指标均值。
    #[serde(flatten)]
    pub averages: TelemetryAverages,
}

/// 用户维度聚合结果。
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryAggregate {
    /// 用户标识。
    pub user_id: String,
    /// 起始时间（回显过滤条件）。
    pub from: Option<DateTime<Utc>>,
    /// 结束时间（回显过滤条件）。
    pub to: Option<DateTime<Utc>>,
    /// 事件总数。
    pub total_events: i64,
    /// 全部事件指标均值。
    pub averages: TelemetryAverages,
    /// 按页面拆分。
    pub pages: Vec<TelemetryPageAggregate>,
}

/// 标识符字符集校验：只允许字母、数字与 `._:-`，避免把 URL / 文本带进库。
pub fn validate_identifier(field: &str, value: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max_len {
        return Err(format!("{field} must be 1..={max_len} characters"));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
    {
        return Err(format!(
            "{field} may only contain ASCII letters, digits and `._:-`"
        ));
    }
    Ok(())
}

/// 数值型字段的有限性校验（JSON 解析通常已拦截 NaN/Inf，这里做防御）。
pub fn validate_finite(field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{field} must be a finite number"));
    }
    Ok(())
}

/// 构造一条校验错误（供自定义 `Validate` 实现使用）。
pub fn validation_error(field: &'static str, message: &'static str) -> ValidationErrors {
    let mut errors = ValidationErrors::new();
    let mut error = ValidationError::new("invalid");
    error.message = Some(message.into());
    errors.add(field, error);
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_event() -> serde_json::Value {
        json!({
            "user_id": "u100",
            "page_id": "page_home",
            "dwell_time": 1200,
            "element_click_rate": 0.35,
            "misclick_rate": 0.02,
            "scroll_depth": 0.8,
            "timestamp": "2026-10-22T06:00:00Z"
        })
    }

    #[test]
    fn single_payload_parses_and_validates() {
        let payload: TelemetryIngestPayload = serde_json::from_value(valid_event()).unwrap();
        assert!(matches!(payload, TelemetryIngestPayload::Single(_)));
        assert!(payload.validate().is_ok());
    }

    #[test]
    fn batch_payload_parses_and_validates() {
        let payload: TelemetryIngestPayload =
            serde_json::from_value(json!({ "events": [valid_event()] })).unwrap();
        assert!(matches!(payload, TelemetryIngestPayload::Batch(_)));
        assert!(payload.validate().is_ok());
    }

    #[test]
    fn unknown_fields_are_rejected_for_data_minimization() {
        let mut value = valid_event();
        value["page_content"] = json!("<html>sensitive</html>");
        let parsed: Result<TelemetryIngestPayload, _> = serde_json::from_value(value);
        assert!(parsed.is_err(), "unknown fields must not be accepted");
    }

    #[test]
    fn ranges_are_enforced() {
        let mut value = valid_event();
        value["element_click_rate"] = json!(1.5);
        let payload: TelemetryIngestPayload = serde_json::from_value(value).unwrap();
        assert!(payload.validate().is_err());

        let mut value = valid_event();
        value["dwell_time"] = json!(-1);
        let payload: TelemetryIngestPayload = serde_json::from_value(value).unwrap();
        assert!(payload.validate().is_err());
    }

    #[test]
    fn identifier_rules_reject_urls_and_text() {
        assert!(validate_identifier("page_id", "scene:flash_sale", 128).is_ok());
        assert!(validate_identifier("page_id", "https://evil.example/x", 128).is_err());
        assert!(validate_identifier("page_id", "页面 内容", 128).is_err());
        assert!(validate_identifier("page_id", "", 128).is_err());
    }
}
