# InTimeUI · 实时UI系统（0codeUI）

实时动态 UI 生成与 AI 意图驱动业务架构。本仓库承载后端服务与后续各模块（M0–M7）。

> 项目看板：`/dashboard/workspaces/dev/projects?code=InTimeUI`

## 仓库结构

```
.
├── Cargo.toml                  # Rust workspace
├── server/                     # 后端服务（intimeui-server，M0 服务基座）
│   ├── migrations/             # SQLite schema 迁移（sqlx，版本化且幂等）
│   ├── src/
│   │   ├── main.rs             # 服务入口（启动 / 迁移 / 优雅停机）
│   │   ├── lib.rs              # 应用组装（路由 + 中间件），供测试复用
│   │   ├── config.rs           # 环境变量配置（含密钥注入）
│   │   ├── db/                 # SQLite 连接池、迁移执行、种子数据
│   │   ├── models.rs           # 核心业务领域模型（Decimal 金额）
│   │   ├── repository/         # 数据访问层 + 原子能力（create_order 等）
│   │   ├── error.rs            # 全局错误码规范 + AppError
│   │   ├── response.rs         # 统一响应结构 {code, msg, data}
│   │   ├── state.rs            # 应用共享状态
│   │   ├── telemetry.rs        # 结构化日志
│   │   ├── validation.rs       # 请求参数校验提取器
│   │   ├── model/              # UI 生成模型接入（配置 / provider / 网关 / 指标 / 校验）
│   │   ├── middleware/         # 鉴权 / 限流 / 超时中间件
│   │   ├── routes/             # 路由注册与端点
│   │   ├── bin/intimeui-seed.rs # 种子数据一键加载 CLI
│   │   └── bin/intimeui-model-bench.rs # 模型延迟 / 成本基线 CLI
│   └── tests/                  # 集成测试（api.rs / data_model.rs / model_api.rs）
├── docs/                       # 模型推理部署、LoRA 微调与延迟基线文档
├── training/                   # M6 训练线：语料 / LoRA·QLoRA / 评测 / 适配器导出
│   ├── configs/                # 24GB 显存训练配置
│   ├── corpus/                 # 语料 schema、模板与种子样本
│   ├── eval/                   # 留出评测集
│   ├── intimeui_training/      # 纯标准库核心包
│   ├── scripts/                # prepare / train / evaluate / export / smoke
│   └── tests/                  # 无 GPU unittest
├── .github/workflows/ci.yml    # CI：fmt + clippy + build + test + 训练线测试
├── deploy/                     # 部署参考（systemd 单元 / 模型推理 compose）
├── Dockerfile
└── .env.example                # 环境变量样例（不含任何真实密钥）
```

## 后端服务（intimeui-server）

- **技术栈**：Rust（stable）· Axum 0.8 · Tokio · tower-http · tracing · validator · SQLx / SQLite · rust_decimal
- **能力（M0 服务基座）**：
  - 统一响应结构 `{code, msg, data}` 与全局错误码规范；
  - API 网关基础：路由注册、请求参数校验、简单鉴权（user_id 体系）、基础限流；
  - 配置与密钥：全部经环境变量注入（本地可用 `.env`，不入仓）；
  - 结构化日志（dev pretty / prod json）与请求 ID 贯穿；
  - 健康检查：`/healthz`（存活）、`/readyz`（含数据库探测）；
  - 数据底座：用户 / 商品 / 订单 / 订单项 / 操作记录五张表，版本化迁移 + 幂等种子数据；
  - 原子能力数据层：`create_order` / `query_user_balance` / `update_item_status` / `delete_record`（含审计记录）；
  - **UI 生成模型接入（REQ-202610-1181）**：OpenAI 兼容 provider（vLLM / Ollama / DeepSeek）、
    并发信号量 + 有界排队、超时 / 重试 / 备用模型、多模型灰度、SSE 流式输出、
    单文件 HTML 安全校验、延迟分位与 token / 成本指标；
  - **UI 语料与 LoRA/QLoRA 微调管线（REQ-202610-1187）**：语料采集 / 清洗 / PII 脱敏 /
    去重 / 确定性切分、24GB 单卡 LoRA·QLoRA 训练、五项质量指标评测、
    PEFT 适配器清单与 vLLM 热加载。

### 快速开始（本地）

```bash
# 1. 安装 Rust（已安装可跳过）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. 准备配置（默认 dev 配置即可直接跑）
cp .env.example .env

# 3. 加载示例种子数据（自动执行幂等迁移；重复执行无副作用）
cargo run -p intimeui-server --bin intimeui-seed

# 4. 启动服务（默认监听 0.0.0.0:8080；启动时自动执行迁移）
cargo run -p intimeui-server

# 5. 健康检查
curl -s http://127.0.0.1:8080/healthz
# {"code":0,"msg":"ok","data":{"status":"ok","service":"intimeui-server",...}}
curl -s http://127.0.0.1:8080/readyz

# 6. 示例订单查询（PRD §5.1 business_data 结构）
curl -s http://127.0.0.1:8080/api/v1/orders/ORD_20261014_001/business-data \
  -H 'Authorization: Bearer dev:9527'
# {"code":0,"msg":"ok","data":{"order_id":"ORD_20261014_001","status":"PAID",
#  "items":[{"name":"智能降噪耳机","price":299.00,"qty":1}]}}

# 7. 示例接口（鉴权 + 参数校验）
curl -s -X POST http://127.0.0.1:8080/api/v1/echo \
  -H 'Authorization: Bearer dev:1001' \
  -H 'Content-Type: application/json' \
  -d '{"message":"hello","priority":5}'

# 8. UI 生成（默认 mock provider；真实模型部署见 docs/model-inference-deployment.md）
curl -s -X POST http://127.0.0.1:8080/api/v1/ui/model/generate \
  -H 'Authorization: Bearer dev:1001' \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"生成一个订单详情页，黑白灰主题，支持明暗切换"}'

# 9. 训练线自检（无 GPU；真实 24GB 训练见 docs/lora-finetuning.md）
python3 -m unittest discover -s training/tests -t training -v
python3 training/scripts/smoke_pipeline.py
```

### 环境变量

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `APP_ENV` | `dev` | 运行环境：`dev` / `prod` |
| `APP_HOST` | `0.0.0.0` | 监听地址 |
| `APP_PORT` | `8080` | 监听端口 |
| `LOG_LEVEL` | `info` | 日志级别（亦兼容 `RUST_LOG`） |
| `LOG_FORMAT` | dev=`pretty` / prod=`json` | 日志格式 |
| `AUTH_DEV_MODE` | dev=`true` / prod=`false` | 开发鉴权（接受 `Bearer dev:<user_id>`），prod 禁止开启 |
| `AUTH_STATIC_TOKENS` | 空 | 静态令牌表，`token:user_id` 逗号分隔 |
| `RATE_LIMIT_ENABLED` | `true` | 是否启用限流 |
| `RATE_LIMIT_RPS` | `50` | 每秒补充令牌数 |
| `RATE_LIMIT_BURST` | `100` | 令牌桶容量（突发额度） |
| `REQUEST_TIMEOUT_SECS` | `30` | 请求处理超时 |
| `DATABASE_URL` | `sqlite://data/intimeui.db` | SQLite 连接串（文件自动创建；测试可用 `sqlite::memory:`） |
| `MODEL_ENABLED` | `true` | 是否启用模型接口 |
| `MODEL_PROVIDER` | `mock` | `mock`（仅 dev / test）或 `openai_compatible`（vLLM / Ollama / DeepSeek） |
| `MODEL_BASE_URL` | `http://127.0.0.1:8000/v1` | OpenAI 兼容服务地址（不含 `/chat/completions`） |
| `MODEL_API_KEY` | 空 | 服务端密钥；本机 vLLM 可留空 |
| `MODEL_NAME` | `Qwen2.5-Coder-7B-Instruct-AWQ` | 主模型；加载 LoRA 后改为适配器 served name（见 `docs/lora-finetuning.md`） |
| `MODEL_FALLBACKS` | 空 | 备用模型链（逗号分隔） |
| `MODEL_CANARY_MODEL` / `MODEL_CANARY_PERCENT` | 空 / `0` | 灰度模型与流量百分比（0–100） |
| `MODEL_REQUEST_TIMEOUT_MS` | `1500` | 非流式请求总超时 |
| `MODEL_STREAM_TIMEOUT_MS` | `30000` | 流式分片读取超时（空闲超时） |
| `MODEL_MAX_CONCURRENCY` | `4` | 最大并发推理数（不超过 vLLM `--max-num-seqs`） |
| `MODEL_MAX_QUEUE` | `64` | 最大排队数 |
| `MODEL_QUEUE_TIMEOUT_MS` | `1500` | 排队等待上限 |
| `MODEL_MAX_RETRIES` | `1` | 可重试错误的重试次数 |
| `MODEL_MAX_TOKENS` / `MODEL_TEMPERATURE` | `2048` / `0.2` | 默认生成参数 |
| `MODEL_PRICE_INPUT_PER_1K` / `MODEL_PRICE_OUTPUT_PER_1K` | `0` / `0` | 成本估算单价（美元 / 千 token；自托管填 0） |

> **密钥管理**：仅通过环境变量 / Secrets 注入，严禁写入仓库或数据库。生产环境请用部署平台（systemd `EnvironmentFile`、Docker secrets、K8s Secret 等）下发。

### 统一响应与错误码

所有接口返回 `{ "code": number, "msg": string, "data": any | null }`；`code = 0` 为成功。

| code | HTTP | 含义 |
| --- | --- | --- |
| `0` | 200 | 成功 |
| `40000` | 400 | 请求参数校验失败 |
| `40100` | 401 | 未认证 / 凭据无效 |
| `40300` | 403 | 无权限 |
| `40400` | 404 | 资源不存在 |
| `40900` | 409 | 资源冲突（唯一键重复） |
| `42900` | 429 | 触发限流（附 `Retry-After`） |
| `50000` | 500 | 服务内部错误 |
| `50200` | 502 | 模型上游错误 / 输出不合法 |
| `50300` | 503 | 服务暂不可用（含模型排队超时） |
| `50400` | 504 | 请求处理超时 |

### 接口一览（M0 示例）

| 方法 | 路径 | 鉴权 | 说明 |
| --- | --- | --- | --- |
| GET | `/` | 否 | 服务自述 |
| GET | `/healthz`、`/readyz` | 否 | 健康检查（免限流） |
| GET | `/api/v1/ping` | 否 | 探活示例 |
| GET | `/api/v1/me` | 是 | 返回当前用户标识（user_id） |
| POST | `/api/v1/echo` | 是 | 参数校验示例（`message` 1~500 字；`priority` 1~9） |
| GET | `/api/v1/orders/{order_id}/business-data` | 是 | 订单业务数据（PRD §5.1 `business_data` 同构） |
| GET | `/api/v1/users/{user_id}/balance` | 是 | 用户余额（精确十进制金额） |
| POST | `/api/v1/ui/model/generate` | 是 | UI 生成（非流式，返回单文件 HTML / 耗时 / token / 成本） |
| POST | `/api/v1/ui/model/stream` | 是 | UI 生成（SSE：`meta` → `delta` → `done`） |
| GET | `/api/v1/ui/model/info` | 是 | 模型提供者、模型列表与并发 / 超时配置 |
| GET | `/api/v1/ui/model/metrics` | 是 | 请求量、P50/P95/P99、token 与成本快照 |

鉴权方式：请求头 `Authorization: Bearer <token>`；开发模式额外接受 `Bearer dev:<user_id>`。

### 数据模型与种子数据（M0）

**表结构**（`server/migrations/0001_core_schema.sql`）：

| 表 | 说明 |
| --- | --- |
| `users` | 用户（`usr_9527` 示例用户，余额 1000.00 CNY） |
| `products` | 商品（`智能降噪耳机` 299.00、`机械键盘` 499.00） |
| `orders` | 订单（`ORD_20261014_001`，状态 `PAID`，合计 299.00） |
| `order_items` | 订单项（商品名称/单价为下单时快照） |
| `operation_records` | 原子能力操作审计（create_order / query_user_balance / update_item_status / delete_record） |

**金额精度**：SQLite 没有原生 `DECIMAL`，`NUMERIC` 亲和性会退化为 IEEE-754 浮点。
因此数据库以「分」为单位的整数（`*_cents`）精确存储，Rust 领域层统一使用
`rust_decimal::Decimal` 计算与序列化（JSON 数字），全链路不使用 `f64`。
`decimal_to_cents` 会拒绝超过两位小数的金额，避免静默舍入。

**迁移**：`server/migrations/*.sql` 由 sqlx 版本化管理（`_sqlx_migrations`），
服务启动时自动执行；重复执行幂等。新增表结构请追加迁移文件，不要修改已发布迁移。

**种子数据一键加载**：

```bash
# 使用 DATABASE_URL（默认 sqlite://data/intimeui.db）
cargo run -p intimeui-server --bin intimeui-seed

# 或显式指定数据库
cargo run -p intimeui-server --bin intimeui-seed -- --database-url sqlite://data/intimeui.db
```

种子加载使用 `INSERT OR IGNORE`，重复执行不会产生重复记录；返回本次实际插入行数。

### 测试与检查

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release

# 模型延迟 / 成本基线（mock 无需 GPU；真实模型先按部署文档启动 vLLM）
cargo run --release -p intimeui-server --bin intimeui-model-bench -- \
  --requests 100 --concurrency 4 --output /tmp/model-bench.json

# 训练线：单元测试 + 无 GPU 全链路自检
python3 -m unittest discover -s training/tests -t training -v
python3 training/scripts/smoke_pipeline.py
```

### CI

`.github/workflows/ci.yml`：push / PR 触发，执行 fmt 检查、clippy（`-D warnings`）、
Rust 构建与测试，以及训练线 Python unittest + mock 全链路自检。

### 部署

**模型推理（24GB 显存，vLLM）**：见 [`docs/model-inference-deployment.md`](docs/model-inference-deployment.md)，
参考 compose 位于 [`deploy/model/docker-compose.yml`](deploy/model/docker-compose.yml)；
延迟 / 成本基线见 [`docs/model-latency-baseline.md`](docs/model-latency-baseline.md)。

**LoRA 适配器（M6）**：训练、评测、导出与热加载见
[`docs/lora-finetuning.md`](docs/lora-finetuning.md)；带 LoRA 的 vLLM 叠加层为
[`deploy/model/docker-compose.lora.yml`](deploy/model/docker-compose.lora.yml)。

**方式一：systemd（裸机 / VPS）**

```bash
cargo build --release
install -m 755 target/release/intimeui-server /usr/local/bin/intimeui-server
install -m 755 target/release/intimeui-seed /usr/local/bin/intimeui-seed
install -m 644 deploy/intimeui-server.service /etc/systemd/system/
install -d -m 750 /etc/intimeui
install -d -o intimeui -g intimeui -m 750 /opt/intimeui/data   # SQLite 数据目录
install -m 600 .env.example /etc/intimeui/intimeui.env   # 按需填写，含密钥请收紧权限
sudo -u intimeui DATABASE_URL=sqlite:///opt/intimeui/data/intimeui.db intimeui-seed
systemctl daemon-reload && systemctl enable --now intimeui-server
curl -s http://127.0.0.1:8080/healthz
curl -s http://127.0.0.1:8080/readyz
```

**方式二：Docker**

```bash
docker build -t intimeui-server:latest .
docker volume create intimeui-data
docker run -d --name intimeui-server -p 8080:8080 \
  -v intimeui-data:/data \
  -e APP_ENV=prod -e AUTH_STATIC_TOKENS=... intimeui-server:latest
# 首次加载种子数据（容器内已内置 intimeui-seed）
docker exec intimeui-server intimeui-seed
```

> 生产环境建议置于网关（Nginx / Cloudflare）之后，由其终止 TLS 并透传 `X-Forwarded-For`。

## 路线图（对应 REQ-202610-1168 拆解）

- **M0 基础工程**：REQ-202610-1169 服务基座（已交付）、REQ-202610-1170 数据模型与种子数据（已交付）
- **M6 训练线**：REQ-202610-1187 UI 语料与 LoRA/QLoRA 微调管线（本批交付；真实 24GB 训练待目标机回填）
- M1 模板兜底链路 / M2 埋点画像 / M3 意图引擎 / M4 动态生成 / M5 安全 / M7 客户端与验收
