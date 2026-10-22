//! 输出混淆验收测试（REQ-202610-1184）。
//!
//! 覆盖：混淆前后校验器均通过、关键内容 / 内联脚本语义保留、类名与变量名
//! 已随机化、同一页面两次请求不同、同会话类名映射稳定、HTTP 接口契约。

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use intimeui_server::build_app;
use intimeui_server::config::AppConfig;
use intimeui_server::html_validator::validate_single_file_html;
use intimeui_server::obfuscation::{
    obfuscate_html, ObfuscationConfig, ObfuscationLevel, ObfuscationStats,
};
use intimeui_server::state::AppState;
use intimeui_server::ui_template::{DeviceInfo, TemplateEngine};
use serde_json::{json, Value};
use tower::ServiceExt;

fn device() -> DeviceInfo {
    DeviceInfo {
        screen_width: 390,
        screen_height: 844,
        platform: "android".to_string(),
    }
}

fn order_detail_html() -> String {
    let data = json!({
        "order_id": "ORD_20261014_001",
        "status": "PAID",
        "items": [
            { "name": "智能降噪耳机", "price": 299.00, "qty": 1 },
            { "name": "机械键盘", "price": 499.00, "qty": 2 }
        ]
    });
    TemplateEngine::global()
        .render("ORDER_DETAIL_PAGE", &data, &device())
        .expect("order detail should render")
        .html
}

fn aggressive() -> ObfuscationConfig {
    ObfuscationConfig {
        level: ObfuscationLevel::Aggressive,
        ..ObfuscationConfig::default()
    }
}

fn class_tokens(html: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut rest = html;
    while let Some(index) = rest.find("class=\"") {
        let start = index + "class=\"".len();
        let Some(end) = rest[start..].find('"') else {
            break;
        };
        for token in rest[start..start + end].split_whitespace() {
            tokens.insert(token.to_string());
        }
        rest = &rest[start + end + 1..];
    }
    tokens
}

#[test]
fn obfuscated_order_detail_remains_valid_and_keeps_functional_content() {
    let original = order_detail_html();
    assert!(
        validate_single_file_html(&original).valid,
        "source template must pass validator"
    );

    let result = obfuscate_html(&original, &aggressive(), Some("sess_acceptance")).unwrap();
    let validation = validate_single_file_html(&result.html);
    assert!(
        validation.valid,
        "obfuscated output: {:?}",
        validation.issues
    );

    assert!(result.html.starts_with("<!DOCTYPE html>"));
    assert!(result.html.trim_end().ends_with("</html>"));
    assert!(result.html.contains("ORD_20261014_001"));
    assert!(result.html.contains("智能降噪耳机"));
    assert!(result.html.contains("机械键盘"));
    assert!(result.html.contains("299.00"));
    assert!(result.html.contains("刷新订单状态"));
    assert!(result.html.contains("复制订单号"));

    // 内联事件绑定与全局对象不能被变量名混淆破坏。
    assert!(result
        .html
        .contains("fetch('/api/v1/orders/ORD_20261014_001/business-data'"));
    assert!(result.html.contains("IntimeUI.token()"));
    assert!(result
        .html
        .contains("document.getElementById('order-status')"));
    assert!(result.html.contains("localStorage"));
    assert!(result.html.contains("window.IntimeUI"));

    // 原始类名不应再出现在 class 属性中；CSS 选择器同步改名。
    assert!(!result.html.contains("class=\"order-hero\""));
    assert!(!result.html.contains("class=\"page\""));
    assert!(!result.html.contains(".order-hero {"));

    assert!(result.stats.classes_total >= 10);
    assert_eq!(result.stats.classes_renamed, result.stats.classes_total);
    assert!(result.stats.identifiers_total >= 8);
    assert!(result.stats.identifier_coverage >= 0.8);
    assert!(result.stats.wrappers_added > 0);
    assert!(result.stats.decoys_added > 0);
    assert!(result.stats.attributes_shuffled > 0);
    assert!(result.stats.bytes_after > 0);
}

#[test]
fn two_requests_for_same_page_produce_different_output() {
    let original = order_detail_html();
    let first = obfuscate_html(&original, &aggressive(), None).unwrap();
    let second = obfuscate_html(&original, &aggressive(), None).unwrap();
    assert_ne!(first.html, second.html);
    assert_ne!(first.stats.classes_renamed, 0);
}

#[test]
fn same_session_reuses_class_mapping() {
    let original = order_detail_html();
    let first = obfuscate_html(&original, &aggressive(), Some("sess_same")).unwrap();
    let second = obfuscate_html(&original, &aggressive(), Some("sess_same")).unwrap();
    assert_eq!(class_tokens(&first.html), class_tokens(&second.html));
    assert_ne!(
        first.html, second.html,
        "wrappers/attribute order should vary"
    );
}

#[test]
fn different_sessions_use_different_class_names() {
    let original = order_detail_html();
    let first = obfuscate_html(&original, &aggressive(), Some("sess_a")).unwrap();
    let second = obfuscate_html(&original, &aggressive(), Some("sess_b")).unwrap();
    assert_ne!(class_tokens(&first.html), class_tokens(&second.html));
}

#[test]
fn reserved_globals_and_property_access_are_not_renamed() {
    let original = order_detail_html();
    let result = obfuscate_html(&original, &aggressive(), Some("sess_reserved")).unwrap();
    for needle in [
        "document.documentElement",
        "window.matchMedia",
        "navigator.clipboard",
        "localStorage.getItem",
        "fetch(",
        "IntimeUI.token",
        "textContent",
    ] {
        assert!(result.html.contains(needle), "missing `{needle}`");
    }
}

#[tokio::test]
async fn obfuscate_api_returns_html_and_stats() {
    let app = test_app().await;
    let html = order_detail_html();
    let payload = json!({
        "html": html,
        "session_id": "sess_api",
        "obfuscation": { "level": "aggressive" }
    });
    let request = with_bearer(post_json("/api/v1/ui/obfuscate", payload), "dev:9527");
    let (status, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    assert!(body["data"]["html"]
        .as_str()
        .unwrap()
        .contains("ORD_20261014_001"));
    let stats: ObfuscationStats =
        serde_json::from_value(body["data"]["stats"].clone()).expect("stats shape");
    assert!(stats.wrappers_added > 0);
    assert!(stats.identifier_coverage >= 0.8);
}

#[tokio::test]
async fn obfuscate_api_requires_authentication() {
    let app = test_app().await;
    let payload = json!({ "html": "<!DOCTYPE html><html><body>x</body></html>" });
    let (status, body) = send(&app, post_json("/api/v1/ui/obfuscate", payload)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40100);
}

#[tokio::test]
async fn ui_render_two_calls_differ() {
    let app = test_app().await;
    let payload = json!({
        "scene_code": "ORDER_DETAIL_PAGE",
        "business_data": {
            "order_id": "ORD_20261014_001",
            "status": "PAID",
            "items": [{ "name": "智能降噪耳机", "price": 299.00, "qty": 1 }]
        }
    });
    let first = with_bearer(post_json("/api/v1/ui/render", payload.clone()), "dev:9527");
    let second = with_bearer(post_json("/api/v1/ui/render", payload), "dev:9527");
    let (status, first) = send(&app, first).await;
    assert_eq!(status, StatusCode::OK);
    let (status, second) = send(&app, second).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(first["data"]["html"], second["data"]["html"]);
}

fn test_config() -> AppConfig {
    AppConfig {
        rate_limit_enabled: false,
        ..AppConfig::default()
    }
}

async fn test_app() -> axum::Router {
    let db = intimeui_server::db::Db::connect_in_memory()
        .await
        .expect("failed to open in-memory database");
    db.migrate().await.expect("failed to run migrations");
    db.seed().await.expect("failed to load seed data");
    build_app(AppState::new(test_config(), db))
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
