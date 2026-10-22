//! 集成测试：意图理解与 Tool Call Agent HTTP 契约。
//!
//! 覆盖验收点：
//! - 示例句解析（含价格/日期条件）；
//! - 指代消解（页面实体优先、多候选澄清）；
//! - 非法模型输出拦截、重试与最终澄清；
//! - 多意图分解与执行顺序；
//! - 鉴权、用户隔离与参数校验。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use http_body_util::BodyExt;
use intimeui_server::build_app;
use intimeui_server::config::AppConfig;
use intimeui_server::intent_agent::model::{Clock, ModelError, ModelInput};
use intimeui_server::intent_agent::rules::RuleBasedModel;
use intimeui_server::intent_agent::{IntentModel, ToolCallAgent};
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

fn fixed_agent() -> Arc<ToolCallAgent> {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(
        Utc.with_ymd_and_hms(2026, 10, 22, 8, 0, 0).unwrap(),
    ));
    let model: Arc<dyn IntentModel> = Arc::new(RuleBasedModel::new(clock.clone()));
    Arc::new(ToolCallAgent::with_clock(model, 2, clock))
}

struct ScriptedModel {
    outputs: Mutex<VecDeque<Result<String, ModelError>>>,
}

impl ScriptedModel {
    fn new(outputs: Vec<Result<String, ModelError>>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into()),
        }
    }
}

impl IntentModel for ScriptedModel {
    fn name(&self) -> &'static str {
        "scripted"
    }

    fn generate(&self, _input: &ModelInput<'_>) -> Result<String, ModelError> {
        self.outputs
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(ModelError::Unavailable("no more outputs".to_string())))
    }
}

fn test_config() -> AppConfig {
    AppConfig {
        rate_limit_enabled: false,
        ..AppConfig::default()
    }
}

async fn app_with_agent(agent: Arc<ToolCallAgent>) -> axum::Router {
    let db = intimeui_server::db::Db::connect_in_memory()
        .await
        .expect("failed to open in-memory database");
    db.migrate().await.expect("failed to run migrations");
    db.seed().await.expect("failed to load seed data");
    build_app(AppState::new(test_config(), db).with_intent_agent(agent))
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

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

fn post_json(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn with_bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}

fn order_context() -> Value {
    json!({
        "scene_code": "ORDER_DETAIL_PAGE",
        "entity_id": "ORD_20261014_001",
        "entity_type": "order"
    })
}

fn refund_data_context() -> Value {
    json!({
        "orders": [{
            "id": "ORD_20261014_001",
            "status": "PAID",
            "items": [
                {
                    "id": "ITM_1",
                    "name": "智能降噪耳机",
                    "price": 299.0,
                    "created_at": "2026-10-21T09:00:00Z",
                    "status": "PAID"
                },
                {
                    "id": "ITM_2",
                    "name": "数据线",
                    "price": 29.0,
                    "created_at": "2026-10-21T09:00:00Z",
                    "status": "PAID"
                }
            ]
        }]
    })
}

fn valid_delete_decision() -> String {
    json!({
        "status": "READY",
        "intents": [{
            "intent_id": "intent_1",
            "intent_type": "DELETE",
            "summary": "删除订单",
            "confidence": 0.9,
            "requires_confirmation": true,
            "match_mode": "all",
            "conditions": [],
            "tool_calls": [{
                "call_id": "call_1_1",
                "capability": "delete_record",
                "parameters": {"entity_type": "order"},
                "depends_on": [],
                "description": "删除订单"
            }]
        }],
        "execution_order": ["intent_1"],
        "clarification": null
    })
    .to_string()
}

#[tokio::test]
async fn understand_requires_authentication() {
    let app = app_with_agent(fixed_agent()).await;
    let (status, body) = send(
        &app,
        post_json("/api/v1/intent/understand", json!({"text": "查余额"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40100);
}

#[tokio::test]
async fn understand_rejects_user_id_mismatch() {
    let app = app_with_agent(fixed_agent()).await;
    let request = with_bearer(
        post_json(
            "/api/v1/intent/understand",
            json!({"user_id": "usr_other", "text": "查余额"}),
        ),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], 40300);
}

#[tokio::test]
async fn understand_parses_refund_example() {
    let app = app_with_agent(fixed_agent()).await;
    let request = with_bearer(
        post_json(
            "/api/v1/intent/understand",
            json!({
                "text": "帮我退掉昨天买的那个超 50 元的商品",
                "page_context": order_context(),
                "data_context": refund_data_context()
            }),
        ),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["status"], "READY");
    let intent = &body["data"]["decision"]["intents"][0];
    assert_eq!(intent["intent_type"], "REFUND");
    assert_eq!(intent["requires_confirmation"], true);
    assert_eq!(intent["tool_calls"][0]["capability"], "update_item_status");
    assert_eq!(intent["tool_calls"][0]["parameters"]["status"], "REFUNDED");
    assert_eq!(intent["tool_calls"][0]["parameters"]["item_id"], "ITM_1");
}

#[tokio::test]
async fn understand_resolves_pronoun_to_page_entity() {
    let app = app_with_agent(fixed_agent()).await;
    let request = with_bearer(
        post_json(
            "/api/v1/intent/understand",
            json!({
                "text": "把它删了",
                "page_context": order_context()
            }),
        ),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["status"], "READY");
    let call = &body["data"]["decision"]["intents"][0]["tool_calls"][0];
    assert_eq!(call["capability"], "delete_record");
    assert_eq!(call["parameters"]["entity_type"], "order");
    assert_eq!(call["parameters"]["entity_id"], "ORD_20261014_001");
}

#[tokio::test]
async fn understand_ambiguous_context_returns_clarification() {
    let app = app_with_agent(fixed_agent()).await;
    let request = with_bearer(
        post_json(
            "/api/v1/intent/understand",
            json!({
                "text": "把订单删了",
                "data_context": {
                    "orders": [
                        {"id": "ORD_1", "status": "PAID"},
                        {"id": "ORD_2", "status": "PAID"}
                    ]
                }
            }),
        ),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["status"], "NEEDS_CLARIFICATION");
    assert_eq!(body["data"]["meta"]["attempts"], 1);
    assert!(body["data"]["decision"].is_null());
    let options = body["data"]["clarification"]["options"]
        .as_array()
        .expect("options is array");
    assert_eq!(options.len(), 2);
}

#[tokio::test]
async fn understand_unknown_request_returns_clarification() {
    let app = app_with_agent(fixed_agent()).await;
    let request = with_bearer(
        post_json("/api/v1/intent/understand", json!({"text": "帮我弄一下"})),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["data"]["status"], "NEEDS_CLARIFICATION",
        "body: {body}"
    );
    assert!(body["data"]["clarification"]["question"]
        .as_str()
        .unwrap_or_default()
        .contains("请补充"));
}

#[tokio::test]
async fn understand_multi_intent_keeps_order_and_dependencies() {
    let app = app_with_agent(fixed_agent()).await;
    let request = with_bearer(
        post_json(
            "/api/v1/intent/understand",
            json!({
                "text": "先查一下我的余额，然后查询订单 ORD_20261014_001 的详情"
            }),
        ),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["status"], "READY", "body: {body}");
    assert_eq!(
        body["data"]["decision"]["intents"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        body["data"]["decision"]["execution_order"],
        json!(["intent_1", "intent_2"])
    );
    assert_eq!(
        body["data"]["decision"]["intents"][1]["tool_calls"][0]["depends_on"],
        json!(["call_1_1"])
    );
}

#[tokio::test]
async fn invalid_model_output_is_retried_then_accepted() {
    let model = Arc::new(ScriptedModel::new(vec![
        Ok("not-json".to_string()),
        Ok(valid_delete_decision()),
    ]));
    let app = app_with_agent(Arc::new(ToolCallAgent::new(model, 2))).await;
    let request = with_bearer(
        post_json(
            "/api/v1/intent/understand",
            json!({
                "text": "把订单删了",
                "page_context": order_context()
            }),
        ),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["status"], "READY");
    assert_eq!(body["data"]["meta"]["attempts"], 2);
    assert_eq!(
        body["data"]["decision"]["intents"][0]["tool_calls"][0]["parameters"]["entity_id"],
        "ORD_20261014_001"
    );
}

#[tokio::test]
async fn invalid_model_output_is_intercepted_after_retries() {
    let model = Arc::new(ScriptedModel::new(vec![
        Ok("not-json".to_string()),
        Ok("still-not-json".to_string()),
    ]));
    let app = app_with_agent(Arc::new(ToolCallAgent::new(model, 2))).await;
    let request = with_bearer(
        post_json("/api/v1/intent/understand", json!({"text": "把订单删了"})),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["status"], "NEEDS_CLARIFICATION");
    assert_eq!(body["data"]["meta"]["attempts"], 2);
    assert!(!body["data"]["validation_errors"]
        .as_array()
        .expect("validation_errors is array")
        .is_empty());
}

#[tokio::test]
async fn understand_rejects_blank_text_and_unknown_fields() {
    let app = app_with_agent(fixed_agent()).await;

    let request = with_bearer(
        post_json("/api/v1/intent/understand", json!({"text": "   "})),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);

    let request = with_bearer(
        post_json(
            "/api/v1/intent/understand",
            json!({"text": "查余额", "unexpected": true}),
        ),
        "dev:9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);
}

#[tokio::test]
async fn tools_endpoint_returns_capability_catalog() {
    let app = app_with_agent(fixed_agent()).await;
    let request = with_bearer(get("/api/v1/intent/tools"), "dev:9527");
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    let tools = body["data"].as_array().expect("tools is array");
    assert_eq!(tools.len(), 6);
    assert!(tools.iter().any(|tool| tool["name"] == "delete_record"));
    assert!(tools.iter().any(|tool| {
        tool["function"]["name"] == "query_order_business_data"
            && tool["function"]["parameters"]["type"] == "object"
    }));
}
