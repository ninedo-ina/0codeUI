//! 埋点事件采集与查询端点（REQ-202610-1174，PRD §3.2）。
//!
//! - `POST /api/v1/telemetry/event`：单条 / 批量上报；
//! - `GET  /api/v1/telemetry/events`：按用户维度查询明细（可过滤页面与时间）；
//! - `GET  /api/v1/telemetry/aggregate`：按用户维度聚合（总体 + 分页面）。
//!
//! 所有端点均需鉴权；用户只能读写自己的埋点数据（`user_id` 与鉴权用户不一致返回 403）。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Duration, Utc};

use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::repository::Repository;
use crate::response::ApiResponse;
use crate::state::AppState;
use crate::telemetry_events::{
    validate_finite, validate_identifier, TelemetryAggregate, TelemetryEvent, TelemetryEventInput,
    TelemetryEventPage, TelemetryFilter, TelemetryIngestPayload, TelemetryIngestResult,
};
use crate::validation::ValidatedJson;

/// 事件时间允许的未来偏移（秒）。
const MAX_FUTURE_SKEW_SECS: i64 = 300;
/// 事件时间允许的最大历史跨度（秒）：90 天。
const MAX_PAST_AGE_SECS: i64 = 90 * 24 * 3600;
/// 明细查询默认页大小。
const DEFAULT_LIMIT: i64 = 50;
/// 明细查询最大页大小。
const MAX_LIMIT: i64 = 200;

/// `POST /api/v1/telemetry/event`
///
/// 兼容两种载荷：
/// ```json
/// { "user_id": "u1", "page_id": "home", "dwell_time": 1200,
///   "element_click_rate": 0.3, "misclick_rate": 0.01, "scroll_depth": 0.8,
///   "timestamp": "2026-10-22T06:00:00Z" }
/// ```
/// 或批量 `{ "events": [ ... ] }`。
pub async fn ingest_event(
    State(state): State<AppState>,
    user: AuthUser,
    ValidatedJson(payload): ValidatedJson<TelemetryIngestPayload>,
) -> Result<Json<ApiResponse<TelemetryIngestResult>>, AppError> {
    let inputs = match payload {
        TelemetryIngestPayload::Single(event) => vec![event],
        TelemetryIngestPayload::Batch(batch) => batch.events,
    };

    let max_batch = state.telemetry_quota.max_batch_size();
    if inputs.len() > max_batch {
        return Err(AppError::invalid_param(format!(
            "batch size {} exceeds the limit of {max_batch}",
            inputs.len()
        )));
    }

    let events = build_events(inputs, &user.user_id)?;

    // 防刷配额：按事件条数扣减，超出返回 429 + Retry-After。
    state
        .telemetry_quota
        .check(&user.user_id, events.len())
        .map_err(AppError::rate_limited)?;

    let repository = Repository::from_db(&state.db);
    let report = repository.insert_telemetry_events(&events).await?;
    Ok(Json(ApiResponse::success(report)))
}

/// `GET /api/v1/telemetry/events?user_id=&page_id=&from=&to=&limit=&offset=`
pub async fn list_events(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<TelemetryEventPage>>, AppError> {
    let user_id = resolve_query_user(&params, &user.user_id)?;
    let parsed = parse_telemetry_query(&params)?;

    let repository = Repository::from_db(&state.db);
    let (items, total) = repository
        .list_telemetry_events(&user_id, &parsed.filter, parsed.limit, parsed.offset)
        .await?;

    Ok(Json(ApiResponse::success(TelemetryEventPage {
        items,
        total,
        limit: parsed.limit,
        offset: parsed.offset,
    })))
}

/// `GET /api/v1/telemetry/aggregate?user_id=&page_id=&from=&to=`
pub async fn aggregate_events(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<TelemetryAggregate>>, AppError> {
    let user_id = resolve_query_user(&params, &user.user_id)?;
    let parsed = parse_telemetry_query(&params)?;

    let repository = Repository::from_db(&state.db);
    let aggregate = repository
        .aggregate_telemetry_events(&user_id, &parsed.filter)
        .await?;

    Ok(Json(ApiResponse::success(aggregate)))
}

/// 校验并转换上报事件；`user_id` 必须与鉴权用户一致。
fn build_events(
    inputs: Vec<TelemetryEventInput>,
    auth_user_id: &str,
) -> Result<Vec<TelemetryEvent>, AppError> {
    let now = Utc::now();
    let mut events = Vec::with_capacity(inputs.len());

    for input in inputs {
        let user_id = input.user_id.unwrap_or_else(|| auth_user_id.to_string());
        if user_id != auth_user_id {
            return Err(AppError::forbidden());
        }

        validate_identifier("user_id", &user_id, 64).map_err(AppError::invalid_param)?;
        validate_identifier("page_id", &input.page_id, 128).map_err(AppError::invalid_param)?;
        validate_finite("element_click_rate", input.element_click_rate)
            .map_err(AppError::invalid_param)?;
        validate_finite("misclick_rate", input.misclick_rate).map_err(AppError::invalid_param)?;
        validate_finite("scroll_depth", input.scroll_depth).map_err(AppError::invalid_param)?;

        let timestamp = input.timestamp.unwrap_or(now);
        validate_event_time(timestamp, now)?;

        events.push(TelemetryEvent {
            id: new_id("TELE"),
            event_id: input.event_id.unwrap_or_else(|| new_id("EVT")),
            user_id,
            page_id: input.page_id,
            dwell_time: input.dwell_time,
            element_click_rate: input.element_click_rate,
            misclick_rate: input.misclick_rate,
            scroll_depth: input.scroll_depth,
            timestamp,
            received_at: now,
        });
    }

    Ok(events)
}

/// 事件时间必须落在「90 天前 ~ 未来 5 分钟」内。
fn validate_event_time(timestamp: DateTime<Utc>, now: DateTime<Utc>) -> Result<(), AppError> {
    if timestamp > now + Duration::seconds(MAX_FUTURE_SKEW_SECS) {
        return Err(AppError::invalid_param(
            "timestamp must not be more than 5 minutes in the future",
        ));
    }
    if timestamp < now - Duration::seconds(MAX_PAST_AGE_SECS) {
        return Err(AppError::invalid_param(
            "timestamp must not be older than 90 days",
        ));
    }
    Ok(())
}

/// 查询用户：缺省取鉴权用户，显式指定时必须是本人。
fn resolve_query_user(
    params: &HashMap<String, String>,
    auth_user_id: &str,
) -> Result<String, AppError> {
    match params.get("user_id") {
        Some(value) if value != auth_user_id => Err(AppError::forbidden()),
        Some(value) => {
            validate_identifier("user_id", value, 64).map_err(AppError::invalid_param)?;
            Ok(value.clone())
        }
        None => Ok(auth_user_id.to_string()),
    }
}

struct ParsedTelemetryQuery {
    filter: TelemetryFilter,
    limit: i64,
    offset: i64,
}

/// 解析页面 / 时间过滤与分页参数。
fn parse_telemetry_query(
    params: &HashMap<String, String>,
) -> Result<ParsedTelemetryQuery, AppError> {
    let page_id = params.get("page_id").cloned();
    if let Some(page_id) = &page_id {
        validate_identifier("page_id", page_id, 128).map_err(AppError::invalid_param)?;
    }

    let from = params
        .get("from")
        .map(|value| parse_rfc3339("from", value))
        .transpose()?;
    let to = params
        .get("to")
        .map(|value| parse_rfc3339("to", value))
        .transpose()?;
    if let (Some(from), Some(to)) = (from.as_ref(), to.as_ref()) {
        if from > to {
            return Err(AppError::invalid_param(
                "from must be earlier than or equal to to",
            ));
        }
    }

    Ok(ParsedTelemetryQuery {
        filter: TelemetryFilter { page_id, from, to },
        limit: parse_bounded_i64(params, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?,
        offset: parse_bounded_i64(params, "offset", 0, 0, 1_000_000)?,
    })
}

fn parse_rfc3339(field: &str, value: &str) -> Result<DateTime<Utc>, AppError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| AppError::invalid_param(format!("{field} must be an RFC3339 timestamp")))
}

fn parse_bounded_i64(
    params: &HashMap<String, String>,
    field: &str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, AppError> {
    match params.get(field) {
        None => Ok(default),
        Some(raw) => {
            let value: i64 = raw
                .parse()
                .map_err(|_| AppError::invalid_param(format!("{field} must be an integer")))?;
            if value < min || value > max {
                return Err(AppError::invalid_param(format!(
                    "{field} must be within {min}..={max}"
                )));
            }
            Ok(value)
        }
    }
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}
