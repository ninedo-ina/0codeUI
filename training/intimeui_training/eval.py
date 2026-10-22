"""UI 生成质量评测：结构合法率、单文件率、风格一致性、场景要素覆盖。

评测不依赖模型框架：任何能返回 HTML 的 provider（mock / OpenAI 兼容 vLLM）
都可以复用同一套指标，便于基线模型与 LoRA 适配器做同口径对比。
"""

from __future__ import annotations

import hashlib
import json
import re
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Mapping, Protocol

from .corpus import (
    extract_html,
    html_structure_issues,
    single_file_issues,
)
from .prompts import SYSTEM_PROMPT, build_user_prompt

METRIC_KEYS = (
    "structure_valid_rate",
    "single_file_rate",
    "style_consistency_rate",
    "scene_coverage_rate",
    "accessibility_rate",
)
METRIC_WEIGHTS = {
    "structure_valid_rate": 0.35,
    "single_file_rate": 0.20,
    "style_consistency_rate": 0.25,
    "scene_coverage_rate": 0.10,
    "accessibility_rate": 0.10,
}

HEX_RE = re.compile(r"#([0-9a-fA-F]{3,8})\b")
RGB_RE = re.compile(
    r"rgba?\(\s*([\d.]+%?)\s*[, ]\s*([\d.]+%?)\s*[, ]\s*([\d.]+%?)(?:\s*[,/]\s*[\d.]+%?)?\s*\)",
    re.IGNORECASE,
)
HSL_RE = re.compile(
    r"hsla?\(\s*([\d.]+)(?:deg)?\s*[, ]\s*([\d.]+)%\s*[, ]\s*([\d.]+)%",
    re.IGNORECASE,
)
COLOR_PROP_RE = re.compile(
    r"(?:^|[;{])\s*(?:color|background(?:-color)?|border(?:-[a-z]+)?-color|fill|stroke)\s*:\s*([a-z]+)",
    re.IGNORECASE,
)
ANIMATED_PROPS = {"transform", "opacity", "clip-path", "filter", "none"}
COLOR_NAMES = {
    "red",
    "green",
    "blue",
    "orange",
    "purple",
    "pink",
    "yellow",
    "cyan",
    "magenta",
    "brown",
    "gold",
    "teal",
    "navy",
    "maroon",
    "olive",
    "lime",
    "aqua",
    "fuchsia",
}


class Provider(Protocol):
    """评测 provider 协议。"""

    def describe(self) -> dict[str, Any]: ...

    def generate(
        self, messages: list[dict[str, str]], max_tokens: int, temperature: float
    ) -> str: ...


def _css(html: str) -> str:
    return "\n".join(re.findall(r"<style\b[^>]*>(.*?)</style>", html, re.IGNORECASE | re.DOTALL))


def _rgb_components(token: str) -> int:
    token = token.strip()
    if token.endswith("%"):
        return round(float(token[:-1]) * 255 / 100)
    return round(float(token))


def _hex_rgb(value: str) -> tuple[int, int, int] | None:
    digits = value.lstrip("#")
    if len(digits) in (3, 4):
        digits = "".join(ch * 2 for ch in digits[:3])
    if len(digits) in (6, 8):
        return tuple(int(digits[index : index + 2], 16) for index in (0, 2, 4))  # type: ignore[return-value]
    return None


def _is_gray(rgb: tuple[int, int, int], tolerance: int = 16) -> bool:
    return max(rgb) - min(rgb) <= tolerance


def non_grayscale_colors(html: str) -> list[str]:
    """提取明显带色相的 CSS 颜色；黑白灰全部通过。"""
    css = _css(html)
    colored: list[str] = []
    for match in HEX_RE.finditer(css):
        rgb = _hex_rgb(match.group(0))
        if rgb is not None and not _is_gray(rgb):
            colored.append(match.group(0))
    for match in RGB_RE.finditer(css):
        try:
            rgb = tuple(_rgb_components(match.group(index)) for index in (1, 2, 3))
        except ValueError:
            continue
        if not _is_gray(rgb):  # type: ignore[arg-type]
            colored.append(match.group(0))
    for match in HSL_RE.finditer(css):
        try:
            saturation = float(match.group(2))
        except ValueError:
            continue
        if saturation > 8:
            colored.append(match.group(0))
    for match in COLOR_PROP_RE.finditer(css):
        name = match.group(1).lower()
        if name in COLOR_NAMES:
            colored.append(name)
    return sorted(set(colored))


def style_consistency_issues(html: str) -> list[str]:
    """黑白灰主题、明暗切换、CSS 变量、语义标签、响应式与交互态。"""
    css = _css(html)
    lower = html.lower()
    issues: list[str] = []
    colored = non_grayscale_colors(html)
    if colored:
        issues.append(f"non-grayscale colors: {colored[:5]}")
    theme_markers = (
        "prefers-color-scheme",
        "data-theme",
        "classlist.toggle('dark')",
        'classlist.toggle("dark")',
        "theme-toggle",
        "切换主题",
        "深色模式",
    )
    if not any(marker in lower for marker in theme_markers):
        issues.append("missing light/dark theme support")
    if len(re.findall(r"--[a-z0-9-]+\s*:", css)) < 3:
        issues.append("fewer than 3 CSS custom properties")
    semantic = {
        tag
        for tag in ("header", "nav", "main", "section", "article", "footer", "aside")
        if re.search(rf"<{tag}\b", lower)
    }
    if len(semantic) < 2:
        issues.append("missing semantic layout tags")
    if '<meta name="viewport"' not in lower and "<meta name='viewport'" not in lower:
        issues.append("missing viewport meta")
    if "@media" not in css:
        issues.append("missing responsive @media rule")
    for state in (":hover", ":active"):
        if state not in css:
            issues.append(f"missing {state} state")
    if ":focus-visible" not in css and ":focus" not in css:
        issues.append("missing focus state")
    if "prefers-reduced-motion" not in css:
        issues.append("missing prefers-reduced-motion")
    for match in re.finditer(r"transition\s*:\s*([^;}]+)", css, re.IGNORECASE):
        for part in match.group(1).split(","):
            prop = part.strip().split()[0] if part.strip() else ""
            if prop and prop not in ANIMATED_PROPS:
                issues.append(f"transition animates non-allowed property: {prop}")
    return issues


def accessibility_issues(html: str) -> list[str]:
    lower = html.lower()
    css = _css(html)
    issues: list[str] = []
    if not re.search(r"<html\b[^>]*\blang\s*=", lower):
        issues.append("missing html lang attribute")
    if '<meta name="viewport"' not in lower and "<meta name='viewport'" not in lower:
        issues.append("missing viewport meta")
    if ":focus-visible" not in css and ":focus" not in css:
        issues.append("missing visible focus style")
    if len(
        [
            tag
            for tag in ("header", "nav", "main", "section", "article", "footer")
            if re.search(rf"<{tag}\b", lower)
        ]
    ) < 2:
        issues.append("missing semantic landmarks")
    return issues


def scene_coverage(html: str, expected_elements: Iterable[str]) -> float:
    expected = [item for item in expected_elements if item]
    if not expected:
        return 1.0
    lower = html.lower()
    hits = sum(1 for item in expected if item.lower() in lower)
    return round(hits / len(expected), 4)


def evaluate_html(html: str, expected_elements: Iterable[str]) -> dict[str, Any]:
    structure = html_structure_issues(html)
    single_file = single_file_issues(html)
    style = style_consistency_issues(html)
    accessibility = accessibility_issues(html)
    coverage = scene_coverage(html, expected_elements)
    metrics = {
        "structure_valid": not structure,
        "single_file": not single_file,
        "style_consistent": not style,
        "scene_coverage": coverage,
        "accessible": not accessibility,
    }
    return {
        "metrics": metrics,
        "issues": {
            "structure": structure,
            "single_file": single_file,
            "style": style,
            "accessibility": accessibility,
        },
        "html_sha256": hashlib.sha256(html.encode("utf-8")).hexdigest(),
    }


def load_eval_cases(path: str | Path) -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    for line_number, line in enumerate(Path(path).read_text(encoding="utf-8").splitlines(), 1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        try:
            case = json.loads(stripped)
        except json.JSONDecodeError as exc:
            raise ValueError(f"{path}:{line_number}: invalid JSON: {exc}") from exc
        if not isinstance(case, dict) or not all(
            key in case for key in ("id", "scene_code", "instruction")
        ):
            raise ValueError(f"{path}:{line_number}: id / scene_code / instruction are required")
        cases.append(case)
    if not cases:
        raise ValueError(f"{path}: no eval cases")
    return cases


def _provider_messages(case: Mapping[str, Any]) -> list[dict[str, str]]:
    sample = {
        "instruction": case["instruction"],
        "scene_code": case["scene_code"],
        "style": case.get("style"),
        "business_data": case.get("business_data"),
    }
    return [
        {"role": "system", "content": SYSTEM_PROMPT},
        {"role": "user", "content": build_user_prompt(sample)},
    ]


def evaluate_cases(
    cases: list[dict[str, Any]],
    provider: Provider,
    dataset_path: str | Path,
    max_tokens: int = 2048,
    temperature: float = 0.2,
) -> dict[str, Any]:
    results: list[dict[str, Any]] = []
    for case in cases:
        started = time.perf_counter()
        error: str | None = None
        html = ""
        try:
            raw = provider.generate(_provider_messages(case), max_tokens, temperature)
            html = extract_html(raw)
        except Exception as exc:  # 单条失败不中断整批评测
            error = f"{type(exc).__name__}: {exc}"
        latency_ms = round((time.perf_counter() - started) * 1000, 3)
        if error:
            results.append(
                {
                    "id": case["id"],
                    "scene_code": case["scene_code"],
                    "latency_ms": latency_ms,
                    "error": error,
                    "metrics": {
                        "structure_valid": False,
                        "single_file": False,
                        "style_consistent": False,
                        "scene_coverage": 0.0,
                        "accessible": False,
                    },
                    "issues": {"provider": [error]},
                    "html_sha256": None,
                }
            )
            continue
        evaluated = evaluate_html(html, case.get("expected_elements", []))
        results.append(
            {
                "id": case["id"],
                "scene_code": case["scene_code"],
                "latency_ms": latency_ms,
                **evaluated,
            }
        )

    aggregate = _aggregate(results)
    provider_info = provider.describe()
    quality_claim = bool(provider_info.get("quality_claim", True)) and all(
        "error" not in item for item in results
    )
    report = {
        "schema_version": "1",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "provider": provider_info,
        "dataset": {
            "path": str(dataset_path),
            "count": len(cases),
            "sha256": hashlib.sha256(Path(dataset_path).read_bytes()).hexdigest(),
        },
        "aggregate": aggregate,
        "quality_claim": quality_claim,
        "cases": results,
        "notes": [
            "overall_score = 0.35*structure + 0.20*single_file + 0.25*style + 0.10*scene + 0.10*a11y，满分 100。",
            "mock provider 仅验证评测链路，不能作为真实模型质量结论。",
        ],
    }
    if not quality_claim:
        report["notes"].append("本次评测包含 mock provider 或请求错误，结论仅用于链路验证。")
    return report


def _aggregate(results: list[dict[str, Any]]) -> dict[str, Any]:
    count = len(results)
    if count == 0:
        return {"count": 0, **{key: 0.0 for key in METRIC_KEYS}, "overall_score": 0.0}
    structure = sum(1 for item in results if item["metrics"]["structure_valid"]) / count
    single = sum(1 for item in results if item["metrics"]["single_file"]) / count
    style = sum(1 for item in results if item["metrics"]["style_consistent"]) / count
    coverage = sum(float(item["metrics"]["scene_coverage"]) for item in results) / count
    accessible = sum(1 for item in results if item["metrics"]["accessible"]) / count
    metrics = {
        "structure_valid_rate": round(structure, 4),
        "single_file_rate": round(single, 4),
        "style_consistency_rate": round(style, 4),
        "scene_coverage_rate": round(coverage, 4),
        "accessibility_rate": round(accessible, 4),
    }
    overall = sum(metrics[key] * METRIC_WEIGHTS[key] for key in METRIC_KEYS) * 100
    return {"count": count, **metrics, "overall_score": round(overall, 2)}


def write_report_json(report: Mapping[str, Any], path: str | Path) -> None:
    Path(path).write_text(
        json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def write_report_markdown(report: Mapping[str, Any], path: str | Path) -> None:
    provider = report["provider"]
    aggregate = report["aggregate"]
    lines = [
        "# UI 生成质量评测报告",
        "",
        f"- 生成时间：{report['generated_at']}",
        f"- Provider：`{provider.get('kind')}` / `{provider.get('model')}`",
        f"- 数据集：`{report['dataset']['path']}`（{report['dataset']['count']} 条）",
        f"- 质量结论有效：{'是' if report.get('quality_claim') else '否（仅链路验证）'}",
        "",
        "## 汇总",
        "",
        "| 指标 | 数值 |",
        "| --- | --- |",
        f"| 结构合法率 | {aggregate['structure_valid_rate']:.2%} |",
        f"| 单文件率 | {aggregate['single_file_rate']:.2%} |",
        f"| 风格一致性 | {aggregate['style_consistency_rate']:.2%} |",
        f"| 场景要素覆盖 | {aggregate['scene_coverage_rate']:.2%} |",
        f"| 可访问性 | {aggregate['accessibility_rate']:.2%} |",
        f"| **综合得分** | **{aggregate['overall_score']:.2f} / 100** |",
        "",
        "## 分用例",
        "",
        "| 用例 | 场景 | 结构 | 单文件 | 风格 | 覆盖 | 可访问 | 延迟(ms) |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for case in report["cases"]:
        metrics = case["metrics"]
        mark = lambda value: "✅" if value else "❌"  # noqa: E731
        lines.append(
            f"| {case['id']} | {case['scene_code']} | {mark(metrics['structure_valid'])} | "
            f"{mark(metrics['single_file'])} | {mark(metrics['style_consistent'])} | "
            f"{metrics['scene_coverage']:.0%} | {mark(metrics['accessible'])} | {case['latency_ms']:.1f} |"
        )
    lines.extend(["", "## 说明", ""])
    lines.extend(f"- {note}" for note in report.get("notes", []))
    Path(path).write_text("\n".join(lines) + "\n", encoding="utf-8")


def compare_reports(baseline: Mapping[str, Any], candidate: Mapping[str, Any]) -> dict[str, Any]:
    """对比两份评测报告，输出各指标 delta 与结论。"""
    base_agg = baseline["aggregate"]
    cand_agg = candidate["aggregate"]
    deltas = {
        key: round(float(cand_agg[key]) - float(base_agg[key]), 4) for key in METRIC_KEYS
    }
    overall_delta = round(float(cand_agg["overall_score"]) - float(base_agg["overall_score"]), 2)
    quality_claim = bool(baseline.get("quality_claim")) and bool(candidate.get("quality_claim"))
    if not quality_claim:
        conclusion = "plumbing_only"
    elif overall_delta >= 2:
        conclusion = "improved"
    elif overall_delta <= -2:
        conclusion = "regressed"
    else:
        conclusion = "unchanged"
    return {
        "schema_version": "1",
        "baseline": {
            "provider": baseline["provider"],
            "overall_score": base_agg["overall_score"],
            "count": base_agg["count"],
        },
        "candidate": {
            "provider": candidate["provider"],
            "overall_score": cand_agg["overall_score"],
            "count": cand_agg["count"],
        },
        "deltas": deltas,
        "overall_delta": overall_delta,
        "quality_claim": quality_claim,
        "conclusion": conclusion,
    }
