#!/usr/bin/env python3
"""无 GPU 全链路自检：语料清洗 → 训练 dry-run → mock 评测。

该脚本只验证脚本接口与数据格式，不代表模型质量；真实训练与评测需在
24GB GPU 机器上执行（见 docs/lora-finetuning.md）。
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

TRAINING_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TRAINING_DIR))

from intimeui_training.corpus import prepare_dataset  # noqa: E402


def _run(command: list[str]) -> dict:
    completed = subprocess.run(command, check=False, capture_output=True, text=True)
    if completed.returncode != 0:
        raise RuntimeError(
            f"command failed ({completed.returncode}): {' '.join(command)}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return json.loads(completed.stdout)


def run_smoke(work_dir: Path) -> dict:
    prepared_dir = work_dir / "prepared"
    eval_dir = work_dir / "eval"
    stats = prepare_dataset([TRAINING_DIR / "corpus" / "seed"], prepared_dir)
    dry_run = _run(
        [
            sys.executable,
            str(TRAINING_DIR / "scripts" / "train_lora.py"),
            "--config",
            str(TRAINING_DIR / "configs" / "qlora_qwen2_5_coder_7b.json"),
            "--train-file",
            str(prepared_dir / "train.jsonl"),
            "--val-file",
            str(prepared_dir / "val.jsonl"),
            "--output-dir",
            str(work_dir / "adapter"),
            "--dry-run",
        ]
    )
    evaluation = _run(
        [
            sys.executable,
            str(TRAINING_DIR / "scripts" / "evaluate.py"),
            "--provider",
            "mock",
            "--testset",
            str(TRAINING_DIR / "eval" / "testset.jsonl"),
            "--dataset",
            str(prepared_dir / "test.jsonl"),
            "--output-dir",
            str(eval_dir),
            "--label",
            "smoke-mock",
        ]
    )
    return {
        "work_dir": str(work_dir),
        "corpus": {
            "accepted": stats["accepted"],
            "rejected": stats["rejected"],
            "duplicates": stats["duplicates"],
            "splits": stats["splits"],
        },
        "train_dry_run": dry_run,
        "mock_evaluation": evaluation,
        "quality_claim": False,
        "note": "mock 评测只验证链路；真实质量结论需用 --provider openai 指向 vLLM。",
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-dir", type=Path, default=None)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.work_dir:
        work_dir = args.work_dir.resolve()
        work_dir.mkdir(parents=True, exist_ok=True)
        return _run_and_print(work_dir)
    with tempfile.TemporaryDirectory(prefix="intimeui-smoke-") as temporary:
        return _run_and_print(Path(temporary))


def _run_and_print(work_dir: Path) -> int:
    try:
        result = run_smoke(work_dir)
    except Exception as exc:  # 自检失败必须给出完整命令输出
        print(f"smoke_pipeline failed: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
