#![recursion_limit = "256"]

//! InTimeUI 后端服务（M0 基础工程）。
//!
//! 模块划分：
//! - [`config`]：环境变量配置与密钥注入；
//! - [`db`]：SQLite 连接池、迁移与种子数据；
//! - [`error`]：全局错误码规范与错误类型；
//! - [`models`]：核心业务领域模型；
//! - [`repository`]：数据访问层与原子能力实现；
//! - [`replay`]：API 防重放签名协议与校验服务（REQ-202610-1186）；
//! - [`response`]：统一响应结构 `{code, msg, data}`；
//! - [`middleware`]：鉴权 / 限流 / 超时 / 防重放中间件；
//! - [`routes`]：路由注册与端点实现；
//! - [`state`]：应用共享状态；
//! - [`telemetry`]：结构化日志；
//! - [`telemetry_events`]：埋点事件领域模型（REQ-202610-1174）；
//! - [`ui`]：UI 生成契约、场景注册与模板渲染；
//! - [`validation`]：请求参数校验提取器。

pub mod config;
pub mod db;
pub mod error;
pub mod html_validator;
pub mod intent;
pub mod intent_agent;
pub mod middleware;
pub mod model;
pub mod models;
pub mod obfuscation;
pub mod pipeline;
pub mod profile;
pub mod prompt;
pub mod replay;
pub mod repository;
pub mod response;
pub mod routes;
pub mod state;
pub mod telemetry;
pub mod telemetry_events;
pub mod ui;
pub mod ui_template;
pub mod validation;

use std::time::Duration;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::error::ErrorCode;
use crate::response::ApiResponse;
use crate::state::AppState;

/// 组装完整应用（路由 + 网关基础中间件），供 `main` 与集成测试共用。
///
/// 中间件顺序（由外到内）：请求 ID → 访问日志 → panic 兜底 → 超时 →
/// 路由（限流 → 鉴权 → handler）。
pub fn build_app(state: AppState) -> Router {
    let timeout_state = state.clone();

    routes::router(state)
        .layer(axum::middleware::from_fn_with_state(
            timeout_state,
            middleware::timeout::enforce_timeout,
        ))
        .layer(CatchPanicLayer::custom(handle_panic))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::extract::Request| {
                    let request_id = req
                        .headers()
                        .get("x-request-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("-");
                    tracing::info_span!(
                        "http_request",
                        method = %req.method(),
                        path = %req.uri().path(),
                        request_id = %request_id,
                    )
                })
                .on_response(
                    |res: &Response<Body>, latency: Duration, _span: &tracing::Span| {
                        tracing::info!(
                            status = res.status().as_u16(),
                            latency_ms = latency.as_millis() as u64,
                            "request completed"
                        );
                    },
                ),
        )
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

/// panic 兜底：记录日志并返回统一 500 响应，避免连接被直接断开。
fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> Response<Body> {
    let detail = if let Some(text) = err.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = err.downcast_ref::<&'static str>() {
        (*text).to_string()
    } else {
        "unknown panic payload".to_string()
    };
    tracing::error!(panic = %detail, "request handler panicked");

    let body = ApiResponse::<()>::error(
        ErrorCode::Internal.as_i32(),
        ErrorCode::Internal.default_message(),
    );
    (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(body)).into_response()
}
