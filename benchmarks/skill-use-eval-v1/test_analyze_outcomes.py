"""Tests for raw verifier transitions and evidence-backed telemetry only."""

from __future__ import annotations

import contextlib
import copy
import io
import json
import tempfile
import unittest
from pathlib import Path

from analyze_outcomes import analyze, main
from validate_dataset import CONDITIONS
import validate_report as vr


HERE = Path(__file__).parent
FIXTURE = json.loads((HERE / "dataset.json").read_text(encoding="utf-8"))
CONFIG = {
    "config_id": "synthetic-outcomes-v1",
    "route_profile_id": "local-test-profile",
    "base_prompt_sha256": "1" * 64,
    "toolset_sha256": "2" * 64,
    "generation_settings_sha256": "3" * 64,
    "verifier_id": "fixture-verifier",
    "verifier_version": "1.0",
}
ROUTE_FIELDS = (
    "route_action",
    "selected_skill_ids",
    "review_subject_ids",
    "target_skill_ids",
    "ingest_decision",
)


def measured(value, evidence_ref="runs/synthetic.jsonl"):
    return {"value": value, "evidence_ref": evidence_ref, "unavailable_reason": None}


def unavailable(reason="not instrumented"):
    return {"value": None, "evidence_ref": None, "unavailable_reason": reason}


def make_report():
    config_digest = vr.canonical_sha256(CONFIG)
    cells = []
    for case in FIXTURE["cases"]:
        for condition in CONDITIONS:
            cells.append(
                {
                    "case_id": case["id"],
                    "condition": condition,
                    "config_sha256": config_digest,
                    "route": {
                        field: copy.deepcopy(case["route_gold"][field])
                        for field in ROUTE_FIELDS
                    },
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
                "unavailable_reason": "Synthetic tests do not run a model.",
            },
        },
        "cells": cells,
    }


def find_cell(report, case_id, condition):
    return next(
        cell
        for cell in report["cells"]
        if cell["case_id"] == case_id and cell["condition"] == condition
    )


class AnalyzeOutcomesTests(unittest.TestCase):
    def test_complete_matrix_returns_per_case_statuses_and_descriptive_counts(self):
        result = analyze(make_report(), FIXTURE, expected_config=CONFIG)
        self.assertEqual(result["case_count"], 8)
        self.assertEqual(len(result["cases"]), 8)
        self.assertEqual(result["condition_counts"]["no_skill"], {"pass": 8, "fail": 0, "unavailable": 0})
        self.assertEqual(
            result["transition_counts_vs_no_skill"]["focused_skill"]["unchanged_pass"],
            8,
        )
        self.assertEqual(len(result["measured_telemetry_deltas"]), 64)
        self.assertTrue(all(item["delta"] == 0 for item in result["measured_telemetry_deltas"]))

    def test_records_raw_transition_classes_and_evidence_references_per_case(self):
        report = make_report()
        find_cell(report, "one-off-addition", "no_skill")["verifier_status"] = measured("pass", "runs/base.jsonl")
        find_cell(report, "one-off-addition", "focused_skill")["verifier_status"] = measured("fail", "runs/focused.jsonl")
        find_cell(report, "one-off-addition", "small_pack")["verifier_status"] = unavailable("verifier timed out")
        find_cell(report, "paid-total-exact-cents", "no_skill")["verifier_status"] = measured("fail")
        find_cell(report, "paid-total-exact-cents", "focused_skill")["verifier_status"] = measured("pass")

        result = analyze(report, FIXTURE)
        one_off = next(row for row in result["cases"] if row["case_id"] == "one-off-addition")
        self.assertEqual(one_off["verifier_statuses"]["small_pack"], "unavailable")
        self.assertEqual(one_off["verifier_evidence_refs"]["focused_skill"], "runs/focused.jsonl")
        self.assertEqual(one_off["transitions_vs_no_skill"]["focused_skill"]["transition"], "pass_to_fail")
        self.assertEqual(one_off["transitions_vs_no_skill"]["small_pack"]["transition"], "unavailable")
        self.assertEqual(
            result["transition_counts_vs_no_skill"]["focused_skill"]["fail_to_pass"],
            1,
        )

    def test_telemetry_delta_requires_two_measured_values_with_evidence(self):
        report = make_report()
        base = find_cell(report, "one-off-addition", "no_skill")["telemetry"]["latency_ms"]
        focused = find_cell(report, "one-off-addition", "focused_skill")["telemetry"]["latency_ms"]
        base.update(measured(120, "runs/base-latency.json"))
        focused.update(measured(165, "runs/focused-latency.json"))
        find_cell(report, "one-off-addition", "small_pack")["telemetry"]["latency_ms"] = unavailable("timer absent")

        result = analyze(report, FIXTURE)
        delta = next(
            item
            for item in result["measured_telemetry_deltas"]
            if item["case_id"] == "one-off-addition"
            and item["condition"] == "focused_skill"
            and item["metric"] == "latency_ms"
        )
        self.assertEqual(delta["delta"], 45)
        self.assertEqual(delta["baseline_evidence_ref"], "runs/base-latency.json")
        self.assertEqual(delta["condition_evidence_ref"], "runs/focused-latency.json")
        self.assertFalse(
            any(
                item["case_id"] == "one-off-addition"
                and item["condition"] == "small_pack"
                and item["metric"] == "latency_ms"
                for item in result["measured_telemetry_deltas"]
            )
        )
        self.assertEqual(
            result["telemetry_delta_counts"]["small_pack"]["latency_ms"]["unavailable"],
            1,
        )

    def test_structurally_invalid_receipt_is_rejected_before_analysis(self):
        report = make_report()
        report["cells"].pop()
        with self.assertRaisesRegex(ValueError, "receipt validation failed"):
            analyze(report, FIXTURE)

    def test_cli_prints_raw_transitions_and_claim_boundary(self):
        report = make_report()
        find_cell(report, "one-off-addition", "focused_skill")["verifier_status"] = measured("fail")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipt.json"
            path.write_text(json.dumps(report), encoding="utf-8")
            stdout, stderr = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                exit_code = main([str(path)])
        self.assertEqual(exit_code, 0)
        self.assertEqual(stderr.getvalue(), "")
        self.assertIn("one-off-addition: pass / fail / pass; focused_vs_no_skill=pass_to_fail", stdout.getvalue())
        self.assertIn("fixture gold is synthetic", stdout.getvalue())
        self.assertIn("evidence references are not opened or authenticated", stdout.getvalue())
        self.assertNotIn("efficacy score", stdout.getvalue())


if __name__ == "__main__":
    unittest.main()
