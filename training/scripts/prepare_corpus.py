#!/usr/bin/env python3
"""清洗 UI 语料并生成 train / val / test JSONL。

示例：
    python3 training/scripts/prepare_corpus.py \
      --input training/corpus/seed \
      --output-dir training/data/prepared
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

TRAINING_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TRAINING_DIR))

from intimeui_training.corpus import CorpusError, prepare_dataset  # noqa: E402


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--input",
        action="append",
        required=True,
        help="语料文件或目录，可重复传入",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=TRAINING_DIR / "data" / "prepared",
        help="输出目录（train.jsonl / val.jsonl / test.jsonl / stats.json）",
    )
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--train-ratio", type=float, default=0.9)
    parser.add_argument("--val-ratio", type=float, default=0.05)
    parser.add_argument("--max-samples", type=int, default=None)
    args = parser.parse_args()
    try:
        stats = prepare_dataset(
            args.input,
            args.output_dir,
            seed=args.seed,
            train_ratio=args.train_ratio,
            val_ratio=args.val_ratio,
            max_samples=args.max_samples,
        )
    except (CorpusError, OSError, json.JSONDecodeError) as exc:
        print(f"prepare_corpus failed: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(stats, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
