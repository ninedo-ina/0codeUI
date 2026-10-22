"""UI 语料清洗、校验、去重、切分与训练格式转换。

输入：一行一个 JSON 的原始语料（见 `training/corpus/schema.json`）。
输出：`train.jsonl` / `val.jsonl` / `test.jsonl`，每行是 chat messages 格式，
以及可复现的 `stats.json` 与 `rejects.jsonl`。

设计原则：
- 只依赖标准库，便于 CI / 无 GPU 机器运行；
- 所有校验失败都记录原因，不静默丢弃；
- 输出顺序、切分结果由 seed 决定，可复现；
- 敏感字段先脱敏，再做 PII 检查。
"""

from __future__ import annotations

import hashlib
import json
import random
import re
from dataclasses import dataclass
from html import escape
from html.parser import HTMLParser
from pathlib import Path
from typing import Any, Iterable, Mapping

from .prompts import SYSTEM_PROMPT, build_user_prompt

SCHEMA_VERSION = "1"
MAX_HTML_BYTES = 2 * 1024 * 1024
MIN_HTML_BYTES = 100
MAX_BUSINESS_DATA_BYTES = 64 * 1024
ALLOWED_SCENES = (
    "ORDER_DETAIL_PAGE",
    "ORDER_LIST_PAGE",
    "PRODUCT_DETAIL_PAGE",
)
ALLOWED_THEMES = ("light", "dark")

ID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{2,63}$")
EMAIL_RE = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
CN_MOBILE_RE = re.compile(r"(?<!\d)1[3-9]\d{9}(?!\d)")
ID_CARD_RE = re.compile(r"(?<!\d)\d{17}[\dXx](?!\d)")
SAFE_EMAIL_DOMAINS = ("example.com", "example.org", "example.net", "invalid")

FORBIDDEN_MARKERS = (
    "<script src",
    "<link",
    "@import",
    "url(http",
    "url(//",
    "<iframe",
    "<object",
    "<embed",
    "javascript:",
)
EXTERNAL_URL_RE = re.compile(
    r"""(?:src|href)\s*=\s*["']\s*(?:https?:)?//""", re.IGNORECASE
)

SENSITIVE_KEYS = {
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "accesstoken",
    "refreshtoken",
    "apikey",
    "authorization",
    "cookie",
    "session",
    "sessionid",
    "csrf",
    "privatekey",
    "clientsecret",
    "idcard",
    "identitycard",
    "bankcard",
    "cardnumber",
    "cvv",
    "cvc",
    "ssn",
    "passport",
    "phone",
    "mobile",
    "email",
}
SENSITIVE_SUFFIXES = ("password", "secret", "token", "apikey")


class CorpusError(ValueError):
    """语料整体不可用（目录为空、参数非法等）。"""


@dataclass
class ValidationResult:
    """单条语料的清洗结果。"""

    sample: dict[str, Any] | None
    errors: list[str]
    warnings: list[str]

    @property
    def ok(self) -> bool:
        return self.sample is not None and not self.errors


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def extract_html(value: str) -> str:
    """从模型输出 / 语料中提取单文件 HTML，容忍 Markdown 围栏与前后说明。"""
    text = value.strip().lstrip("﻿")
    if text.startswith("```"):
        first_newline = text.find("\n")
        if first_newline != -1:
            text = text[first_newline + 1 :]
        if text.rstrip().endswith("```"):
            text = text.rstrip()[:-3]
    lower = text.lower()
    start = lower.find("<!doctype")
    if start > 0:
        text = text[start:]
    end = text.lower().rfind("</html>")
    if end != -1:
        text = text[: end + len("</html>")]
    return text.strip()


def normalize_html(value: str) -> str:
    """统一换行、去行尾空白、压缩连续空行，保持可渲染内容不变。"""
    text = value.replace("\r\n", "\n").replace("\r", "\n").lstrip("﻿")
    lines = [line.rstrip() for line in text.split("\n")]
    normalized: list[str] = []
    blank = False
    for line in lines:
        if line == "":
            if not blank:
                normalized.append(line)
            blank = True
        else:
            normalized.append(line)
            blank = False
    return "\n".join(normalized).strip()


class _BalanceParser(HTMLParser):
    """轻量标签闭合检查；可选闭合标签不报错，避免误伤合法 HTML。"""

    VOID = {
        "area",
        "base",
        "br",
        "col",
        "embed",
        "hr",
        "img",
        "input",
        "link",
        "meta",
        "param",
        "source",
        "track",
        "wbr",
    }
    OPTIONAL_CLOSE = {
        "p",
        "li",
        "dt",
        "dd",
        "tr",
        "td",
        "th",
        "thead",
        "tbody",
        "tfoot",
        "option",
    }

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.stack: list[str] = []
        self.issues: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag not in self.VOID:
            self.stack.append(tag)

    def handle_startendtag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        return None

    def handle_endtag(self, tag: str) -> None:
        if tag in self.VOID:
            return
        if tag not in self.stack:
            if tag not in self.OPTIONAL_CLOSE:
                self.issues.append(f"unexpected closing tag </{tag}>")
            return
        while self.stack:
            top = self.stack.pop()
            if top == tag:
                break
            if top not in self.OPTIONAL_CLOSE:
                self.issues.append(f"unclosed tag <{top}>")

    def finish(self) -> list[str]:
        for tag in self.stack:
            if tag not in self.OPTIONAL_CLOSE:
                self.issues.append(f"unclosed tag <{tag}>")
        return self.issues


def html_structure_issues(html: str) -> list[str]:
    """检查单文件 HTML 的必备结构与标签闭合。"""
    issues: list[str] = []
    lower = html.lower().lstrip()
    if not lower.startswith("<!doctype html"):
        issues.append("missing <!DOCTYPE html>")
    for tag in ("html", "head", "body"):
        if f"<{tag}" not in lower:
            issues.append(f"missing <{tag}>")
        if f"</{tag}>" not in lower:
            issues.append(f"missing </{tag}>")
    if issues:
        return issues
    parser = _BalanceParser()
    try:
        parser.feed(html)
        parser.close()
    except Exception as exc:  # HTMLParser 极少抛错，保留原因便于定位
        issues.append(f"html parse error: {exc}")
        return issues
    issues.extend(parser.finish())
    return issues


def single_file_issues(html: str) -> list[str]:
    """检查单文件约束：不允许外链脚本 / 样式 / 字体 / 图片 / 内嵌框架。"""
    lower = html.lower()
    issues = [f"forbidden marker: {marker}" for marker in FORBIDDEN_MARKERS if marker in lower]
    if EXTERNAL_URL_RE.search(html):
        issues.append("external src/href URL is not allowed")
    return issues


def style_issues(style: Any) -> list[str]:
    if not isinstance(style, dict):
        return ["style must be an object"]
    issues: list[str] = []
    theme = style.get("theme")
    if theme not in ALLOWED_THEMES:
        issues.append("style.theme must be light or dark")
    ranges = {
        "density": (0.0, 1.0),
        "font_scale": (0.8, 1.4),
        "radius": (0.0, 24.0),
        "spacing": (0.75, 1.5),
    }
    for key, (low, high) in ranges.items():
        if key not in style:
            issues.append(f"style.{key} is required")
            continue
        value = style[key]
        if not isinstance(value, (int, float)) or isinstance(value, bool):
            issues.append(f"style.{key} must be a number")
        elif not low <= float(value) <= high:
            issues.append(f"style.{key} out of range [{low}, {high}]")
    unknown = set(style) - {"theme", "density", "font_scale", "radius", "spacing"}
    if unknown:
        issues.append(f"unknown style fields: {sorted(unknown)}")
    return issues


def pii_hits(text: str) -> list[str]:
    """检测明显未脱敏的邮箱 / 手机号 / 身份证号。"""
    hits: list[str] = []
    for match in EMAIL_RE.finditer(text):
        domain = match.group(0).split("@", 1)[1].lower()
        if not any(domain == safe or domain.endswith("." + safe) for safe in SAFE_EMAIL_DOMAINS):
            hits.append(f"email: {match.group(0)}")
    hits.extend(f"mobile: {match.group(0)}" for match in CN_MOBILE_RE.finditer(text))
    hits.extend(f"id_card: {match.group(0)}" for match in ID_CARD_RE.finditer(text))
    return hits


def _normalize_key(key: str) -> str:
    return re.sub(r"[_\-]", "", key).lower()


def is_sensitive_key(key: str) -> bool:
    normalized = _normalize_key(key)
    return normalized in SENSITIVE_KEYS or normalized.endswith(SENSITIVE_SUFFIXES)


def redact_sensitive(value: Any, path: str = "business_data") -> tuple[Any, list[str]]:
    """递归脱敏敏感字段，返回新对象与已脱敏路径（不修改入参）。"""
    if isinstance(value, dict):
        result: dict[str, Any] = {}
        paths: list[str] = []
        for key in sorted(value):
            child_path = f"{path}.{key}"
            if is_sensitive_key(str(key)):
                result[key] = "[REDACTED]"
                paths.append(child_path)
            else:
                child, child_paths = redact_sensitive(value[key], child_path)
                result[key] = child
                paths.extend(child_paths)
        return result, paths
    if isinstance(value, list):
        result_list: list[Any] = []
        paths = []
        for index, item in enumerate(value):
            child, child_paths = redact_sensitive(item, f"{path}[{index}]")
            result_list.append(child)
            paths.extend(child_paths)
        return result_list, paths
    return value, []


def _control_chars(value: str) -> bool:
    return any(ord(ch) < 0x20 and ch not in "\n\t" for ch in value)


def validate_sample(raw: Any) -> ValidationResult:
    """校验并清洗单条语料；返回新对象，不修改输入。"""
    errors: list[str] = []
    warnings: list[str] = []
    if not isinstance(raw, dict):
        return ValidationResult(None, ["sample must be a JSON object"], warnings)

    sample_id = raw.get("id")
    if not isinstance(sample_id, str) or not ID_RE.match(sample_id):
        errors.append("id must match ^[a-z0-9][a-z0-9._-]{2,63}$")
    scene = raw.get("scene_code")
    if scene not in ALLOWED_SCENES:
        errors.append(f"scene_code must be one of {list(ALLOWED_SCENES)}")
    instruction = raw.get("instruction")
    if not isinstance(instruction, str) or not 8 <= len(instruction.strip()) <= 2000:
        errors.append("instruction length must be 8..2000")
    elif _control_chars(instruction):
        errors.append("instruction contains control characters")

    html_value = raw.get("output_html")
    html = ""
    if not isinstance(html_value, str):
        errors.append("output_html must be a string")
    else:
        html = normalize_html(extract_html(html_value))
        if not MIN_HTML_BYTES <= len(html.encode("utf-8")) <= MAX_HTML_BYTES:
            errors.append(f"output_html size must be {MIN_HTML_BYTES}..{MAX_HTML_BYTES} bytes")
        errors.extend(html_structure_issues(html))
        errors.extend(single_file_issues(html))

    style = raw.get("style")
    errors.extend(style_issues(style))

    business_data = raw.get("business_data")
    redacted_paths: list[str] = []
    if business_data is not None:
        if not isinstance(business_data, dict):
            errors.append("business_data must be an object")
        else:
            business_data, redacted_paths = redact_sensitive(business_data)
            encoded = json.dumps(business_data, ensure_ascii=False, sort_keys=True)
            if len(encoded.encode("utf-8")) > MAX_BUSINESS_DATA_BYTES:
                errors.append(f"business_data exceeds {MAX_BUSINESS_DATA_BYTES} bytes")

    for field in ("instruction", "output_html"):
        value = raw.get(field)
        if isinstance(value, str):
            errors.extend(f"{field} {hit}" for hit in pii_hits(value))
    if isinstance(business_data, dict):
        errors.extend(
            f"business_data {hit}"
            for hit in pii_hits(json.dumps(business_data, ensure_ascii=False))
        )

    if redacted_paths:
        warnings.append(f"redacted {len(redacted_paths)} sensitive field(s)")

    if errors:
        return ValidationResult(None, sorted(set(errors)), warnings)

    cleaned = {
        "id": sample_id,
        "scene_code": scene,
        "instruction": instruction.strip(),
        "style": {
            "theme": style["theme"],
            "density": float(style["density"]),
            "font_scale": float(style["font_scale"]),
            "radius": float(style["radius"]),
            "spacing": float(style["spacing"]),
        },
        "output_html": html,
    }
    if business_data is not None:
        cleaned["business_data"] = business_data
    for optional in ("license", "source", "notes"):
        if isinstance(raw.get(optional), str):
            cleaned[optional] = raw[optional]
    return ValidationResult(cleaned, [], warnings)


def sample_hash(sample: Mapping[str, Any]) -> str:
    payload = f"{sample['scene_code']}\n{sample['output_html']}"
    return sha256_text(payload)


def load_samples(paths: Iterable[str | Path]) -> list[tuple[dict[str, Any], str]]:
    """读取文件或目录下的 .jsonl / .json 语料，返回 (sample, source) 列表。"""
    files: list[Path] = []
    for raw_path in paths:
        path = Path(raw_path)
        if path.is_dir():
            files.extend(sorted(path.glob("*.jsonl")))
            files.extend(sorted(path.glob("*.json")))
        elif path.is_file():
            files.append(path)
        else:
            raise CorpusError(f"input path does not exist: {path}")
    if not files:
        raise CorpusError("no corpus files found")

    samples: list[tuple[dict[str, Any], str]] = []
    for path in files:
        text = path.read_text(encoding="utf-8")
        if path.suffix == ".json":
            payload = json.loads(text)
            if isinstance(payload, dict) and "samples" in payload:
                payload = payload["samples"]
            if not isinstance(payload, list):
                raise CorpusError(f"{path}: JSON must be an array or {{samples: [...]}}")
            samples.extend((item, str(path)) for item in payload)
            continue
        for line_number, line in enumerate(text.splitlines(), 1):
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            try:
                samples.append((json.loads(stripped), f"{path}:{line_number}"))
            except json.JSONDecodeError as exc:
                raise CorpusError(f"{path}:{line_number}: invalid JSON: {exc}") from exc
    return samples


def split_samples(
    samples: list[dict[str, Any]],
    seed: int,
    train_ratio: float = 0.9,
    val_ratio: float = 0.05,
) -> dict[str, list[dict[str, Any]]]:
    """按 seed 确定性切分；样本数足够时保证 val / test 至少各 1 条。"""
    if not 0 < train_ratio < 1 or not 0 <= val_ratio < 1:
        raise CorpusError("train_ratio must be in (0,1), val_ratio in [0,1)")
    if train_ratio + val_ratio >= 1:
        raise CorpusError("train_ratio + val_ratio must be < 1")
    ordered = sorted(samples, key=lambda item: item["id"])
    random.Random(seed).shuffle(ordered)
    total = len(ordered)
    if total < 3:
        return {"train": ordered, "val": [], "test": []}
    n_val = max(1, round(total * val_ratio))
    n_test = max(1, total - round(total * train_ratio) - n_val)
    n_train = total - n_val - n_test
    if n_train < 1:
        n_train, n_val, n_test = total - 2, 1, 1
    return {
        "train": ordered[:n_train],
        "val": ordered[n_train : n_train + n_val],
        "test": ordered[n_train + n_val :],
    }


def to_chat_record(sample: Mapping[str, Any]) -> dict[str, Any]:
    """转换为训练用的 chat messages 记录。"""
    record = {
        "id": sample["id"],
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": build_user_prompt(sample)},
            {"role": "assistant", "content": sample["output_html"]},
        ],
        "meta": {
            "scene_code": sample["scene_code"],
            "style": sample["style"],
            "source": sample.get("source", "unknown"),
            "license": sample.get("license", "unknown"),
        },
    }
    return record


def write_jsonl(path: Path, records: Iterable[Mapping[str, Any]]) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    with path.open("w", encoding="utf-8", newline="\n") as handle:
        for record in records:
            line = json.dumps(record, ensure_ascii=False, sort_keys=True)
            handle.write(line + "\n")
            digest.update((line + "\n").encode("utf-8"))
    return digest.hexdigest()


def prepare_dataset(
    input_paths: Iterable[str | Path],
    output_dir: str | Path,
    seed: int = 42,
    train_ratio: float = 0.9,
    val_ratio: float = 0.05,
    max_samples: int | None = None,
) -> dict[str, Any]:
    """完整清洗流程：读取 → 校验 → 去重 → 切分 → 写训练文件与统计。"""
    raw_samples = load_samples(input_paths)
    accepted: list[dict[str, Any]] = []
    rejects: list[dict[str, Any]] = []
    seen: dict[str, str] = {}
    duplicates = 0
    redacted = 0

    for raw, source in raw_samples:
        result = validate_sample(raw)
        if not result.ok:
            rejects.append(
                {
                    "source": source,
                    "id": raw.get("id") if isinstance(raw, dict) else None,
                    "errors": result.errors,
                }
            )
            continue
        sample = result.sample
        assert sample is not None
        if result.warnings:
            redacted += 1
        digest = sample_hash(sample)
        if digest in seen:
            duplicates += 1
            continue
        seen[digest] = sample["id"]
        accepted.append(sample)
        if max_samples is not None and len(accepted) >= max_samples:
            break

    if not accepted:
        raise CorpusError("no valid samples after cleaning")

    splits = split_samples(accepted, seed, train_ratio, val_ratio)
    out = Path(output_dir)
    out.mkdir(parents=True, exist_ok=True)
    hashes = {
        name: write_jsonl(out / f"{name}.jsonl", (to_chat_record(item) for item in items))
        for name, items in splits.items()
    }
    (out / "rejects.jsonl").write_text(
        "".join(json.dumps(item, ensure_ascii=False, sort_keys=True) + "\n" for item in rejects),
        encoding="utf-8",
    )

    scene_counts: dict[str, int] = {}
    theme_counts: dict[str, int] = {}
    for sample in accepted:
        scene_counts[sample["scene_code"]] = scene_counts.get(sample["scene_code"], 0) + 1
        theme = sample["style"]["theme"]
        theme_counts[theme] = theme_counts.get(theme, 0) + 1

    stats = {
        "schema_version": SCHEMA_VERSION,
        "seed": seed,
        "input_files": sorted({source.rsplit(":", 1)[0] for _, source in raw_samples}),
        "total_read": len(raw_samples),
        "accepted": len(accepted),
        "rejected": len(rejects),
        "duplicates": duplicates,
        "samples_with_redactions": redacted,
        "splits": {name: len(items) for name, items in splits.items()},
        "scene_counts": dict(sorted(scene_counts.items())),
        "theme_counts": dict(sorted(theme_counts.items())),
        "html_bytes": sum(len(item["output_html"].encode("utf-8")) for item in accepted),
        "sha256": hashes,
    }
    (out / "stats.json").write_text(
        json.dumps(stats, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return stats


# 让调用方无需重复导入 escape（模板渲染脚本也会用到）。
__all__ = [
    "ALLOWED_SCENES",
    "ALLOWED_THEMES",
    "CorpusError",
    "MAX_HTML_BYTES",
    "SCHEMA_VERSION",
    "escape",
    "extract_html",
    "html_structure_issues",
    "load_samples",
    "normalize_html",
    "pii_hits",
    "prepare_dataset",
    "redact_sensitive",
    "sample_hash",
    "sha256_file",
    "sha256_text",
    "single_file_issues",
    "split_samples",
    "style_issues",
    "to_chat_record",
    "validate_sample",
]
