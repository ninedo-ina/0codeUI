//! 配置加载：全部来自环境变量（本地开发可选用 `.env`），密钥不落库、不入仓。
//!
//! 所有可配置项见 [`AppConfig`] 字段注释；样例见仓库根目录 `.env.example`。

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::model::config::ModelConfig;
use crate::obfuscation::{ObfuscationConfig, ObfuscationLevel};
use crate::pipeline::config::PipelineConfig;
use crate::replay::SigningSecret;

/// 运行环境。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// 开发环境。
    Dev,
    /// 生产环境。
    Prod,
}

impl Environment {
    /// 环境名称（用于日志与健康检查输出）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Prod => "prod",
        }
    }
}

impl FromStr for Environment {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dev" | "development" => Ok(Self::Dev),
            "prod" | "production" => Ok(Self::Prod),
            other => Err(format!("expected `dev` or `prod`, got `{other}`")),
        }
    }
}

/// 日志输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// 本地开发友好的多行格式。
    Pretty,
    /// 生产友好的 JSON 单行格式。
    Json,
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pretty" | "text" => Ok(Self::Pretty),
            "json" => Ok(Self::Json),
            other => Err(format!("expected `pretty` or `json`, got `{other}`")),
        }
    }
}

/// 配置错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// 出错的配置项（环境变量名）。
    pub key: &'static str,
    /// 错误说明。
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid config `{}`: {}", self.key, self.message)
    }
}

impl std::error::Error for ConfigError {}

/// 服务配置（全部可通过环境变量注入）。
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// 运行环境：`APP_ENV`（dev / prod，默认 dev）。
    pub env: Environment,
    /// 监听地址：`APP_HOST`（默认 0.0.0.0）。
    pub host: String,
    /// 监听端口：`APP_PORT`（默认 8080）。
    pub port: u16,
    /// 日志级别：`LOG_LEVEL`（默认 info，亦兼容 `RUST_LOG`）。
    pub log_level: String,
    /// 日志格式：`LOG_FORMAT`（默认 dev=pretty / prod=json）。
    pub log_format: LogFormat,
    /// 开发鉴权模式：`AUTH_DEV_MODE`（默认 dev=true / prod=false）。
    ///
    /// 开启后接受 `Authorization: Bearer dev:<user_id>`，仅供本地联调；生产环境强制关闭。
    pub auth_dev_mode: bool,
    /// 静态令牌表：`AUTH_STATIC_TOKENS`（格式 `token1:user_id1,token2:user_id2`）。
    pub auth_static_tokens: HashMap<String, String>,
    /// 是否启用限流：`RATE_LIMIT_ENABLED`（默认 true）。
    pub rate_limit_enabled: bool,
    /// 限流速率：`RATE_LIMIT_RPS`（每秒补充令牌数，默认 50）。
    pub rate_limit_rps: f64,
    /// 限流桶容量：`RATE_LIMIT_BURST`（默认 100）。
    pub rate_limit_burst: u32,
    /// 请求处理超时秒数：`REQUEST_TIMEOUT_SECS`（默认 30）。
    pub request_timeout_secs: u64,
    /// 是否启用埋点防刷配额：`TELEMETRY_QUOTA_ENABLED`（默认 true）。
    pub telemetry_quota_enabled: bool,
    /// 埋点配额：每用户每分钟允许的事件条数（`TELEMETRY_QUOTA_PER_MINUTE`，默认 600）。
    pub telemetry_quota_per_minute: u32,
    /// 埋点单次上报最大条数（`TELEMETRY_MAX_BATCH_SIZE`，默认 100）。
    pub telemetry_max_batch_size: usize,
    /// 是否启用 API 防重放签名校验（`REPLAY_PROTECTION_ENABLED`，默认 false）。
    ///
    /// 兼容旧变量 `TELEMETRY_REPLAY_ENABLED`（仅作为未设置新变量时的回退）。
    pub replay_protection_enabled: bool,
    /// 防重放 HMAC 共享密钥（`REPLAY_SIGNING_SECRET`，启用时必填且至少 32 字节）。
    ///
    /// 仅从环境变量注入，`Debug` 输出已脱敏，不落库、不入仓。
    pub replay_signing_secret: Option<SigningSecret>,
    /// 允许的客户端时钟偏移秒数（`REPLAY_CLOCK_SKEW_SECS`，默认 300，范围 30..=900）。
    pub replay_clock_skew_secs: i64,
    /// Nonce 去重记录的保留秒数（`REPLAY_NONCE_TTL_SECS`，默认 600，且不小于 2×时钟偏移）。
    pub replay_nonce_ttl_secs: i64,
    /// 防重放中间件允许缓冲的最大请求体字节数（`REPLAY_MAX_BODY_BYTES`，默认 2 MiB）。
    pub replay_max_body_bytes: usize,
    /// 意图 Agent 模型输出最大尝试次数：`INTENT_AGENT_MAX_ATTEMPTS`（默认 2，范围 1..=3）。
    pub intent_agent_max_attempts: usize,
    /// 意图会话 TTL 秒数：`INTENT_SESSION_TTL_SECS`（默认 1800，最小 1）。
    pub intent_session_ttl_secs: u64,
    /// 数据库连接串：`DATABASE_URL`（默认 `sqlite://data/intimeui.db`）。
    pub database_url: String,
    /// UI 生成模型接入与推理服务配置（`MODEL_*`）。
    pub model: ModelConfig,
    /// 画像 EWMA 基础平滑系数：`PROFILE_EWMA_ALPHA`（默认 0.15）。
    pub profile_ewma_alpha: f64,
    /// 画像 EWMA 平滑系数上限：`PROFILE_MAX_ALPHA`（默认 0.6）。
    pub profile_max_alpha: f64,
    /// 画像两次自动更新的最小间隔秒数：`PROFILE_MIN_UPDATE_INTERVAL_SECS`（默认 60）。
    pub profile_min_update_interval_secs: u64,
    /// 单次画像刷新最多处理的样本数：`PROFILE_MAX_OBSERVATIONS`（默认 1000）。
    pub profile_max_observations: usize,
    /// AI 生成管线配置（`PIPELINE_*`）。
    pub pipeline: PipelineConfig,
    /// 是否启用输出混淆：`UI_OBFUSCATION_ENABLED`（默认 true）。
    pub ui_obfuscation_enabled: bool,
    /// 混淆强度：`UI_OBFUSCATION_LEVEL`（off / basic / standard / aggressive，默认 standard）。
    pub ui_obfuscation_level: ObfuscationLevel,
    /// 会话类名映射表 TTL：`UI_OBFUSCATION_SESSION_TTL_SECS`（默认 1800）。
    pub ui_obfuscation_session_ttl_secs: u64,
    /// 会话映射表容量：`UI_OBFUSCATION_MAX_SESSIONS`（默认 1000）。
    pub ui_obfuscation_max_sessions: usize,
    /// 非保留标识符覆盖率下限：`UI_OBFUSCATION_MIN_IDENTIFIER_COVERAGE`（默认 0.8）。
    pub ui_obfuscation_min_identifier_coverage: f64,
}

impl AppConfig {
    /// 从环境变量加载配置。
    pub fn from_env() -> Result<Self, ConfigError> {
        let env = match optional_env("APP_ENV") {
            Some(value) => Environment::from_str(&value).map_err(|message| ConfigError {
                key: "APP_ENV",
                message,
            })?,
            None => Environment::Dev,
        };

        let host = env_string("APP_HOST", "0.0.0.0");
        let port = env_parse("APP_PORT", 8080u16)?;
        let log_level = optional_env("LOG_LEVEL")
            .or_else(|| optional_env("RUST_LOG"))
            .unwrap_or_else(|| "info".to_string());

        let log_format = match optional_env("LOG_FORMAT") {
            Some(value) => LogFormat::from_str(&value).map_err(|message| ConfigError {
                key: "LOG_FORMAT",
                message,
            })?,
            None => match env {
                Environment::Dev => LogFormat::Pretty,
                Environment::Prod => LogFormat::Json,
            },
        };

        let auth_dev_mode = match optional_env("AUTH_DEV_MODE") {
            Some(value) => parse_bool("AUTH_DEV_MODE", &value)?,
            None => env == Environment::Dev,
        };
        if env == Environment::Prod && auth_dev_mode {
            return Err(ConfigError {
                key: "AUTH_DEV_MODE",
                message: "dev auth mode must be disabled in prod".to_string(),
            });
        }

        let auth_static_tokens = match optional_env("AUTH_STATIC_TOKENS") {
            Some(value) => parse_static_tokens(&value).map_err(|message| ConfigError {
                key: "AUTH_STATIC_TOKENS",
                message,
            })?,
            None => HashMap::new(),
        };

        let rate_limit_enabled = match optional_env("RATE_LIMIT_ENABLED") {
            Some(value) => parse_bool("RATE_LIMIT_ENABLED", &value)?,
            None => true,
        };
        let rate_limit_rps = env_parse("RATE_LIMIT_RPS", 50.0f64)?;
        if !(rate_limit_rps.is_finite() && rate_limit_rps > 0.0) {
            return Err(ConfigError {
                key: "RATE_LIMIT_RPS",
                message: "must be a positive number".to_string(),
            });
        }
        let rate_limit_burst = env_parse("RATE_LIMIT_BURST", 100u32)?;
        if rate_limit_burst == 0 {
            return Err(ConfigError {
                key: "RATE_LIMIT_BURST",
                message: "must be >= 1".to_string(),
            });
        }
        let request_timeout_secs = env_parse("REQUEST_TIMEOUT_SECS", 30u64)?;
        if request_timeout_secs == 0 {
            return Err(ConfigError {
                key: "REQUEST_TIMEOUT_SECS",
                message: "must be >= 1".to_string(),
            });
        }

        let telemetry_quota_enabled = match optional_env("TELEMETRY_QUOTA_ENABLED") {
            Some(value) => parse_bool("TELEMETRY_QUOTA_ENABLED", &value)?,
            None => true,
        };
        let telemetry_quota_per_minute = env_parse("TELEMETRY_QUOTA_PER_MINUTE", 600u32)?;
        if telemetry_quota_per_minute == 0 {
            return Err(ConfigError {
                key: "TELEMETRY_QUOTA_PER_MINUTE",
                message: "must be >= 1".to_string(),
            });
        }
        let telemetry_max_batch_size = env_parse("TELEMETRY_MAX_BATCH_SIZE", 100usize)?;
        if !(1..=1000).contains(&telemetry_max_batch_size) {
            return Err(ConfigError {
                key: "TELEMETRY_MAX_BATCH_SIZE",
                message: "must be within 1..=1000".to_string(),
            });
        }
        if telemetry_quota_per_minute < telemetry_max_batch_size as u32 {
            return Err(ConfigError {
                key: "TELEMETRY_QUOTA_PER_MINUTE",
                message: "must be >= TELEMETRY_MAX_BATCH_SIZE".to_string(),
            });
        }
        let replay_protection_enabled = match optional_env("REPLAY_PROTECTION_ENABLED") {
            Some(value) => parse_bool("REPLAY_PROTECTION_ENABLED", &value)?,
            None => match optional_env("TELEMETRY_REPLAY_ENABLED") {
                Some(value) => parse_bool("TELEMETRY_REPLAY_ENABLED", &value)?,
                None => false,
            },
        };
        let replay_signing_secret = optional_env("REPLAY_SIGNING_SECRET").map(SigningSecret::new);
        if let Some(secret) = &replay_signing_secret {
            if secret.len() < 32 {
                return Err(ConfigError {
                    key: "REPLAY_SIGNING_SECRET",
                    message: "must be at least 32 bytes".to_string(),
                });
            }
        }
        if replay_protection_enabled && replay_signing_secret.is_none() {
            return Err(ConfigError {
                key: "REPLAY_SIGNING_SECRET",
                message: "is required when REPLAY_PROTECTION_ENABLED=true".to_string(),
            });
        }
        let replay_clock_skew_secs = env_parse("REPLAY_CLOCK_SKEW_SECS", 300i64)?;
        if !(30..=900).contains(&replay_clock_skew_secs) {
            return Err(ConfigError {
                key: "REPLAY_CLOCK_SKEW_SECS",
                message: "must be within 30..=900".to_string(),
            });
        }
        let replay_nonce_ttl_secs = env_parse("REPLAY_NONCE_TTL_SECS", 600i64)?;
        if replay_nonce_ttl_secs < replay_clock_skew_secs.saturating_mul(2) {
            return Err(ConfigError {
                key: "REPLAY_NONCE_TTL_SECS",
                message: "must be >= 2 × REPLAY_CLOCK_SKEW_SECS".to_string(),
            });
        }
        let replay_max_body_bytes = env_parse("REPLAY_MAX_BODY_BYTES", 2 * 1024 * 1024usize)?;
        if !(1024..=16 * 1024 * 1024).contains(&replay_max_body_bytes) {
            return Err(ConfigError {
                key: "REPLAY_MAX_BODY_BYTES",
                message: "must be within 1024..=16777216".to_string(),
            });
        }
        let intent_agent_max_attempts = env_parse("INTENT_AGENT_MAX_ATTEMPTS", 2usize)?;
        if !(1..=3).contains(&intent_agent_max_attempts) {
            return Err(ConfigError {
                key: "INTENT_AGENT_MAX_ATTEMPTS",
                message: "must be between 1 and 3".to_string(),
            });
        }
        let intent_session_ttl_secs = env_parse("INTENT_SESSION_TTL_SECS", 1800u64)?;
        if intent_session_ttl_secs == 0 {
            return Err(ConfigError {
                key: "INTENT_SESSION_TTL_SECS",
                message: "must be >= 1".to_string(),
            });
        }
        let database_url = env_string("DATABASE_URL", "sqlite://data/intimeui.db");
        let model = ModelConfig::from_env()?;
        if env == Environment::Prod && model.provider.is_mock() {
            return Err(ConfigError {
                key: "MODEL_PROVIDER",
                message: "mock provider is not allowed in prod".to_string(),
            });
        }

        let profile_ewma_alpha = env_parse("PROFILE_EWMA_ALPHA", 0.15f64)?;
        if !(profile_ewma_alpha.is_finite()
            && profile_ewma_alpha > 0.0
            && profile_ewma_alpha <= 1.0)
        {
            return Err(ConfigError {
                key: "PROFILE_EWMA_ALPHA",
                message: "must be in (0, 1]".to_string(),
            });
        }
        let profile_max_alpha = env_parse("PROFILE_MAX_ALPHA", 0.6f64)?;
        if !(profile_max_alpha.is_finite()
            && profile_max_alpha >= profile_ewma_alpha
            && profile_max_alpha <= 1.0)
        {
            return Err(ConfigError {
                key: "PROFILE_MAX_ALPHA",
                message: "must be in [PROFILE_EWMA_ALPHA, 1]".to_string(),
            });
        }
        let profile_min_update_interval_secs =
            env_parse("PROFILE_MIN_UPDATE_INTERVAL_SECS", 60u64)?;
        let profile_max_observations = env_parse("PROFILE_MAX_OBSERVATIONS", 1000usize)?;
        if !(1..=10_000).contains(&profile_max_observations) {
            return Err(ConfigError {
                key: "PROFILE_MAX_OBSERVATIONS",
                message: "must be in 1..=10000".to_string(),
            });
        }
        let pipeline = PipelineConfig::from_env()?;

        let ui_obfuscation_enabled = match optional_env("UI_OBFUSCATION_ENABLED") {
            Some(value) => parse_bool("UI_OBFUSCATION_ENABLED", &value)?,
            None => true,
        };
        let ui_obfuscation_level = match optional_env("UI_OBFUSCATION_LEVEL") {
            Some(value) => ObfuscationLevel::from_str(&value).map_err(|message| ConfigError {
                key: "UI_OBFUSCATION_LEVEL",
                message,
            })?,
            None => ObfuscationLevel::Standard,
        };
        let ui_obfuscation_session_ttl_secs =
            env_parse("UI_OBFUSCATION_SESSION_TTL_SECS", 1800u64)?;
        if ui_obfuscation_session_ttl_secs == 0 {
            return Err(ConfigError {
                key: "UI_OBFUSCATION_SESSION_TTL_SECS",
                message: "must be >= 1".to_string(),
            });
        }
        let ui_obfuscation_max_sessions = env_parse("UI_OBFUSCATION_MAX_SESSIONS", 1000usize)?;
        if ui_obfuscation_max_sessions == 0 {
            return Err(ConfigError {
                key: "UI_OBFUSCATION_MAX_SESSIONS",
                message: "must be >= 1".to_string(),
            });
        }
        let ui_obfuscation_min_identifier_coverage =
            env_parse("UI_OBFUSCATION_MIN_IDENTIFIER_COVERAGE", 0.8f64)?;
        if !(0.0..=1.0).contains(&ui_obfuscation_min_identifier_coverage) {
            return Err(ConfigError {
                key: "UI_OBFUSCATION_MIN_IDENTIFIER_COVERAGE",
                message: "must be within 0.0..=1.0".to_string(),
            });
        }

        Ok(Self {
            env,
            host,
            port,
            log_level,
            log_format,
            auth_dev_mode,
            auth_static_tokens,
            rate_limit_enabled,
            rate_limit_rps,
            rate_limit_burst,
            request_timeout_secs,
            telemetry_quota_enabled,
            telemetry_quota_per_minute,
            telemetry_max_batch_size,
            replay_protection_enabled,
            replay_signing_secret,
            replay_clock_skew_secs,
            replay_nonce_ttl_secs,
            replay_max_body_bytes,
            intent_agent_max_attempts,
            intent_session_ttl_secs,
            database_url,
            model,
            profile_ewma_alpha,
            profile_max_alpha,
            profile_min_update_interval_secs,
            profile_max_observations,
            pipeline,
            ui_obfuscation_enabled,
            ui_obfuscation_level,
            ui_obfuscation_session_ttl_secs,
            ui_obfuscation_max_sessions,
            ui_obfuscation_min_identifier_coverage,
        })
    }
}

impl AppConfig {
    /// 当前配置对应的输出混淆参数。
    pub fn obfuscation_config(&self) -> ObfuscationConfig {
        ObfuscationConfig {
            level: if self.ui_obfuscation_enabled {
                self.ui_obfuscation_level
            } else {
                ObfuscationLevel::Off
            },
            session_ttl_secs: self.ui_obfuscation_session_ttl_secs,
            max_sessions: self.ui_obfuscation_max_sessions,
            min_identifier_coverage: self.ui_obfuscation_min_identifier_coverage,
        }
    }
}

impl Default for AppConfig {
    /// 开发环境友好的默认配置（供测试与本地快速启动使用）。
    fn default() -> Self {
        Self {
            env: Environment::Dev,
            host: "0.0.0.0".to_string(),
            port: 8080,
            log_level: "info".to_string(),
            log_format: LogFormat::Pretty,
            auth_dev_mode: true,
            auth_static_tokens: HashMap::new(),
            rate_limit_enabled: true,
            rate_limit_rps: 50.0,
            rate_limit_burst: 100,
            request_timeout_secs: 30,
            telemetry_quota_enabled: true,
            telemetry_quota_per_minute: 600,
            telemetry_max_batch_size: 100,
            replay_protection_enabled: false,
            replay_signing_secret: None,
            replay_clock_skew_secs: 300,
            replay_nonce_ttl_secs: 600,
            replay_max_body_bytes: 2 * 1024 * 1024,
            intent_agent_max_attempts: 2,
            intent_session_ttl_secs: 1800,
            database_url: "sqlite://data/intimeui.db".to_string(),
            model: ModelConfig::default(),
            profile_ewma_alpha: 0.15,
            profile_max_alpha: 0.6,
            profile_min_update_interval_secs: 60,
            profile_max_observations: 1000,
            pipeline: PipelineConfig::default(),
            ui_obfuscation_enabled: true,
            ui_obfuscation_level: ObfuscationLevel::Standard,
            ui_obfuscation_session_ttl_secs: 1800,
            ui_obfuscation_max_sessions: 1000,
            ui_obfuscation_min_identifier_coverage: 0.8,
        }
    }
}

pub(crate) fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn env_string(key: &str, default: &str) -> String {
    optional_env(key).unwrap_or_else(|| default.to_string())
}

pub(crate) fn env_parse<T>(key: &'static str, default: T) -> Result<T, ConfigError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    match optional_env(key) {
        Some(value) => value.parse::<T>().map_err(|error| ConfigError {
            key,
            message: error.to_string(),
        }),
        None => Ok(default),
    }
}

pub(crate) fn parse_bool(key: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(ConfigError {
            key,
            message: format!("expected boolean, got `{other}`"),
        }),
    }
}

/// 解析静态令牌表：`token1:user_id1,token2:user_id2`。
fn parse_static_tokens(value: &str) -> Result<HashMap<String, String>, String> {
    let mut tokens = HashMap::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (token, user_id) = entry
            .split_once(':')
            .ok_or_else(|| format!("entry `{entry}` must be in `token:user_id` form"))?;
        let token = token.trim();
        let user_id = user_id.trim();
        if token.is_empty() || user_id.is_empty() {
            return Err(format!("entry `{entry}` must be in `token:user_id` form"));
        }
        tokens.insert(token.to_string(), user_id.to_string());
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_static_tokens_accepts_pairs() {
        let tokens = parse_static_tokens("t1:u1, t2:u2").unwrap();
        assert_eq!(tokens.get("t1").map(String::as_str), Some("u1"));
        assert_eq!(tokens.get("t2").map(String::as_str), Some("u2"));
    }

    #[test]
    fn parse_static_tokens_rejects_malformed_entries() {
        assert!(parse_static_tokens("bad-entry").is_err());
        assert!(parse_static_tokens("token:").is_err());
        assert!(parse_static_tokens(":user").is_err());
    }

    #[test]
    fn environment_parsing_is_case_insensitive() {
        assert_eq!(Environment::from_str("dev").unwrap(), Environment::Dev);
        assert_eq!(Environment::from_str("PROD").unwrap(), Environment::Prod);
        assert!(Environment::from_str("staging").is_err());
    }

    #[test]
    fn default_config_is_dev_friendly() {
        let config = AppConfig::default();
        assert_eq!(config.env, Environment::Dev);
        assert!(config.auth_dev_mode);
        assert!(config.rate_limit_enabled);
    }
}
