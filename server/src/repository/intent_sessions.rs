//! 意图会话状态存取（REQ-202610-1177）。
//!
//! 会话按 `session_id` 唯一，按 `user_id` 隔离；上层负责 TTL 判断，
//! 本模块只做纯存取，不解释业务语义。

use chrono::{DateTime, Utc};

use crate::intent::model::{ActionType, IntentSession, SessionContext};

use super::{map_db_error, Repository, RepositoryError};

#[derive(Debug, sqlx::FromRow)]
struct IntentSessionRow {
    session_id: String,
    user_id: String,
    active_page: Option<String>,
    entity_id: Option<String>,
    last_intent_summary: Option<String>,
    last_action_type: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl IntentSessionRow {
    fn into_session(self) -> IntentSession {
        IntentSession {
            session_id: self.session_id,
            user_id: self.user_id,
            context: SessionContext {
                active_page: self.active_page,
                entity_id: self.entity_id,
            },
            last_intent_summary: self.last_intent_summary,
            last_action_type: self
                .last_action_type
                .as_deref()
                .and_then(ActionType::from_db),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

impl Repository {
    /// 按会话 ID 读取会话状态（不存在返回 `None`）。
    pub async fn find_intent_session(
        &self,
        session_id: &str,
    ) -> Result<Option<IntentSession>, RepositoryError> {
        let row: Option<IntentSessionRow> = sqlx::query_as(
            "SELECT session_id, user_id, active_page, entity_id,
                    last_intent_summary, last_action_type, created_at, updated_at
             FROM intent_sessions WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;

        Ok(row.map(IntentSessionRow::into_session))
    }

    /// 写入或更新会话状态（`created_at` 在更新时保持不变）。
    pub async fn upsert_intent_session(
        &self,
        session: &IntentSession,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO intent_sessions
                 (session_id, user_id, active_page, entity_id,
                  last_intent_summary, last_action_type, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
                 user_id = excluded.user_id,
                 active_page = excluded.active_page,
                 entity_id = excluded.entity_id,
                 last_intent_summary = excluded.last_intent_summary,
                 last_action_type = excluded.last_action_type,
                 updated_at = excluded.updated_at",
        )
        .bind(&session.session_id)
        .bind(&session.user_id)
        .bind(&session.context.active_page)
        .bind(&session.context.entity_id)
        .bind(&session.last_intent_summary)
        .bind(session.last_action_type.map(ActionType::as_str))
        .bind(session.created_at)
        .bind(session.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(())
    }
}
