from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path

from intimeui_training.corpus import (
    extract_html,
    load_samples,
    prepare_dataset,
    redact_sensitive,
    split_samples,
    to_chat_record,
    validate_sample,
)
from intimeui_training.prompts import SYSTEM_PROMPT

TRAINING_DIR = Path(__file__).resolve().parents[1]
SEED_PATH = TRAINING_DIR / "corpus" / "seed" / "seed_corpus.jsonl"
PROMPT_RS = TRAINING_DIR.parent / "server" / "src" / "model" / "prompt.rs"


def first_sample() -> dict:
    samples = load_samples([SEED_PATH])
    result = validate_sample(samples[0][0])
    assert result.ok and result.sample is not None
    return result.sample


class CorpusTests(unittest.TestCase):
    def test_system_prompt_matches_rust_source(self) -> None:
        source = PROMPT_RS.read_text(encoding="utf-8")
        start = source.index('pub const SYSTEM_PROMPT: &str = r#"') + len(
            'pub const SYSTEM_PROMPT: &str = r#"'
        )
        end = source.index('"#;', start)
        self.assertEqual(source[start:end], SYSTEM_PROMPT)

    def test_extract_html_strips_fence_and_prose(self) -> None:
        raw = "说明文字\n```html\n<!DOCTYPE html><html><head></head><body>x</body></html>\n```\n"
        extracted = extract_html(raw)
        self.assertTrue(extracted.startswith("<!DOCTYPE html>"))
        self.assertTrue(extracted.endswith("</html>"))

    def test_validate_seed_sample(self) -> None:
        result = validate_sample(first_sample())
        self.assertTrue(result.ok, result.errors)
        self.assertEqual(result.errors, [])

    def test_validate_rejects_external_script(self) -> None:
        sample = copy.deepcopy(first_sample())
        sample["output_html"] = sample["output_html"].replace(
            "</body>", '<script src="https://evil.example/x.js"></script></body>'
        )
        result = validate_sample(sample)
        self.assertFalse(result.ok)
        self.assertTrue(any("forbidden marker" in item for item in result.errors))

    def test_validate_rejects_pii(self) -> None:
        sample = copy.deepcopy(first_sample())
        sample["instruction"] = "请联系 13800138000 生成订单详情页"
        result = validate_sample(sample)
        self.assertFalse(result.ok)
        self.assertTrue(any("mobile" in item for item in result.errors))

    def test_redact_sensitive_is_immutable(self) -> None:
        original = {"user": {"password": "secret", "name": "张三"}, "items": [{"token": "abc"}]}
        frozen = copy.deepcopy(original)
        redacted, paths = redact_sensitive(original)
        self.assertEqual(original, frozen)
        self.assertEqual(redacted["user"]["password"], "[REDACTED]")
        self.assertEqual(redacted["items"][0]["token"], "[REDACTED]")
        self.assertEqual(len(paths), 2)

    def test_split_is_deterministic_and_has_all_splits(self) -> None:
        samples = [{"id": f"sample-{index:03d}"} for index in range(12)]
        first = split_samples(samples, seed=42)
        second = split_samples(samples, seed=42)
        self.assertEqual(
            [item["id"] for item in first["train"]],
            [item["id"] for item in second["train"]],
        )
        self.assertEqual(len(first["train"]), 10)
        self.assertEqual(len(first["val"]), 1)
        self.assertEqual(len(first["test"]), 1)

    def test_prepare_dataset_end_to_end(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            stats = prepare_dataset([TRAINING_DIR / "corpus" / "seed"], temporary)
            self.assertEqual(stats["accepted"], 12)
            self.assertEqual(stats["rejected"], 0)
            self.assertEqual(stats["duplicates"], 0)
            self.assertEqual(stats["splits"], {"train": 10, "val": 1, "test": 1})
            self.assertEqual(stats["theme_counts"], {"dark": 6, "light": 6})
            train_line = (Path(temporary) / "train.jsonl").read_text(encoding="utf-8").splitlines()[0]
            record = json.loads(train_line)
            self.assertEqual(record["messages"][0]["content"], SYSTEM_PROMPT)

    def test_to_chat_record_shape(self) -> None:
        record = to_chat_record(first_sample())
        self.assertEqual([item["role"] for item in record["messages"]], ["system", "user", "assistant"])
        self.assertIn("场景编码：", record["messages"][1]["content"])
        self.assertIn("业务数据（JSON）：", record["messages"][1]["content"])


if __name__ == "__main__":
    unittest.main()
