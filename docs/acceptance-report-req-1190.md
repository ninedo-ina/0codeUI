# REQ-202610-1190 端到端场景验收与性能 / 降级演练报告

- **需求编号**：REQ-202610-1190
- **验收日期**：2026-10-22
- **验收分支**：`feat/req-202610-1190-e2e-acceptance`
- **验收基线**：`7f57741`（已合并 REQ-202610-1186 防重放、REQ-202610-1184 输出混淆）
- **验收环境**：Linux x86_64 / 8 核 / Rust stable / SQLite 内存库 / 模型 Mock Provider（单次延迟 10ms）/ debug 构建
- **验收范围**：订单详情页、抢购页、性能指标、降级演练、防重放与混淆强度抽验

## 1. 验收结论

| 验收项 | 结论 |
| --- | --- |
| 场景一：订单详情页模板模式 + 动态模式 | 通过 |
| 场景二：抢购页动态 UI + 意图操作 | 通过 |
| 静态渲染 ≤ 200ms | 通过（实测 19.0ms） |
| 动态渲染 ≤ 1.5s | 通过（实测 19.7ms） |
| 断模型 / 超时 / 校验失败降级无白屏 | 通过（3 类故障均返回完整静态页） |
| 防重放抽验 | 通过（正常 200 / 重放 409 / 篡改 401） |
| 输出混淆强度抽验 | 通过（类名改写 > 0，标识符覆盖率 ≥ 0.8，跨会话输出不同） |

**总体结论：本次端到端验收通过。** 性能数据基于 Mock Provider 与本地 debug 构建，真实模型与生产峰值流量需在部署环境复测，详见第 7 节。

## 2. 场景验收

### 2.1 场景一：订单详情页

**模板模式**：`POST /api/v1/ui/render`

- 场景编码：`ORDER_DETAIL_PAGE`
- 业务数据：订单号 `ORD_20261014_001`、状态 `PAID`、商品明细
- 实测结果：HTTP 200，`strategy=template`，返回完整单文件 HTML，包含订单号与订单状态

**动态模式**：`POST /api/v1/ui/generate/dynamic`

- 场景编码：`ORDER_DETAIL_PAGE`，`bypass_cache=true`
- 实测结果：HTTP 200，`strategy=dynamic`，`degraded=false`，返回完整单文件 HTML
- 分阶段耗时：总耗时 11.831ms，模型 6.939ms，HTML 校验 0.213ms，输出混淆 0.400ms

**结论**：模板模式与动态模式均端到端跑通。

### 2.2 场景二：抢购 / 秒杀页

**动态 UI**：`POST /api/v1/ui/generate/dynamic`

- 场景编码：`FLASH_SALE_PAGE`（本次新增场景与静态兜底模板）
- 业务数据：活动标题、抢购价 / 原价、总库存 1000、剩余库存 137、倒计时 3661 秒、状态 `LIVE`
- 实测结果：HTTP 200，`strategy=dynamic`，`degraded=false`，页面包含活动标题、价格、倒计时、库存进度与抢购按钮

**意图操作**：`POST /api/v1/intent/execute`

- 输入类型：`UI_INTERACTION`
- 事件树：`TAP btn_query_balance`，页面 `FLASH_SALE_PAGE`
- 实测结果：HTTP 200，`action_type=EXECUTE`，`target_capability=query_user_balance`，余额返回 `1000.00 CNY`

**高并发响应**：12 个并行动态生成请求（不同 `sale_id`，`bypass_cache=true`）

- 全部 HTTP 200，`degraded=false`
- 总墙钟耗时 60.9ms，P95 57.1ms，无超时、无降级、无 5xx

**结论**：抢购页动态 UI、意图操作与并发响应均通过。

## 3. 性能验收

| 指标 | PRD 目标 | 实测值 | 结论 |
| --- | --- | --- | --- |
| 静态模板渲染 | ≤ 200ms | 19.0ms | 通过 |
| 动态生成（端到端） | ≤ 1.5s | 19.7ms | 通过 |
| 订单详情动态链路 | ≤ 1.5s | 11.831ms | 通过 |
| 12 并发抢购页 | 无超时 / 无明显劣化 | 墙钟 60.9ms，P95 57.1ms | 通过 |

说明：

- 测试使用进程内 Axum Router 与内存 SQLite，未计入真实网络 RTT、TLS、网关与客户端渲染时间。
- 动态性能基于 Mock Provider（10ms 模拟延迟），真实模型推理耗时需按部署环境单独基线化。
- 性能断言已固化在 `server/tests/e2e_acceptance.rs`，后续回归可直接复跑。

## 4. 降级演练

| 故障场景 | 注入方式 | 期望 | 实测 |
| --- | --- | --- | --- |
| 模型超时 | Mock 延迟 5s，管线预算 300ms | 降级静态模板，无白屏 | HTTP 200，`degraded=true`，`degradation_reason=model_timeout`，返回完整抢购页 |
| 断模型 | Provider 返回 `ModelError::Disabled` | 降级静态模板，无白屏 | HTTP 200，`degraded=true`，`degradation_reason=model_disabled`，返回完整订单详情页 |
| 输出校验失败 | Provider 返回不完整 HTML | 降级静态模板，无白屏 | HTTP 200，`degraded=true`，`degradation_reason=validation_failed`，返回完整订单详情页 |

无白屏判定标准：HTTP 200、响应为 `<!DOCTYPE html>` 开头且以 `</html>` 结尾、包含业务关键文本、页面体量非空。三类故障全部满足。

## 5. 安全抽验

### 5.1 防重放（REQ-202610-1186）

对 `POST /api/v1/ui/generate` 使用 `INTIMEUI-REPLAY-V1` 协议签名：

- 首次签名请求：HTTP 200
- 同一 `timestamp + nonce + body` 重放：HTTP 409，业务码 `40900`
- 签名后篡改请求体：HTTP 401，业务码 `40100`

结论：重放与篡改均被拒绝，正常请求不受影响。

### 5.2 输出混淆强度（REQ-202610-1184）

对包含 CSS 类名、DOM id 与 JS 局部变量的样例页面调用 `POST /api/v1/ui/obfuscate`，强度 `aggressive`：

- `classes_renamed > 0`，输出不再包含原始 `class="card"`
- `identifier_coverage ≥ 0.8`（引擎默认下限）
- 不同 `session_id` 产生不同输出，避免跨会话固定映射
- 混淆后 HTML 仍通过单文件 HTML 校验

结论：混淆强度满足抽验要求。

## 6. 测试与构建结果

| 检查项 | 命令 | 结果 |
| --- | --- | --- |
| 格式 | `cargo fmt --all -- --check` | 通过 |
| 静态检查 | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 通过 |
| 全量测试 | `cargo test --workspace --all-features` | 215 个单元测试 + 223 个集成测试 + 1 个文档测试通过；1 个文档测试 ignored；0 失败 |
| 构建 | `cargo build --workspace --all-features` | 通过 |
| 验收专项 | `cargo test -p intimeui-server --test e2e_acceptance -- --nocapture` | 11/11 通过 |

验收专项覆盖：

1. `scenario_one_order_detail_template_and_dynamic_modes_pass`
2. `scenario_two_flash_sale_dynamic_and_intent_operation_pass`
3. `performance_static_under_200ms`
4. `performance_dynamic_under_1500ms`
5. `degradation_timeout_falls_back_to_flash_sale_template`
6. `degradation_model_disabled_falls_back_to_order_template`
7. `degradation_validation_failure_falls_back_to_order_template`
8. `security_replay_protection_spot_check`
9. `security_obfuscation_spot_check`
10. `flash_sale_high_concurrency_stays_responsive`
11. `pipeline_metrics_endpoint_exposes_acceptance_signals`

## 7. 差距与优化项

1. **真实模型延迟未覆盖**：本次使用 Mock Provider 验证链路与降级逻辑，真实 vLLM / 外部模型的 P95、冷启动与显存排队需在部署环境用 `intimeui-model-bench` 复测。
2. **并发规模有限**：12 并发仅验证单机进程内无超时与降级，未模拟秒杀峰值（建议至少 200 / 1000 并发阶梯压测，并观察 SQLite 写锁、模型并发队列与 P95）。
3. **静态性能为 debug 构建**：生产 release 构建预计更低，但应在目标机型上重新建立基线并纳入 CI 趋势。
4. **兜底模板覆盖范围**：动态管线要求场景必须有静态模板；当前仅 `ORDER_DETAIL_PAGE` 与 `FLASH_SALE_PAGE` 具备模板，后续新增动态场景必须同步补模板，否则无法兜底。
5. **混淆抽验非全量**：本次只验证类名、标识符覆盖率与会话随机性，建议补充真实浏览器加载、交互与截图对比，确认混淆不影响业务功能。
6. **网络与端侧未计入**：当前为进程内调用，生产验收应补充网关、TLS、CDN 与 WebView 首屏渲染耗时。

## 8. 交付物

- `server/tests/e2e_acceptance.rs`：端到端验收测试（场景、性能、降级、并发、安全）
- `server/src/ui_template/flash_sale.rs`：`FLASH_SALE_PAGE` 业务数据校验与视图模型
- `server/templates/flash_sale.html`：抢购页单文件静态模板（兼作动态管线兜底）
- `server/src/ui_template/catalog.rs` / `mod.rs`：注册抢购页模板与渲染入口
- `server/src/ui/scene.rs`：注册 `FLASH_SALE_PAGE` 场景
- `server/src/prompt/scenes.rs`：补充抢购页 Prompt 约束
- `server/src/obfuscation/adapter.rs`：将输出混淆引擎接入动态管线
- 本报告：`docs/acceptance-report-req-1190.md`
