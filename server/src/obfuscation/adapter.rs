//! 将 [`ObfuscationEngine`] 适配为动态生成管线使用的 [`HtmlObfuscator`]。
//!
//! 管线只依赖抽象接口，本适配器负责把应用级混淆配置与会话映射表接进来，
//! 使模板兜底与动态生成共享同一套混淆策略。

use std::sync::Arc;

use crate::obfuscation::{ObfuscationConfig, ObfuscationEngine};
use crate::pipeline::obfuscation::{HtmlObfuscator, ObfuscationOutput};

/// 基于进程内混淆引擎的管线混淆器。
pub struct EngineObfuscator {
    engine: Arc<ObfuscationEngine>,
    config: ObfuscationConfig,
}

impl EngineObfuscator {
    /// 使用指定引擎与配置构造适配器。
    pub fn new(engine: Arc<ObfuscationEngine>, config: ObfuscationConfig) -> Self {
        Self { engine, config }
    }
}

impl HtmlObfuscator for EngineObfuscator {
    fn obfuscate(&self, html: &str, session_id: Option<&str>) -> Result<ObfuscationOutput, String> {
        let result = self
            .engine
            .obfuscate(html, &self.config, session_id)
            .map_err(|error| error.to_string())?;
        Ok(ObfuscationOutput {
            html: result.html,
            applied: self.config.level.enabled(),
        })
    }
}
