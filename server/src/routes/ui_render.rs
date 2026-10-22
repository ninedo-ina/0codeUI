//! `POST /api/v1/ui/render`：静态模板渲染接口（REQ-202610-1172）。
//!
//! 该接口是 UI 生成接口（REQ-202610-1171）的静态模式落点：接收场景编码、
//! 业务 JSON 与设备信息，返回可直接在 WebView 加载的单文件 HTML。
//! 动态生成管线（REQ-202610-1185）失败时，也复用同一渲染引擎作为兜底。

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;
use validator::Validate;

use crate::error::{AppError, ErrorCode};
use crate::obfuscation::{ObfuscationError, ObfuscationOptions};
use crate::response::ApiResponse;
use crate::state::AppState;
use crate::ui_template::{DeviceInfo, RenderedPage, TemplateEngine, TemplateError};
use crate::validation::ValidatedJson;

/// 静态渲染请求。
#[derive(Debug, Deserialize, Validate)]
pub struct UiRenderRequest {
    /// 场景编码，如 `ORDER_DETAIL_PAGE`。
    #[validate(length(min = 1, max = 64, message = "must be 1..=64 characters"))]
    pub scene_code: String,
    /// 业务数据（PRD §5.1 同构 JSON）。
    pub business_data: Value,
    /// 设备信息；缺省按 390×844 web 处理。
    #[serde(default)]
    pub device_info: Option<DeviceInfo>,
    /// 会话标识；同一会话复用类名映射，缺省则每次请求生成新映射。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 请求级混淆选项（缺省走服务端配置）。
    #[serde(default)]
    pub obfuscation: Option<ObfuscationOptions>,
}

/// 渲染并返回单文件 HTML。
pub async fn render_ui(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<UiRenderRequest>,
) -> Result<Json<ApiResponse<RenderedPage>>, AppError> {
    let device = payload.device_info.unwrap_or_default();
    let mut page = TemplateEngine::global()
        .render(&payload.scene_code, &payload.business_data, &device)
        .map_err(map_template_error)?;

    let options = payload.obfuscation.unwrap_or_default();
    let config = state.config.obfuscation_config().with_options(&options);
    let result = state
        .obfuscation
        .obfuscate(&page.html, &config, payload.session_id.as_deref())
        .map_err(map_obfuscation_error)?;
    page.html = result.html;
    tracing::debug!(
        scene_code = %page.scene_code,
        level = %result.stats.level,
        classes = result.stats.classes_renamed,
        identifiers = result.stats.identifiers_renamed,
        wrappers = result.stats.wrappers_added,
        "ui html obfuscated"
    );
    Ok(Json(ApiResponse::success(page)))
}

fn map_obfuscation_error(error: ObfuscationError) -> AppError {
    match error {
        ObfuscationError::InvalidSessionId(message) | ObfuscationError::InvalidHtml(message) => {
            AppError::invalid_param(message)
        }
        other => AppError::internal(other),
    }
}

fn map_template_error(error: TemplateError) -> AppError {
    match error {
        TemplateError::SceneNotFound(_) => {
            AppError::new(ErrorCode::SceneNotFound, "scene not found")
        }
        TemplateError::InvalidBusinessData(message) => AppError::invalid_param(message),
        TemplateError::Render(message) => AppError::internal(message),
    }
}
