from __future__ import annotations

import unittest
from pathlib import Path

from intimeui_training.corpus import load_samples, validate_sample
from intimeui_training.eval import (
    compare_reports,
    evaluate_cases,
    evaluate_html,
    load_eval_cases,
    non_grayscale_colors,
    scene_coverage,
)
from intimeui_training.providers import MockProvider

TRAINING_DIR = Path(__file__).resolve().parents[1]


def seed_html() -> str:
    raw = load_samples([TRAINING_DIR / "corpus" / "seed" / "seed_corpus.jsonl"])[0][0]
    result = validate_sample(raw)
    assert result.sample is not None
    return result.sample["output_html"]


class EvalTests(unittest.TestCase):
    def test_seed_template_scores_full_marks(self) -> None:
        result = evaluate_html(seed_html(), ["订单详情", "订单号", "订单状态", "商品明细", "金额合计"])
        self.assertTrue(result["metrics"]["structure_valid"], result["issues"]["structure"])
        self.assertTrue(result["metrics"]["single_file"], result["issues"]["single_file"])
        self.assertTrue(result["metrics"]["style_consistent"], result["issues"]["style"])
        self.assertTrue(result["metrics"]["accessible"], result["issues"]["accessibility"])
        self.assertEqual(result["metrics"]["scene_coverage"], 1.0)

    def test_non_grayscale_detection(self) -> None:
        html = "<style>:root { --x: #ff0000; color: blue; }</style>"
        colors = non_grayscale_colors(html)
        self.assertIn("#ff0000", colors)
        self.assertIn("blue", colors)

    def test_scene_coverage_partial(self) -> None:
        self.assertEqual(scene_coverage("<h1>订单列表</h1>", ["订单列表", "订单号"]), 0.5)

    def test_mock_provider_is_not_quality_claim(self) -> None:
        cases = load_eval_cases(TRAINING_DIR / "eval" / "testset.jsonl")
        report = evaluate_cases(
            cases,
            MockProvider(),
            TRAINING_DIR / "eval" / "testset.jsonl",
        )
        self.assertFalse(report["quality_claim"])
        self.assertEqual(report["aggregate"]["count"], 6)
        self.assertEqual(report["aggregate"]["structure_valid_rate"], 1.0)
        self.assertEqual(report["aggregate"]["single_file_rate"], 1.0)

    def test_compare_reports_marks_plumbing_only_for_mock(self) -> None:
        base = {
            "provider": {"kind": "mock"},
            "aggregate": {"count": 6, "overall_score": 50.0, "structure_valid_rate": 0.5, "single_file_rate": 0.5, "style_consistency_rate": 0.5, "scene_coverage_rate": 0.5, "accessibility_rate": 0.5},
            "quality_claim": False,
        }
        candidate = {
            "provider": {"kind": "mock"},
            "aggregate": {"count": 6, "overall_score": 90.0, "structure_valid_rate": 0.9, "single_file_rate": 0.9, "style_consistency_rate": 0.9, "scene_coverage_rate": 0.9, "accessibility_rate": 0.9},
            "quality_claim": False,
        }
        comparison = compare_reports(base, candidate)
        self.assertEqual(comparison["conclusion"], "plumbing_only")
        self.assertEqual(comparison["overall_delta"], 40.0)

    def test_compare_reports_improved(self) -> None:
        aggregate = {"count": 6, "overall_score": 80.0, "structure_valid_rate": 0.8, "single_file_rate": 0.8, "style_consistency_rate": 0.8, "scene_coverage_rate": 0.8, "accessibility_rate": 0.8}
        base = {"provider": {}, "aggregate": aggregate, "quality_claim": True}
        candidate = {
            "provider": {},
            "aggregate": {**aggregate, "overall_score": 85.0},
            "quality_claim": True,
        }
        self.assertEqual(compare_reports(base, candidate)["conclusion"], "improved")


if __name__ == "__main__":
    unittest.main()
