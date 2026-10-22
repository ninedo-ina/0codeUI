from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from intimeui_training.manifest import (
    build_manifest,
    read_manifest,
    vllm_lora_modules,
    verify_manifest,
    write_manifest,
)


def make_adapter(directory: Path) -> None:
    (directory / "adapter_config.json").write_text(
        json.dumps({"base_model_name_or_path": "Qwen/Qwen2.5-Coder-7B-Instruct"}),
        encoding="utf-8",
    )
    (directory / "adapter_model.safetensors").write_bytes(b"fake-weights")


class ManifestTests(unittest.TestCase):
    def test_build_and_verify(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            adapter = Path(temporary)
            make_adapter(adapter)
            manifest = build_manifest(
                adapter,
                adapter_name="ui-lora",
                base_model="Qwen/Qwen2.5-Coder-7B-Instruct",
                method="qlora",
                served_model_name="ui-lora",
            )
            write_manifest(manifest, adapter)
            self.assertEqual(verify_manifest(adapter), [])
            loaded = read_manifest(adapter)
            self.assertEqual(loaded["served_model_name"], "ui-lora")
            self.assertIn("adapter_model.safetensors", loaded["files"])

    def test_tamper_detection(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            adapter = Path(temporary)
            make_adapter(adapter)
            write_manifest(
                build_manifest(
                    adapter,
                    adapter_name="ui-lora",
                    base_model="base",
                    method="lora",
                ),
                adapter,
            )
            (adapter / "adapter_model.safetensors").write_bytes(b"tampered")
            issues = verify_manifest(adapter)
            self.assertTrue(any("sha256 mismatch" in item for item in issues))

    def test_checkpoints_are_not_fingerprinted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            adapter = Path(temporary)
            make_adapter(adapter)
            checkpoint = adapter / "checkpoint-100"
            checkpoint.mkdir()
            (checkpoint / "optimizer.pt").write_bytes(b"large")
            write_manifest(
                build_manifest(
                    adapter,
                    adapter_name="ui-lora",
                    base_model="base",
                    method="qlora",
                ),
                adapter,
            )
            issues = verify_manifest(adapter)
            self.assertEqual(issues, [])

    def test_vllm_lora_modules(self) -> None:
        manifest = {"adapter_name": "ui-lora", "served_model_name": "ui-lora-v1"}
        self.assertEqual(vllm_lora_modules(manifest, "/adapters/"), "ui-lora-v1=/adapters/ui-lora-v1")


if __name__ == "__main__":
    unittest.main()
