//! `POST /api/v1/ui/obfuscate`：单文件 HTML 输出混淆接口（REQ-202610-1184）。
//!
//! 主要供生成管线、调试台与验收脚本直接调用；`/ui/render` 与 `/ui/generate`
//! 也会在返回前自动执行同一套混淆。请求可覆盖强度与会话标识，响应携带统计。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::error::AppError;
use crate::obfuscation::{ObfuscationError, ObfuscationOptions, ObfuscationStats};
use crate::response::ApiResponse;
use crate::state::AppState;
use crate::validation::ValidatedJson;

/// 混淆请求。
#[derive(Debug, Deserialize, Validate)]
pub struct UiObfuscateRequest {
    /// 待混淆的完整 HTML。
    #[validate(length(
        min = 1,
        max = 2097152,
        message = "html must be 1..=2097152 characters"
    ))]
    pub html: String,
    /// 会话标识；同一会话复用类名映射。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 请求级强度 / 覆盖率选项。
    #[serde(default)]
    pub obfuscation: Option<ObfuscationOptions>,
}

/// 混淆响应数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiObfuscateData {
    /// 混淆后的 HTML。
    pub html: String,
    /// 统计信息。
    pub stats: ObfuscationStats,
}

/// `POST /api/v1/ui/obfuscate`
pub async fn obfuscate(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<UiObfuscateRequest>,
) -> Result<Json<ApiResponse<UiObfuscateData>>, AppError> {
    let options = payload.obfuscation.unwrap_or_default();
    let config = state.config.obfuscation_config().with_options(&options);
    let result = state
        .obfuscation
        .obfuscate(&payload.html, &config, payload.session_id.as_deref())
        .map_err(map_obfuscation_error)?;

    Ok(Json(ApiResponse::success(UiObfuscateData {
        html: result.html,
        stats: result.stats,
    })))
}

fn map_obfuscation_error(error: ObfuscationError) -> AppError {
    match error {
        ObfuscationError::InvalidSessionId(message) | ObfuscationError::InvalidHtml(message) => {
            AppError::invalid_param(message)
        }
        other => AppError::internal(other),
    }
}
