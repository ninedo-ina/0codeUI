#!/usr/bin/env python3
"""LoRA / QLoRA 训练入口。

重依赖（torch / transformers / peft / bitsandbytes）只在真正训练时导入；
`--dry-run` 仅校验配置与数据，可在无 GPU 的 CI 机器上运行。

示例：
    python3 training/scripts/train_lora.py \
      --config training/configs/qlora_qwen2_5_coder_7b.json \
      --train-file training/data/prepared/train.jsonl \
      --val-file training/data/prepared/val.jsonl \
      --output-dir training/artifacts/qwen2.5-coder-7b-ui-lora
"""

from __future__ import annotations

import argparse
import hashlib
import inspect
import json
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping, Sequence

TRAINING_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TRAINING_DIR))

from intimeui_training.config import (  # noqa: E402
    ConfigError,
    config_sha256,
    load_config,
    validate_config,
)
from intimeui_training.manifest import build_manifest, write_manifest  # noqa: E402
from intimeui_training.prompts import SYSTEM_PROMPT  # noqa: E402


class TrainingDataError(ValueError):
    """训练数据不符合对话格式。"""


def read_jsonl(path: str | Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for line_number, line in enumerate(Path(path).read_text(encoding="utf-8").splitlines(), 1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        try:
            record = json.loads(stripped)
        except json.JSONDecodeError as exc:
            raise TrainingDataError(f"{path}:{line_number}: invalid JSON: {exc}") from exc
        validate_record(record, f"{path}:{line_number}")
        records.append(record)
    if not records:
        raise TrainingDataError(f"{path}: no training records")
    return records


def validate_record(record: Any, where: str) -> None:
    if not isinstance(record, dict):
        raise TrainingDataError(f"{where}: record must be an object")
    messages = record.get("messages")
    if not isinstance(messages, list) or len(messages) != 3:
        raise TrainingDataError(f"{where}: messages must contain system / user / assistant")
    roles = [item.get("role") if isinstance(item, dict) else None for item in messages]
    if roles != ["system", "user", "assistant"]:
        raise TrainingDataError(f"{where}: unexpected roles {roles}")
    contents = [item.get("content") for item in messages]
    if any(not isinstance(content, str) or not content.strip() for content in contents):
        raise TrainingDataError(f"{where}: every message needs non-empty content")
    if contents[0] != SYSTEM_PROMPT:
        raise TrainingDataError(
            f"{where}: system prompt differs from server/src/model/prompt.rs::SYSTEM_PROMPT"
        )


def file_sha256(path: str | Path) -> str:
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def estimate_tokens(records: Sequence[Mapping[str, Any]]) -> int:
    """粗略 token 估算：中文按 1.5 字符/token，其余按 4 字符/token。"""
    total = 0
    for record in records:
        for message in record["messages"]:
            text = message["content"]
            cjk = sum(1 for char in text if "一" <= char <= "鿿")
            total += int(cjk / 1.5 + (len(text) - cjk) / 4) + 8
    return total


def dry_run(
    config_path: Path,
    config: Mapping[str, Any],
    train_records: list[dict[str, Any]],
    val_records: list[dict[str, Any]],
    output_dir: Path,
) -> dict[str, Any]:
    return {
        "dry_run": True,
        "config": str(config_path),
        "config_sha256": config_sha256(config),
        "base_model": config["base_model"],
        "method": config["method"],
        "train_records": len(train_records),
        "val_records": len(val_records),
        "estimated_train_tokens": estimate_tokens(train_records),
        "estimated_val_tokens": estimate_tokens(val_records),
        "estimated_vram_gb": config.get("estimated_vram_gb"),
        "vram_budget_gb": config.get("vram_budget_gb"),
        "output_dir": str(output_dir),
        "notes": [
            "dry-run 未导入 torch，也未写适配器；请在 24GB GPU 机器上执行真实训练。",
            "训练前请先运行 prepare_corpus.py 并核对 stats.json 的 rejected / duplicates。",
        ],
    }


def _package_versions() -> dict[str, str]:
    from importlib import metadata

    versions: dict[str, str] = {}
    for name in ("torch", "transformers", "peft", "accelerate", "bitsandbytes", "datasets"):
        try:
            versions[name] = metadata.version(name)
        except metadata.PackageNotFoundError:
            versions[name] = "not-installed"
    return versions


def _load_tokenizer(base_model: str):
    from transformers import AutoTokenizer

    tokenizer = AutoTokenizer.from_pretrained(base_model, trust_remote_code=True)
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token
    tokenizer.padding_side = "right"
    return tokenizer


def _load_model(config: Mapping[str, Any]):
    import torch
    from transformers import AutoModelForCausalLM, BitsAndBytesConfig

    dtype = torch.bfloat16 if config["training"].get("bf16") else torch.float16
    kwargs: dict[str, Any] = {
        "trust_remote_code": True,
        "device_map": "auto",
        "torch_dtype": dtype,
    }
    if config["method"] == "qlora":
        quant = config.get("quantization") or {}
        kwargs["quantization_config"] = BitsAndBytesConfig(
            load_in_4bit=bool(quant.get("load_in_4bit", True)),
            bnb_4bit_quant_type=str(quant.get("bnb_4bit_quant_type", "nf4")),
            bnb_4bit_compute_dtype=dtype,
            bnb_4bit_use_double_quant=bool(quant.get("bnb_4bit_use_double_quant", True)),
        )
    model = AutoModelForCausalLM.from_pretrained(config["base_model"], **kwargs)
    model.config.use_cache = False
    return model


def _apply_peft(model, config: Mapping[str, Any]):
    from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training

    training = config["training"]
    if config["method"] == "qlora":
        model = prepare_model_for_kbit_training(
            model, use_gradient_checkpointing=bool(training.get("gradient_checkpointing"))
        )
    if training.get("gradient_checkpointing"):
        model.gradient_checkpointing_enable()
        if hasattr(model, "enable_input_require_grads"):
            model.enable_input_require_grads()
    lora = config["lora"]
    peft_config = LoraConfig(
        r=int(lora["r"]),
        lora_alpha=int(lora["alpha"]),
        lora_dropout=float(lora.get("dropout", 0.0)),
        target_modules=list(lora["target_modules"]),
        bias=str(lora.get("bias", "none")),
        task_type=str(lora.get("task_type", "CAUSAL_LM")),
    )
    return get_peft_model(model, peft_config)


def _tokenize_record(tokenizer, record: Mapping[str, Any], max_seq_length: int):
    messages = record["messages"]
    prompt_ids = tokenizer.apply_chat_template(
        messages[:-1], tokenize=True, add_generation_prompt=True
    )
    full_ids = tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=False)
    full_ids = tokenizer(full_ids, add_special_tokens=False)["input_ids"]
    if full_ids[: len(prompt_ids)] != prompt_ids:
        raise TrainingDataError("chat template prefix mismatch; cannot mask prompt tokens")
    if len(prompt_ids) >= max_seq_length:
        return None
    labels = list(full_ids)
    labels[: len(prompt_ids)] = [-100] * len(prompt_ids)
    input_ids = full_ids[:max_seq_length]
    return {
        "input_ids": input_ids,
        "attention_mask": [1] * len(input_ids),
        "labels": labels[:max_seq_length],
    }


class _TokenizedDataset:
    def __init__(self, items: list[dict[str, list[int]]]) -> None:
        self.items = items

    def __len__(self) -> int:
        return len(self.items)

    def __getitem__(self, index: int) -> dict[str, list[int]]:
        return self.items[index]


def _collate(features: list[dict[str, list[int]]], pad_token_id: int) -> dict[str, Any]:
    import torch

    max_length = max(len(item["input_ids"]) for item in features)
    input_ids, attention_mask, labels = [], [], []
    for item in features:
        padding = max_length - len(item["input_ids"])
        input_ids.append(item["input_ids"] + [pad_token_id] * padding)
        attention_mask.append(item["attention_mask"] + [0] * padding)
        labels.append(item["labels"] + [-100] * padding)
    return {
        "input_ids": torch.tensor(input_ids, dtype=torch.long),
        "attention_mask": torch.tensor(attention_mask, dtype=torch.long),
        "labels": torch.tensor(labels, dtype=torch.long),
    }


def _training_arguments(config: Mapping[str, Any], output_dir: Path, has_eval: bool):
    from transformers import TrainingArguments

    training = config["training"]
    kwargs: dict[str, Any] = {
        "output_dir": str(output_dir),
        "num_train_epochs": float(training["num_train_epochs"]),
        "per_device_train_batch_size": int(training["per_device_train_batch_size"]),
        "gradient_accumulation_steps": int(training["gradient_accumulation_steps"]),
        "learning_rate": float(training["learning_rate"]),
        "lr_scheduler_type": str(training.get("lr_scheduler_type", "cosine")),
        "warmup_ratio": float(training.get("warmup_ratio", 0.0)),
        "logging_steps": int(training.get("logging_steps", 10)),
        "save_strategy": "steps",
        "save_steps": int(training.get("save_steps", 100)),
        "save_total_limit": int(training.get("save_total_limit", 2)),
        "bf16": bool(training.get("bf16", False)),
        "gradient_checkpointing": bool(training.get("gradient_checkpointing", False)),
        "optim": str(training.get("optim", "adamw_torch")),
        "seed": int(training.get("seed", 42)),
        "data_seed": int(training.get("seed", 42)),
        "report_to": [],
        "remove_unused_columns": False,
    }
    parameters = inspect.signature(TrainingArguments.__init__).parameters
    eval_key = "eval_strategy" if "eval_strategy" in parameters else "evaluation_strategy"
    kwargs[eval_key] = "steps" if has_eval else "no"
    if has_eval:
        kwargs["eval_steps"] = int(training.get("save_steps", 100))
    return TrainingArguments(**kwargs)


def _build_datasets(tokenizer, records, max_seq_length: int) -> tuple[_TokenizedDataset, int]:
    items: list[dict[str, list[int]]] = []
    skipped = 0
    for record in records:
        tokenized = _tokenize_record(tokenizer, record, max_seq_length)
        if tokenized is None:
            skipped += 1
        else:
            items.append(tokenized)
    if not items:
        raise TrainingDataError("all records were truncated away; increase max_seq_length")
    return _TokenizedDataset(items), skipped


def _gpu_info(torch) -> dict[str, Any]:
    if not torch.cuda.is_available():
        return {"name": None, "total_memory_gb": None}
    return {
        "name": torch.cuda.get_device_name(0),
        "total_memory_gb": round(torch.cuda.get_device_properties(0).total_memory / 1024**3, 2),
    }


def _build_summary(
    config: Mapping[str, Any],
    train_result,
    adapter_name: str,
    train_file: Path,
    val_file: Path | None,
    train_records: list[dict[str, Any]],
    val_records: list[dict[str, Any]],
    train_skipped: int,
    val_skipped: int,
    duration: float,
    resume_from_checkpoint: str | None,
    torch,
) -> dict[str, Any]:
    return {
        "adapter_name": adapter_name,
        "base_model": config["base_model"],
        "method": config["method"],
        "config_sha256": config_sha256(config),
        "train_file": str(train_file),
        "train_sha256": file_sha256(train_file),
        "val_file": str(val_file) if val_file else None,
        "val_sha256": file_sha256(val_file) if val_file else None,
        "train_records": len(train_records),
        "val_records": len(val_records),
        "train_skipped_too_long": train_skipped,
        "val_skipped_too_long": val_skipped,
        "train_metrics": dict(train_result.metrics),
        "duration_seconds": duration,
        "packages": _package_versions(),
        "gpu": _gpu_info(torch),
        "resume_from_checkpoint": resume_from_checkpoint,
    }


def _save_outputs(
    output_dir: Path, adapter_name: str, config: Mapping[str, Any], summary: dict[str, Any]
) -> Path:
    (output_dir / "training_summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    manifest = build_manifest(
        output_dir,
        adapter_name=adapter_name,
        base_model=config["base_model"],
        method=config["method"],
        served_model_name=adapter_name,
        training=summary,
        evaluation={"status": "pending", "report": None},
        notes=[
            "训练完成后请运行 training/scripts/evaluate.py 生成基线/适配器对比报告。",
            "部署前运行 training/scripts/export_adapter.py --verify-only 复核文件指纹。",
        ],
    )
    return write_manifest(manifest, output_dir)


def run_training(
    config: Mapping[str, Any],
    train_records: list[dict[str, Any]],
    val_records: list[dict[str, Any]],
    output_dir: Path,
    resume_from_checkpoint: str | None,
    adapter_name: str,
    train_file: Path,
    val_file: Path | None,
) -> dict[str, Any]:
    import torch
    from transformers import Trainer

    started = time.time()
    tokenizer = _load_tokenizer(config["base_model"])
    model = _apply_peft(_load_model(config), config)
    max_seq = int(config["training"]["max_seq_length"])
    train_dataset, train_skipped = _build_datasets(tokenizer, train_records, max_seq)
    val_dataset, val_skipped = (
        _build_datasets(tokenizer, val_records, max_seq) if val_records else (None, 0)
    )
    output_dir.mkdir(parents=True, exist_ok=True)
    trainer = Trainer(
        model=model,
        args=_training_arguments(config, output_dir, has_eval=val_dataset is not None),
        train_dataset=train_dataset,
        eval_dataset=val_dataset,
        data_collator=lambda features: _collate(features, tokenizer.pad_token_id),
    )
    train_result = trainer.train(resume_from_checkpoint=resume_from_checkpoint)
    model.save_pretrained(output_dir, safe_serialization=True)
    duration = round(time.time() - started, 2)
    summary = _build_summary(
        config, train_result, adapter_name, train_file, val_file,
        train_records, val_records, train_skipped, val_skipped,
        duration, resume_from_checkpoint, torch,
    )
    manifest_path = _save_outputs(output_dir, adapter_name, config, summary)
    return {
        "output_dir": str(output_dir),
        "manifest": str(manifest_path),
        "duration_seconds": duration,
        "train_metrics": summary["train_metrics"],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True, help="训练配置 JSON")
    parser.add_argument(
        "--train-file",
        type=Path,
        default=TRAINING_DIR / "data" / "prepared" / "train.jsonl",
    )
    parser.add_argument(
        "--val-file",
        type=Path,
        default=TRAINING_DIR / "data" / "prepared" / "val.jsonl",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=TRAINING_DIR / "artifacts" / "ui-lora-adapter",
    )
    parser.add_argument("--adapter-name", default="ui-lora-adapter")
    parser.add_argument("--resume-from-checkpoint", default=None)
    parser.add_argument("--dry-run", action="store_true", help="只校验配置与数据，不导入训练框架")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        config = load_config(args.config)
        train_records = read_jsonl(args.train_file)
        val_records = read_jsonl(args.val_file) if args.val_file.is_file() else []
    except (ConfigError, TrainingDataError, OSError, json.JSONDecodeError) as exc:
        print(f"train_lora failed: {exc}", file=sys.stderr)
        return 1
    if args.dry_run:
        print(
            json.dumps(
                dry_run(args.config, config, train_records, val_records, args.output_dir),
                ensure_ascii=False,
                indent=2,
                sort_keys=True,
            )
        )
        return 0
    try:
        result = run_training(
            config=config,
            train_records=train_records,
            val_records=val_records,
            output_dir=args.output_dir,
            resume_from_checkpoint=args.resume_from_checkpoint,
            adapter_name=args.adapter_name,
            train_file=args.train_file,
            val_file=args.val_file if args.val_file.is_file() else None,
        )
    except Exception as exc:  # 训练失败要打印真实原因，避免静默
        print(f"train_lora failed: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
