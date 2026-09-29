"""Tests for raw route-field comparison; no models or providers are called."""

from __future__ import annotations

import contextlib
import copy
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from analyze_routes import ROUTE_FIELDS, analyze, main
from validate_dataset import CONDITIONS
import validate_report as vr


HERE = Path(__file__).parent
FIXTURE = json.loads((HERE / "dataset.json").read_text(encoding="utf-8"))
CONFIG = {
    "config_id": "synthetic-route-diff-v1",
    "route_profile_id": "local-test-profile",
    "base_prompt_sha256": "1" * 64,
    "toolset_sha256": "2" * 64,
    "generation_settings_sha256": "3" * 64,
    "verifier_id": "fixture-verifier",
    "verifier_version": "1.0",
}


def measured(value):
    return {
        "value": value,
        "evidence_ref": "runs/synthetic.jsonl",
        "unavailable_reason": None,
    }


def make_report():
    config_digest = vr.canonical_sha256(CONFIG)
    cells = []
    for case in FIXTURE["cases"]:
        gold = case["route_gold"]
        route = {field: copy.deepcopy(gold[field]) for field in ROUTE_FIELDS}
        for condition in CONDITIONS:
            cells.append(
                {
                    "case_id": case["id"],
                    "condition": condition,
                    "config_sha256": config_digest,
                    "route": copy.deepcopy(route),
                    "answer": "synthetic test answer",
                    "verifier_status": measured("pass"),
                    "telemetry": {
                        "opened_skill_ids": measured([]),
                        "performed_import_ids": measured([]),
                        "added_context_tokens": measured(0),
                        "latency_ms": measured(0),
                        "cost_microusd": measured(0),
                        "tool_calls": measured(0),
                    },
                }
            )
    return {
        "schema_version": "buzz-skill-use-report-v1",
        "claim_boundary": vr.CLAIM_BOUNDARY,
        "fixture": {
            "schema_version": FIXTURE["schema_version"],
            "payload_sha256": vr.canonical_sha256(FIXTURE),
        },
        "config": copy.deepcopy(CONFIG),
        "config_sha256": config_digest,
        "model_provider_identity": {
            "self_reported": {
                "value": {"provider": "synthetic", "model": "test", "revision": None},
                "source_ref": "runs/synthetic.jsonl",
                "unavailable_reason": None,
            },
            "observed": {
                "value": None,
                "source_ref": None,
                "unavailable_reason": "Synthetic test only; no model was run.",
            },
        },
        "cells": cells,
    }


class AnalyzeRoutesTests(unittest.TestCase):
    def test_complete_24_cell_matrix_matches_all_fixture_routes(self):
        report = make_report()
        self.assertEqual(len(report["cells"]), 24)
        self.assertEqual(analyze(report, FIXTURE, expected_config=CONFIG), [])

    def test_each_raw_route_field_mismatch_reports_exact_cell_and_field(self):
        replacement = {
            "route_action": "propose_create",
            "selected_skill_ids": ["exact_money"],
            "review_subject_ids": ["grounded_fields"],
            "target_skill_ids": ["timeline_normalize"],
            "ingest_decision": True,
        }
        for field in ROUTE_FIELDS[:-1]:
            with self.subTest(field=field):
                report = make_report()
                cell = report["cells"][0]
                expected = copy.deepcopy(cell["route"][field])
                cell["route"][field] = replacement[field]
                mismatches = analyze(report, FIXTURE, expected_config=CONFIG)
                self.assertEqual(
                    mismatches,
                    [
                        {
                            "case_id": cell["case_id"],
                            "condition": cell["condition"],
                            "field": field,
                            "expected": expected,
                            "actual": replacement[field],
                        }
                    ],
                )

    def test_ingest_decision_mismatch_is_reported_exactly(self):
        report = make_report()
        cell = next(
            item
            for item in report["cells"]
            if item["case_id"] == "one-off-addition" and item["condition"] == "no_skill"
        )
        cell["route"]["ingest_decision"] = True
        self.assertEqual(
            analyze(report, FIXTURE, expected_config=CONFIG),
            [
                {
                    "case_id": "one-off-addition",
                    "condition": "no_skill",
                    "field": "ingest_decision",
                    "expected": False,
                    "actual": True,
                }
            ],
        )

    def test_cli_prints_exact_case_condition_field_difference(self):
        report = make_report()
        report["cells"][0]["route"]["route_action"] = "propose_create"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipt.json"
            path.write_text(json.dumps(report), encoding="utf-8")
            config_path = self._write_config(directory)
            stdout, stderr = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                exit_code = main([str(path), "--expected-config", str(config_path)])
        self.assertEqual(exit_code, 0)
        self.assertEqual(stderr.getvalue(), "")
        self.assertIn(
            "case=one-off-addition condition=no_skill field=route_action "
            'expected="none" actual="propose_create"',
            stdout.getvalue(),
        )
        self.assertIn("not a score, grade, utility", stdout.getvalue())

    def test_cli_rejects_duplicate_key_json_before_comparison(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "malformed.json"
            path.write_text(
                '{"schema_version":"buzz-skill-use-report-v1",'
                '"schema_version":"buzz-skill-use-report-v1"}',
                encoding="utf-8",
            )
            stdout, stderr = io.StringIO(), io.StringIO()
            with patch("analyze_routes._compare_validated_routes") as compare:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    exit_code = main([str(path)])
        self.assertEqual(exit_code, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("duplicate JSON object key", stderr.getvalue())
        compare.assert_not_called()

    def test_cli_rejects_syntax_malformed_json_before_comparison(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "syntax-malformed.json"
            path.write_text('{"schema_version":', encoding="utf-8")
            stdout, stderr = io.StringIO(), io.StringIO()
            with patch("analyze_routes._compare_validated_routes") as compare:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    exit_code = main([str(path)])
        self.assertEqual(exit_code, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("route comparison stopped:", stderr.getvalue())
        compare.assert_not_called()

    def test_cli_rejects_unpinned_receipt_before_comparison(self):
        report = make_report()
        report["fixture"]["payload_sha256"] = "0" * 64
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "unpinned.json"
            path.write_text(json.dumps(report), encoding="utf-8")
            stdout, stderr = io.StringIO(), io.StringIO()
            with patch("analyze_routes._compare_validated_routes") as compare:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    exit_code = main([str(path)])
        self.assertEqual(exit_code, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn(
            "fixture identity does not match the pinned dataset",
            stderr.getvalue(),
        )
        compare.assert_not_called()

    def _write_config(self, directory: str) -> Path:
        path = Path(directory) / "expected-config.json"
        path.write_text(json.dumps(CONFIG), encoding="utf-8")
        return path


if __name__ == "__main__":
    unittest.main()
