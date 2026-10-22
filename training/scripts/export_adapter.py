#!/usr/bin/env python3
"""校验 / 安装 LoRA 适配器，并输出 vLLM 加载参数。

示例：
    python3 training/scripts/export_adapter.py \
      --adapter-dir training/artifacts/qwen2.5-coder-7b-ui-lora \
      --served-name qwen2.5-coder-7b-ui-lora \
      --eval-report training/artifacts/eval/adapter/eval_report.json \
      --print-vllm
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path
from typing import Any

TRAINING_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TRAINING_DIR))

from intimeui_training.manifest import (  # noqa: E402
    MANIFEST_NAME,
    ManifestError,
    build_manifest,
    read_manifest,
    verify_manifest,
    write_manifest,
)


def _base_model_from_adapter(adapter_dir: Path) -> str | None:
    config_path = adapter_dir / "adapter_config.json"
    if not config_path.is_file():
        return None
    payload = json.loads(config_path.read_text(encoding="utf-8"))
    value = payload.get("base_model_name_or_path")
    return value if isinstance(value, str) and value else None


def _evaluation_from_report(path: Path) -> dict[str, Any]:
    report = json.loads(path.read_text(encoding="utf-8"))
    return {
        "status": "complete",
        "report": str(path),
        "quality_claim": bool(report.get("quality_claim")),
        "aggregate": report.get("aggregate", {}),
    }


def ensure_manifest(
    adapter_dir: Path,
    adapter_name: str,
    served_name: str,
    base_model: str | None,
    method: str | None,
    eval_report: Path | None,
) -> dict[str, Any]:
    manifest_path = adapter_dir / MANIFEST_NAME
    if manifest_path.is_file():
        manifest = read_manifest(adapter_dir)
    else:
        resolved_base = base_model or _base_model_from_adapter(adapter_dir)
        if not resolved_base:
            raise ManifestError("base model unknown: pass --base-model or provide adapter_config.json")
        manifest = build_manifest(
            adapter_dir,
            adapter_name=adapter_name,
            base_model=resolved_base,
            method=method or "qlora",
            served_model_name=served_name,
        )
    if eval_report:
        manifest["evaluation"] = _evaluation_from_report(eval_report)
    manifest["adapter_name"] = adapter_name
    manifest["served_model_name"] = served_name
    write_manifest(manifest, adapter_dir)
    return manifest


def _vllm_payload(manifest: dict[str, Any], container_dir: str) -> dict[str, Any]:
    name = manifest["served_model_name"]
    lora_modules = f"{name}={container_dir.rstrip('/')}/{name}"
    max_rank = int(manifest.get("vllm", {}).get("max_lora_rank", 32))
    return {
        "enable_lora": True,
        "lora_modules": lora_modules,
        "max_lora_rank": max_rank,
        "env": {"VLLM_ALLOW_RUNTIME_LORA_UPDATING": "1"},
        "serve_args": [
            "--enable-lora",
            "--lora-modules",
            lora_modules,
            "--max-lora-rank",
            str(max_rank),
        ],
        "hot_swap": {
            "load": f"POST /v1/load_lora_adapter {{\"lora_name\":\"{name}\",\"lora_path\":\"{container_dir.rstrip('/')}/{name}\"}}",
            "unload": f"POST /v1/unload_lora_adapter {{\"lora_name\":\"{name}\"}}",
        },
    }


def _ignore_checkpoints(directory: str, names: list[str]) -> set[str]:
    del directory
    return {
        name
        for name in names
        if name.startswith(("checkpoint-", "__pycache__"))
    }


def install_adapter(adapter_dir: Path, install_dir: Path, name: str, force: bool) -> Path:
    source = adapter_dir.resolve()
    target = (install_dir / name).resolve()
    if target == source or source in target.parents or target in source.parents:
        raise ManifestError("install target must be outside the adapter directory")
    if target.exists():
        if not force:
            raise ManifestError(f"install target exists: {target} (use --force to replace)")
        shutil.rmtree(target)
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(adapter_dir, target, ignore=_ignore_checkpoints)
    return target


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter-dir", type=Path, required=True)
    parser.add_argument("--adapter-name", default=None)
    parser.add_argument("--served-name", default=None)
    parser.add_argument("--base-model", default=None)
    parser.add_argument("--method", choices=("lora", "qlora"), default=None)
    parser.add_argument("--eval-report", type=Path, default=None)
    parser.add_argument("--install-dir", type=Path, default=None)
    parser.add_argument("--container-dir", default="/adapters")
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--print-vllm", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    adapter_dir = args.adapter_dir.resolve()
    adapter_name = args.adapter_name or adapter_dir.name
    served_name = args.served_name or adapter_name
    try:
        if args.verify_only:
            issues = verify_manifest(adapter_dir)
            if issues:
                print(json.dumps({"ok": False, "issues": issues}, ensure_ascii=False, indent=2))
                return 1
            manifest = read_manifest(adapter_dir)
        else:
            manifest = ensure_manifest(
                adapter_dir,
                adapter_name,
                served_name,
                args.base_model,
                args.method,
                args.eval_report,
            )
            issues = verify_manifest(adapter_dir)
            if issues:
                raise ManifestError(f"manifest verification failed: {issues}")
        installed = None
        if args.install_dir:
            installed = install_adapter(adapter_dir, args.install_dir.resolve(), served_name, args.force)
            issues = verify_manifest(installed)
            if issues:
                raise ManifestError(f"installed adapter verification failed: {issues}")
    except (ManifestError, OSError, json.JSONDecodeError) as exc:
        print(f"export_adapter failed: {exc}", file=sys.stderr)
        return 1
    result: dict[str, Any] = {
        "ok": True,
        "adapter_dir": str(adapter_dir),
        "installed": str(installed) if installed else None,
        "manifest": manifest,
    }
    if args.print_vllm:
        result["vllm"] = _vllm_payload(manifest, args.container_dir)
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
