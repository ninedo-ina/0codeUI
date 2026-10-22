//! 路由注册。
//!
//! - 公开路由：`/`、`/healthz`、`/readyz`、`/api/v1/ping`；
//! - 受保护路由（需鉴权）：`/api/v1/me`、`/api/v1/echo`、
//!   `/api/v1/orders/{order_id}/business-data`、`/api/v1/users/{user_id}/balance`、
//!   `/api/v1/telemetry/events`、`/api/v1/telemetry/aggregate`、
//!   `/api/v1/intent/understand`、`/api/v1/intent/tools`、`/api/v1/ui/render`、
//!   `/api/v1/ui/obfuscate`；
//! - 需鉴权且需防重放签名的写接口（REQ-202610-1186）：
//!   `/api/v1/telemetry/event`、`/api/v1/intent/execute`、`/api/v1/ui/generate`；
//! - 未匹配路径统一返回 404 响应结构。

pub mod example;
pub mod health;
pub mod intent;
pub mod intent_agent;
pub mod model;
pub mod orders;
pub mod pipeline;
pub mod profiles;
pub mod telemetry;
pub mod ui;
pub mod ui_obfuscate;
pub mod ui_render;

use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum::Router;

use crate::error::AppError;
use crate::middleware::{auth, rate_limit, replay};
use crate::state::AppState;

/// 组装全部业务路由（含限流与鉴权）。
pub fn router(state: AppState) -> Router {
    let public_routes = Router::new()
        .route("/", get(example::root))
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/api/v1/ping", get(example::ping));

    // 三条写接口额外要求防重放签名；鉴权层最后添加（最外层），保证先注入 AuthUser。
    let signed_routes = Router::new()
        .route("/api/v1/telemetry/event", post(telemetry::ingest_event))
        .route("/api/v1/intent/execute", post(intent::execute))
        .route("/api/v1/ui/generate", post(ui::generate))
        .route_layer(from_fn_with_state(state.clone(), replay::require_signature))
        .route_layer(from_fn_with_state(state.clone(), auth::require_auth));

    let protected_routes = Router::new()
        .route("/api/v1/me", get(example::me))
        .route("/api/v1/echo", post(example::echo))
        .route(
            "/api/v1/orders/{order_id}/business-data",
            get(orders::order_business_data),
        )
        .route("/api/v1/users/{user_id}/balance", get(orders::user_balance))
        .route(
            "/api/v1/ui/generate/dynamic",
            post(pipeline::generate_dynamic),
        )
        .route("/api/v1/telemetry/events", get(telemetry::list_events))
        .route(
            "/api/v1/telemetry/aggregate",
            get(telemetry::aggregate_events),
        )
        .route("/api/v1/intent/understand", post(intent_agent::understand))
        .route("/api/v1/intent/tools", get(intent_agent::tools))
        .route("/api/v1/ui/render", post(ui_render::render_ui))
        .route("/api/v1/ui/pipeline/metrics", get(pipeline::metrics))
        .route("/api/v1/ui/pipeline/info", get(pipeline::info))
        .route("/api/v1/ui/model/generate", post(model::generate))
        .route("/api/v1/ui/model/stream", post(model::stream))
        .route("/api/v1/ui/model/info", get(model::info))
        .route("/api/v1/ui/model/metrics", get(model::metrics))
        .route("/api/v1/ui/obfuscate", post(ui_obfuscate::obfuscate))
        .route(
            "/api/v1/users/{user_id}/profile",
            get(profiles::get_profile).put(profiles::put_profile),
        )
        .route(
            "/api/v1/users/{user_id}/profile/refresh",
            post(profiles::refresh_profile),
        )
        .route_layer(from_fn_with_state(state.clone(), auth::require_auth));

    Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .merge(signed_routes)
        .fallback(fallback_not_found)
        .layer(from_fn_with_state(
            state.clone(),
            rate_limit::limit_requests,
        ))
        .with_state(state)
}

/// 未匹配路由：统一 404 响应。
async fn fallback_not_found() -> AppError {
    AppError::not_found()
}
