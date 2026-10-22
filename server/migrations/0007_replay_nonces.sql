-- REQ-202610-1186：API 防重放 Nonce 去重表。
--
-- 以 (user_id, nonce) 为主键，保证同一用户同一 Nonce 只能成功消费一次；
-- expires_at 为 Unix 秒，服务端每次校验时顺带清理过期行。
CREATE TABLE IF NOT EXISTS replay_nonces (
    user_id      TEXT    NOT NULL,
    nonce        TEXT    NOT NULL,
    request_path TEXT    NOT NULL,
    expires_at   INTEGER NOT NULL,
    created_at   TEXT    NOT NULL,
    PRIMARY KEY (user_id, nonce)
);

CREATE INDEX IF NOT EXISTS idx_replay_nonces_expires_at
    ON replay_nonces(expires_at);
