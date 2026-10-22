//! 集成测试：埋点事件采集、校验、配额、幂等与用户维度查询（REQ-202610-1174）。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::{Duration, SecondsFormat, Utc};
use http_body_util::BodyExt;
use intimeui_server::build_app;
use intimeui_server::config::AppConfig;
use intimeui_server::db::Db;
use intimeui_server::state::AppState;
use serde_json::{json, Value};
use tower::ServiceExt;

fn test_config() -> AppConfig {
    AppConfig {
        rate_limit_enabled: false,
        telemetry_quota_enabled: false,
        ..AppConfig::default()
    }
}

async fn app_with(config: AppConfig) -> (axum::Router, Db) {
    let db = Db::connect_in_memory()
        .await
        .expect("failed to open in-memory database");
    db.migrate().await.expect("failed to run migrations");
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

fn now_iso(minutes_ago: i64) -> String {
    (Utc::now() - Duration::minutes(minutes_ago)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn now_iso_secs(seconds_ago: i64) -> String {
    (Utc::now() - Duration::seconds(seconds_ago)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn event(user_id: &str, page_id: &str, dwell_time: i64, minutes_ago: i64) -> Value {
    json!({
        "user_id": user_id,
        "page_id": page_id,
        "dwell_time": dwell_time,
        "element_click_rate": 0.2,
        "misclick_rate": 0.01,
        "scroll_depth": 0.5,
        "timestamp": now_iso(minutes_ago)
    })
}

#[tokio::test]
async fn single_event_is_persisted_and_queryable() {
    let (app, _) = app_with(test_config()).await;

    let payload = json!({
        "user_id": "u100",
        "page_id": "scene:flash_sale",
        "dwell_time": 1200,
        "element_click_rate": 0.35,
        "misclick_rate": 0.02,
        "scroll_depth": 0.8,
        "timestamp": now_iso(1)
    });
    let (status, body) = send(
        &app,
        with_bearer(post_json("/api/v1/telemetry/event", payload), "dev:u100"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["accepted"], 1);
    assert_eq!(body["data"]["duplicates"], 0);
    assert_eq!(body["data"]["event_ids"].as_array().unwrap().len(), 1);

    let (status, body) = send(
        &app,
        with_bearer(get("/api/v1/telemetry/events?user_id=u100"), "dev:u100"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["total"], 1);
    let item = &body["data"]["items"][0];
    assert_eq!(item["user_id"], "u100");
    assert_eq!(item["page_id"], "scene:flash_sale");
    assert_eq!(item["dwell_time"], 1200);
    assert_eq!(item["element_click_rate"], 0.35);
    assert_eq!(item["misclick_rate"], 0.02);
    assert_eq!(item["scroll_depth"], 0.8);
    assert!(item["timestamp"].is_string());
    assert!(item["received_at"].is_string());
}

#[tokio::test]
async fn batch_ingest_and_aggregate_by_user() {
    let (app, _) = app_with(test_config()).await;

    let payload = json!({
        "events": [
            event("u200", "page_home", 1000, 3),
            event("u200", "page_home", 3000, 2),
            event("u200", "page_cart", 2000, 1)
        ]
    });
    let (status, body) = send(
        &app,
        with_bearer(post_json("/api/v1/telemetry/event", payload), "dev:u200"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["accepted"], 3);

    let (status, body) = send(
        &app,
        with_bearer(get("/api/v1/telemetry/aggregate?user_id=u200"), "dev:u200"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["total_events"], 3);
    assert!((data["averages"]["avg_dwell_time"].as_f64().unwrap() - 2000.0).abs() < 1e-9);

    let pages = data["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
    let home = pages
        .iter()
        .find(|page| page["page_id"] == "page_home")
        .unwrap();
    assert_eq!(home["event_count"], 2);
    assert!((home["avg_dwell_time"].as_f64().unwrap() - 2000.0).abs() < 1e-9);
    assert!((home["avg_element_click_rate"].as_f64().unwrap() - 0.2).abs() < 1e-9);
}

#[tokio::test]
async fn invalid_fields_are_rejected() {
    let (app, _) = app_with(test_config()).await;

    let mut invalid_rate = event("u300", "page_home", 1000, 1);
    invalid_rate["element_click_rate"] = json!(1.5);
    let (status, body) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", invalid_rate),
            "dev:u300",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);

    let mut negative_dwell = event("u300", "page_home", 1000, 1);
    negative_dwell["dwell_time"] = json!(-1);
    let (status, body) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", negative_dwell),
            "dev:u300",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);

    let mut with_sensitive_content = event("u300", "page_home", 1000, 1);
    with_sensitive_content["page_content"] = json!("<html>user data</html>");
    let (status, body) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", with_sensitive_content),
            "dev:u300",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);

    let mut missing_page = event("u300", "page_home", 1000, 1);
    missing_page.as_object_mut().unwrap().remove("page_id");
    let (status, body) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", missing_page),
            "dev:u300",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);
}

#[tokio::test]
async fn user_id_must_match_authenticated_user() {
    let (app, _) = app_with(test_config()).await;

    let payload = event("u400", "page_home", 1000, 1);
    let (status, body) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", payload),
            "dev:someone-else",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], 40300);

    let (status, body) = send(
        &app,
        with_bearer(get("/api/v1/telemetry/events?user_id=u400"), "dev:u401"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], 40300);
}

#[tokio::test]
async fn telemetry_requires_authentication() {
    let (app, _) = app_with(test_config()).await;
    let (status, body) = send(
        &app,
        post_json(
            "/api/v1/telemetry/event",
            event("u500", "page_home", 1000, 1),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40100);
}

#[tokio::test]
async fn duplicate_event_id_is_idempotent() {
    let (app, _) = app_with(test_config()).await;

    let mut payload = event("u600", "page_home", 1000, 1);
    payload["event_id"] = json!("evt-600-1");

    let (status, body) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", payload.clone()),
            "dev:u600",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["accepted"], 1);

    let (status, body) = send(
        &app,
        with_bearer(post_json("/api/v1/telemetry/event", payload), "dev:u600"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["accepted"], 0);
    assert_eq!(body["data"]["duplicates"], 1);

    let (_, body) = send(
        &app,
        with_bearer(get("/api/v1/telemetry/events?user_id=u600"), "dev:u600"),
    )
    .await;
    assert_eq!(body["data"]["total"], 1);
}

#[tokio::test]
async fn batch_size_limit_is_enforced() {
    let (app, _) = app_with(test_config()).await;

    let events: Vec<Value> = (0..101_i64)
        .map(|index| event("u700", "page_home", 1000 + index, 1))
        .collect();
    let (status, body) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", json!({ "events": events })),
            "dev:u700",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);

    let (status, body) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", json!({ "events": [] })),
            "dev:u700",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40000);
}

#[tokio::test]
async fn per_user_quota_returns_429() {
    let config = AppConfig {
        rate_limit_enabled: false,
        telemetry_quota_enabled: true,
        telemetry_quota_per_minute: 2,
        telemetry_max_batch_size: 2,
        ..AppConfig::default()
    };
    let (app, _) = app_with(config).await;

    let batch = json!({ "events": [
        event("u800", "page_home", 1000, 2),
        event("u800", "page_home", 1100, 1)
    ] });
    let (status, _) = send(
        &app,
        with_bearer(post_json("/api/v1/telemetry/event", batch), "dev:u800"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &app,
        with_bearer(
            post_json(
                "/api/v1/telemetry/event",
                event("u800", "page_home", 1200, 0),
            ),
            "dev:u800",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], 42900);
}

#[tokio::test]
async fn time_filters_and_pagination_work() {
    let (app, _) = app_with(test_config()).await;

    let events: Vec<Value> = (0..5_i64)
        .map(|index| event("u900", "page_home", 1000 + index, 10 - index))
        .collect();
    let (status, _) = send(
        &app,
        with_bearer(
            post_json("/api/v1/telemetry/event", json!({ "events": events })),
            "dev:u900",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let from = now_iso_secs(8 * 60 + 30);
    let to = now_iso_secs(5 * 60 + 30);
    let path =
        format!("/api/v1/telemetry/events?user_id=u900&from={from}&to={to}&limit=2&offset=1");
    let (status, body) = send(&app, with_bearer(get(&path), "dev:u900")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["total"], 3);
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["data"]["limit"], 2);
    assert_eq!(body["data"]["offset"], 1);
}

#[tokio::test]
async fn schema_stores_only_interaction_metrics() {
    let (_, db) = app_with(test_config()).await;
    use sqlx::Row;

    let rows = sqlx::query("PRAGMA table_info(telemetry_events)")
        .fetch_all(db.pool())
        .await
        .unwrap();
    let columns: Vec<String> = rows
        .iter()
        .map(|row| row.get::<String, _>("name"))
        .collect();
    assert_eq!(
        columns,
        vec![
            "id",
            "event_id",
            "user_id",
            "page_id",
            "dwell_time",
            "element_click_rate",
            "misclick_rate",
            "scroll_depth",
            "occurred_at",
            "received_at",
        ]
    );
}
