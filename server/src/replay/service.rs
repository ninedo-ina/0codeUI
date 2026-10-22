//! 服务端防重放校验编排：时间窗 → 动态令牌 → 请求签名 → Nonce 去重。

use axum::http::HeaderMap;
use chrono::Utc;
use sqlx::SqlitePool;

use crate::config::AppConfig;
use crate::error::{AppError, ErrorCode};
use crate::repository::Repository;

use super::protocol::{
    body_sha256_hex, canonical_string, constant_time_hex_eq, derive_dynamic_token,
    parse_replay_headers, sign_canonical, validate_timestamp,
};
use super::SigningSecret;

/// 一次待校验请求的最小上下文。
#[derive(Debug)]
pub struct ReplayRequest<'a> {
    /// 已鉴权用户。
    pub user_id: &'a str,
    /// HTTP 方法（大写不敏感）。
    pub method: &'a str,
    /// 路径与查询串（如 `/api/v1/ui/generate?x=1`）。
    pub path_and_query: &'a str,
    /// 请求头。
    pub headers: &'a HeaderMap,
    /// 原始请求体字节。
    pub body: &'a [u8],
}

/// 防重放服务（挂在 [`crate::state::AppState`] 上，供中间件调用）。
#[derive(Debug, Clone)]
pub struct ReplayService {
    enabled: bool,
    secret: Option<SigningSecret>,
    clock_skew_secs: i64,
    nonce_ttl_secs: i64,
    max_body_bytes: usize,
    pool: SqlitePool,
}

impl ReplayService {
    /// 按配置构造。
    pub fn new(config: &AppConfig, pool: SqlitePool) -> Self {
        Self {
            enabled: config.replay_protection_enabled,
            secret: config.replay_signing_secret.clone(),
            clock_skew_secs: config.replay_clock_skew_secs,
            nonce_ttl_secs: config.replay_nonce_ttl_secs,
            max_body_bytes: config.replay_max_body_bytes,
            pool,
        }
    }

    /// 是否启用。
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 中间件读取请求体时允许的最大字节数。
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }

    /// 校验一次请求。
    ///
    /// 校验顺序刻意保证：先做无状态校验（时间窗 / 动态令牌 / 签名），
    /// 最后才消费 Nonce，避免无效请求提前占用合法 Nonce。
    pub async fn verify(&self, request: &ReplayRequest<'_>) -> Result<(), AppError> {
        if !self.enabled {
            return Ok(());
        }
        let secret = self.secret.as_ref().ok_or_else(|| {
            AppError::internal("replay protection is enabled but REPLAY_SIGNING_SECRET is missing")
        })?;

        let headers = parse_replay_headers(request.headers)?;
        let now = Utc::now().timestamp();
        validate_timestamp(headers.timestamp, now, self.clock_skew_secs)?;

        let expected_token =
            derive_dynamic_token(secret.as_bytes(), headers.timestamp, &headers.nonce);
        if !constant_time_hex_eq(&expected_token, &headers.dynamic_token) {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                "invalid dynamic token",
            ));
        }

        let body_hash = body_sha256_hex(request.body);
        let canonical = canonical_string(
            request.method,
            request.path_and_query,
            headers.timestamp,
            &headers.nonce,
            request.user_id,
            &body_hash,
        );
        let expected_signature = sign_canonical(&expected_token, &canonical);
        if !constant_time_hex_eq(&expected_signature, &headers.signature) {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                "invalid request signature",
            ));
        }

        let expires_at = now.saturating_add(self.nonce_ttl_secs);
        let accepted = Repository::new(self.pool.clone())
            .consume_replay_nonce(
                request.user_id,
                &headers.nonce,
                request.path_and_query,
                now,
                expires_at,
            )
            .await
            .map_err(AppError::internal)?;
        if !accepted {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "replay detected: nonce has already been used",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::replay::ReplaySigner;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    async fn service() -> (ReplayService, Db) {
        let db = Db::connect_in_memory().await.unwrap();
        db.migrate().await.unwrap();
        let config = AppConfig {
            replay_protection_enabled: true,
            replay_signing_secret: Some(SigningSecret::new(SECRET)),
            ..AppConfig::default()
        };
        (ReplayService::new(&config, db.pool().clone()), db)
    }

    fn request<'a>(headers: &'a HeaderMap, body: &'a [u8]) -> ReplayRequest<'a> {
        ReplayRequest {
            user_id: "u1",
            method: "POST",
            path_and_query: "/api/v1/telemetry/event",
            headers,
            body,
        }
    }

    #[tokio::test]
    async fn valid_signature_is_accepted_once() {
        let (service, _db) = service().await;
        let body = br#"{"events":[]}"#;
        let signer = ReplaySigner::new(SECRET);
        let signed = signer.sign(
            "POST",
            "/api/v1/telemetry/event",
            Utc::now().timestamp(),
            "nonce-0123456789",
            "u1",
            body,
        );
        let mut headers = HeaderMap::new();
        signed.insert_into(&mut headers);

        assert!(service.verify(&request(&headers, body)).await.is_ok());
        let error = service.verify(&request(&headers, body)).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
    }

    #[tokio::test]
    async fn tampered_body_is_rejected() {
        let (service, _db) = service().await;
        let signer = ReplaySigner::new(SECRET);
        let signed = signer.sign(
            "POST",
            "/api/v1/telemetry/event",
            Utc::now().timestamp(),
            "nonce-0123456790",
            "u1",
            br#"{"events":[]}"#,
        );
        let mut headers = HeaderMap::new();
        signed.insert_into(&mut headers);
        let error = service
            .verify(&request(&headers, br#"{"events":[{}]}"#))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Unauthorized);
    }

    #[tokio::test]
    async fn disabled_service_skips_all_checks() {
        let db = Db::connect_in_memory().await.unwrap();
        let service = ReplayService::new(&AppConfig::default(), db.pool().clone());
        assert!(service
            .verify(&request(&HeaderMap::new(), b"{}"))
            .await
            .is_ok());
    }
}
