//! 防重放 Nonce 去重（REQ-202610-1186）。
//!
//! `(user_id, nonce)` 为主键；`INSERT OR IGNORE` 的受影响行数即是否首次消费。
//! 每次消费前顺带删除过期行，避免表无限增长。

use super::{map_db_error, Repository, RepositoryError};

impl Repository {
    /// 消费一个 Nonce；返回 `true` 表示首次使用，`false` 表示重放。
    pub async fn consume_replay_nonce(
        &self,
        user_id: &str,
        nonce: &str,
        request_path: &str,
        now: i64,
        expires_at: i64,
    ) -> Result<bool, RepositoryError> {
        sqlx::query("DELETE FROM replay_nonces WHERE expires_at <= ?")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(map_db_error)?;

        let result = sqlx::query(
            "INSERT OR IGNORE INTO replay_nonces
                 (user_id, nonce, request_path, expires_at, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(nonce)
        .bind(request_path)
        .bind(expires_at)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?;

        Ok(result.rows_affected() == 1)
    }
}
