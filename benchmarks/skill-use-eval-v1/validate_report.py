#!/usr/bin/env python3
"""Validate receipt shape and pinned provenance; never score model output."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

from validate_dataset import CONDITIONS, V1_PAYLOAD_SHA256, validate as validate_fixture, safe_relative_path


SCHEMA_VERSION = "buzz-skill-use-report-v1"
CLAIM_BOUNDARY = (
    "Structure and provenance validation only; no verification of score correctness or skill efficacy."
)
CONFIG_KEYS = {
    "config_id",
    "route_profile_id",
    "base_prompt_sha256",
    "toolset_sha256",
    "generation_settings_sha256",
    "verifier_id",
    "verifier_version",
}
REPORT_KEYS = {
    "schema_version",
    "claim_boundary",
    "fixture",
    "config",
    "config_sha256",
    "model_provider_identity",
    "cells",
}
SHA256 = re.compile(r"^[0-9a-f]{64}$")
ROUTE_ACTIONS = {
    "none",
    "use_existing",
    "propose_create",
    "propose_merge",
    "propose_split",
    "defer_review",
    "import_approved",
}
ROUTE_KEYS = {
    "route_action",
    "selected_skill_ids",
    "review_subject_ids",
    "target_skill_ids",
    "ingest_decision",
}
CELL_KEYS = {
    "case_id",
    "condition",
    "config_sha256",
    "route",
    "answer",
    "verifier_status",
    "telemetry",
}
TELEMETRY_KEYS = {
    "opened_skill_ids",
    "performed_import_ids",
    "added_context_tokens",
    "latency_ms",
    "cost_microusd",
    "tool_calls",
}
MEASUREMENT_KEYS = {"value", "evidence_ref", "unavailable_reason"}
IDENTITY_FIELDS = {"provider", "model", "revision"}
IDENTITY_RECORD_KEYS = {"value", "source_ref", "unavailable_reason"}


def canonical_sha256(value: Any) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _is_text(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def _keys(value: Any, expected: set[str], where: str, errors: list[str]) -> bool:
    if not isinstance(value, dict):
        errors.append(f"{where} must be an object")
        return False
    if set(value) != expected:
        extra = set(value) - expected
        missing = expected - set(value)
        if extra:
            errors.append(f"{where} has unsupported fields: {sorted(extra)}")
        if missing:
            errors.append(f"{where} is missing fields: {sorted(missing)}")
        return False
    return True


def _safe_ref(value: Any) -> bool:
    return safe_relative_path(value)


def _check_measurement(
    value: Any,
    where: str,
    errors: list[str],
    validate_value: Any,
) -> None:
    if not _keys(value, MEASUREMENT_KEYS, where, errors):
        return
    measured = value["value"]
    evidence = value["evidence_ref"]
    unavailable = value["unavailable_reason"]
    if measured is None:
        if not _is_text(unavailable):
            errors.append(f"{where}.unavailable_reason must explain a null measurement")
        if evidence is not None:
            errors.append(f"{where}.evidence_ref must be null when the measurement is unavailable")
        return
    if unavailable is not None:
        errors.append(f"{where}.unavailable_reason must be null when a measurement is present")
    if not _safe_ref(evidence):
        errors.append(f"{where}.evidence_ref must be a safe repo-relative evidence reference")
    validate_value(measured, f"{where}.value", errors)


def _check_identity_record(value: Any, where: str, errors: list[str]) -> None:
    if not _keys(value, IDENTITY_RECORD_KEYS, where, errors):
        return
    identity = value["value"]
    source_ref = value["source_ref"]
    unavailable = value["unavailable_reason"]
    if identity is None:
        if not _is_text(unavailable):
            errors.append(f"{where}.unavailable_reason must explain unavailable identity")
        if source_ref is not None:
            errors.append(f"{where}.source_ref must be null when identity is unavailable")
        return
    if not _keys(identity, IDENTITY_FIELDS, f"{where}.value", errors):
        return
    present = False
    for field, item in identity.items():
        if item is not None:
            present = True
            if not _is_text(item):
                errors.append(f"{where}.value.{field} must be a nonempty string or null")
    if not present:
        errors.append(f"{where}.value must be null when all identity fields are unavailable")
    if unavailable is not None:
        errors.append(f"{where}.unavailable_reason must be null when identity is present")
    if not _safe_ref(source_ref):
        errors.append(f"{where}.source_ref must be a safe repo-relative evidence reference")


def validate(
    report: Any,
    *,
    fixture: Any,
    expected_config: Any = None,
) -> list[str]:
    """Return structural/provenance errors without judging outcomes or efficacy."""
    errors: list[str] = []
    if not isinstance(fixture, dict):
        return ["pinned fixture must be an object"]
    fixture_errors = validate_fixture(fixture)
    if fixture_errors:
        return ["bundled fixture is invalid: " + fixture_errors[0]]
    fixture_digest = canonical_sha256(fixture)
    if fixture_digest != V1_PAYLOAD_SHA256:
        return ["bundled fixture digest differs from the pinned v1 content"]
    case_ids = [case["id"] for case in fixture["cases"]]
    skill_ids = set(fixture["skills"])

    if not _keys(report, REPORT_KEYS, "report", errors):
        return errors
    if report["schema_version"] != SCHEMA_VERSION:
        errors.append(f"schema_version must be {SCHEMA_VERSION}")
    if report["claim_boundary"] != CLAIM_BOUNDARY:
        errors.append("claim_boundary must preserve the structure-only limitation")

    expected_fixture = {
        "schema_version": fixture["schema_version"],
        "payload_sha256": fixture_digest,
    }
    if report["fixture"] != expected_fixture:
        errors.append("report fixture identity does not match the pinned dataset")

    config = report["config"]
    if _keys(config, CONFIG_KEYS, "config", errors):
        for field in ("config_id", "route_profile_id", "verifier_id", "verifier_version"):
            if not _is_text(config[field]):
                errors.append(f"config.{field} must be a nonempty string")
        for field in ("base_prompt_sha256", "toolset_sha256", "generation_settings_sha256"):
            if not isinstance(config[field], str) or not SHA256.fullmatch(config[field]):
                errors.append(f"config.{field} must be a lowercase SHA-256 digest")
        computed_config_digest = canonical_sha256(config)
        if report["config_sha256"] != computed_config_digest:
            errors.append("config_sha256 does not match the canonical config identity")
        if expected_config is not None:
            if not _keys(expected_config, CONFIG_KEYS, "expected config", errors):
                pass
            elif config != expected_config:
                errors.append("report config identity does not match the supplied preregistered config")
    elif not isinstance(report["config_sha256"], str) or not SHA256.fullmatch(report["config_sha256"]):
        errors.append("config_sha256 must be a lowercase SHA-256 digest")

    identity = report["model_provider_identity"]
    if _keys(identity, {"self_reported", "observed"}, "model_provider_identity", errors):
        _check_identity_record(identity["self_reported"], "model_provider_identity.self_reported", errors)
        _check_identity_record(identity["observed"], "model_provider_identity.observed", errors)

    cells = report["cells"]
    if not isinstance(cells, list):
        errors.append("cells must be an array")
        return errors
    expected_pairs = {(case_id, condition) for case_id in case_ids for condition in CONDITIONS}
    seen_pairs: set[tuple[str, str]] = set()
    for index, cell in enumerate(cells):
        where = f"cells[{index}]"
        if not _keys(cell, CELL_KEYS, where, errors):
            continue
        case_id, condition = cell["case_id"], cell["condition"]
        if not isinstance(case_id, str) or not isinstance(condition, str):
            errors.append(f"{where} case_id and condition must be strings")
        else:
            pair = (case_id, condition)
            if pair in seen_pairs:
                errors.append(f"{where} duplicates planned cell {case_id}/{condition}")
            seen_pairs.add(pair)
            if case_id not in case_ids:
                errors.append(f"{where} has an unrecognized case ID {case_id!r}")
            if condition not in CONDITIONS:
                errors.append(f"{where} has an unrecognized condition {condition!r}")
            if pair not in expected_pairs:
                errors.append(f"{where} is not one of the pinned case × condition cells")

        if cell["config_sha256"] != report["config_sha256"]:
            errors.append(f"{where}.config_sha256 differs from the report config identity")

        route = cell["route"]
        if _keys(route, ROUTE_KEYS, f"{where}.route", errors):
            if not isinstance(route["route_action"], str) or route["route_action"] not in ROUTE_ACTIONS:
                errors.append(f"{where}.route.route_action is unknown")
            if not isinstance(route["ingest_decision"], bool):
                errors.append(f"{where}.route.ingest_decision must be boolean")
            for field in ("selected_skill_ids", "review_subject_ids", "target_skill_ids"):
                values = route[field]
                if not isinstance(values, list) or any(not isinstance(item, str) for item in values):
                    errors.append(f"{where}.route.{field} must be an array of skill IDs")
                    continue
                if len(values) != len(set(values)):
                    errors.append(f"{where}.route.{field} must not contain duplicate IDs")
                for item in values:
                    if item not in skill_ids:
                        errors.append(f"{where}.route.{field} contains unknown skill ID {item!r}")
        if not _is_text(cell["answer"]):
            errors.append(f"{where}.answer must retain a nonempty raw answer string")

        _check_measurement(
            cell["verifier_status"],
            f"{where}.verifier_status",
            errors,
            lambda item, location, errs: (
                errs.append(f"{location} must be pass, fail, or null")
                if not isinstance(item, str) or item not in {"pass", "fail"}
                else None
            ),
        )
        telemetry = cell["telemetry"]
        if not _keys(telemetry, TELEMETRY_KEYS, f"{where}.telemetry", errors):
            continue
        for field in ("opened_skill_ids", "performed_import_ids"):
            def check_ids(value: Any, location: str, errs: list[str]) -> None:
                if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
                    errs.append(f"{location} must be an array of skill IDs")
                    return
                if len(value) != len(set(value)):
                    errs.append(f"{location} must not contain duplicate IDs")
                for item in value:
                    if item not in skill_ids:
                        errs.append(f"{location} contains unknown skill ID {item!r}")

            _check_measurement(
                telemetry[field], f"{where}.telemetry.{field}", errors, check_ids
            )
        for field in ("added_context_tokens", "latency_ms", "cost_microusd", "tool_calls"):
            def check_nonnegative_int(value: Any, location: str, errs: list[str]) -> None:
                if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                    errs.append(f"{location} must be a nonnegative integer")

            _check_measurement(
                telemetry[field], f"{where}.telemetry.{field}", errors, check_nonnegative_int
            )

    if len(cells) != 24:
        errors.append("report must contain exactly 24 planned case × condition cells")
    missing_pairs = expected_pairs - seen_pairs
    if missing_pairs:
        errors.append(f"report is missing planned cells: {sorted(missing_pairs)}")
    if len(seen_pairs) != len(expected_pairs):
        # Duplicate detection above explains collisions; this guards the exact matrix as a whole.
        errors.append("report cells do not form the exact 24-cell fixture matrix")
    return errors


def _read_json(path: Path) -> Any:
    def reject_constant(value: str) -> None:
        raise ValueError(f"non-standard JSON constant {value}")

    def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON object key: {key!r}")
            result[key] = value
        return result

    return json.loads(
        path.read_text(encoding="utf-8"),
        object_pairs_hook=unique_object,
        parse_constant=reject_constant,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path, help="receipt JSON to validate")
    parser.add_argument(
        "--expected-config",
        type=Path,
        help="optional preregistered config JSON; must contain the exact config identity object",
    )
    args = parser.parse_args()
    fixture_path = Path(__file__).with_name("dataset.json")
    try:
        report = _read_json(args.report)
        fixture = _read_json(fixture_path)
        expected_config = _read_json(args.expected_config) if args.expected_config else None
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        print(f"invalid report input: {exc}", file=sys.stderr)
        return 1
    errors = validate(report, fixture=fixture, expected_config=expected_config)
    if errors:
        print("report validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("report structure valid: pinned fixture identity and 24 planned cells; no score/effect claims verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
