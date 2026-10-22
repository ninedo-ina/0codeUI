//! 意图执行接口集成测试（REQ-202610-1177）。
//!
//! 覆盖 PRD §5.2 示例、读类直接执行、写类确认、会话上下文传递、
//! UI 事件树、鉴权与跨用户隔离。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use intimeui_server::build_app;
use intimeui_server::config::AppConfig;
use intimeui_server::db::Db;
use intimeui_server::repository::Repository;
use intimeui_server::state::AppState;
use serde_json::{json, Value};
use tower::ServiceExt;

fn test_config() -> AppConfig {
    AppConfig {
        rate_limit_enabled: false,
        ..AppConfig::default()
    }
}

async fn app_and_db(config: AppConfig) -> (axum::Router, Db) {
    let db = Db::connect_in_memory()
        .await
        .expect("failed to open in-memory database");
    db.migrate().await.expect("failed to run migrations");
    db.seed().await.expect("failed to load seed data");
    let app = build_app(AppState::new(config, db.clone()));
    (app, db)
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

fn intent(body: Value, token: &str) -> Request<Body> {
    with_bearer(post_json("/api/v1/intent/execute", body), token)
}

#[tokio::test]
async fn prd_5_2_example_returns_confirmation_without_executing() {
    let (app, db) = app_and_db(test_config()).await;

    let request = intent(
        json!({
            "user_id": "usr_9527",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "把这个订单的收货地址修改为北京市海淀区中关村南大街1号",
            "current_context": {
                "active_page": "ORDER_DETAIL_PAGE",
                "entity_id": "ORD_20261014_001"
            }
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["code"], 0);
    assert_eq!(body["msg"], "ok");

    let data = &body["data"];
    assert_eq!(data["action_type"], "REQUIRE_CONFIRMATION");
    assert_eq!(data["intent_summary"], "更新订单收货地址");
    assert_eq!(
        data["proposed_changes"],
        json!({
            "target_capability": "update_order_address",
            "parameters": {
                "order_id": "ORD_20261014_001",
                "new_address": "北京市海淀区中关村南大街1号"
            }
        })
    );
    assert_eq!(
        data["confirmation_prompt"],
        "确认将订单 ORD_20261014_001 的收货地址修改为【北京市海淀区中关村南大街1号】吗？"
    );
    assert!(data["session_id"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(data["context"]["active_page"], "ORDER_DETAIL_PAGE");
    assert_eq!(data["context"]["entity_id"], "ORD_20261014_001");

    // 写类能力不得自动执行：订单地址保持不变，且没有对应审计记录。
    let repository = Repository::from_db(&db);
    let order = repository
        .find_order("ORD_20261014_001")
        .await
        .expect("seed order");
    assert_eq!(
        order.shipping_address.as_deref(),
        Some("北京市海淀区中关村南大街1号")
    );
    let records = repository
        .list_operation_records("order", "ORD_20261014_001")
        .await
        .unwrap();
    assert!(!records
        .iter()
        .any(|record| record.capability == "update_order_address"));
}

#[tokio::test]
async fn balance_query_executes_and_returns_data() {
    let (app, db) = app_and_db(test_config()).await;

    let request = intent(
        json!({
            "user_id": "usr_9527",
            "session_id": "sess_balance_1",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查一下我的账户余额"
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "EXECUTE");
    assert_eq!(body["data"]["intent_summary"], "查询账户余额");
    assert_eq!(
        body["data"]["proposed_changes"]["target_capability"],
        "query_user_balance"
    );
    assert!(body["data"]["confirmation_prompt"].is_null());

    let results = body["data"]["execution_results"]
        .as_array()
        .expect("execution_results is array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["status"], "SUCCESS");
    assert_eq!(results[0]["data"]["user_id"], "usr_9527");
    assert_eq!(results[0]["data"]["balance"].to_string(), "1000.00");
    assert_eq!(results[0]["data"]["currency"], "CNY");

    let repository = Repository::from_db(&db);
    let records = repository
        .list_operation_records("user", "usr_9527")
        .await
        .unwrap();
    assert!(records
        .iter()
        .any(|record| record.capability == "query_user_balance"));
}

#[tokio::test]
async fn session_context_carries_across_requests() {
    let (app, _db) = app_and_db(test_config()).await;

    let first = intent(
        json!({
            "user_id": "usr_9527",
            "session_id": "sess_context_1",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查看订单详情",
            "current_context": {
                "active_page": "ORDER_DETAIL_PAGE",
                "entity_id": "ORD_20261014_001"
            }
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, first).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "EXECUTE");
    assert_eq!(
        body["data"]["execution_results"][0]["data"]["order_id"],
        "ORD_20261014_001"
    );

    // 第二轮不传 current_context，靠会话状态沿用订单上下文。
    let second = intent(
        json!({
            "user_id": "usr_9527",
            "session_id": "sess_context_1",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "把它删了"
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, second).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "REQUIRE_CONFIRMATION");
    assert_eq!(body["data"]["intent_summary"], "删除订单");
    assert_eq!(
        body["data"]["proposed_changes"]["parameters"],
        json!({"entity_type": "order", "entity_id": "ORD_20261014_001"})
    );
    assert_eq!(body["data"]["context"]["entity_id"], "ORD_20261014_001");
}

#[tokio::test]
async fn ui_interaction_event_tree_updates_address_proposal() {
    let (app, _db) = app_and_db(test_config()).await;

    let request = intent(
        json!({
            "user_id": "usr_9527",
            "input_type": "UI_INTERACTION",
            "raw_input": {
                "event": {"type": "SUBMIT", "target_id": "btn_save_address"},
                "path": [
                    {"id": "ORDER_DETAIL_PAGE", "type": "page"},
                    {"id": "address_form", "type": "form"},
                    {"id": "btn_save_address", "type": "button", "action": "update_order_address"}
                ],
                "data": {
                    "order_id": "ORD_20261014_001",
                    "shipping_address": "北京市朝阳区望京街道1号"
                }
            }
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "REQUIRE_CONFIRMATION");
    assert_eq!(body["data"]["intent_summary"], "更新订单收货地址");
    assert_eq!(
        body["data"]["proposed_changes"]["target_capability"],
        "update_order_address"
    );
    assert_eq!(
        body["data"]["proposed_changes"]["parameters"]["new_address"],
        "北京市朝阳区望京街道1号"
    );
    assert_eq!(body["data"]["context"]["active_page"], "ORDER_DETAIL_PAGE");
}

#[tokio::test]
async fn delete_order_requires_confirmation_and_keeps_record() {
    let (app, db) = app_and_db(test_config()).await;

    let request = intent(
        json!({
            "user_id": "usr_9527",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "删除订单 ORD_20261015_002",
            "current_context": {"active_page": "ORDER_DETAIL_PAGE"}
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["action_type"], "REQUIRE_CONFIRMATION");
    assert_eq!(
        body["data"]["confirmation_prompt"],
        "确认删除订单 ORD_20261015_002吗？"
    );

    let repository = Repository::from_db(&db);
    assert!(repository.find_order("ORD_20261015_002").await.is_ok());
}

#[tokio::test]
async fn cross_user_order_access_is_forbidden() {
    let (app, _db) = app_and_db(test_config()).await;

    let request = intent(
        json!({
            "user_id": "usr_0001",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "把这个订单的收货地址修改为上海市浦东新区世纪大道100号",
            "current_context": {
                "active_page": "ORDER_DETAIL_PAGE",
                "entity_id": "ORD_20261014_001"
            }
        }),
        "dev:usr_0001",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], 40300);
}

#[tokio::test]
async fn body_user_id_must_match_authenticated_user() {
    let (app, _db) = app_and_db(test_config()).await;

    let request = intent(
        json!({
            "user_id": "usr_0001",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查一下我的账户余额"
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], 40300);
}

#[tokio::test]
async fn session_cannot_be_reused_by_another_user() {
    let (app, _db) = app_and_db(test_config()).await;

    let first = intent(
        json!({
            "user_id": "usr_9527",
            "session_id": "sess_shared",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查一下我的账户余额"
        }),
        "dev:usr_9527",
    );
    let (status, _body) = send(&app, first).await;
    assert_eq!(status, StatusCode::OK);

    let second = intent(
        json!({
            "user_id": "usr_0001",
            "session_id": "sess_shared",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查一下我的账户余额"
        }),
        "dev:usr_0001",
    );
    let (status, body) = send(&app, second).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], 40300);
}

#[tokio::test]
async fn expired_session_context_is_ignored() {
    let (app, db) = app_and_db(test_config()).await;

    let first = intent(
        json!({
            "user_id": "usr_9527",
            "session_id": "sess_ttl",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查看订单详情",
            "current_context": {
                "active_page": "ORDER_DETAIL_PAGE",
                "entity_id": "ORD_20261014_001"
            }
        }),
        "dev:usr_9527",
    );
    let (status, _body) = send(&app, first).await;
    assert_eq!(status, StatusCode::OK);

    // 直接把会话时间戳改旧，验证过期后不再沿用上下文。
    let repository = Repository::from_db(&db);
    let mut stale = repository
        .find_intent_session("sess_ttl")
        .await
        .unwrap()
        .expect("session exists");
    stale.updated_at = chrono::Utc::now() - chrono::Duration::hours(2);
    repository.upsert_intent_session(&stale).await.unwrap();

    let second = intent(
        json!({
            "user_id": "usr_9527",
            "session_id": "sess_ttl",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "把它删了"
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, second).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["code"], 40000);
}

#[tokio::test]
async fn unknown_intent_returns_400() {
    let (app, _db) = app_and_db(test_config()).await;
    let request = intent(
        json!({
            "user_id": "usr_9527",
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "今天天气怎么样"
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);
}

#[tokio::test]
async fn invalid_payloads_are_rejected_with_unified_error() {
    let (app, _db) = app_and_db(test_config()).await;

    // 未认证
    let (status, body) = send(
        &app,
        post_json(
            "/api/v1/intent/execute",
            json!({"input_type": "NATURAL_LANGUAGE", "raw_input": "hi"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40100);

    // 未知 input_type
    let request = intent(
        json!({"input_type": "VOICE", "raw_input": "hi"}),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);

    // NATURAL_LANGUAGE 的 raw_input 必须是字符串
    let request = intent(
        json!({"input_type": "NATURAL_LANGUAGE", "raw_input": {"text": "hi"}}),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);

    // 上下文中的实体 ID 只允许安全字符
    let request = intent(
        json!({
            "input_type": "NATURAL_LANGUAGE",
            "raw_input": "查一下我的账户余额",
            "current_context": {"entity_id": "ORD_1; DROP TABLE orders"}
        }),
        "dev:usr_9527",
    );
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);
}
