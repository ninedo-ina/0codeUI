//! UI 生成端点（REQ-202610-1171）。
//!
//! `POST /api/v1/ui/generate`：鉴权 → 校验 `user_id` 归属 → 调用生成用例。
//! 成功响应 `{code: 0, msg: "ok", data: {render_type, version, html_content}}`。

use axum::extract::State;
use axum::Json;

use crate::error::{AppError, ErrorCode};
use crate::middleware::auth::AuthUser;
use crate::obfuscation::ObfuscationError;
use crate::response::ApiResponse;
use crate::state::AppState;
use crate::ui::generate::{generate_ui, UiGenerateData, UiGenerateRequest};
use crate::validation::ValidatedJson;

/// `POST /api/v1/ui/generate`
pub async fn generate(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(payload): ValidatedJson<UiGenerateRequest>,
) -> Result<Json<ApiResponse<UiGenerateData>>, AppError> {
    ensure_user_scope(&state, &auth, &payload.user_id)?;
    let mut data = generate_ui(&payload)?;
    let config = state.config.obfuscation_config();
    let result = state
        .obfuscation
        .obfuscate(&data.html_content, &config, None)
        .map_err(map_obfuscation_error)?;
    data.html_content = result.html;
    Ok(Json(ApiResponse::success(data)))
}

fn map_obfuscation_error(error: ObfuscationError) -> AppError {
    match error {
        ObfuscationError::InvalidSessionId(message) | ObfuscationError::InvalidHtml(message) => {
            AppError::invalid_param(message)
        }
        other => AppError::internal(other),
    }
}

/// 生产模式下请求的 `user_id` 必须与凭据一致；开发模式允许跨用户联调。
pub(crate) fn ensure_user_scope(
    state: &AppState,
    auth: &AuthUser,
    requested_user_id: &str,
) -> Result<(), AppError> {
    if state.config.auth_dev_mode || auth.user_id == requested_user_id {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::Forbidden,
        "user_id does not match authenticated user",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(user_id: &str) -> AuthUser {
        AuthUser {
            user_id: user_id.to_string(),
        }
    }

    #[tokio::test]
    async fn dev_mode_allows_any_user_id() {
        let config = crate::config::AppConfig {
            auth_dev_mode: true,
            ..crate::config::AppConfig::default()
        };
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let state = AppState::new(config, db);
        assert!(ensure_user_scope(&state, &auth("u1"), "u2").is_ok());
    }

    #[tokio::test]
    async fn production_requires_matching_user_id() {
        let config = crate::config::AppConfig {
            auth_dev_mode: false,
            ..crate::config::AppConfig::default()
        };
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let state = AppState::new(config, db);
        assert!(ensure_user_scope(&state, &auth("u1"), "u1").is_ok());
        let error = ensure_user_scope(&state, &auth("u1"), "u2").unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden);
    }
}
