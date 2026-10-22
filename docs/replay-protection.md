# API 防重放签名（REQ-202610-1186）

面向客户端（含 REQ-202610-1186 后续的客户端实现任务 #21）与服务端联调的完整协议说明。
协议版本：`INTIMEUI-REPLAY-V1`。

## 1. 目标与覆盖范围

服务端对以下三条写接口启用防重放签名校验：

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/api/v1/ui/generate` | UI 生成（03） |
| POST | `/api/v1/intent/execute` | 意图执行（09） |
| POST | `/api/v1/telemetry/event` | 埋点上报（06） |

其余接口（含所有 GET 查询）不要求签名。开关由 `REPLAY_PROTECTION_ENABLED` 控制，默认关闭；
开启后上述接口缺少或伪造签名一律拒绝，属于 fail-closed。

协议同时防御：

- **重放**：同一用户重复使用同一 Nonce 返回 `40900`；
- **篡改**：方法、路径与查询串、时间戳、Nonce、用户、请求体任一字节变化都会导致签名不匹配；
- **过期请求**：时间戳超出允许窗口返回 `40000`；
- **动态凭据伪造**：动态令牌由服务端共享密钥派生，攻击者无法仅凭抓包构造新请求。

> 传输层必须使用 HTTPS。签名不替代 TLS，也不替代 `Authorization` 鉴权：请求必须先通过
> Bearer 鉴权，再校验防重放签名。

## 2. 请求头

| 请求头 | 必填 | 格式 | 说明 |
| --- | --- | --- | --- |
| `x-timestamp` | 是 | Unix 秒（十进制整数） | 客户端签名时刻 |
| `x-nonce` | 是 | 16–128 字符，`[A-Za-z0-9._:-]` | 一次性随机数，建议 ≥128 bit 熵 |
| `x-dynamic-token` | 是 | 64 位小写 hex | 本次请求的动态令牌 |
| `x-signature` | 是 | 64 位小写 hex | 本次请求的 HMAC-SHA256 签名 |

`Authorization: Bearer <token>` 仍按原有方式携带。

## 3. 密钥与动态令牌

服务端通过环境变量 `REPLAY_SIGNING_SECRET` 注入共享密钥（至少 32 字节，建议 32 字节随机值）。
客户端必须通过安全渠道获得同一密钥；**严禁把生产密钥写进公开前端代码或仓库**。
浏览器 / 移动端建议由业务后端在登录后下发短期密钥，或由业务后端代为签名。

动态令牌按请求派生，不落库、不缓存：

```text
dynamic_token = HMAC-SHA256(
    key  = REPLAY_SIGNING_SECRET,
    data = "intimeui.replay.dynamic-token.v1\n" + timestamp + "\n" + nonce
)
```

输出为小写 hex（64 字符）。

## 4. 请求签名

### 4.1 请求体摘要

对**实际发送的原始请求体字节**计算 SHA-256，输出小写 hex：

```text
body_sha256 = SHA-256(raw_body_bytes)
```

不得先解析 JSON 再重新序列化后计算，否则空格、字段顺序、转义差异都会导致签名不一致。
无请求体时对空字节序列计算。

### 4.2 规范串

各字段以单个 `\n` 连接，最后一行末尾**不加**换行：

```text
INTIMEUI-REPLAY-V1
{METHOD}
{PATH_AND_QUERY}
{TIMESTAMP}
{NONCE}
{USER_ID}
{BODY_SHA256}
```

- `METHOD`：大写 HTTP 方法，例如 `POST`；
- `PATH_AND_QUERY`：请求路径与查询串，必须与 HTTP 请求行完全一致（含 URL 编码与 `?`）；
- `TIMESTAMP`：与 `x-timestamp` 相同的十进制字符串；
- `NONCE`：与 `x-nonce` 相同；
- `USER_ID`：Bearer 凭据解析出的用户标识（不是请求体里的 `user_id`）；
- `BODY_SHA256`：上一节计算的小写 hex。

### 4.3 签名

```text
signature = HMAC-SHA256(
    key  = dynamic_token（64 位 hex 字符串的 ASCII 字节）,
    data = canonical_string 的 UTF-8 字节
)
```

输出为小写 hex（64 字符）。

## 5. 客户端生成步骤

1. 生成一次性 Nonce：使用 CSPRNG 生成 ≥16 字节随机数，编码为 16–128 个允许字符；
   推荐 `n_` + 32 位 hex。
2. 取当前 Unix 秒作为 `timestamp`；先通过 NTP 或服务端 `Date` 响应头校准时钟。
3. 序列化请求体并保留最终字节（发送与签名必须使用同一份字节）。
4. 计算 `dynamic_token`。
5. 计算 `body_sha256` 与规范串，再计算 `signature`。
6. 设置四个请求头并发送。
7. **无论成功失败，Nonce 都不得复用**；重试必须生成新的 Nonce 与时间戳。

### 5.1 JavaScript（Web Crypto，浏览器 / Node 18+）

```js
const enc = new TextEncoder();

const toHex = (buffer) =>
  [...new Uint8Array(buffer)].map((b) => b.toString(16).padStart(2, "0")).join("");

async function hmacSha256Hex(keyBytes, messageBytes) {
  const key = await crypto.subtle.importKey(
    "raw",
    keyBytes,
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return toHex(await crypto.subtle.sign("HMAC", key, messageBytes));
}

export async function signRequest({
  secret,
  method,
  pathAndQuery,
  userId,
  bodyBytes, // Uint8Array：实际发送的原始字节
  timestamp = Math.floor(Date.now() / 1000),
  nonce = "n_" + toHex(crypto.getRandomValues(new Uint8Array(16))),
}) {
  const secretBytes = enc.encode(secret);
  const dynamicToken = await hmacSha256Hex(
    secretBytes,
    enc.encode(`intimeui.replay.dynamic-token.v1\n${timestamp}\n${nonce}`),
  );
  const bodySha256 = toHex(await crypto.subtle.digest("SHA-256", bodyBytes));
  const canonical = [
    "INTIMEUI-REPLAY-V1",
    method.toUpperCase(),
    pathAndQuery,
    String(timestamp),
    nonce,
    userId,
    bodySha256,
  ].join("\n");
  const signature = await hmacSha256Hex(enc.encode(dynamicToken), enc.encode(canonical));

  return {
    "x-timestamp": String(timestamp),
    "x-nonce": nonce,
    "x-dynamic-token": dynamicToken,
    "x-signature": signature,
  };
}
```

### 5.2 Rust（与服务端测试共用 `ReplaySigner`）

```rust
use intimeui_server::replay::ReplaySigner;

let body = serde_json::to_vec(&payload)?;
let signed = ReplaySigner::new(secret).sign(
    "POST",
    "/api/v1/telemetry/event",
    chrono::Utc::now().timestamp(),
    &intimeui_server::replay::new_nonce(),
    user_id,
    &body,
);
let mut request = http::Request::builder()
    .method("POST")
    .uri("/api/v1/telemetry/event")
    .header("content-type", "application/json")
    .header("authorization", format!("Bearer {token}"))
    .body(body)?;
signed.insert_into(request.headers_mut());
```

## 6. 服务端校验顺序

1. `Authorization` 鉴权，得到 `AuthUser.user_id`；
2. 防重放开关关闭 → 直接放行；
3. 解析四个头并校验格式（缺失 / 非法 → `40000`）；
4. 校验时间窗（默认 ±300 秒，`REPLAY_CLOCK_SKEW_SECS` 可配）；
5. 派生动态令牌并常量时间比较（不匹配 → `40100`）；
6. 计算请求体 SHA-256、规范串与签名，常量时间比较（不匹配 → `40100`）；
7. 在 SQLite `replay_nonces` 表消费 Nonce：
   - 首次使用 → 放行；
   - 已存在且未过期 → `40900`；
8. 重建请求体并交给 handler。

**顺序保证**：签名不通过的请求不会写入 Nonce 表，避免攻击者用无效请求消耗合法客户端的 Nonce。

## 7. 时钟偏差与失败处理

| 场景 | 服务端返回 | 客户端处理 |
| --- | --- | --- |
| 缺少 / 格式错误的头 | `40000` | 检查四个头是否全部设置；Nonce 是否符合字符集与长度 |
| `x-timestamp` 超出窗口 | `40000` | 重新校准时钟（NTP / 服务端 `Date`），用**新 Nonce** 重试一次 |
| 动态令牌或签名不匹配 | `40100` | 检查密钥、协议版本、路径与查询串、原始请求体字节、`user_id` 来源；不要盲目重试 |
| Nonce 已使用 | `40900` | 生成新 Nonce 后重试；若业务非幂等，先确认上一次请求是否已生效 |
| 服务端 `50000` / 超时 | `50000` / `50400` | 指数退避；重试必须使用新 Nonce。埋点建议携带稳定 `event_id` 以利用幂等 |

客户端应维护本地时钟偏移量：可在启动时请求 `GET /healthz`，读取响应 `Date` 头，
计算 `server_time - local_time` 并在签名时补偿。偏移量超过 ±300 秒时先告警并重新同步。

## 8. Nonce 存储与清理

迁移：`server/migrations/0007_replay_nonces.sql`。

```sql
CREATE TABLE replay_nonces (
    user_id      TEXT    NOT NULL,
    nonce        TEXT    NOT NULL,
    request_path TEXT    NOT NULL,
    expires_at   INTEGER NOT NULL, -- Unix 秒
    created_at   TEXT    NOT NULL,
    PRIMARY KEY (user_id, nonce)
);
```

- 作用域：`(user_id, nonce)`，不同用户可使用相同 Nonce；
- 保留时长：`REPLAY_NONCE_TTL_SECS`，默认 600 秒，且必须 ≥ `2 × REPLAY_CLOCK_SKEW_SECS`；
- 清理：每次消费前执行 `DELETE FROM replay_nonces WHERE expires_at <= now`，并走 `expires_at` 索引；
- 多实例部署：所有实例必须共享同一数据库文件 / 存储，否则 Nonce 去重失效。

## 9. 配置

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `REPLAY_PROTECTION_ENABLED` | `false` | 是否启用三条写接口的签名校验 |
| `REPLAY_SIGNING_SECRET` | 无 | 共享密钥，启用时必填，≥32 字节；Debug 输出脱敏 |
| `REPLAY_CLOCK_SKEW_SECS` | `300` | 允许时钟偏移，范围 30–900 |
| `REPLAY_NONCE_TTL_SECS` | `600` | Nonce 去重记录保留秒数，≥2×时钟偏移 |
| `REPLAY_MAX_BODY_BYTES` | `2097152` | 中间件缓冲请求体上限（2 MiB），范围 1 KiB–16 MiB |

兼容说明：未设置 `REPLAY_PROTECTION_ENABLED` 时，旧变量 `TELEMETRY_REPLAY_ENABLED` 仍可作为回退。

**上线建议**：

1. 先在所有客户端接入签名逻辑，并在测试环境用 `REPLAY_PROTECTION_ENABLED=true` 验证；
2. 生产灰度开启，观察 `40000/40100/40900` 指标；
3. 开启后不要随意轮换 `REPLAY_SIGNING_SECRET`；轮换需客户端与服务端同步切换（可先双密钥过渡，
   当前版本仅支持单密钥，需要停机窗口或网关层兼容）；
4. 密钥只通过 Secrets / `EnvironmentFile` 注入，禁止提交到仓库。

## 10. 错误码与响应

统一响应结构不变：

```json
{ "code": 40900, "msg": "replay detected: nonce has already been used", "data": null }
```

| code | HTTP | 触发条件 |
| --- | --- | --- |
| `40000` | 400 | 缺头、头格式错误、Nonce 非法、时间窗超出、请求体超限 |
| `40100` | 401 | 动态令牌或签名不匹配 |
| `40900` | 409 | Nonce 重放 |
| `50000` | 500 | Nonce 存储故障等内部错误 |

## 11. 测试覆盖

`server/tests/replay_protection.rs` 覆盖：

- 三条接口的正常签名请求零误伤；
- 同一 Nonce 重放 → `40900`；
- 请求体 / 签名篡改 → `40100`；
- 时间窗外（过去与未来）→ `40000`；
- 缺头 → `40000`；
- 鉴权先于防重放（无 Bearer 返回 `40100` 而非 `40000`）；
- Nonce 按用户隔离；
- 过期 Nonce 清理后可重新写入；
- 开关关闭时无签名请求正常通过。

协议纯函数单测位于 `server/src/replay/protocol.rs`，服务编排单测位于
`server/src/replay/service.rs`。
