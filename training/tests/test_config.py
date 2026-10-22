from __future__ import annotations

import unittest
from pathlib import Path

from intimeui_training.config import ConfigError, load_config, validate_config

TRAINING_DIR = Path(__file__).resolve().parents[1]


class ConfigTests(unittest.TestCase):
    def test_both_configs_load(self) -> None:
        for name in ("lora_qwen2_5_coder_7b.json", "qlora_qwen2_5_coder_7b.json"):
            config = load_config(TRAINING_DIR / "configs" / name)
            self.assertIn(config["method"], ("lora", "qlora"))
            self.assertLessEqual(config["estimated_vram_gb"], config["vram_budget_gb"])

    def test_rejects_over_budget(self) -> None:
        config = load_config(TRAINING_DIR / "configs" / "qlora_qwen2_5_coder_7b.json")
        broken = dict(config)
        broken["estimated_vram_gb"] = 32
        with self.assertRaises(ConfigError):
            validate_config(broken)

    def test_rejects_unknown_method(self) -> None:
        config = load_config(TRAINING_DIR / "configs" / "qlora_qwen2_5_coder_7b.json")
        broken = dict(config)
        broken["method"] = "full"
        with self.assertRaises(ConfigError):
            validate_config(broken)

    def test_rejects_bad_lora_rank(self) -> None:
        config = load_config(TRAINING_DIR / "configs" / "qlora_qwen2_5_coder_7b.json")
        broken = {**config, "lora": {**config["lora"], "r": 0}}
        with self.assertRaises(ConfigError):
            validate_config(broken)


if __name__ == "__main__":
    unittest.main()
