-- 埋点事件采集（REQ-202610-1174，PRD §3.2）
--
-- 数据最小化：只存交互指标（停留时长 / 点击率 / 误触率 / 滚动深度）与
-- 定位所需的 user_id、page_id、时间戳；不存页面内容、URL、文本、截图等敏感数据。
--
-- 幂等性：sqlx 迁移器通过 _sqlx_migrations 记录版本；本文件同时使用 IF NOT EXISTS，
-- 重复执行不会报错、不会重复建表。

CREATE TABLE IF NOT EXISTS telemetry_events (
    id                 TEXT PRIMARY KEY NOT NULL,
    event_id           TEXT NOT NULL,
    user_id            TEXT NOT NULL,
    page_id            TEXT NOT NULL,
    dwell_time         INTEGER NOT NULL
                       CHECK (dwell_time >= 0 AND dwell_time <= 86400000),
    element_click_rate REAL NOT NULL
                       CHECK (element_click_rate >= 0.0 AND element_click_rate <= 1.0),
    misclick_rate      REAL NOT NULL
                       CHECK (misclick_rate >= 0.0 AND misclick_rate <= 1.0),
    scroll_depth       REAL NOT NULL
                       CHECK (scroll_depth >= 0.0 AND scroll_depth <= 1.0),
    occurred_at        TEXT NOT NULL,
    received_at        TEXT NOT NULL,
    UNIQUE (user_id, event_id)
);

-- 查询维度：按用户 / 页面 / 时间（验收要求「按用户/页面/时间可查」）。
CREATE INDEX IF NOT EXISTS idx_telemetry_events_user_time
    ON telemetry_events(user_id, occurred_at);
CREATE INDEX IF NOT EXISTS idx_telemetry_events_page_time
    ON telemetry_events(page_id, occurred_at);
CREATE INDEX IF NOT EXISTS idx_telemetry_events_time
    ON telemetry_events(occurred_at);
CREATE INDEX IF NOT EXISTS idx_telemetry_events_user_page_time
    ON telemetry_events(user_id, page_id, occurred_at);
