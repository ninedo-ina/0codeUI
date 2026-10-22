//! 防重放协议原语：请求头解析、动态令牌派生、规范串与 HMAC 签名。
//!
//! 所有函数均为纯函数，便于单测与客户端 SDK 对照实现。

use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::AppError;

type HmacSha256 = Hmac<Sha256>;

/// 时间戳请求头（Unix 秒）。
pub const TIMESTAMP_HEADER: &str = "x-timestamp";
/// 随机数请求头。
pub const NONCE_HEADER: &str = "x-nonce";
/// 动态令牌请求头（hex HMAC-SHA256）。
pub const DYNAMIC_TOKEN_HEADER: &str = "x-dynamic-token";
/// 请求签名请求头（hex HMAC-SHA256）。
pub const SIGNATURE_HEADER: &str = "x-signature";

/// 默认允许的时钟偏移（秒）。
pub const MAX_CLOCK_SKEW_SECS: i64 = 300;

/// 协议版本前缀：动态令牌派生。
const DYNAMIC_TOKEN_PREFIX: &str = "intimeui.replay.dynamic-token.v1";
/// 协议版本前缀：请求签名。
const SIGNATURE_PREFIX: &str = "INTIMEUI-REPLAY-V1";

/// 解析后的防重放请求头。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHeaders {
    /// Unix 秒时间戳。
    pub timestamp: i64,
    /// 客户端随机数。
    pub nonce: String,
    /// 动态令牌（hex）。
    pub dynamic_token: String,
    /// 请求签名（hex）。
    pub signature: String,
}

/// 从请求头解析并做格式校验（不含时间窗与密码学校验）。
pub fn parse_replay_headers(headers: &HeaderMap) -> Result<ReplayHeaders, AppError> {
    let timestamp_raw = header_value(headers, TIMESTAMP_HEADER)?;
    let timestamp: i64 = timestamp_raw
        .parse()
        .map_err(|_| AppError::invalid_param("x-timestamp must be a Unix timestamp in seconds"))?;
    let nonce = header_value(headers, NONCE_HEADER)?.to_string();
    validate_nonce(&nonce)?;
    let dynamic_token = header_value(headers, DYNAMIC_TOKEN_HEADER)?.to_string();
    let signature = header_value(headers, SIGNATURE_HEADER)?.to_string();

    Ok(ReplayHeaders {
        timestamp,
        nonce,
        dynamic_token,
        signature,
    })
}

/// 校验 Nonce 格式：16–128 个 URL 安全字符。
pub fn validate_nonce(nonce: &str) -> Result<(), AppError> {
    let valid_chars = nonce
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if !(16..=128).contains(&nonce.len()) || !valid_chars {
        return Err(AppError::invalid_param(
            "x-nonce must be 16..=128 characters of [A-Za-z0-9._:-]",
        ));
    }
    Ok(())
}

/// 校验时间戳是否落在允许的时钟偏移内。
pub fn validate_timestamp(timestamp: i64, now: i64, skew_secs: i64) -> Result<(), AppError> {
    let skew = skew_secs.max(0) as u64;
    if now.abs_diff(timestamp) > skew {
        return Err(AppError::invalid_param(format!(
            "x-timestamp is outside the allowed ±{skew_secs}s window"
        )));
    }
    Ok(())
}

/// 派生动态令牌：`HMAC-SHA256(secret, "intimeui.replay.dynamic-token.v1\n{timestamp}\n{nonce}")`。
pub fn derive_dynamic_token(secret: &[u8], timestamp: i64, nonce: &str) -> String {
    let message = format!("{DYNAMIC_TOKEN_PREFIX}\n{timestamp}\n{nonce}");
    hex::encode(hmac_sha256(secret, message.as_bytes()))
}

/// 请求体 SHA-256（小写 hex）。
pub fn body_sha256_hex(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

/// 构造待签名规范串。
///
/// ```text
/// INTIMEUI-REPLAY-V1
/// {METHOD}
/// {PATH_AND_QUERY}
/// {TIMESTAMP}
/// {NONCE}
/// {USER_ID}
/// {BODY_SHA256_HEX}
/// ```
pub fn canonical_string(
    method: &str,
    path_and_query: &str,
    timestamp: i64,
    nonce: &str,
    user_id: &str,
    body_sha256: &str,
) -> String {
    format!(
        "{SIGNATURE_PREFIX}\n{}\n{}\n{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        path_and_query,
        timestamp,
        nonce,
        user_id,
        body_sha256
    )
}

/// 以动态令牌为密钥计算规范串签名（小写 hex）。
pub fn sign_canonical(dynamic_token: &str, canonical: &str) -> String {
    hex::encode(hmac_sha256(dynamic_token.as_bytes(), canonical.as_bytes()))
}

/// 常量时间比较两个 hex 字符串。
pub fn constant_time_hex_eq(expected_hex: &str, actual_hex: &str) -> bool {
    let (Ok(expected), Ok(actual)) = (hex::decode(expected_hex), hex::decode(actual_hex)) else {
        return false;
    };
    expected.as_slice().ct_eq(actual.as_slice()).into()
}

/// 客户端签名器（同时供集成测试与后续客户端 SDK 参考）。
#[derive(Clone)]
pub struct ReplaySigner {
    secret: Vec<u8>,
}

impl ReplaySigner {
    /// 以共享密钥构造。
    pub fn new(secret: impl AsRef<[u8]>) -> Self {
        Self {
            secret: secret.as_ref().to_vec(),
        }
    }

    /// 为一次请求生成四个防重放头的值。
    pub fn sign(
        &self,
        method: &str,
        path_and_query: &str,
        timestamp: i64,
        nonce: &str,
        user_id: &str,
        body: &[u8],
    ) -> SignedHeaders {
        let dynamic_token = derive_dynamic_token(&self.secret, timestamp, nonce);
        let body_hash = body_sha256_hex(body);
        let canonical = canonical_string(
            method,
            path_and_query,
            timestamp,
            nonce,
            user_id,
            &body_hash,
        );
        let signature = sign_canonical(&dynamic_token, &canonical);
        SignedHeaders {
            timestamp,
            nonce: nonce.to_string(),
            dynamic_token,
            signature,
        }
    }
}

/// 一次签名产生的请求头值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedHeaders {
    /// Unix 秒时间戳。
    pub timestamp: i64,
    /// 随机数。
    pub nonce: String,
    /// 动态令牌。
    pub dynamic_token: String,
    /// 请求签名。
    pub signature: String,
}

impl SignedHeaders {
    /// 写入请求头。
    pub fn insert_into(&self, headers: &mut HeaderMap) {
        insert_header(headers, TIMESTAMP_HEADER, &self.timestamp.to_string());
        insert_header(headers, NONCE_HEADER, &self.nonce);
        insert_header(headers, DYNAMIC_TOKEN_HEADER, &self.dynamic_token);
        insert_header(headers, SIGNATURE_HEADER, &self.signature);
    }
}

/// 生成 34 字符的随机 Nonce（`n_` + UUID v4 去连字符）。
pub fn new_nonce() -> String {
    format!("n_{}", uuid::Uuid::new_v4().simple())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, AppError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::invalid_param(format!("missing replay protection header: {name}")))
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(value) = value.parse() {
        headers.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with(values: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in values {
            headers.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn dynamic_token_is_deterministic_and_timestamp_bound() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let first = derive_dynamic_token(secret, 1_700_000_000, "nonce-0123456789");
        let second = derive_dynamic_token(secret, 1_700_000_000, "nonce-0123456789");
        let later = derive_dynamic_token(secret, 1_700_000_001, "nonce-0123456789");
        assert_eq!(first, second);
        assert_ne!(first, later);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn signature_covers_method_path_nonce_user_and_body() {
        let signer = ReplaySigner::new("0123456789abcdef0123456789abcdef");
        let base = signer.sign(
            "POST",
            "/api/v1/ui/generate",
            1_700_000_000,
            "nonce-0123456789",
            "u1",
            b"{}",
        );
        let other_path = signer.sign(
            "POST",
            "/api/v1/intent/execute",
            1_700_000_000,
            "nonce-0123456789",
            "u1",
            b"{}",
        );
        let other_body = signer.sign(
            "POST",
            "/api/v1/ui/generate",
            1_700_000_000,
            "nonce-0123456789",
            "u1",
            b"{\"a\":1}",
        );
        assert_ne!(base.signature, other_path.signature);
        assert_ne!(base.signature, other_body.signature);
    }

    #[test]
    fn parse_headers_accepts_valid_values() {
        let headers = headers_with(&[
            (TIMESTAMP_HEADER, "1700000000"),
            (NONCE_HEADER, "nonce-0123456789"),
            (DYNAMIC_TOKEN_HEADER, "aa"),
            (SIGNATURE_HEADER, "bb"),
        ]);
        let parsed = parse_replay_headers(&headers).unwrap();
        assert_eq!(parsed.timestamp, 1_700_000_000);
        assert_eq!(parsed.nonce, "nonce-0123456789");
    }

    #[test]
    fn parse_headers_rejects_missing_and_malformed_values() {
        assert!(parse_replay_headers(&HeaderMap::new()).is_err());
        let headers = headers_with(&[
            (TIMESTAMP_HEADER, "not-a-number"),
            (NONCE_HEADER, "nonce-0123456789"),
            (DYNAMIC_TOKEN_HEADER, "aa"),
            (SIGNATURE_HEADER, "bb"),
        ]);
        assert!(parse_replay_headers(&headers).is_err());
    }

    #[test]
    fn nonce_format_is_enforced() {
        assert!(validate_nonce("short").is_err());
        assert!(validate_nonce("has space 0123456789").is_err());
        assert!(validate_nonce("nonce-0123456789").is_ok());
        assert!(validate_nonce(&"a".repeat(129)).is_err());
    }

    #[test]
    fn timestamp_window_is_enforced() {
        assert!(validate_timestamp(1_000, 1_000, 300).is_ok());
        assert!(validate_timestamp(700, 1_000, 300).is_ok());
        assert!(validate_timestamp(699, 1_000, 300).is_err());
        assert!(validate_timestamp(1_301, 1_000, 300).is_err());
    }

    #[test]
    fn constant_time_compare_rejects_tampering() {
        let signer = ReplaySigner::new("0123456789abcdef0123456789abcdef");
        let signed = signer.sign("POST", "/p", 1, "nonce-0123456789", "u", b"body");
        assert!(constant_time_hex_eq(&signed.signature, &signed.signature));
        let mut tampered = signed.signature.clone();
        tampered.replace_range(0..1, if tampered.starts_with('0') { "1" } else { "0" });
        assert!(!constant_time_hex_eq(&signed.signature, &tampered));
        assert!(!constant_time_hex_eq(&signed.signature, "not-hex"));
    }
}
