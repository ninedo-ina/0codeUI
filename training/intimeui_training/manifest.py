"""LoRA 适配器清单：校验、指纹、vLLM 加载参数与热插拔元数据。

适配器目录必须包含 PEFT 产物：
- `adapter_config.json`
- `adapter_model.safetensors`（或旧版 `adapter_model.bin`）

清单 `adapter_manifest.json` 是推理侧识别适配器版本、基线模型与评测结果的
唯一入口；`export_adapter.py --verify-only` 可在部署前复核文件完整性。
"""

from __future__ import annotations

import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping

MANIFEST_NAME = "adapter_manifest.json"
REQUIRED_FILES = ("adapter_config.json",)
WEIGHT_FILES = ("adapter_model.safetensors", "adapter_model.bin")
SCHEMA_VERSION = "1"
IGNORED_DIR_PREFIXES = ("checkpoint-", "__pycache__")


class ManifestError(ValueError):
    """适配器目录或清单不合法。"""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def find_weight_file(adapter_dir: Path) -> Path:
    for name in WEIGHT_FILES:
        candidate = adapter_dir / name
        if candidate.is_file():
            return candidate
    raise ManifestError(
        f"adapter weights not found in {adapter_dir} (expected one of {WEIGHT_FILES})"
    )


def _ignored_relative(relative: Path) -> bool:
    return any(part.startswith(IGNORED_DIR_PREFIXES) for part in relative.parts)


def collect_files(adapter_dir: Path) -> dict[str, dict[str, Any]]:
    """收集适配器目录下除清单 / checkpoint 外的文件指纹。"""
    if not adapter_dir.is_dir():
        raise ManifestError(f"adapter directory does not exist: {adapter_dir}")
    for name in REQUIRED_FILES:
        if not (adapter_dir / name).is_file():
            raise ManifestError(f"missing required adapter file: {name}")
    find_weight_file(adapter_dir)
    files: dict[str, dict[str, Any]] = {}
    for path in sorted(adapter_dir.rglob("*")):
        relative_path = path.relative_to(adapter_dir)
        if not path.is_file() or path.name == MANIFEST_NAME or _ignored_relative(relative_path):
            continue
        files[relative_path.as_posix()] = {
            "sha256": sha256_file(path),
            "bytes": path.stat().st_size,
        }
    return files


def build_manifest(
    adapter_dir: str | Path,
    adapter_name: str,
    base_model: str,
    method: str,
    served_model_name: str | None = None,
    training: Mapping[str, Any] | None = None,
    evaluation: Mapping[str, Any] | None = None,
    notes: list[str] | None = None,
) -> dict[str, Any]:
    directory = Path(adapter_dir)
    files = collect_files(directory)
    weight = find_weight_file(directory)
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "adapter_name": adapter_name,
        "served_model_name": served_model_name or adapter_name,
        "base_model": base_model,
        "method": method,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "files": files,
        "weights": weight.name,
        "vllm": {
            "enable_lora": True,
            "lora_modules": f"{served_model_name or adapter_name}={{adapter_dir}}",
            "max_lora_rank": 32,
        },
        "training": dict(training or {}),
        "evaluation": dict(evaluation or {}),
        "notes": list(notes or []),
    }
    return manifest


def write_manifest(manifest: Mapping[str, Any], adapter_dir: str | Path) -> Path:
    path = Path(adapter_dir) / MANIFEST_NAME
    path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return path


def read_manifest(adapter_dir: str | Path) -> dict[str, Any]:
    path = Path(adapter_dir) / MANIFEST_NAME
    if not path.is_file():
        raise ManifestError(f"manifest not found: {path}")
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ManifestError("manifest must be a JSON object")
    return payload


def verify_manifest(adapter_dir: str | Path) -> list[str]:
    """按清单复核文件指纹，返回问题列表（空表示通过）。"""
    directory = Path(adapter_dir)
    manifest = read_manifest(directory)
    issues: list[str] = []
    files = manifest.get("files")
    if not isinstance(files, dict) or not files:
        return ["manifest.files is empty"]
    for relative, expected in files.items():
        path = directory / relative
        if not path.is_file():
            issues.append(f"missing file: {relative}")
            continue
        actual = sha256_file(path)
        expected_hash = expected.get("sha256") if isinstance(expected, dict) else None
        if actual != expected_hash:
            issues.append(f"sha256 mismatch: {relative}")
    actual_files = {
        path.relative_to(directory).as_posix()
        for path in directory.rglob("*")
        if path.is_file()
        and path.name != MANIFEST_NAME
        and not _ignored_relative(path.relative_to(directory))
    }
    for extra in sorted(actual_files - set(files)):
        issues.append(f"unlisted file: {extra}")
    return issues


def vllm_lora_modules(manifest: Mapping[str, Any], container_dir: str) -> str:
    """生成 `--lora-modules` 参数值，路径使用容器内挂载目录。"""
    name = manifest.get("served_model_name") or manifest.get("adapter_name")
    return f"{name}={container_dir.rstrip('/')}/{name}"
