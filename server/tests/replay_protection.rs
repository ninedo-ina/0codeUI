//! 集成测试：API 防重放签名（REQ-202610-1186）。
//!
//! 覆盖三条写接口（UI 生成 / 意图执行 / 埋点上报）的正常签名、重放、
//! 篡改、时间窗外、缺头、Nonce 清理与跨用户隔离。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::Utc;
use http_body_util::BodyExt;
use intimeui_server::build_app;
use intimeui_server::config::AppConfig;
use intimeui_server::db::Db;
use intimeui_server::replay::{new_nonce, ReplaySigner, SigningSecret};
use intimeui_server::repository::Repository;
use intimeui_server::state::AppState;
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &str = "test-secret-0123456789abcdef0123456789abcdef";

fn test_config() -> AppConfig {
    AppConfig {
        rate_limit_enabled: false,
        telemetry_quota_enabled: false,
        ..AppConfig::default()
    }
}

fn protected_config() -> AppConfig {
    AppConfig {
        replay_protection_enabled: true,
        replay_signing_secret: Some(SigningSecret::new(SECRET)),
        ..test_config()
    }
}

async fn app_with(config: AppConfig) -> (axum::Router, Db) {
    let db = Db::connect_in_memory()
        .await
        .expect("failed to open in-memory database");
    db.migrate().await.expect("failed to run migrations");
    db.seed().await.expect("failed to load seed data");
    (build_app(AppState::new(config, db.clone())), db)
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

fn post_json(path: &str, body: &[u8]) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer dev:usr_9527")
        .body(Body::from(body.to_vec()))
        .unwrap()
}

fn sign_request(
    path: &str,
    body: &[u8],
    user_id: &str,
    timestamp: i64,
    nonce: &str,
) -> Request<Body> {
    let signed = ReplaySigner::new(SECRET).sign("POST", path, timestamp, nonce, user_id, body);
    let mut request = post_json(path, body);
    signed.insert_into(request.headers_mut());
    request
}

fn signed_payload(path: &str, payload: &Value, user_id: &str, nonce: &str) -> Request<Body> {
    let body = serde_json::to_vec(payload).unwrap();
    sign_request(path, &body, user_id, Utc::now().timestamp(), nonce)
}

fn telemetry_payload(user_id: &str) -> Value {
    json!({
        "user_id": user_id,
        "page_id": "page_home",
        "dwell_time": 1200,
        "element_click_rate": 0.2,
        "misclick_rate": 0.01,
        "scroll_depth": 0.5,
        "timestamp": Utc::now().to_rfc3339()
    })
}

fn intent_payload(user_id: &str) -> Value {
    json!({
        "user_id": user_id,
        "input_type": "NATURAL_LANGUAGE",
        "raw_input": "把这个订单的收货地址修改为北京市海淀区中关村南大街1号",
        "current_context": {
            "active_page": "ORDER_DETAIL_PAGE",
            "entity_id": "ORD_20261014_001"
        }
    })
}

fn ui_payload(user_id: &str) -> Value {
    json!({
        "user_id": user_id,
        "scene_code": "ORDER_DETAIL_PAGE",
        "device_info": {
            "screen_width": 390,
            "screen_height": 844,
            "platform": "android"
        },
        "business_data": {
            "order_id": "ORD_20261014_001",
            "status": "PAID",
            "items": [{ "name": "智能降噪耳机", "price": 299.00, "qty": 1 }]
        }
    })
}

#[tokio::test]
async fn signed_telemetry_request_is_accepted() {
    let (app, _) = app_with(protected_config()).await;
    let payload = telemetry_payload("usr_9527");
    let request = signed_payload(
        "/api/v1/telemetry/event",
        &payload,
        "usr_9527",
        "nonce-telemetry-0001",
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["accepted"], 1);
}

#[tokio::test]
async fn signed_ui_generate_request_is_accepted() {
    let (app, _) = app_with(protected_config()).await;
    let payload = ui_payload("usr_9527");
    let request = signed_payload(
        "/api/v1/ui/generate",
        &payload,
        "usr_9527",
        "nonce-ui-generate-0001",
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["render_type"], "SINGLE_FILE_HTML");
}

#[tokio::test]
async fn signed_intent_execute_request_is_accepted() {
    let (app, _) = app_with(protected_config()).await;
    let payload = intent_payload("usr_9527");
    let request = signed_payload(
        "/api/v1/intent/execute",
        &payload,
        "usr_9527",
        "nonce-intent-execute-0001",
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["action_type"], "REQUIRE_CONFIRMATION");
}

#[tokio::test]
async fn replayed_nonce_is_rejected_with_409() {
    let (app, _) = app_with(protected_config()).await;
    let payload = telemetry_payload("usr_9527");
    let body = serde_json::to_vec(&payload).unwrap();
    let timestamp = Utc::now().timestamp();
    let nonce = "nonce-replay-00000001";

    let first = sign_request(
        "/api/v1/telemetry/event",
        &body,
        "usr_9527",
        timestamp,
        nonce,
    );
    let (status, _) = send(&app, first).await;
    assert_eq!(status, StatusCode::OK);

    let replay = sign_request(
        "/api/v1/telemetry/event",
        &body,
        "usr_9527",
        timestamp,
        nonce,
    );
    let (status, body) = send(&app, replay).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], 40900);
}

#[tokio::test]
async fn tampered_body_is_rejected_with_401() {
    let (app, _) = app_with(protected_config()).await;
    let original = serde_json::to_vec(&telemetry_payload("usr_9527")).unwrap();
    let tampered = serde_json::to_vec(&telemetry_payload("usr_9999")).unwrap();
    let signed = ReplaySigner::new(SECRET).sign(
        "POST",
        "/api/v1/telemetry/event",
        Utc::now().timestamp(),
        "nonce-tamper-body-0001",
        "usr_9527",
        &original,
    );
    let mut request = post_json("/api/v1/telemetry/event", &tampered);
    signed.insert_into(request.headers_mut());

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40100);
}

#[tokio::test]
async fn tampered_signature_is_rejected_with_401() {
    let (app, _) = app_with(protected_config()).await;
    let payload = telemetry_payload("usr_9527");
    let body = serde_json::to_vec(&payload).unwrap();
    let mut request = sign_request(
        "/api/v1/telemetry/event",
        &body,
        "usr_9527",
        Utc::now().timestamp(),
        "nonce-tamper-signature-0001",
    );
    let signature = request.headers()["x-signature"].to_str().unwrap();
    let replacement = if signature.starts_with('0') { "1" } else { "0" };
    let tampered = format!("{replacement}{}", &signature[1..]);
    request
        .headers_mut()
        .insert("x-signature", tampered.parse().unwrap());

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40100);
}

#[tokio::test]
async fn timestamps_outside_window_are_rejected() {
    let (app, _) = app_with(protected_config()).await;
    let payload = telemetry_payload("usr_9527");
    let body = serde_json::to_vec(&payload).unwrap();
    let now = Utc::now().timestamp();

    for (index, timestamp) in [now - 301, now + 301].into_iter().enumerate() {
        let nonce = format!("nonce-stale-0000000{index}");
        let request = sign_request(
            "/api/v1/telemetry/event",
            &body,
            "usr_9527",
            timestamp,
            &nonce,
        );
        let (status, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], 40000);
    }
}

#[tokio::test]
async fn missing_replay_headers_are_rejected() {
    let (app, _) = app_with(protected_config()).await;
    for (path, payload) in [
        ("/api/v1/telemetry/event", telemetry_payload("usr_9527")),
        ("/api/v1/ui/generate", ui_payload("usr_9527")),
        ("/api/v1/intent/execute", intent_payload("usr_9527")),
    ] {
        let body = serde_json::to_vec(&payload).unwrap();
        let (status, response) = send(&app, post_json(path, &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "path: {path}");
        assert_eq!(response["code"], 40000, "path: {path}");
    }
}

#[tokio::test]
async fn authentication_runs_before_replay_checks() {
    let (app, _) = app_with(protected_config()).await;
    let body = serde_json::to_vec(&telemetry_payload("usr_9527")).unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/telemetry/event")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40100);
}

#[tokio::test]
async fn nonce_is_scoped_per_user() {
    let (app, _) = app_with(protected_config()).await;
    let nonce = "nonce-shared-between-users-0001";

    let first = signed_payload(
        "/api/v1/telemetry/event",
        &telemetry_payload("usr_9527"),
        "usr_9527",
        nonce,
    );
    let (status, _) = send(&app, first).await;
    assert_eq!(status, StatusCode::OK);

    // 另一个用户使用同一 Nonce 不构成重放（主键为 user_id + nonce）。
    let body = serde_json::to_vec(&telemetry_payload("usr_1001")).unwrap();
    let mut second = post_json("/api/v1/telemetry/event", &body);
    // 该请求的鉴权用户是 usr_9527，改用 dev:usr_1001 覆盖凭据。
    second.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer dev:usr_1001".parse().unwrap(),
    );
    ReplaySigner::new(SECRET)
        .sign(
            "POST",
            "/api/v1/telemetry/event",
            Utc::now().timestamp(),
            nonce,
            "usr_1001",
            &body,
        )
        .insert_into(second.headers_mut());
    let (status, body) = send(&app, second).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

#[tokio::test]
async fn expired_nonce_rows_are_cleaned_up() {
    let (_, db) = app_with(test_config()).await;
    let repository = Repository::from_db(&db);
    let now = Utc::now().timestamp();
    let nonce = "nonce-expired-cleanup-0001";

    let first = repository
        .consume_replay_nonce(
            "usr_9527",
            nonce,
            "/api/v1/telemetry/event",
            now - 1000,
            now - 1,
        )
        .await
        .unwrap();
    assert!(first);

    // 过期行在下次消费前被删除，因此同一 Nonce 可再次写入。
    let second = repository
        .consume_replay_nonce("usr_9527", nonce, "/api/v1/telemetry/event", now, now + 600)
        .await
        .unwrap();
    assert!(second);
}

#[tokio::test]
async fn disabled_replay_allows_unsigned_requests() {
    let (app, _) = app_with(test_config()).await;
    let body = serde_json::to_vec(&telemetry_payload("usr_9527")).unwrap();
    let (status, body) = send(&app, post_json("/api/v1/telemetry/event", &body)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

#[test]
fn generated_nonce_is_protocol_compliant() {
    let nonce = new_nonce();
    assert!((16..=128).contains(&nonce.len()));
    assert!(nonce.starts_with("n_"));
    assert_ne!(nonce, new_nonce());
}
