# 意图执行接口契约（REQ-202610-1177）

`POST /api/v1/intent/execute` 实现 PRD §5.2 的意图输入 / 输出契约，并按 §3.3
完成「意图 → 函数调用序列 → 能力执行 / 二次确认」的编排。

## 1. 请求

```http
POST /api/v1/intent/execute
Authorization: Bearer <token>
Content-Type: application/json
```

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `user_id` | string | 否 | 用户标识；省略时取鉴权用户。显式提供时必须与鉴权用户一致，否则 `40300`。 |
| `session_id` | string | 否 | 会话标识；省略时服务端生成。同一会话内可延续上下文。 |
| `input_type` | string | 是 | `NATURAL_LANGUAGE` 或 `UI_INTERACTION`。 |
| `raw_input` | string \| object | 是 | 自然语言文本，或 UI 事件树对象。 |
| `current_context` | object | 否 | `{ "active_page": string, "entity_id": string }`，两个字段均可选。 |

### 1.1 自然语言示例（PRD §5.2）

```json
{
  "user_id": "usr_9527",
  "input_type": "NATURAL_LANGUAGE",
  "raw_input": "把这个订单的收货地址修改为北京市海淀区中关村南大街1号",
  "current_context": {
    "active_page": "ORDER_DETAIL_PAGE",
    "entity_id": "ORD_20261014_001"
  }
}
```

### 1.2 UI 事件树

```json
{
  "user_id": "usr_9527",
  "input_type": "UI_INTERACTION",
  "raw_input": {
    "event": { "type": "SUBMIT", "target_id": "btn_save_address" },
    "path": [
      { "id": "ORDER_DETAIL_PAGE", "type": "page" },
      { "id": "address_form", "type": "form" },
      { "id": "btn_save_address", "type": "button", "action": "update_order_address" }
    ],
    "data": {
      "order_id": "ORD_20261014_001",
      "shipping_address": "北京市朝阳区望京街道1号"
    }
  }
}
```

- `path` 为从页面根节点到触发节点的路径，节点可声明 `action`（目标能力）；
- 未声明 `action` 时，按 `event.target_id` 的关键字推断（`address` / `balance` / `delete` / `ship` / `order-detail`）；
- `data` 携带表单值或实体标识；`page` / `entity_id` 也可放在事件树顶层。

## 2. 响应

项目统一包络为 `{ "code": 0, "msg": "ok", "data": ... }`，PRD §5.2 字段位于 `data` 内。

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `session_id` | string | 会话标识，后续请求回传即可延续上下文。 |
| `action_type` | string | `EXECUTE`（读类，已执行）或 `REQUIRE_CONFIRMATION`（写 / 删类，未执行）。 |
| `intent_summary` | string | 意图摘要。 |
| `proposed_changes` | object | `{ target_capability, parameters, additional_calls? }`。 |
| `confirmation_prompt` | string | 二次确认文案；`EXECUTE` 时省略。 |
| `context` | object | 合并后的会话上下文。 |
| `execution_results` | array | 读类能力执行结果；`REQUIRE_CONFIRMATION` 时省略。 |

### 2.1 PRD §5.2 示例响应

```json
{
  "code": 0,
  "msg": "ok",
  "data": {
    "session_id": "sess_...",
    "action_type": "REQUIRE_CONFIRMATION",
    "intent_summary": "更新订单收货地址",
    "proposed_changes": {
      "target_capability": "update_order_address",
      "parameters": {
        "order_id": "ORD_20261014_001",
        "new_address": "北京市海淀区中关村南大街1号"
      }
    },
    "confirmation_prompt": "确认将订单 ORD_20261014_001 的收货地址修改为【北京市海淀区中关村南大街1号】吗？",
    "context": {
      "active_page": "ORDER_DETAIL_PAGE",
      "entity_id": "ORD_20261014_001"
    }
  }
}
```

## 3. 动作决策

| 能力 | 风险 | 动作 |
| --- | --- | --- |
| `query_user_balance` | 读 | `EXECUTE`，返回余额。 |
| `query_order_business_data` | 读 | `EXECUTE`，返回 PRD §5.1 `business_data`。 |
| `update_order_address` | 写 | `REQUIRE_CONFIRMATION`，绝不自动执行。 |
| `update_item_status` | 写 | `REQUIRE_CONFIRMATION`。 |
| `delete_record` | 删 | `REQUIRE_CONFIRMATION`。 |
| `create_order` | 写 | `REQUIRE_CONFIRMATION`（能力目录预留）。 |

混合调用序列中只要包含任一写 / 删能力，整条序列都进入 `REQUIRE_CONFIRMATION`，
不会出现「前半段已执行、后半段待确认」的部分执行。

## 4. 会话状态

- 会话持久化在 `intent_sessions` 表，按 `session_id` 唯一、按 `user_id` 隔离；
- 上下文合并优先级：本次 `current_context` > 会话已存上下文 > 规划器从事件树推断的提示；
- 会话 TTL 由 `INTENT_SESSION_TTL_SECS` 控制（默认 1800 秒）；过期后按新会话处理，
  不会沿用旧上下文；
- 同一 `session_id` 被其他用户使用时返回 `40300`。

## 5. 安全约束

- `user_id` 必须与鉴权用户一致；
- 订单 / 订单项按归属校验，跨用户操作返回 `40300`；
- 商品与审计记录不允许通过意图接口修改 / 删除；
- `session_id`、`active_page`、`entity_id` 仅允许 `[A-Za-z0-9._:-]`；
- 能力参数静态校验（标识符字符集、地址长度、订单项状态白名单等）；
- `raw_input` 最大 32 KiB；
- 写 / 删能力只生成确认卡片，实际执行必须走后续确认接口（REQ-202610-1178/1180 接入点）。

## 6. 依赖与扩展点

- `intent::capabilities`：原子能力目录与参数约束；REQ-202610-1178 能力中心落地后替换实现；
- `intent::planner::IntentPlanner`：函数调用序列生成端口；当前为确定性规则规划器，
  REQ-202610-1180 的模型版 Tool Call Agent 实现同一 trait 即可替换；
- `intent::service::IntentService`：会话、归属校验、执行与确认编排，不依赖具体规划器实现。

## 7. 测试

`server/tests/intent_execute.rs` 覆盖：

- PRD §5.2 地址修改示例的完整响应与「未执行」验证；
- 余额查询的 `EXECUTE` 与审计落库；
- 同一会话跨请求的上下文传递；
- UI 事件树路径；
- 删除 / 写操作的确认卡片；
- 未认证、参数非法、跨用户订单、会话复用等错误路径；
- 会话 TTL 过期后不沿用旧上下文。
