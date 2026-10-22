//! 单文件 HTML 输出混淆（REQ-202610-1184）。
//!
//! 对模板 / 生成管线产出的单文件 HTML 做发布前的随机化处理：
//! - DOM 结构：包装节点、属性顺序打乱、诱饵节点；
//! - CSS：类名按会话映射表随机化，选择器同步改写；
//! - JS：局部变量、函数名与参数名随机化，保留全局对象与属性访问；
//! - 同一页面多次请求使用不同随机种子，避免静态指纹。
//!
//! 混淆后的 HTML 会再次经过 [`crate::html_validator`] 校验，保证结构完整；
//! 功能等价性由 [`engine::ObfuscationEngine::obfuscate`] 的输入输出契约与测试覆盖。

pub mod adapter;
pub mod config;
mod css;
mod dom;
mod engine;
mod js;
mod rng;
mod session;

pub use config::{ObfuscationConfig, ObfuscationLevel, ObfuscationOptions};
pub use engine::{
    obfuscate_html, ObfuscationEngine, ObfuscationError, ObfuscationResult, ObfuscationStats,
};
