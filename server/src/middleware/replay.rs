//! API 防重放签名中间件（REQ-202610-1186）。
//!
//! 仅挂在需要签名的三条写接口上（UI 生成 / 意图执行 / 埋点上报）。
//! 鉴权中间件必须先执行，以便把 [`AuthUser`] 注入扩展后由本中间件绑定 `user_id`。
//!
//! 处理流程：
//! 1. 开关关闭时直接放行（零开销，不缓冲请求体）；
//! 2. 读取并缓存请求体（上限 [`crate::replay::ReplayService::max_body_bytes`]）；
//! 3. 校验时间窗、动态令牌、HMAC 签名；
//! 4. 消费 Nonce（SQLite 去重），重放返回 409；
//! 5. 用缓存的字节重建请求体，交给下游 handler。

use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::replay::ReplayRequest;
use crate::state::AppState;

/// 防重放签名校验中间件。
pub async fn require_signature(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if !state.replay.enabled() {
        return Ok(next.run(request).await);
    }

    let user_id = request
        .extensions()
        .get::<AuthUser>()
        .map(|user| user.user_id.clone())
        .ok_or_else(AppError::unauthorized)?;

    let (parts, body) = request.into_parts();
    let method = parts.method.as_str().to_string();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());

    let bytes = to_bytes(body, state.replay.max_body_bytes())
        .await
        .map_err(|error| {
            AppError::invalid_param(format!(
                "request body could not be read for signature verification: {error}"
            ))
        })?;

    state
        .replay
        .verify(&ReplayRequest {
            user_id: &user_id,
            method: &method,
            path_and_query: &path_and_query,
            headers: &parts.headers,
            body: &bytes,
        })
        .await?;

    let request = Request::from_parts(parts, Body::from(bytes));
    Ok(next.run(request).await)
}
