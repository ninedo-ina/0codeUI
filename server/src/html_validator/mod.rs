//! 单文件 HTML 输出校验器（REQ-202610-1173）。
//!
//! 作为动态 UI 生成管线的安全守门器：校验 AI / 模板产出的 HTML 是否结构完整、
//! 是否满足单文件约束（CSS/JS 内联、无外部依赖）以及安全基线（事件属性白名单、
//! 危险 URL 协议、禁用元素等）。校验结果结构化返回错误类型、位置与片段，
//! 供降级判定与日志使用。
//!
//! 快速开始：
//! ```
//! use intimeui_server::html_validator::validate_single_file_html;
//!
//! let result = validate_single_file_html(
//!     "<!DOCTYPE html><html><head></head><body><p>ok</p></body></html>",
//! );
//! assert!(result.valid);
//! ```

mod checks;
pub mod config;
mod css;
pub mod model;
pub(crate) mod sink;
pub(crate) mod source;
pub(crate) mod tokenizer;
pub(crate) mod url;

pub use config::ValidatorConfig;
pub use model::{
    IssueKind, Position, Severity, ValidationIssue, ValidationResult, ValidationStats,
};

/// 使用默认配置校验单文件 HTML。
pub fn validate_single_file_html(html: &str) -> ValidationResult {
    validate_single_file_html_with_config(html, &ValidatorConfig::default())
}

/// 使用自定义配置校验单文件 HTML。
pub fn validate_single_file_html_with_config(
    html: &str,
    config: &ValidatorConfig,
) -> ValidationResult {
    checks::validate(html, config)
}
