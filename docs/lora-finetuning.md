# UI 语料与 LoRA / QLoRA 微调管线（24GB 显存）

> 对应需求：REQ-202610-1187【模型】UI 语料与 LoRA/QLoRA 微调管线
>
> 依赖：#13 推理环境（REQ-202610-1181）的 vLLM OpenAI 兼容服务。
>
> 目标：把「采集 → 清洗 → 训练 → 评测 → 适配器热加载」做成可复现流水线，
> 在单卡 24GB 环境完成一次 UI 生成微调，并用统一指标对比基线。

## 1. 结论先行

- **训练方式**：默认 QLoRA（4-bit NF4 + bf16 计算 + gradient checkpointing），
  7B 模型在 24GB 单卡可跑；非量化 LoRA 作为 bitsandbytes 不可用时的回退。
- **语料格式**：一行一个 JSON 样本；清洗后转成 `system / user / assistant` 对话，
  只对 assistant 的 HTML 计算 loss。
- **质量口径**：结构合法率、单文件率、风格一致性、场景要素覆盖、可访问性五项加权，
  基线模型与适配器使用同一测试集、同一 prompt、同一采样参数。
- **交付物**：PEFT 适配器 + `adapter_manifest.json`（SHA-256 指纹）+ 评测报告；
  vLLM 通过 `--enable-lora --lora-modules` 加载，支持运行时 load / unload 热切换。
- **本仓库 CI 无 GPU**：单元测试与 mock 全链路可跑；真实 24GB 训练与基线/适配器
  对比需按 §7 在目标机器执行并回填 §9 的验收记录。

## 2. 目录与职责

| 路径 | 职责 |
| --- | --- |
| `training/corpus/schema.json` | 语料字段、范围与约束 |
| `training/corpus/templates/` | 三类场景的种子模板（与 mock provider 共用） |
| `training/corpus/seed_manifest.json` | 12 条可复现种子样本 |
| `training/corpus/build_seed_corpus.py` | 由清单渲染种子语料 |
| `training/intimeui_training/corpus.py` | 清洗、PII、去重、切分、chat 格式 |
| `training/intimeui_training/eval.py` | 五项指标、报告、基线对比 |
| `training/intimeui_training/manifest.py` | 适配器指纹、vLLM 参数 |
| `training/scripts/prepare_corpus.py` | 语料 CLI |
| `training/scripts/train_lora.py` | LoRA / QLoRA 训练（含 dry-run） |
| `training/scripts/evaluate.py` | mock / OpenAI 兼容评测与对比 |
| `training/scripts/export_adapter.py` | 校验、安装、生成 vLLM 参数 |
| `training/scripts/smoke_pipeline.py` | 无 GPU 全链路自检 |
| `deploy/model/docker-compose.lora.yml` | 带 LoRA 的 vLLM compose 叠加层 |

## 3. 语料规范

### 3.1 样本字段

完整 schema 见 `training/corpus/schema.json`，核心字段：

```json
{
  "id": "order-detail-light-001",
  "scene_code": "ORDER_DETAIL_PAGE",
  "instruction": "生成一个订单详情页，浅色主题，信息密度适中。",
  "style": {
    "theme": "light",
    "density": 0.5,
    "font_scale": 1.0,
    "radius": 12.0,
    "spacing": 1.0
  },
  "business_data": {
    "order_id": "ORD_20261014_001",
    "status": "PAID",
    "items": [{"name": "智能降噪耳机", "price": 299.0, "qty": 1}]
  },
  "output_html": "<!DOCTYPE html>...",
  "license": "CC-BY-4.0",
  "source": "seed"
}
```

约束：

| 字段 | 约束 |
| --- | --- |
| `id` | `^[a-z0-9][a-z0-9._-]{2,63}$`，批内唯一 |
| `scene_code` | `ORDER_DETAIL_PAGE` / `ORDER_LIST_PAGE` / `PRODUCT_DETAIL_PAGE` |
| `instruction` | 8–2000 字符，不含控制字符 |
| `style.theme` | `light` / `dark` |
| `style.density` | 0–1 |
| `style.font_scale` | 0.8–1.4 |
| `style.radius` | 0–24 |
| `style.spacing` | 0.75–1.5 |
| `output_html` | 100B–2MiB，单文件，无外部资源 |
| `business_data` | 对象，序列化 ≤64KB |
| `license` / `source` | 必填，授权不明不得入库 |

### 3.2 采集与清洗流程

1. **采集**：真实用户授权样本、内部设计稿转 HTML、合作方授权页面；
   每条记录来源、授权、采集时间与审核人。
2. **提取 HTML**：容忍 Markdown 围栏与前后说明，只保留
   `<!DOCTYPE html>` 到 `</html>`。
3. **规范化**：统一换行、去行尾空白、压缩连续空行。
4. **结构校验**：DOCTYPE / html / head / body 完整，标签闭合（可选闭合标签宽容）。
5. **单文件校验**：拒绝 `<script src>`、`<link>`、`@import`、`url(http...)`、
   iframe / object / embed、`javascript:` 与外部 `src` / `href`。
6. **风格校验**：黑白灰主题、light/dark、CSS 变量、语义标签、响应式、交互态。
7. **PII**：检测邮箱、手机号、身份证号；敏感键名（password/token/email/phone 等）
   递归替换为 `[REDACTED]` 并记 warning。
8. **去重**：以 `scene_code + output_html` 的 SHA-256 精确去重。
9. **切分**：按 `id` 排序后用固定 seed 洗牌，默认 90% / 5% / 5%，
   样本数 ≥3 时保证 val / test 各至少 1 条。
10. **落盘**：`train.jsonl` / `val.jsonl` / `test.jsonl` / `rejects.jsonl` / `stats.json`。

生产语料建议 **500–2000 条**，三类场景均衡，light/dark 各半，密度与字号覆盖
低/中/高三档；种子语料只有 12 条，仅用于验证链路。

### 3.3 训练格式

清洗后的每行是：

```json
{
  "id": "order-detail-light-001",
  "messages": [
    {"role": "system", "content": "你是 InTimeUI 的资深前端工程师……"},
    {"role": "user", "content": "用户需求：……\n场景编码：ORDER_DETAIL_PAGE\n用户界面偏好（JSON）：{...}\n业务数据（JSON）：\n{...}"},
    {"role": "assistant", "content": "<!DOCTYPE html>..."}
  ],
  "meta": {"scene_code": "...", "style": {...}, "source": "seed", "license": "CC-BY-4.0"}
}
```

`system` 与 `server/src/model/prompt.rs::SYSTEM_PROMPT` **逐字一致**，由单元测试守护。
训练脚本用 tokenizer 的 chat template 生成 token，并把 system + user + assistant 起始标记
全部 mask 为 `-100`，只训练 assistant 的 HTML 与结束符。

## 4. 24GB 显存预算

### 4.1 QLoRA（默认）

| 项目 | 估算 |
| --- | --- |
| 4-bit NF4 基座（7B） | ≈4.0GB |
| embedding / lm_head / 量化元数据 | ≈1.5GB |
| LoRA 参数与 8-bit 优化器状态 | ≈0.5GB |
| 激活值（4096 tokens，batch 1，checkpointing） | ≈3–5GB |
| CUDA context / 碎片 / 临时缓冲 | ≈2–3GB |
| **合计（保守）** | **≈21.5GB** |

配置 `training/configs/qlora_qwen2_5_coder_7b.json`：r=16、alpha=32、dropout=0.05，
target 覆盖 attention 与 MLP 七类投影，3 epochs，gradient accumulation 16，
学习率 2e-4，cosine，paged AdamW 8bit。

### 4.2 非量化 LoRA（回退）

bf16 权重约 14GB，加激活与临时缓冲约 22.5GB；因此配置把
`max_seq_length` 降到 2048、r 降到 8、只训练 attention 四类投影。
长样本会被截断，质量与上下文完整性不如 QLoRA，仅作回退方案。

### 4.3 显存不足时的顺序

1. `max_seq_length`: 4096 → 3072 → 2048；
2. `lora.r`: 16 → 8；
3. `per_device_train_batch_size` 保持 1，增大 `gradient_accumulation_steps`；
4. 确认没有其他进程占用 GPU（`nvidia-smi`）；
5. 仍 OOM 则换 14B 以下模型或使用多卡。

## 5. 训练

### 5.1 环境

```bash
python3 -m venv .venv-training
source .venv-training/bin/activate
pip install --upgrade pip
pip install -r training/requirements.txt
python -c "import torch; print(torch.__version__, torch.cuda.is_available())"
nvidia-smi
```

要求：NVIDIA 24GB（RTX 4090 / 3090 / A10 / L4 等），驱动 ≥550，CUDA ≥12.4。
`bitsandbytes` 必须能加载 CUDA 库；若失败，改用非量化 LoRA 配置。

### 5.2 准备语料

```bash
python3 training/scripts/prepare_corpus.py \
  --input training/corpus/raw \
  --output-dir training/data/prepared \
  --seed 42
```

检查 `training/data/prepared/stats.json`：

- `rejected` 与 `duplicates` 的原因在 `rejects.jsonl`；
- `splits` 是否合理（test 太少时增加语料）；
- `scene_counts` / `theme_counts` 是否均衡；
- `sha256` 记录在训练摘要中，保证数据可追溯。

### 5.3 Dry-run

```bash
python3 training/scripts/train_lora.py \
  --config training/configs/qlora_qwen2_5_coder_7b.json \
  --train-file training/data/prepared/train.jsonl \
  --val-file training/data/prepared/val.jsonl \
  --output-dir training/artifacts/qwen2.5-coder-7b-ui-lora \
  --dry-run
```

dry-run 不导入 torch，只校验配置、数据格式、system prompt 与 token 估算。

### 5.4 正式训练

```bash
python3 training/scripts/train_lora.py \
  --config training/configs/qlora_qwen2_5_coder_7b.json \
  --train-file training/data/prepared/train.jsonl \
  --val-file training/data/prepared/val.jsonl \
  --output-dir training/artifacts/qwen2.5-coder-7b-ui-lora \
  --adapter-name qwen2.5-coder-7b-ui-lora
```

断点续训：

```bash
python3 training/scripts/train_lora.py ... \
  --resume-from-checkpoint training/artifacts/qwen2.5-coder-7b-ui-lora/checkpoint-100
```

训练产物：

```
training/artifacts/qwen2.5-coder-7b-ui-lora/
├── adapter_config.json
├── adapter_model.safetensors
├── adapter_manifest.json
├── training_summary.json
└── checkpoint-*/...
```

`training_summary.json` 记录：配置 SHA-256、训练/验证文件 SHA-256、样本数、
截断样本数、训练指标、耗时、GPU 型号与显存、依赖版本、恢复的 checkpoint。
训练脚本会用这些信息自动生成 `adapter_manifest.json`。

## 6. 评测

### 6.1 测试集

`training/eval/testset.jsonl` 是 6 条留出用例（三场景 × light/dark），每条包含
`instruction`、`style`、`business_data` 与 `expected_elements`。生产评测集应
扩充到 ≥30 条，覆盖更多业务数据形态与边界情况，且**不得**出现在训练集中。

### 6.2 指标

| 指标 | 权重 | 判定 |
| --- | --- | --- |
| 结构合法率 | 35% | DOCTYPE / html / head / body 完整、标签闭合 |
| 单文件率 | 20% | 无外部脚本 / 样式 / 字体 / 图片 / iframe |
| 风格一致性 | 25% | 黑白灰、light/dark、CSS 变量、语义标签、响应式、hover/focus/active、reduced-motion、仅允许 transform/opacity/clip-path/filter 动画 |
| 场景要素覆盖率 | 10% | `expected_elements` 命中比例 |
| 可访问性 | 10% | lang、viewport、focus、语义 landmark |

综合得分 = 加权和 × 100。基线/适配器对比：总分提升 ≥2 为 `improved`，
下降 ≥2 为 `regressed`，否则 `unchanged`；任一报告来自 mock 则为 `plumbing_only`。

### 6.3 基线

先按 `deploy/model/docker-compose.yml` 启动基础模型，然后：

```bash
python3 training/scripts/evaluate.py \
  --provider openai \
  --base-url http://127.0.0.1:8000/v1 \
  --model Qwen2.5-Coder-7B-Instruct-AWQ \
  --testset training/eval/testset.jsonl \
  --dataset training/data/prepared/test.jsonl \
  --output-dir training/artifacts/eval/baseline \
  --label baseline
```

### 6.4 适配器

按 §7 启动带 LoRA 的 vLLM 后：

```bash
python3 training/scripts/evaluate.py \
  --provider openai \
  --base-url http://127.0.0.1:8000/v1 \
  --model qwen2.5-coder-7b-ui-lora \
  --testset training/eval/testset.jsonl \
  --dataset training/data/prepared/test.jsonl \
  --output-dir training/artifacts/eval/adapter \
  --label adapter
```

### 6.5 对比与归档

```bash
python3 training/scripts/evaluate.py \
  --compare training/artifacts/eval/baseline/eval_report.json \
  training/artifacts/eval/adapter/eval_report.json \
  --comparison-dir training/artifacts/eval/comparison
```

把 `eval_report.json`、`comparison.json` 与 `training_summary.json` 一起归档；
若结论为 `regressed`，不得上线适配器，先检查数据泄漏、过拟合、学习率与 epoch。

## 7. 适配器导出与热加载

### 7.1 校验与安装

```bash
python3 training/scripts/export_adapter.py \
  --adapter-dir training/artifacts/qwen2.5-coder-7b-ui-lora \
  --served-name qwen2.5-coder-7b-ui-lora \
  --eval-report training/artifacts/eval/adapter/eval_report.json \
  --install-dir deploy/model/adapters \
  --print-vllm
```

`--verify-only` 可在每次部署前复核 `adapter_manifest.json` 中的 SHA-256；
任何文件被替换 / 缺失都会返回非 0。

### 7.2 启动带 LoRA 的 vLLM

```bash
docker compose \
  -f deploy/model/docker-compose.yml \
  -f deploy/model/docker-compose.lora.yml \
  up -d
```

叠加层做四件事：

1. 把 `deploy/model/adapters/` 只读挂载到容器 `/adapters`；
2. 增加 `--enable-lora`、`--lora-modules <name>=/adapters/<name>`、`--max-lora-rank 32`；
3. 设置 `VLLM_ALLOW_RUNTIME_LORA_UPDATING=1`；
4. 保留基础 compose 的量化、上下文、并发参数。

验证：

```bash
curl -s http://127.0.0.1:8000/v1/models | jq '.data[].id'
# 应同时看到基础模型与 qwen2.5-coder-7b-ui-lora
```

### 7.3 接入 intimeui-server

```bash
export MODEL_PROVIDER=openai_compatible
export MODEL_BASE_URL=http://127.0.0.1:8000/v1
export MODEL_NAME=qwen2.5-coder-7b-ui-lora
export MODEL_MAX_CONCURRENCY=4
```

`MODEL_NAME` 必须是适配器的 `served_model_name`，不是基础模型名。回滚时把
`MODEL_NAME` 改回基础模型并重启后端即可，无需重启 vLLM。

### 7.4 运行时热切换

vLLM 开启 `VLLM_ALLOW_RUNTIME_LORA_UPDATING=1` 后：

```bash
# 加载新版本（先安装到 deploy/model/adapters/<new-name>）
curl -s -X POST http://127.0.0.1:8000/v1/load_lora_adapter \
  -H 'Content-Type: application/json' \
  -d '{"lora_name":"qwen2.5-coder-7b-ui-lora-v2","lora_path":"/adapters/qwen2.5-coder-7b-ui-lora-v2"}'

# 卸载旧版本
curl -s -X POST http://127.0.0.1:8000/v1/unload_lora_adapter \
  -H 'Content-Type: application/json' \
  -d '{"lora_name":"qwen2.5-coder-7b-ui-lora"}'
```

后端把 `MODEL_NAME` 指向新 served name 并滚动重启即可完成灰度切换；
生产建议保留上一版本适配器目录，出现质量回退时秒级回滚。

## 8. 复现实验记录模板

每次训练在 `training/artifacts/<run>/run_notes.md` 记录：

```markdown
# 训练记录

- 日期：
- 需求 / 分支 / commit：
- GPU 型号 / 显存 / 驱动 / CUDA：
- 镜像 digest / vLLM 版本：
- 基座模型与 revision：
- 训练配置 SHA-256：
- 训练 / 验证 / 测试样本数：
- 训练时长 / 峰值显存（nvidia-smi）：
- 最终 train loss / eval loss：
- 基线 overall_score：
- 适配器 overall_score：
- 结论（improved / regressed / unchanged）：
- 失败与修复：
```

## 9. 24GB 验收清单

- [ ] `nvidia-smi` 显示 24GB GPU，驱动 / CUDA 满足要求
- [ ] `pip install -r training/requirements.txt` 成功，`torch.cuda.is_available()` 为 True
- [ ] `prepare_corpus.py` 生成 train / val / test，`stats.json` 无异常拒绝
- [ ] `train_lora.py --dry-run` 通过
- [ ] `train_lora.py` 完成 ≥1 个完整 epoch，产出 `adapter_model.safetensors`
- [ ] `training_summary.json` 记录 GPU、依赖版本、数据哈希
- [ ] `export_adapter.py --verify-only` 通过
- [ ] vLLM 的 `/v1/models` 同时列出基础模型与适配器
- [ ] 基线与适配器各跑完同一测试集，生成两份 `eval_report.json`
- [ ] `comparison.json` 的 `conclusion` 为 `improved`（或给出未提升的归因结论）
- [ ] `MODEL_NAME` 指向适配器后，`/api/v1/ui/model/generate` 返回合法单文件 HTML
- [ ] 记录镜像 digest、模型 revision、适配器 SHA-256 与回滚方式

> 本仓库开发机无 NVIDIA GPU，以上真实训练 / 对比项未在本机执行；
> 代码、配置、dry-run、mock 评测与单元测试已通过，验收记录需在 24GB 机器回填。

## 10. 常见问题

| 现象 | 排查 |
| --- | --- |
| `bitsandbytes` 找不到 CUDA | 确认驱动 / CUDA 版本；改用 `lora_qwen2_5_coder_7b.json` |
| CUDA OOM | 按 §4.3 降 seq / r；确认没有其他进程占卡 |
| loss 不下降 | 检查 assistant-only mask 是否生效；学习率是否过大；语料是否过少 |
| 输出仍是 Markdown 围栏 | 训练数据 assistant 必须是纯 HTML；system prompt 必须与线上一致 |
| vLLM 报 adapter not found | 检查 `/adapters/<name>` 挂载与 `--lora-modules` 名称 |
| `MODEL_NAME` 404 | 适配器 served name 与 `--lora-modules` 左侧名称不一致 |
| 评测分数异常高 | 检查测试集是否泄漏进训练集；mock provider 结果不能作为质量结论 |
| 适配器加载后 OOM | 降低 `--max-model-len` / `--max-num-seqs`；LoRA 会额外占用少量显存 |
