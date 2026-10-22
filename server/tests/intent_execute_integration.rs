//! #09 意图执行接口 × #1180 Tool Call Agent 联调测试。
//!
//! 通过 `POST /api/v1/intent/execute` 验证适配器：
//! - 读类意图直接执行；
//! - 写/删类返回确认卡片；
//! - 会话上下文可延续指代；
//! - 多意图按序进入 `additional_calls`；
//! - UI 事件树回退到 #09 规则规划器。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use http_body_util::BodyExt;
use intimeui_server::build_app;
use intimeui_server::config::AppConfig;
use intimeui_server::intent_agent::model::Clock;
use intimeui_server::intent_agent::rules::RuleBasedModel;
use intimeui_server::intent_agent::{IntentModel, ToolCallAgent, ToolCallPlanner};
use intimeui_server::state::AppState;
use serde_json::{json, Value};
use tower::ServiceExt;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

fn fixed_agent() -> ToolCallAgent {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(
        Utc.with_ymd_and_hms(2026, 10, 22, 8, 0, 0).unwrap(),
    ));
    let model: Arc<dyn IntentModel> = Arc::new(RuleBasedModel::new(clock.clone()));
    ToolCallAgent::with_clock(model, 2, clock)
}

fn test_config() -> AppConfig {
    AppConfig {
        rate_limit_enabled: false,
        ..AppConfig::default()
    }
}

async fn app_with_tool_call_planner() -> axum::Router {
    let db = intimeui_server::db::Db::connect_in_memory()
        .await
        .expect("failed to open in-memory database");
    db.migrate().await.expect("failed to run migrations");
    db.seed().await.expect("failed to load seed data");
    let state = AppState::new(test_config(), db)
        .with_intent_planner(Arc::new(ToolCallPlanner::new(fixed_agent())));
    build_app(state)
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("request failed");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read body")
        .to_bytes();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body is not valid JSON")
    };
    (status, value)
}

fn intent(body: Value) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/v1/intent/execute")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer dev:usr_9527".parse().unwrap(),
    );
    request
}

#[tokio::test]
async fn balance_is_executed_through_intent_execute() {
    let app = app_with_tool_call_planner().await;
    let (status, body) = send(
        &app,
        intent(json!({
            "user_id": "usr_9527",
            "session_id": "sess_agent_balance",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查一下我的账户余额"
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "EXECUTE");
    assert_eq!(
        body["data"]["proposed_changes"]["target_capability"],
        "query_user_balance"
    );
    assert_eq!(
        body["data"]["execution_results"][0]["data"]["balance"].to_string(),
        "1000.00"
    );
}

#[tokio::test]
async fn session_context_carries_pronoun_to_delete_confirmation() {
    let app = app_with_tool_call_planner().await;
    let (status, body) = send(
        &app,
        intent(json!({
            "user_id": "usr_9527",
            "session_id": "sess_agent_context",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查看订单详情",
            "current_context": {
                "active_page": "ORDER_DETAIL_PAGE",
                "entity_id": "ORD_20261014_001"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "EXECUTE");

    let (status, body) = send(
        &app,
        intent(json!({
            "user_id": "usr_9527",
            "session_id": "sess_agent_context",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "把它删了"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "REQUIRE_CONFIRMATION");
    assert_eq!(
        body["data"]["proposed_changes"]["target_capability"],
        "delete_record"
    );
    assert_eq!(
        body["data"]["proposed_changes"]["parameters"],
        json!({"entity_type": "order", "entity_id": "ORD_20261014_001"})
    );
}

#[tokio::test]
async fn multi_intent_is_exposed_as_additional_calls() {
    let app = app_with_tool_call_planner().await;
    let (status, body) = send(
        &app,
        intent(json!({
            "user_id": "usr_9527",
            "session_id": "sess_agent_multi",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "先查一下我的余额，然后查询订单 ORD_20261014_001 的详情"
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "EXECUTE");
    assert_eq!(
        body["data"]["proposed_changes"]["target_capability"],
        "query_user_balance"
    );
    assert_eq!(
        body["data"]["proposed_changes"]["additional_calls"][0]["target_capability"],
        "query_order_business_data"
    );
}

#[tokio::test]
async fn ui_interaction_uses_rule_planner_fallback() {
    let app = app_with_tool_call_planner().await;
    let (status, body) = send(
        &app,
        intent(json!({
            "user_id": "usr_9527",
            "input_type": "UI_INTERACTION",
            "raw_input": {
                "event": {"type": "SUBMIT", "target_id": "btn_save_address"},
                "path": [
                    {"id": "ORDER_DETAIL_PAGE", "type": "page"},
                    {"id": "btn_save_address", "type": "button", "action": "update_order_address"}
                ],
                "data": {
                    "order_id": "ORD_20261014_001",
                    "shipping_address": "北京市朝阳区望京街道1号"
                }
            }
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "REQUIRE_CONFIRMATION");
    assert_eq!(
        body["data"]["proposed_changes"]["target_capability"],
        "update_order_address"
    );
}

#[tokio::test]
async fn clarification_is_returned_as_invalid_param() {
    let app = app_with_tool_call_planner().await;
    let (status, body) = send(
        &app,
        intent(json!({
            "user_id": "usr_9527",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "帮我弄一下"
        })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["code"], 40000);
    assert!(body["msg"].as_str().unwrap_or_default().contains("请补充"));
}
