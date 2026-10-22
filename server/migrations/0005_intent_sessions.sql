-- InTimeUI 意图会话状态（REQ-202610-1177）
--
-- 会话状态用于在同一 session_id 内传递上下文：
--   active_page：当前活动页面（如 ORDER_DETAIL_PAGE）；
--   entity_id：当前选中实体（如 ORD_20261014_001）。
-- 会话按用户隔离，过期由应用层按 INTENT_SESSION_TTL_SECS 惰性重置。
-- 迁移编号 0005 预留 1174/1175/1176 的 0002~0004，合并顺序不影响本表。

CREATE TABLE IF NOT EXISTS intent_sessions (
    session_id          TEXT PRIMARY KEY NOT NULL,
    user_id             TEXT NOT NULL,
    active_page         TEXT,
    entity_id           TEXT,
    last_intent_summary TEXT,
    last_action_type    TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_intent_sessions_user_id ON intent_sessions(user_id);
CREATE INDEX IF NOT EXISTS idx_intent_sessions_updated_at ON intent_sessions(updated_at);
