"""训练配置加载与边界校验（不依赖 torch）。"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any, Mapping

REQUIRED_TOP_LEVEL = ("base_model", "method", "lora", "training")
ALLOWED_METHODS = ("lora", "qlora")


class ConfigError(ValueError):
    """配置不合法。"""


def load_config(path: str | Path) -> dict[str, Any]:
    payload = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ConfigError("config must be a JSON object")
    validate_config(payload)
    return payload


def config_sha256(config: Mapping[str, Any]) -> str:
    canonical = json.dumps(config, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def validate_config(config: Mapping[str, Any]) -> None:
    for key in REQUIRED_TOP_LEVEL:
        if key not in config:
            raise ConfigError(f"missing config field: {key}")
    if config["method"] not in ALLOWED_METHODS:
        raise ConfigError(f"method must be one of {ALLOWED_METHODS}")
    if not isinstance(config["base_model"], str) or not config["base_model"].strip():
        raise ConfigError("base_model must be a non-empty string")

    lora = config["lora"]
    if not isinstance(lora, dict):
        raise ConfigError("lora must be an object")
    rank = lora.get("r")
    alpha = lora.get("alpha")
    if not isinstance(rank, int) or not 1 <= rank <= 256:
        raise ConfigError("lora.r must be an integer in 1..256")
    if not isinstance(alpha, int) or not 1 <= alpha <= 512:
        raise ConfigError("lora.alpha must be an integer in 1..512")
    dropout = lora.get("dropout", 0.0)
    if not isinstance(dropout, (int, float)) or not 0 <= float(dropout) < 1:
        raise ConfigError("lora.dropout must be in [0,1)")
    targets = lora.get("target_modules")
    if not isinstance(targets, list) or not targets or not all(
        isinstance(item, str) and item for item in targets
    ):
        raise ConfigError("lora.target_modules must be a non-empty string array")

    training = config["training"]
    if not isinstance(training, dict):
        raise ConfigError("training must be an object")
    ranges = {
        "num_train_epochs": (0.0, 100.0, False),
        "per_device_train_batch_size": (1, 1024, True),
        "gradient_accumulation_steps": (1, 1024, True),
        "learning_rate": (0.0, 1.0, False),
        "max_seq_length": (128, 32768, True),
    }
    for key, (low, high, inclusive_low) in ranges.items():
        value = training.get(key)
        if not isinstance(value, (int, float)) or isinstance(value, bool):
            raise ConfigError(f"training.{key} must be a number")
        above_low = float(value) >= low if inclusive_low else float(value) > low
        if not above_low or float(value) > high:
            bracket = "[" if inclusive_low else "("
            raise ConfigError(f"training.{key} out of range {bracket}{low}, {high}]")
    if training.get("bf16") not in (True, False):
        raise ConfigError("training.bf16 must be boolean")
    if training.get("gradient_checkpointing") not in (True, False):
        raise ConfigError("training.gradient_checkpointing must be boolean")

    budget = config.get("vram_budget_gb")
    estimate = config.get("estimated_vram_gb")
    if budget is not None and estimate is not None:
        if float(estimate) > float(budget):
            raise ConfigError(
                f"estimated_vram_gb ({estimate}) exceeds vram_budget_gb ({budget})"
            )
