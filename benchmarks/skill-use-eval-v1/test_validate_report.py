"""Tests for receipt shape/provenance only; no evaluation runs are performed."""

from __future__ import annotations

import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import validate_report as vr
from validate_dataset import CONDITIONS


HERE = Path(__file__).parent
FIXTURE = json.loads((HERE / "dataset.json").read_text(encoding="utf-8"))
CONFIG = {
    "config_id": "synthetic-smoke-v1",
    "route_profile_id": "local-test-profile",
    "base_prompt_sha256": "1" * 64,
    "toolset_sha256": "2" * 64,
    "generation_settings_sha256": "3" * 64,
    "verifier_id": "fixture-verifier",
    "verifier_version": "1.0",
}


def measured(value):
    return {"value": value, "evidence_ref": "runs/synthetic.jsonl", "unavailable_reason": None}


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
                        "route_action": "none",
                        "selected_skill_ids": [],
                        "review_subject_ids": [],
                        "target_skill_ids": [],
                        "ingest_decision": False,
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
                "unavailable_reason": "No real model was run in validator tests.",
            },
        },
        "cells": cells,
    }


class ValidateReportTests(unittest.TestCase):
    def errors(self, report, expected_config=None):
        return vr.validate(report, fixture=FIXTURE, expected_config=expected_config)

    def test_complete_pinned_matrix_is_structurally_valid(self):
        self.assertEqual(self.errors(make_report(), CONFIG), [])

    def test_empty_answer_is_rejected(self):
        report = make_report()
        report["cells"][0]["answer"] = ""
        self.assertTrue(any("nonempty raw answer" in error for error in self.errors(report)))

    def test_whitespace_answer_is_rejected(self):
        report = make_report()
        report["cells"][0]["answer"] = " \t\n "
        self.assertTrue(any("nonempty raw answer" in error for error in self.errors(report)))

    def test_cli_rejects_duplicate_json_object_keys(self):
        with tempfile.TemporaryDirectory(dir=HERE) as directory:
            report_path = Path(directory) / "duplicate-key.json"
            report_path.write_text(
                '{"schema_version":"buzz-skill-use-report-v1",'
                '"schema_version":"buzz-skill-use-report-v1"}',
                encoding="utf-8",
            )
            result = subprocess.run(
                [sys.executable, str(HERE / "validate_report.py"), str(report_path)],
                cwd=HERE,
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("duplicate JSON object key", result.stderr)

    def test_schema_is_parseable_and_rejects_unknown_properties(self):
        schema = json.loads((HERE / "report_schema.json").read_text(encoding="utf-8"))
        self.assertEqual(schema["$schema"], "https://json-schema.org/draft/2020-12/schema")
        self.assertFalse(schema["additionalProperties"])

    def test_missing_cell_is_rejected(self):
        report = make_report()
        report["cells"].pop()
        self.assertTrue(any("exactly 24" in error or "missing planned cells" in error for error in self.errors(report)))

    def test_duplicate_cell_is_rejected(self):
        report = make_report()
        report["cells"][-1] = copy.deepcopy(report["cells"][0])
        self.assertTrue(any("duplicates planned cell" in error for error in self.errors(report)))

    def test_unrecognized_case_or_condition_is_rejected(self):
        report = make_report()
        report["cells"][0]["case_id"] = "invented-case"
        report["cells"][1]["condition"] = "invented-condition"
        errors = self.errors(report)
        self.assertTrue(any("unrecognized case ID" in error for error in errors))
        self.assertTrue(any("unrecognized condition" in error for error in errors))

    def test_fixture_digest_mismatch_is_rejected(self):
        report = make_report()
        report["fixture"]["payload_sha256"] = "0" * 64
        self.assertTrue(any("fixture identity" in error for error in self.errors(report)))

    def test_cell_config_mismatch_is_rejected(self):
        report = make_report()
        report["cells"][0]["config_sha256"] = "0" * 64
        self.assertTrue(any("differs from the report config" in error for error in self.errors(report)))

    def test_preregistered_config_mismatch_is_rejected(self):
        report = make_report()
        report["config"]["config_id"] = "other-config"
        report["config_sha256"] = vr.canonical_sha256(report["config"])
        for cell in report["cells"]:
            cell["config_sha256"] = report["config_sha256"]
        self.assertTrue(any("preregistered config" in error for error in self.errors(report, CONFIG)))

    def test_inconsistent_config_hash_is_rejected(self):
        report = make_report()
        report["config"]["config_id"] = "tampered"
        self.assertTrue(any("does not match the canonical config" in error for error in self.errors(report)))

    def test_fabricated_score_fields_are_rejected(self):
        report = make_report()
        report["overall_score"] = 1.0
        report["cells"][0]["score"] = 1.0
        errors = self.errors(report)
        self.assertTrue(any("unsupported fields" in error for error in errors))

    def test_fabricated_skill_ids_are_rejected(self):
        report = make_report()
        report["cells"][0]["route"]["selected_skill_ids"] = ["invented_skill"]
        report["cells"][1]["telemetry"]["opened_skill_ids"] = measured(["invented_skill"])
        errors = self.errors(report)
        self.assertTrue(any("unknown skill ID" in error for error in errors))

    def test_invalid_boolean_numeric_and_route_types_are_rejected(self):
        report = make_report()
        report["cells"][0]["telemetry"]["latency_ms"] = measured(True)
        report["cells"][1]["route"]["ingest_decision"] = "false"
        report["cells"][2]["route"]["selected_skill_ids"] = "skill_curator"
        errors = self.errors(report)
        self.assertTrue(any("nonnegative integer" in error for error in errors))
        self.assertTrue(any("must be boolean" in error for error in errors))
        self.assertTrue(any("must be an array" in error for error in errors))

    def test_unavailable_telemetry_requires_null_and_reason(self):
        report = make_report()
        report["cells"][0]["telemetry"]["latency_ms"] = unavailable("timer unavailable")
        self.assertEqual(self.errors(report), [])
        report["cells"][0]["telemetry"]["latency_ms"]["unavailable_reason"] = None
        self.assertTrue(any("must explain a null measurement" in error for error in self.errors(report)))

    def test_present_telemetry_requires_evidence_and_no_unavailable_reason(self):
        report = make_report()
        report["cells"][0]["telemetry"]["tool_calls"]["evidence_ref"] = "../private.log"
        errors = self.errors(report)
        self.assertTrue(any("safe repo-relative evidence reference" in error for error in errors))
        report = make_report()
        report["cells"][0]["telemetry"]["tool_calls"]["unavailable_reason"] = "missing"
        self.assertTrue(any("must be null when a measurement is present" in error for error in self.errors(report)))

    def test_self_reported_and_observed_identity_remain_distinct(self):
        report = make_report()
        report["model_provider_identity"]["observed"] = {
            "value": {"provider": "verified-local", "model": "model-a", "revision": "sha-123"},
            "source_ref": "runs/provider-log.jsonl",
            "unavailable_reason": None,
        }
        self.assertEqual(self.errors(report), [])
        report["model_provider_identity"]["observed"]["source_ref"] = None
        self.assertTrue(any("observed.source_ref must be a safe" in error for error in self.errors(report)))


if __name__ == "__main__":
    unittest.main()
