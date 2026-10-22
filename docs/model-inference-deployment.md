# UI 生成模型接入与推理部署（24GB 显存）

> 对应需求：REQ-202610-1181【模型】UI 生成模型接入与推理部署
>
> 目标：在单卡 24GB 显存环境部署 Qwen2.5-Coder / DeepSeek-Coder 系列模型，
> 通过 OpenAI 兼容接口接入 `intimeui-server`，提供并发控制、超时重试、流式输出、
> 多模型路由与灰度能力，并给出可复现的延迟 / 成本测量方法。

## 1. 架构

```
┌──────────────────┐      HTTP/JSON + SSE       ┌─────────────────────┐
│  intimeui-server │  ───────────────────────▶  │  vLLM (OpenAI API)  │
│  ModelGateway    │                            │  Qwen2.5-Coder /    │
│  · 并发 / 排队    │  ◀───────────────────────  │  DeepSeek-Coder     │
│  · 超时 / 重试    │      chat/completions      │  127.0.0.1:8000     │
│  · 路由 / 灰度    │                            └─────────────────────┘
│  · 指标 / 校验    │
└──────────────────┘
```

- 推理服务只监听 `127.0.0.1:8000`（或内网地址），不直接暴露公网。
- `intimeui-server` 通过 `MODEL_PROVIDER=openai_compatible` 接入；HTTPS 由反向代理终止。
- 默认 `MODEL_PROVIDER=mock` 仅用于开发 / CI，生产环境启动时会拒绝 mock。

## 2. 24GB 显存选型

| 模型 | 量化 / 权重 | 24GB 单卡 | 建议 |
| --- | --- | --- | --- |
| `Qwen/Qwen2.5-Coder-7B-Instruct-AWQ` | AWQ 4bit，约 8GB | ✅ 宽裕 | **默认推荐**，`max-model-len=8192`、`max-num-seqs=8` |
| `Qwen/Qwen2.5-Coder-14B-Instruct-AWQ` | AWQ 4bit，约 11GB | ✅ 可运行 | 质量更高；建议 `max-model-len=4096`、`max-num-seqs=4`，先压测再上线 |
| `deepseek-ai/deepseek-coder-6.7b-instruct` | BF16，约 13GB | ✅ 可运行 | DeepSeek-Coder 轻量方案，需确认许可证与中文 UI 效果 |
| `deepseek-ai/DeepSeek-Coder-V2-Lite-Instruct` | BF16，约 31GB | ❌ 单卡放不下 | 需 2×24GB，或使用经过验证的 AWQ/GPTQ 4bit 社区量化版本 |

> 显存估算不含 KV cache。vLLM 的 `--gpu-memory-utilization 0.90` 会把剩余显存用于
> KV cache；并发越高、上下文越长，KV cache 占用越大。24GB 上不要同时启动两个推理服务。

## 3. 环境准备（可复现）

### 3.1 硬件与驱动基线

| 项目 | 要求 / 本次基线 |
| --- | --- |
| GPU | NVIDIA，24GB 显存（RTX 4090 / 3090 / A10 / L4 等） |
| 驱动 | `nvidia-smi` 正常，Driver ≥ 550，CUDA ≥ 12.4 |
| Docker | Docker Engine ≥ 24，Compose v2 |
| NVIDIA Container Toolkit | 已安装，`docker run --rm --gpus all nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi` 可执行 |
| 磁盘 | 模型缓存 ≥ 30GB（7B AWQ 约 8GB，14B AWQ 约 11GB，另留镜像与日志空间） |
| 内存 | ≥ 32GB 建议；至少 16GB |

检查：

```bash
nvidia-smi
docker --version
docker compose version
docker run --rm --gpus all nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi
```

### 3.2 启动 vLLM

仓库提供 `deploy/model/docker-compose.yml`，默认启动 7B AWQ：

```bash
docker compose -f deploy/model/docker-compose.yml up -d
docker compose -f deploy/model/docker-compose.yml logs -f model
# 等待日志出现 Application startup complete
```

首次启动会从 Hugging Face 下载模型；生产环境建议预先下载并固定 revision：

```bash
# 示例：固定模型 revision（把 <commit> 换成实际 commit hash）
MODEL_ID=Qwen/Qwen2.5-Coder-7B-Instruct-AWQ \
SERVED_MODEL_NAME=Qwen2.5-Coder-7B-Instruct-AWQ \
docker compose -f deploy/model/docker-compose.yml up -d
```

> 生产发布应固定 `vllm/vllm-openai` 镜像 tag（建议进一步固定 digest）和模型 revision，
> 不要依赖 `latest` 或浮动的 `main`。

健康检查：

```bash
curl -s http://127.0.0.1:8000/health
curl -s http://127.0.0.1:8000/v1/models
```

### 3.3 接入 intimeui-server

```bash
export MODEL_ENABLED=true
export MODEL_PROVIDER=openai_compatible
export MODEL_BASE_URL=http://127.0.0.1:8000/v1
export MODEL_API_KEY=                 # 本机 vLLM 可留空
export MODEL_NAME=Qwen2.5-Coder-7B-Instruct-AWQ
export MODEL_MAX_CONCURRENCY=4        # 不超过 vLLM --max-num-seqs
export MODEL_MAX_QUEUE=64
export MODEL_REQUEST_TIMEOUT_MS=1500
export MODEL_STREAM_TIMEOUT_MS=30000
```

启动后端：

```bash
cargo run -p intimeui-server
```

验证：

```bash
curl -s http://127.0.0.1:8080/api/v1/ui/model/info \
  -H 'Authorization: Bearer dev:1001'

curl -s -X POST http://127.0.0.1:8080/api/v1/ui/model/generate \
  -H 'Authorization: Bearer dev:1001' \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"生成一个订单详情页，黑白灰主题，支持明暗切换"}'
```

### 3.4 加载 LoRA 适配器（M6）

训练线产出的 PEFT 适配器通过 `deploy/model/docker-compose.lora.yml` 叠加加载；
完整训练 / 评测 / 导出流程见 [`docs/lora-finetuning.md`](lora-finetuning.md)。

```bash
# 1. 校验并安装适配器（训练产物目录 -> deploy/model/adapters/<name>）
python3 training/scripts/export_adapter.py \
  --adapter-dir training/artifacts/qwen2.5-coder-7b-ui-lora \
  --served-name qwen2.5-coder-7b-ui-lora \
  --eval-report training/artifacts/eval/adapter/eval_report.json \
  --install-dir deploy/model/adapters \
  --print-vllm

# 2. 基础 compose + LoRA 叠加层
docker compose \
  -f deploy/model/docker-compose.yml \
  -f deploy/model/docker-compose.lora.yml \
  up -d

# 3. 确认基础模型与适配器同时出现在 /v1/models
curl -s http://127.0.0.1:8000/v1/models | jq '.data[].id'
```

接入后端时把 `MODEL_NAME` 改为适配器的 `served_model_name`：

```bash
export MODEL_PROVIDER=openai_compatible
export MODEL_BASE_URL=http://127.0.0.1:8000/v1
export MODEL_NAME=qwen2.5-coder-7b-ui-lora
```

回滚只需把 `MODEL_NAME` 改回基础模型；若开启
`VLLM_ALLOW_RUNTIME_LORA_UPDATING=1`，也可通过 `/v1/load_lora_adapter` 与
`/v1/unload_lora_adapter` 在线切换版本（命令见 `docs/lora-finetuning.md` §7.4）。

> 适配器基座必须与 `MODEL_ID` 同架构 / 同 tokenizer；训练用 bf16 基座、
> 推理用 AWQ 量化基座时，LoRA 权重仍可加载，但需在目标机实测输出一致性。

## 4. 接口与行为

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/api/v1/ui/model/generate` | 非流式生成，返回单文件 HTML、模型、耗时、token、成本 |
| POST | `/api/v1/ui/model/stream` | SSE 流式生成：`meta` → `delta`* → `done`，异常为 `error` |
| GET | `/api/v1/ui/model/info` | 提供者、模型列表、并发 / 超时配置 |
| GET | `/api/v1/ui/model/metrics` | 进程内指标快照：请求量、P50/P95/P99、token、成本 |

请求体：

```json
{
  "prompt": "生成一个订单详情页，黑白灰主题，支持明暗切换",
  "scene_code": "ORDER_DETAIL_PAGE",
  "model": "Qwen2.5-Coder-7B-Instruct-AWQ",
  "max_tokens": 2048,
  "temperature": 0.2
}
```

流式示例：

```bash
curl -N -X POST http://127.0.0.1:8080/api/v1/ui/model/stream \
  -H 'Authorization: Bearer dev:1001' \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"生成一个登录页"}'
```

SSE 事件：

```
event: meta
data: {"model":"Qwen2.5-Coder-7B-Instruct-AWQ"}

event: delta
data: {"content":"<!DOCTYPE html>..."}

event: done
data: {"model":"...","usage":{...},"latency_ms":1234.5,"estimated_cost_usd":0.0,"finish_reason":"stop"}
```

### 4.1 并发、排队与超时

- 网关使用 `Semaphore` 限制同时在途推理数（`MODEL_MAX_CONCURRENCY`）。
- 达到并发上限后进入有界队列（`MODEL_MAX_QUEUE`），等待超过 `MODEL_QUEUE_TIMEOUT_MS`
  返回 `50300`；队列满返回 `42900`（附 `Retry-After`）。
- 非流式请求总超时 `MODEL_REQUEST_TIMEOUT_MS`；流式请求使用 `MODEL_STREAM_TIMEOUT_MS`
  作为分片读取的空闲超时（超过该时间没有新分片即失败）；超时返回 `50400`。
- 上游 429 / 408 / 5xx 视为可重试；重试次数 `MODEL_MAX_RETRIES`，指数退避
  `MODEL_RETRY_BACKOFF_MS`。
- 主模型失败后按 `MODEL_FALLBACKS` 顺序尝试备用模型；备用模型也必须出现在
  `allowed_models()` 中（主模型 + 备用 + 灰度自动注册）。
- `MODEL_CANARY_MODEL` + `MODEL_CANARY_PERCENT` 按请求计数器做百分比灰度；
  `0` 关闭，`100` 全量。

### 4.2 输出校验

所有模型输出经过 `extract_and_validate`：

1. 从 Markdown 代码块或前置说明中提取 HTML；
2. 必须以 `<!DOCTYPE html>` 开头，包含 `<html>`、`<head>`、`<body>`；
3. 拒绝外部 `<script src>`、`<link>`、`@import`、`url(http...)`、iframe/object/embed；
4. 单文件大小上限 2 MiB；
5. 不合法输出按 `50200` 处理，不返回给前端。

> 这层校验是模型输出进入渲染前的第一道安全边界；前端仍需做 CSP 与沙箱隔离。

## 5. 延迟与成本基线

### 5.1 测量工具

`intimeui-model-bench` 直接调用网关（包含并发、重试、HTML 校验），输出 JSON / Markdown：

```bash
# mock：验证测量链路，不需要 GPU
cargo run --release -p intimeui-server --bin intimeui-model-bench -- \
  --requests 100 --concurrency 4 \
  --output docs/model-latency-baseline.json \
  --markdown docs/model-latency-baseline.md

# 真实模型：先按 §3 启动 vLLM，再设置 MODEL_* 环境变量
MODEL_PROVIDER=openai_compatible \
MODEL_BASE_URL=http://127.0.0.1:8000/v1 \
MODEL_NAME=Qwen2.5-Coder-7B-Instruct-AWQ \
MODEL_MAX_CONCURRENCY=4 \
cargo run --release -p intimeui-server --bin intimeui-model-bench -- \
  --requests 100 --concurrency 4 \
  --output docs/model-latency-baseline.json \
  --markdown docs/model-latency-baseline.md
```

建议至少跑三组：

| 场景 | requests | concurrency | 目的 |
| --- | --- | --- | --- |
| 冷启动 | 20 | 1 | 观察模型加载后首批请求 |
| 稳定单并发 | 100 | 1 | 得到单请求 P50/P95 |
| 目标并发 | 100 | 4 | 验证 24GB 下吞吐与排队 |

### 5.2 指标口径

- `latency_ms`：网关端到端耗时，包含排队、上游推理、输出校验；
- `p50/p95/p99`：成功请求的延迟分位；
- `throughput_rps`：成功请求数 / 墙钟时间；
- `input_tokens` / `output_tokens`：上游返回的 usage 汇总；
- `estimated_cost_usd`：按 `MODEL_PRICE_INPUT_PER_1K` / `MODEL_PRICE_OUTPUT_PER_1K`
  估算；自托管默认 0，只统计 token 量。

### 5.3 PRD §6.1 ≤1.5s 目标

1.5s 是端到端目标，受模型大小、输出长度、并发和 GPU 影响。建议：

1. 先以 `MODEL_REQUEST_TIMEOUT_MS=1500` 作为硬上限；
2. 用 `intimeui-model-bench` 测 P95，若超目标，优先降低 `max_tokens`、启用流式首 token 指标、
   或把完整页面生成切到模板兜底 + 局部模型补全；
3. 将真实基线写入 `docs/model-latency-baseline.md`，每次模型 / 驱动 / vLLM 升级后重跑。

## 6. 运维与故障排查

| 现象 | 排查 |
| --- | --- |
| 启动报 `mock provider is not allowed in prod` | 生产必须设置 `MODEL_PROVIDER=openai_compatible` |
| `50200` 上游错误 | 看 vLLM 日志；确认 `MODEL_BASE_URL` 不带 `/chat/completions`，服务 `/v1/models` 正常 |
| `50300` 队列忙 | 提高 `MODEL_MAX_CONCURRENCY`（不超过 vLLM `--max-num-seqs`）或扩容 GPU |
| `50400` 超时 | 降低 `max_tokens` / 上下文，或提高超时；检查 GPU 是否被其他进程占用 |
| 输出被拒绝 | 模型未按单文件 HTML 输出；检查系统提示词与模型能力，必要时换更大模型 |
| 流式无输出 | 反向代理关闭缓冲（Nginx `proxy_buffering off`），确认 SSE 不被压缩 |
| 显存 OOM | 降低 `--gpu-memory-utilization`、`--max-model-len`、`--max-num-seqs`，或换更小量化 |
| 适配器 404 / 未加载 | 检查 `/adapters/<name>` 挂载、`--lora-modules` 左侧名称与 `MODEL_NAME` 是否一致 |
| 适配器加载后 OOM | 降低 `--max-model-len` / `--max-num-seqs`；LoRA 会额外占用少量显存 |

### 6.1 安全边界

- 推理端口只绑定 `127.0.0.1` / 内网；公网入口必须经过鉴权与 TLS。
- `MODEL_API_KEY` 仅通过环境变量 / Secrets 注入，`SecretString` 的 `Debug` 已脱敏。
- 模型输出禁止外部脚本 / 样式；前端渲染仍需 iframe sandbox + CSP。
- 提示词中的用户内容在 mock provider 中做了 HTML 转义；真实模型输出统一走 §4.2 校验。

## 7. 24GB 复现检查清单

- [ ] `nvidia-smi` 显示 24GB GPU，驱动 / CUDA 满足 §3.1
- [ ] `docker compose -f deploy/model/docker-compose.yml up -d` 成功
- [ ] `curl http://127.0.0.1:8000/health` 返回 200
- [ ] `curl http://127.0.0.1:8000/v1/models` 返回目标模型
- [ ] 后端 `MODEL_PROVIDER=openai_compatible`、`MODEL_BASE_URL` 指向 vLLM
- [ ] `GET /api/v1/ui/model/info` 显示 `provider=openai_compatible`
- [ ] `POST /api/v1/ui/model/generate` 返回合法单文件 HTML
- [ ] `intimeui-model-bench` 三组场景跑完，结果写入 `docs/model-latency-baseline.md`
- [ ] （可选 M6）LoRA 适配器 `export_adapter.py --verify-only` 通过
- [ ] （可选 M6）`/v1/models` 同时列出基础模型与适配器，`MODEL_NAME` 指向适配器
- [ ] 记录镜像 tag / digest、模型 revision、适配器 SHA-256、驱动版本、vLLM 参数与 GPU 型号

> 本仓库 CI 环境无 GPU，默认使用 mock provider 验证完整链路；真实 24GB 基线需按上述
> 清单在目标机器执行并回填。mock 结果不能当作真实模型性能数据。
