//! 静态模板引擎与兜底渲染（REQ-202610-1172）。
//!
//! 本模块是 UI 生成链路（REQ-202610-1171）的静态渲染实现，同时作为动态生成管线
//! （REQ-202610-1185）的兜底通道：动态模型超时、输出校验失败时调用
//! [`TemplateEngine::render_fallback`]，以同一份模板库秒级产出可用页面。
//!
//! 分层：
//! - [`engine`]：与业务无关的占位符填充引擎（默认 HTML 转义）；
//! - 模板库：`server/templates/*.html`，编译期内嵌，无运行时文件依赖；
//! - 场景视图模型：业务 JSON → 模板上下文（校验 + 金额格式化 + 状态映射）。

pub mod catalog;
pub mod engine;
pub mod flash_sale;
pub mod order_detail;

use std::sync::OnceLock;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use engine::{escape_html, render_str};

/// 一次渲染的输入设备信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// 屏幕宽度（CSS px）。
    pub screen_width: u32,
    /// 屏幕高度（CSS px）。
    pub screen_height: u32,
    /// 平台标识：android / ios / web / harmonyos。
    pub platform: String,
}

impl Default for DeviceInfo {
    fn default() -> Self {
        Self {
            screen_width: 390,
            screen_height: 844,
            platform: "web".to_string(),
        }
    }
}

/// 渲染结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedPage {
    /// 完整单文件 HTML。
    pub html: String,
    /// 场景编码。
    pub scene_code: String,
    /// 渲染策略：静态模板固定为 `template`。
    pub strategy: String,
    /// 模板版本，如 `v1`。
    pub template_version: String,
    /// 本次渲染耗时（毫秒，保留三位小数）。
    pub render_ms: f64,
}

/// 模板渲染错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// 场景未注册（HTTP 404 / code 40410）。
    #[error("scene not found: {0}")]
    SceneNotFound(String),
    /// 业务数据不符合场景契约（HTTP 400 / code 40000）。
    #[error("invalid business_data: {0}")]
    InvalidBusinessData(String),
    /// 模板解析 / 渲染内部错误（HTTP 500 / code 50000）。
    #[error("template render failed: {0}")]
    Render(String),
}

/// 模板引擎句柄（模板库编译期内嵌，构造后只读）。
#[derive(Debug)]
pub struct TemplateEngine {
    _private: (),
}

impl TemplateEngine {
    /// 进程级共享引擎。
    pub fn global() -> &'static Self {
        static ENGINE: OnceLock<TemplateEngine> = OnceLock::new();
        ENGINE.get_or_init(|| Self { _private: () })
    }

    /// 渲染任意模板字符串（供模板库与单测使用）。
    pub fn render_str(template: &str, data: &Value) -> Result<String, TemplateError> {
        render_str(template, data)
    }

    /// 按场景编码渲染静态模板。
    pub fn render(
        &self,
        scene_code: &str,
        business_data: &Value,
        device: &DeviceInfo,
    ) -> Result<RenderedPage, TemplateError> {
        let started = Instant::now();
        let definition = catalog::find(scene_code).ok_or_else(|| {
            TemplateError::SceneNotFound(format!(
                "{scene_code} (registered: {})",
                catalog::scene_codes().join(", ")
            ))
        })?;

        let view = (definition.build_view)(business_data, device)?;
        let html = render_str(definition.template, &view)?;

        Ok(RenderedPage {
            html,
            scene_code: definition.scene_code.to_string(),
            strategy: "template".to_string(),
            template_version: definition.template_version.to_string(),
            render_ms: round_millis(started.elapsed().as_secs_f64() * 1000.0),
        })
    }

    /// 动态管线（REQ-202610-1185）降级入口：始终以静态模板输出。
    pub fn render_fallback(
        &self,
        scene_code: &str,
        business_data: &Value,
        device: &DeviceInfo,
    ) -> Result<RenderedPage, TemplateError> {
        self.render(scene_code, business_data, device)
    }
}

/// 适配 REQ-202610-1171 场景注册表：`ORDER_DETAIL_PAGE` 由本模板引擎渲染。
///
/// 保持 03 接口的 [`crate::ui::context::SceneContext`] → `Result<String, SceneError>`
/// 签名，使 `POST /api/v1/ui/generate` 与 `POST /api/v1/ui/render` 共用同一份模板、
/// 同一套业务数据校验与 HTML 转义规则。
pub fn render_order_detail_scene(
    ctx: &crate::ui::context::SceneContext<'_>,
) -> Result<String, crate::ui::context::SceneError> {
    let device = DeviceInfo {
        screen_width: ctx.screen_width,
        screen_height: ctx.screen_height,
        platform: ctx.platform.to_string(),
    };
    TemplateEngine::global()
        .render("ORDER_DETAIL_PAGE", ctx.business_data, &device)
        .map(|page| page.html)
        .map_err(|error| match error {
            TemplateError::InvalidBusinessData(message) => {
                crate::ui::context::SceneError::InvalidBusinessData(message)
            }
            other => crate::ui::context::SceneError::Render(other.to_string()),
        })
}

/// 适配 REQ-202610-1171 场景注册表：`FLASH_SALE_PAGE` 由本模板引擎渲染。
pub fn render_flash_sale_scene(
    ctx: &crate::ui::context::SceneContext<'_>,
) -> Result<String, crate::ui::context::SceneError> {
    let device = DeviceInfo {
        screen_width: ctx.screen_width,
        screen_height: ctx.screen_height,
        platform: ctx.platform.to_string(),
    };
    TemplateEngine::global()
        .render("FLASH_SALE_PAGE", ctx.business_data, &device)
        .map(|page| page.html)
        .map_err(|error| match error {
            TemplateError::InvalidBusinessData(message) => {
                crate::ui::context::SceneError::InvalidBusinessData(message)
            }
            other => crate::ui::context::SceneError::Render(other.to_string()),
        })
}

/// 毫秒保留三位小数，便于响应体展示与性能统计。
fn round_millis(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}
