//! REQ-202610-1190 端到端验收：场景、性能、降级与安全抽验。
//!
//! 覆盖验收标准：
//! 1. 订单详情页模板模式 + 动态模式；
//! 2. 抢购页动态 UI + 意图操作 + 高并发响应；
//! 3. 静态 ≤200ms / 动态 ≤1.5s；
//! 4. 断模型 / 超时 / 校验失败均静态兜底且无白屏；
//! 5. 防重放与输出混淆抽验。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::Utc;
use http_body_util::BodyExt;
use intimeui_server::build_app;
use intimeui_server::config::AppConfig;
use intimeui_server::db::Db;
use intimeui_server::model::{
    ChatRequest, ChatResponse, ModelConfig, ModelError, ModelGateway, ModelProvider, ModelStream,
    StreamChunk, Usage,
};
use intimeui_server::pipeline::PipelineConfig;
use intimeui_server::replay::{new_nonce, ReplaySigner, SigningSecret};
use intimeui_server::state::AppState;
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &str = "acceptance-secret-0123456789abcdef0123456789abcdef";

const ORDER_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>订单详情</title>
  <style>body{margin:0;background:#fff;color:#111}main{padding:24px}</style>
</head>
<body><main><h1>订单详情</h1><p>ORD_20261014_001</p><p>已支付</p></main></body>
</html>"#;

const FLASH_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>限时抢购</title>
  <style>body{margin:0;background:#fff;color:#111}main{padding:24px}.price{font-size:40px}</style>
</head>
<body><main><h1>限时抢购 · 无线降噪耳机</h1><p class="price">¥299.00</p><button>立即抢购</button></main></body>
</html>"#;

const OBFUSCATION_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <title>混淆样例</title>
  <style>.card{padding:16px}.title{font-weight:700}.action{color:#111}</style>
</head>
<body>
  <main class="card">
    <h1 class="title" id="title">验收样例</h1>
    <button class="action" id="action">提交</button>
  </main>
  <script>
    function bindAction() {
      var counter = 0;
      var button = document.getElementById('action');
      button.addEventListener('click', function () {
        counter += 1;
        document.getElementById('title').textContent = '已点击 ' + counter;
      });
    }
    bindAction();
  </script>
</body>
</html>"#;

fn test_config() -> AppConfig {
    AppConfig {
        rate_limit_enabled: false,
        auth_dev_mode: true,
        telemetry_quota_enabled: false,
        model: ModelConfig {
            mock_latency_ms: 10,
            mock_stream_delay_ms: 0,
            ..ModelConfig::default()
        },
        pipeline: PipelineConfig {
            timeout_ms: 1500,
            fallback_reserve_ms: 50,
            cache_enabled: false,
            ..PipelineConfig::default()
        },
        ..AppConfig::default()
    }
}

async fn db() -> Db {
    let db = Db::connect_in_memory()
        .await
        .expect("failed to open in-memory database");
    db.migrate().await.expect("failed to run migrations");
    db.seed().await.expect("failed to load seed data");
    db
}

async fn app_with(config: AppConfig) -> axum::Router {
    build_app(AppState::new(config, db().await))
}

async fn app_with_gateway(config: AppConfig, gateway: Arc<ModelGateway>) -> axum::Router {
    build_app(AppState::with_gateway(config, db().await, gateway))
}

fn post_json(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

fn with_bearer(mut request: Request<Body>) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer dev:usr_9527".parse().unwrap(),
    );
    request
}

async fn send_json(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("request failed");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read body")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body is not valid JSON")
    };
    (status, value)
}

/// 构造一个已签名的 `/api/v1/ui/generate` 请求。
///
/// `signed_body` 参与签名，`actual_body` 实际发送；两者不同即模拟篡改。
fn signed_ui_request(
    signed_body: &[u8],
    actual_body: Vec<u8>,
    timestamp: i64,
    nonce: &str,
) -> Request<Body> {
    let signed = ReplaySigner::new(SECRET).sign(
        "POST",
        "/api/v1/ui/generate",
        timestamp,
        nonce,
        "usr_9527",
        signed_body,
    );
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/v1/ui/generate")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer dev:usr_9527")
        .body(Body::from(actual_body))
        .unwrap();
    signed.insert_into(request.headers_mut());
    request
}

fn order_body(order_id: &str) -> Value {
    json!({
        "user_id": "usr_9527",
        "scene_code": "ORDER_DETAIL_PAGE",
        "device_info": {"screen_width": 390, "screen_height": 844, "platform": "android"},
        "business_data": {
            "order_id": order_id,
            "status": "PAID",
            "items": [{"name": "智能降噪耳机", "price": 299.00, "qty": 1}]
        },
        "bypass_cache": true
    })
}

fn flash_body(sale_id: &str) -> Value {
    json!({
        "user_id": "usr_9527",
        "scene_code": "FLASH_SALE_PAGE",
        "device_info": {"screen_width": 390, "screen_height": 844, "platform": "android"},
        "business_data": {
            "sale_id": sale_id,
            "title": "限时秒杀 · 无线降噪耳机",
            "product_id": "PRD_1001",
            "price": 299.00,
            "original_price": 599.00,
            "stock_total": 1000,
            "stock_remaining": 137,
            "status": "LIVE",
            "countdown_seconds": 3661,
            "description": "限量 1000 件，售完即止。"
        },
        "session_id": "sess_accept_flash",
        "bypass_cache": true
    })
}

struct StubProvider {
    content: String,
    calls: Arc<AtomicUsize>,
    latency: Duration,
}

impl StubProvider {
    fn new(content: impl Into<String>, latency: Duration) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                content: content.into(),
                calls: calls.clone(),
                latency,
            },
            calls,
        )
    }
}

#[async_trait]
impl ModelProvider for StubProvider {
    fn name(&self) -> &'static str {
        "acceptance-stub"
    }

    async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.latency).await;
        Ok(ChatResponse {
            content: self.content.clone(),
            finish_reason: Some("stop".to_string()),
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
            },
        })
    }

    async fn generate_stream(&self, _request: ChatRequest) -> Result<ModelStream, ModelError> {
        Ok(Box::pin(futures_util::stream::empty::<
            Result<StreamChunk, ModelError>,
        >()))
    }
}

struct DisabledProvider;

#[async_trait]
impl ModelProvider for DisabledProvider {
    fn name(&self) -> &'static str {
        "acceptance-disabled"
    }

    async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse, ModelError> {
        Err(ModelError::Disabled)
    }

    async fn generate_stream(&self, _request: ChatRequest) -> Result<ModelStream, ModelError> {
        Err(ModelError::Disabled)
    }
}

fn gateway_with(provider: Arc<dyn ModelProvider>) -> Arc<ModelGateway> {
    Arc::new(ModelGateway::new(ModelConfig::default(), provider))
}

fn assert_complete_page(html: &str, expected_text: &str) {
    assert!(html.starts_with("<!DOCTYPE html>"), "missing doctype");
    assert!(html.trim_end().ends_with("</html>"), "missing closing html");
    assert!(
        html.contains(expected_text),
        "missing page content: {expected_text}"
    );
    assert!(
        html.len() > 300,
        "page is suspiciously small: {} bytes",
        html.len()
    );
}

#[tokio::test]
async fn scenario_one_order_detail_template_and_dynamic_modes_pass() {
    let (provider, _) = StubProvider::new(ORDER_HTML, Duration::from_millis(5));
    let app = app_with_gateway(test_config(), gateway_with(Arc::new(provider))).await;

    // 模板模式
    let template_request = with_bearer(post_json(
        "/api/v1/ui/render",
        json!({
            "scene_code": "ORDER_DETAIL_PAGE",
            "device_info": {"screen_width": 390, "screen_height": 844, "platform": "android"},
            "business_data": {
                "order_id": "ORD_20261014_001",
                "status": "PAID",
                "items": [{"name": "智能降噪耳机", "price": 299.00, "qty": 1}]
            }
        }),
    ));
    let (status, body) = send_json(&app, template_request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["code"], 0);
    let template_html = body["data"]["html"].as_str().unwrap();
    assert_complete_page(template_html, "ORD_20261014_001");
    assert_eq!(body["data"]["strategy"], "template");

    // 动态模式
    let dynamic_request = with_bearer(post_json(
        "/api/v1/ui/generate/dynamic",
        order_body("ORD_20261014_001"),
    ));
    let (status, body) = send_json(&app, dynamic_request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let data = &body["data"];
    assert_eq!(data["strategy"], "dynamic");
    assert_eq!(data["degraded"], false);
    assert_complete_page(data["html_content"].as_str().unwrap(), "ORD_20261014_001");
    println!(
        "scenario1 dynamic total={}ms model={}ms validation={}ms obfuscation={}ms",
        data["timings"]["total_ms"],
        data["timings"]["model_ms"],
        data["timings"]["validation_ms"],
        data["timings"]["obfuscation_ms"]
    );
}

#[tokio::test]
async fn scenario_two_flash_sale_dynamic_and_intent_operation_pass() {
    let (provider, _) = StubProvider::new(FLASH_HTML, Duration::from_millis(5));
    let app = app_with_gateway(test_config(), gateway_with(Arc::new(provider))).await;

    let (status, body) = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/ui/generate/dynamic",
            flash_body("SALE_20261022_001"),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let data = &body["data"];
    assert_eq!(data["strategy"], "dynamic");
    assert_eq!(data["degraded"], false);
    assert_complete_page(data["html_content"].as_str().unwrap(), "限时抢购");

    // 抢购页内的意图操作：点击「查询余额」按钮，读类能力直接执行。
    let (status, body) = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/intent/execute",
            json!({
                "user_id": "usr_9527",
                "input_type": "UI_INTERACTION",
                "raw_input": {
                    "event": {"type": "TAP", "target_id": "btn_query_balance"},
                    "path": [{"id": "FLASH_SALE_PAGE", "type": "page"}]
                }
            }),
        )),
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
async fn performance_static_under_200ms() {
    let (provider, _) = StubProvider::new(ORDER_HTML, Duration::from_millis(10));
    let app = app_with_gateway(test_config(), gateway_with(Arc::new(provider))).await;

    // 预热模板与混淆器，避免首次解析影响测量。
    let _ = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/ui/render",
            json!({
                "scene_code": "ORDER_DETAIL_PAGE",
                "business_data": {
                    "order_id": "ORD_WARMUP",
                    "status": "PAID",
                    "items": [{"name": "预热商品", "price": 1.00, "qty": 1}]
                }
            }),
        )),
    )
    .await;

    let started = Instant::now();
    let (status, body) = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/ui/render",
            json!({
                "scene_code": "ORDER_DETAIL_PAGE",
                "business_data": {
                    "order_id": "ORD_PERF_001",
                    "status": "PAID",
                    "items": [{"name": "智能降噪耳机", "price": 299.00, "qty": 1}]
                }
            }),
        )),
    )
    .await;
    let static_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        static_ms < 200.0,
        "static render exceeded 200ms: {static_ms:.1}ms"
    );
    println!("static performance measured: {static_ms:.1}ms");
}

#[tokio::test]
async fn performance_dynamic_under_1500ms() {
    let (provider, _) = StubProvider::new(ORDER_HTML, Duration::from_millis(10));
    let app = app_with_gateway(test_config(), gateway_with(Arc::new(provider))).await;

    let started = Instant::now();
    let (status, body) = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/ui/generate/dynamic",
            order_body("ORD_PERF_002"),
        )),
    )
    .await;
    let dynamic_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["degraded"], false);
    assert!(
        dynamic_ms < 1500.0,
        "dynamic render exceeded 1500ms: {dynamic_ms:.1}ms"
    );
    println!("dynamic performance measured: {dynamic_ms:.1}ms");
}

#[tokio::test]
async fn degradation_timeout_falls_back_to_flash_sale_template() {
    let mut timeout_config = test_config();
    timeout_config.model.mock_latency_ms = 5_000;
    timeout_config.pipeline = PipelineConfig {
        timeout_ms: 300,
        fallback_reserve_ms: 50,
        cache_enabled: false,
        ..PipelineConfig::default()
    };
    let app = app_with(timeout_config).await;
    let (status, body) = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/ui/generate/dynamic",
            flash_body("SALE_TIMEOUT_001"),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["degraded"], true);
    assert_eq!(body["data"]["degradation_reason"], "model_timeout");
    assert_complete_page(body["data"]["html_content"].as_str().unwrap(), "限时秒杀");
}

#[tokio::test]
async fn degradation_model_disabled_falls_back_to_order_template() {
    let app = app_with_gateway(test_config(), gateway_with(Arc::new(DisabledProvider))).await;
    let (status, body) = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/ui/generate/dynamic",
            order_body("ORD_DISABLED_001"),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["degraded"], true);
    assert_eq!(body["data"]["degradation_reason"], "model_disabled");
    assert_complete_page(
        body["data"]["html_content"].as_str().unwrap(),
        "ORD_DISABLED_001",
    );
}

#[tokio::test]
async fn degradation_validation_failure_falls_back_to_order_template() {
    let (provider, _) = StubProvider::new(
        "<html><body><p>not a complete single-file page</p></body></html>",
        Duration::from_millis(1),
    );
    let app = app_with_gateway(test_config(), gateway_with(Arc::new(provider))).await;
    let (status, body) = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/ui/generate/dynamic",
            order_body("ORD_INVALID_001"),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["degraded"], true);
    assert_eq!(body["data"]["degradation_reason"], "validation_failed");
    assert_complete_page(
        body["data"]["html_content"].as_str().unwrap(),
        "ORD_INVALID_001",
    );
}

#[tokio::test]
async fn security_replay_protection_spot_check() {
    let mut config = test_config();
    config.replay_protection_enabled = true;
    config.replay_signing_secret = Some(SigningSecret::new(SECRET));
    let app = app_with(config).await;

    let body = serde_json::to_vec(&order_body("ORD_20261014_001")).unwrap();
    let timestamp = Utc::now().timestamp();
    let nonce = new_nonce();
    let (status, response) = send_json(
        &app,
        signed_ui_request(&body, body.clone(), timestamp, &nonce),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {response}");

    let (status, response) = send_json(
        &app,
        signed_ui_request(&body, body.clone(), timestamp, &nonce),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["code"], 40900);

    let tampered = serde_json::to_vec(&order_body("ORD_TAMPERED")).unwrap();
    let (status, response) = send_json(
        &app,
        signed_ui_request(&body, tampered, Utc::now().timestamp(), &new_nonce()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(response["code"], 40100);
}

#[tokio::test]
async fn security_obfuscation_spot_check() {
    let app = app_with(test_config()).await;
    let obfuscate = |session: &str| {
        with_bearer(post_json(
            "/api/v1/ui/obfuscate",
            json!({
                "html": OBFUSCATION_HTML,
                "session_id": session,
                "obfuscation": {"level": "aggressive"}
            }),
        ))
    };

    let (status, first) = send_json(&app, obfuscate("sess_accept_obf_1")).await;
    assert_eq!(status, StatusCode::OK, "body: {first}");
    let first_html = first["data"]["html"].as_str().unwrap();
    let stats = &first["data"]["stats"];
    assert!(stats["classes_renamed"].as_u64().unwrap() > 0);
    assert!(
        stats["identifier_coverage"].as_f64().unwrap() >= 0.8,
        "coverage too low: {stats}"
    );
    assert_ne!(first_html, OBFUSCATION_HTML);
    assert!(!first_html.contains("class=\"card\""));

    let (status, second) = send_json(&app, obfuscate("sess_accept_obf_2")).await;
    assert_eq!(status, StatusCode::OK, "body: {second}");
    assert_ne!(
        first_html,
        second["data"]["html"].as_str().unwrap(),
        "different sessions must produce different output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flash_sale_high_concurrency_stays_responsive() {
    let (provider, _) = StubProvider::new(FLASH_HTML, Duration::from_millis(10));
    let app = app_with_gateway(test_config(), gateway_with(Arc::new(provider))).await;

    let started = Instant::now();
    let mut tasks = Vec::new();
    for index in 0..12 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            let request_started = Instant::now();
            let response = send_json(
                &app,
                with_bearer(post_json(
                    "/api/v1/ui/generate/dynamic",
                    flash_body(&format!("SALE_CONCURRENT_{index:02}")),
                )),
            )
            .await;
            (response, request_started.elapsed())
        }));
    }
    let results = futures_util::future::join_all(tasks).await;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut latencies = Vec::new();
    for result in results {
        let ((status, body), elapsed) = result.expect("concurrent task panicked");
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["code"], 0);
        assert_eq!(body["data"]["degraded"], false);
        latencies.push(elapsed.as_secs_f64() * 1000.0);
    }
    latencies.sort_by(|left, right| left.total_cmp(right));
    let p95 = latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)];
    assert!(
        wall_ms < 1500.0,
        "concurrent wall time exceeded 1500ms: {wall_ms:.1}ms"
    );
    assert!(p95 < 1500.0, "p95 exceeded 1500ms: {p95:.1}ms");
    println!(
        "concurrency measured: requests={} wall={wall_ms:.1}ms p95={p95:.1}ms",
        latencies.len()
    );
}

#[tokio::test]
async fn pipeline_metrics_endpoint_exposes_acceptance_signals() {
    let (provider, _) = StubProvider::new(ORDER_HTML, Duration::from_millis(5));
    let app = app_with_gateway(test_config(), gateway_with(Arc::new(provider))).await;
    let _ = send_json(
        &app,
        with_bearer(post_json(
            "/api/v1/ui/generate/dynamic",
            order_body("ORD_METRICS_001"),
        )),
    )
    .await;
    let (status, body) = send_json(&app, with_bearer(get("/api/v1/ui/pipeline/metrics"))).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["total_requests"], 1);
    assert_eq!(body["data"]["dynamic_success"], 1);
    assert_eq!(body["data"]["degraded_requests"], 0);
}
