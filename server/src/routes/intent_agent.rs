//! 意图理解与 Tool Call Agent HTTP 端点。
//!
//! - `POST /api/v1/intent/understand`：自然语言 → 结构化决策树；
//! - `GET /api/v1/intent/tools`：返回与 #10 对齐的原子能力清单。

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::Json;

use crate::error::AppError;
use crate::intent_agent::catalog;
use crate::intent_agent::model::{UnderstandRequest, UnderstandResponse};
use crate::intent_agent::CapabilityDescriptor;
use crate::middleware::auth::AuthUser;
use crate::response::ApiResponse;
use crate::state::AppState;

/// `POST /api/v1/intent/understand`
///
/// 请求体：
/// ```json
/// {
///   "text": "帮我退掉昨天买的那个超 50 元的商品",
///   "page_context": {"scene_code": "ORDER_DETAIL_PAGE", "entity_id": "ORD_20261014_001"},
///   "data_context": {"orders": []}
/// }
/// ```
///
/// 返回 `READY` 决策树或 `NEEDS_CLARIFICATION` 澄清追问；非法模型输出不会
/// 进入执行链路，而是被拦截、重试，最终转为澄清。
pub async fn understand(
    State(state): State<AppState>,
    user: AuthUser,
    payload: Result<Json<UnderstandRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<UnderstandResponse>>, AppError> {
    let Json(mut request) =
        payload.map_err(|rejection| AppError::invalid_param(rejection.body_text()))?;

    if let Some(user_id) = request.user_id.as_deref() {
        if user_id != user.user_id {
            return Err(AppError::forbidden());
        }
    }
    request.user_id = Some(user.user_id);

    let agent = state.intent_agent.clone();
    let response = tokio::task::spawn_blocking(move || agent.understand(request))
        .await
        .map_err(|error| AppError::internal(format!("intent agent task failed: {error}")))??;
    Ok(Json(ApiResponse::success(response)))
}

/// `GET /api/v1/intent/tools`
///
/// 返回全部原子能力描述，供前端/上层 Agent 发现可用函数。
pub async fn tools(_user: AuthUser) -> Json<ApiResponse<Vec<CapabilityDescriptor>>> {
    Json(ApiResponse::success(catalog::descriptors()))
}
