#!/usr/bin/env python3
"""UI 生成质量评测入口。

无 GPU 时用 mock provider 验证链路；真实基线/适配器对比必须指向 vLLM：

    python3 training/scripts/evaluate.py --provider openai \
      --base-url http://127.0.0.1:8000/v1 \
      --model qwen2.5-coder-7b-ui-lora \
      --testset training/eval/testset.jsonl \
      --dataset training/data/prepared/test.jsonl \
      --output-dir training/artifacts/eval/adapter
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

TRAINING_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TRAINING_DIR))

from intimeui_training.eval import (  # noqa: E402
    compare_reports,
    evaluate_cases,
    load_eval_cases,
    write_report_json,
    write_report_markdown,
)
from intimeui_training.providers import (  # noqa: E402
    MockProvider,
    OpenAICompatibleProvider,
)


def _make_provider(args: argparse.Namespace):
    if args.provider == "mock":
        return MockProvider()
    api_key = args.api_key or os.environ.get("INTIMEUI_MODEL_API_KEY", "")
    return OpenAICompatibleProvider(
        base_url=args.base_url,
        model=args.model,
        api_key=api_key or None,
        timeout=args.timeout,
    )


def _write_comparison(
    baseline_path: Path, candidate_path: Path, output_dir: Path | None
) -> dict:
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    candidate = json.loads(candidate_path.read_text(encoding="utf-8"))
    comparison = compare_reports(baseline, candidate)
    if output_dir:
        output_dir.mkdir(parents=True, exist_ok=True)
        (output_dir / "comparison.json").write_text(
            json.dumps(comparison, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        (output_dir / "comparison.md").write_text(
            _comparison_markdown(comparison), encoding="utf-8"
        )
    return comparison


def _comparison_markdown(comparison: dict) -> str:
    lines = [
        "# 基线 / 适配器评测对比",
        "",
        f"- 结论：`{comparison['conclusion']}`",
        f"- 质量结论有效：{'是' if comparison['quality_claim'] else '否（链路验证）'}",
        f"- 基线得分：{comparison['baseline']['overall_score']:.2f}",
        f"- 适配器得分：{comparison['candidate']['overall_score']:.2f}",
        f"- 总分变化：{comparison['overall_delta']:+.2f}",
        "",
        "| 指标 | 变化 |",
        "| --- | --- |",
    ]
    lines.extend(
        f"| {key} | {value:+.4f} |" for key, value in comparison["deltas"].items()
    )
    return "\n".join(lines) + "\n"


def _run_evaluation(args: argparse.Namespace) -> dict:
    cases = load_eval_cases(args.testset)
    provider = _make_provider(args)
    dataset = args.dataset if args.dataset.is_file() else args.testset
    report = evaluate_cases(
        cases,
        provider,
        dataset,
        max_tokens=args.max_tokens,
        temperature=args.temperature,
    )
    report["run_label"] = args.label
    output_dir = args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    json_path = output_dir / "eval_report.json"
    markdown_path = output_dir / "eval_report.md"
    write_report_json(report, json_path)
    write_report_markdown(report, markdown_path)
    return {
        "report_json": str(json_path),
        "report_markdown": str(markdown_path),
        "quality_claim": report["quality_claim"],
        "aggregate": report["aggregate"],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", choices=("mock", "openai"), default="mock")
    parser.add_argument("--base-url", default=os.environ.get("INTIMEUI_MODEL_BASE_URL", "http://127.0.0.1:8000/v1"))
    parser.add_argument("--model", default=os.environ.get("INTIMEUI_MODEL_NAME", "qwen2.5-coder-7b-ui-lora"))
    parser.add_argument("--api-key", default=None)
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--testset", type=Path, default=TRAINING_DIR / "eval" / "testset.jsonl")
    parser.add_argument("--dataset", type=Path, default=TRAINING_DIR / "data" / "prepared" / "test.jsonl")
    parser.add_argument("--output-dir", type=Path, default=TRAINING_DIR / "artifacts" / "eval" / "run")
    parser.add_argument("--label", default="candidate")
    parser.add_argument("--max-tokens", type=int, default=2048)
    parser.add_argument("--temperature", type=float, default=0.2)
    parser.add_argument("--compare", nargs=2, type=Path, metavar=("BASELINE", "CANDIDATE"))
    parser.add_argument("--comparison-dir", type=Path, default=None)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.compare:
            comparison = _write_comparison(
                args.compare[0], args.compare[1], args.comparison_dir or args.output_dir
            )
            print(json.dumps(comparison, ensure_ascii=False, indent=2, sort_keys=True))
            return 0
        result = _run_evaluation(args)
    except Exception as exc:  # 评测失败必须暴露原因
        print(f"evaluate failed: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
