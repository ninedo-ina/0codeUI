//! 意图执行端点（PRD §5.2）。

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::Json;

use crate::error::AppError;
use crate::intent::model::{IntentExecuteRequest, IntentExecuteResponse};
use crate::middleware::auth::AuthUser;
use crate::response::ApiResponse;
use crate::state::AppState;

/// `POST /api/v1/intent/execute`
///
/// 输入自然语言或 UI 事件树；读类意图直接执行，写 / 删类返回确认卡片。
pub async fn execute(
    State(state): State<AppState>,
    auth: AuthUser,
    payload: Result<Json<IntentExecuteRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<IntentExecuteResponse>>, AppError> {
    let Json(request) = payload.map_err(|rejection| {
        AppError::invalid_param(format!("invalid request body: {}", rejection.body_text()))
    })?;
    let response = state.intent_service.execute(&auth.user_id, request).await?;
    Ok(Json(ApiResponse::success(response)))
}
