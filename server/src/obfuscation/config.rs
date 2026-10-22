//! 混淆强度与运行参数。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// 混淆强度。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObfuscationLevel {
    /// 关闭（原样返回）。
    Off,
    /// 基础：类名、属性顺序。
    Basic,
    /// 标准：基础 + JS 变量名 + 包装节点。
    #[default]
    Standard,
    /// 激进：标准 + 诱饵节点 + 更深的包装。
    Aggressive,
}

impl ObfuscationLevel {
    /// 是否启用混淆。
    pub const fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// 是否启用 DOM 包装节点。
    pub const fn wrappers(self) -> bool {
        matches!(self, Self::Standard | Self::Aggressive)
    }

    /// 是否启用诱饵节点。
    pub const fn decoys(self) -> bool {
        matches!(self, Self::Aggressive)
    }

    /// 是否启用 JS 变量名混淆。
    pub const fn js_renaming(self) -> bool {
        matches!(self, Self::Standard | Self::Aggressive)
    }

    /// 包装节点最大深度。
    pub const fn max_wrapper_depth(self) -> usize {
        match self {
            Self::Aggressive => 6,
            Self::Standard => 3,
            _ => 0,
        }
    }
}

impl fmt::Display for ObfuscationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::Basic => "basic",
            Self::Standard => "standard",
            Self::Aggressive => "aggressive",
        })
    }
}

impl FromStr for ObfuscationLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" => Ok(Self::Off),
            "basic" | "light" => Ok(Self::Basic),
            "standard" | "default" => Ok(Self::Standard),
            "aggressive" | "high" | "strong" => Ok(Self::Aggressive),
            other => Err(format!(
                "expected one of off/basic/standard/aggressive, got `{other}`"
            )),
        }
    }
}

/// 一次混淆调用的配置。
#[derive(Debug, Clone, PartialEq)]
pub struct ObfuscationConfig {
    /// 混淆强度。
    pub level: ObfuscationLevel,
    /// 会话映射表 TTL（秒）。
    pub session_ttl_secs: u64,
    /// 会话映射表最大条目数。
    pub max_sessions: usize,
    /// 非保留标识符覆盖率下限（0.0–1.0）。
    pub min_identifier_coverage: f64,
}

impl Default for ObfuscationConfig {
    fn default() -> Self {
        Self {
            level: ObfuscationLevel::Standard,
            session_ttl_secs: 1800,
            max_sessions: 1000,
            min_identifier_coverage: 0.8,
        }
    }
}

impl ObfuscationConfig {
    /// 校验配置范围。
    pub fn validate(&self) -> Result<(), String> {
        if self.session_ttl_secs == 0 {
            return Err("session_ttl_secs must be >= 1".to_string());
        }
        if self.max_sessions == 0 {
            return Err("max_sessions must be >= 1".to_string());
        }
        if !(0.0..=1.0).contains(&self.min_identifier_coverage) {
            return Err("min_identifier_coverage must be within 0.0..=1.0".to_string());
        }
        Ok(())
    }

    /// 从请求级选项覆盖基础配置。
    pub fn with_options(&self, options: &ObfuscationOptions) -> Self {
        let mut config = self.clone();
        if let Some(enabled) = options.enabled {
            if !enabled {
                config.level = ObfuscationLevel::Off;
            }
        }
        // 服务端 `Off` 是总开关；请求级选项不能把全局关闭的混淆重新打开。
        if config.level != ObfuscationLevel::Off {
            if let Some(level) = options.level {
                config.level = level;
            }
        }
        if let Some(coverage) = options.min_identifier_coverage {
            config.min_identifier_coverage = coverage.clamp(0.0, 1.0);
        }
        config
    }
}

/// 请求级混淆选项（所有字段可选，缺省走服务端配置）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ObfuscationOptions {
    /// 是否启用；`false` 时强制关闭。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 覆盖混淆强度。
    #[serde(default)]
    pub level: Option<ObfuscationLevel>,
    /// 覆盖覆盖率下限。
    #[serde(default)]
    pub min_identifier_coverage: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_parsing_is_case_insensitive() {
        assert_eq!(
            ObfuscationLevel::from_str("STANDARD").unwrap(),
            ObfuscationLevel::Standard
        );
        assert_eq!(
            ObfuscationLevel::from_str("high").unwrap(),
            ObfuscationLevel::Aggressive
        );
        assert!(ObfuscationLevel::from_str("extreme").is_err());
    }

    #[test]
    fn options_can_disable_or_override_level() {
        let base = ObfuscationConfig::default();
        let disabled = base.with_options(&ObfuscationOptions {
            enabled: Some(false),
            ..Default::default()
        });
        assert_eq!(disabled.level, ObfuscationLevel::Off);

        let aggressive = base.with_options(&ObfuscationOptions {
            level: Some(ObfuscationLevel::Aggressive),
            ..Default::default()
        });
        assert_eq!(aggressive.level, ObfuscationLevel::Aggressive);
    }

    #[test]
    fn invalid_config_is_rejected() {
        let config = ObfuscationConfig {
            session_ttl_secs: 0,
            ..ObfuscationConfig::default()
        };
        assert!(config.validate().is_err());
    }
}
