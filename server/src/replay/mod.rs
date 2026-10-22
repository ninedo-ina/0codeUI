//! API 防重放签名（REQ-202610-1186）。
//!
//! 协议：`HMAC-SHA256 + 派生式动态令牌 + Nonce + 时间窗 + 服务端 Nonce 去重`。
//! - [`protocol`]：请求头解析、动态令牌派生、规范串与签名算法；
//! - [`service`]：服务端校验编排与 Nonce 消费。
//!
//! 客户端签名约定见 `docs/replay-protection.md`。

pub mod protocol;
pub mod service;

use std::fmt;

pub use protocol::{
    body_sha256_hex, canonical_string, derive_dynamic_token, new_nonce, parse_replay_headers,
    sign_canonical, validate_nonce, validate_timestamp, ReplayHeaders, ReplaySigner, SignedHeaders,
    DYNAMIC_TOKEN_HEADER, MAX_CLOCK_SKEW_SECS, NONCE_HEADER, SIGNATURE_HEADER, TIMESTAMP_HEADER,
};
pub use service::{ReplayRequest, ReplayService};

/// HMAC 签名密钥（`REPLAY_SIGNING_SECRET`）。
///
/// `Debug` 输出固定脱敏，避免密钥进入日志或 panic 信息。
#[derive(Clone, PartialEq, Eq)]
pub struct SigningSecret(Vec<u8>);

impl SigningSecret {
    /// 由任意字符串构造；长度校验由 [`crate::config::AppConfig::from_env`] 负责。
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into().into_bytes())
    }

    /// 原始字节（仅 crate 内可见）。
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// 密钥长度（字节）。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SigningSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigningSecret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_secret_debug_output_is_redacted() {
        let secret = SigningSecret::new("super-secret-value-0123456789abcdef");
        let rendered = format!("{secret:?}");
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains("super-secret-value"));
    }

    #[test]
    fn signing_secret_reports_length() {
        let secret = SigningSecret::new("0123456789abcdef0123456789abcdef");
        assert_eq!(secret.len(), 32);
        assert!(!secret.is_empty());
    }
}
