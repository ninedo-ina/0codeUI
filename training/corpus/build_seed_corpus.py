#!/usr/bin/env python3
"""从 `seed_manifest.json` + 模板生成可提交的种子语料。

种子语料用于验证「清洗 → 训练格式 → 评测」全链路；生产语料应在此基础上
加入真实采集、人工审核与授权记录，不能只用模板合成数据训练上线模型。
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

TRAINING_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TRAINING_DIR))

from intimeui_training.corpus import validate_sample  # noqa: E402
from intimeui_training.providers import render_template  # noqa: E402


def build(manifest_path: Path, output_path: Path) -> dict[str, int]:
    entries = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(entries, list) or not entries:
        raise SystemExit("seed manifest must be a non-empty array")
    output_path.parent.mkdir(parents=True, exist_ok=True)
    counts: dict[str, int] = {}
    with output_path.open("w", encoding="utf-8", newline="\n") as handle:
        for entry in entries:
            sample = dict(entry)
            sample["output_html"] = render_template(
                sample["scene_code"],
                sample.get("business_data", {}),
                sample["style"]["theme"],
            )
            result = validate_sample(sample)
            if not result.ok:
                raise SystemExit(f"{sample.get('id')}: {'; '.join(result.errors)}")
            assert result.sample is not None
            handle.write(json.dumps(result.sample, ensure_ascii=False, sort_keys=True) + "\n")
            counts[sample["scene_code"]] = counts.get(sample["scene_code"], 0) + 1
    return counts


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        type=Path,
        default=Path(__file__).with_name("seed_manifest.json"),
        help="种子样本清单",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(__file__).with_name("seed") / "seed_corpus.jsonl",
        help="输出 JSONL",
    )
    args = parser.parse_args()
    counts = build(args.manifest, args.output)
    print(json.dumps({"output": str(args.output), "counts": counts}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
