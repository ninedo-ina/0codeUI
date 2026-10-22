# InTimeUI 训练线（M6）

UI 语料清洗、LoRA / QLoRA 微调、质量评测与适配器热加载的完整工具链，目标是在
**单卡 24GB** 上完成一次可复现的 UI 生成微调。

> 详细原理、显存预算、验收记录模板见 [`docs/lora-finetuning.md`](../docs/lora-finetuning.md)。

## 目录

```
training/
├── configs/                 # LoRA / QLoRA 训练配置
├── corpus/
│   ├── schema.json          # 语料 JSONL schema
│   ├── seed_manifest.json   # 12 条可复现种子样本
│   ├── templates/           # 种子语料 / mock provider 共用模板
│   ├── seed/                # 生成的种子语料
│   └── build_seed_corpus.py
├── eval/testset.jsonl       # 6 条留出评测用例
├── intimeui_training/       # 纯标准库核心包（corpus / eval / manifest / config）
├── scripts/                 # 清洗、训练、评测、导出、自检 CLI
├── tests/                   # unittest（CI 可跑，无需 GPU）
└── requirements.txt         # 真实训练依赖（torch / transformers / peft / bitsandbytes）
```

## 快速开始（无 GPU）

```bash
# 1. 单元测试
python3 -m unittest discover -s training/tests -t training -v

# 2. 全链路自检：清洗 → 训练 dry-run → mock 评测
python3 training/scripts/smoke_pipeline.py

# 3. 只准备语料
python3 training/scripts/prepare_corpus.py \
  --input training/corpus/seed \
  --output-dir training/data/prepared
```

mock 评测只验证脚本接口与指标链路，**不能作为模型质量结论**。

## 真实训练（24GB GPU）

```bash
python3 -m venv .venv-training
source .venv-training/bin/activate
pip install -r training/requirements.txt

# 语料准备（生产语料放在 training/corpus/raw/，支持 .jsonl / .json）
python3 training/scripts/prepare_corpus.py \
  --input training/corpus/raw \
  --output-dir training/data/prepared

# 先 dry-run 核对配置与数据
python3 training/scripts/train_lora.py \
  --config training/configs/qlora_qwen2_5_coder_7b.json \
  --train-file training/data/prepared/train.jsonl \
  --val-file training/data/prepared/val.jsonl \
  --output-dir training/artifacts/qwen2.5-coder-7b-ui-lora \
  --dry-run

# 正式训练（单卡 24GB，约 2–4 小时 / 3 epochs，视语料规模）
python3 training/scripts/train_lora.py \
  --config training/configs/qlora_qwen2_5_coder_7b.json \
  --train-file training/data/prepared/train.jsonl \
  --val-file training/data/prepared/val.jsonl \
  --output-dir training/artifacts/qwen2.5-coder-7b-ui-lora \
  --adapter-name qwen2.5-coder-7b-ui-lora
```

训练完成后输出 PEFT 适配器、`training_summary.json` 与 `adapter_manifest.json`。

## 评测

```bash
# 基线：先启动基础模型（deploy/model/docker-compose.yml）
python3 training/scripts/evaluate.py \
  --provider openai \
  --base-url http://127.0.0.1:8000/v1 \
  --model Qwen2.5-Coder-7B-Instruct-AWQ \
  --testset training/eval/testset.jsonl \
  --dataset training/data/prepared/test.jsonl \
  --output-dir training/artifacts/eval/baseline \
  --label baseline

# 适配器：按 docs/lora-finetuning.md 启动带 LoRA 的 vLLM 后
python3 training/scripts/evaluate.py \
  --provider openai \
  --base-url http://127.0.0.1:8000/v1 \
  --model qwen2.5-coder-7b-ui-lora \
  --testset training/eval/testset.jsonl \
  --dataset training/data/prepared/test.jsonl \
  --output-dir training/artifacts/eval/adapter \
  --label adapter

# 对比
python3 training/scripts/evaluate.py \
  --compare training/artifacts/eval/baseline/eval_report.json \
  training/artifacts/eval/adapter/eval_report.json \
  --comparison-dir training/artifacts/eval/comparison
```

指标：结构合法率 35%、单文件率 20%、风格一致性 25%、场景要素覆盖 10%、
可访问性 10%，综合得分 0–100。总分提升 ≥2 分记为 `improved`，下降 ≥2 分记为
`regressed`；mock 报告固定为 `plumbing_only`。

## 导出与热加载

```bash
python3 training/scripts/export_adapter.py \
  --adapter-dir training/artifacts/qwen2.5-coder-7b-ui-lora \
  --served-name qwen2.5-coder-7b-ui-lora \
  --eval-report training/artifacts/eval/adapter/eval_report.json \
  --install-dir deploy/model/adapters \
  --print-vllm
```

随后使用 `deploy/model/docker-compose.lora.yml` 叠加启动 vLLM，把
`intimeui-server` 的 `MODEL_NAME` 指向适配器 served name。部署前可用
`--verify-only` 复核 SHA-256 指纹。

## 数据与合规

- 生产语料必须有明确授权（`license` 字段），未知授权不得入库；
- 清洗阶段自动检测邮箱 / 手机号 / 身份证号，敏感键名递归脱敏；
- 禁止外部脚本 / 样式 / 字体 / 图片 / iframe；
- 种子语料仅用于链路验证，不能代表真实用户分布，也不能单独用于上线训练。
