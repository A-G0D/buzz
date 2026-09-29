#!/usr/bin/env python3
"""Compare validated receipt routing fields with the pinned fixture's gold."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from validate_dataset import CONDITIONS
from validate_report import validate as validate_report


ROUTE_FIELDS = (
    "route_action",
    "selected_skill_ids",
    "review_subject_ids",
    "target_skill_ids",
    "ingest_decision",
)
DIAGNOSTIC_LIMIT = (
    "Raw fixture-gold route-field comparison only. This is not a score, grade, "
    "utility, skill-quality, or efficacy result; fixture gold are synthetic "
    "hypotheses, not universal policy. No skill state is changed."
)


def _compare_validated_routes(
    report: dict[str, Any], fixture: dict[str, Any]
) -> list[dict[str, Any]]:
    """Return exact field differences for an already-validated full receipt."""
    cells = {
        (cell["case_id"], cell["condition"]): cell
        for cell in report["cells"]
    }
    mismatches = []
    for case in fixture["cases"]:
        gold = case["route_gold"]
        for condition in CONDITIONS:
            cell = cells[(case["id"], condition)]
            for field in ROUTE_FIELDS:
                expected = gold[field]
                actual = cell["route"][field]
                if actual != expected:
                    mismatches.append(
                        {
                            "case_id": case["id"],
                            "condition": condition,
                            "field": field,
                            "expected": expected,
                            "actual": actual,
                        }
                    )
    return mismatches


def analyze(
    report: Any,
    fixture: Any,
    *,
    expected_config: Any = None,
) -> list[dict[str, Any]]:
    """Validate pinned receipt structure before comparing raw routing fields."""
    errors = validate_report(
        report,
        fixture=fixture,
        expected_config=expected_config,
    )
    if errors:
        raise ValueError("receipt validation failed:\n- " + "\n- ".join(errors))
    return _compare_validated_routes(report, fixture)


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


def _display(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path, help="receipt JSON to compare")
    parser.add_argument(
        "--expected-config",
        type=Path,
        help="optional preregistered config JSON, validated before comparison",
    )
    args = parser.parse_args(argv)
    fixture_path = Path(__file__).with_name("dataset.json")
    try:
        report = _read_json(args.report)
        fixture = _read_json(fixture_path)
        expected_config = (
            _read_json(args.expected_config) if args.expected_config else None
        )
        mismatches = analyze(
            report,
            fixture,
            expected_config=expected_config,
        )
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        print(f"route comparison stopped: {exc}", file=sys.stderr)
        return 1

    if mismatches:
        print("Raw routing-field differences from pinned fixture gold:")
        for mismatch in mismatches:
            print(
                f"- case={mismatch['case_id']} "
                f"condition={mismatch['condition']} "
                f"field={mismatch['field']} "
                f"expected={_display(mismatch['expected'])} "
                f"actual={_display(mismatch['actual'])}"
            )
    else:
        print(
            "No raw route-field differences from pinned fixture gold "
            "across the complete 24-cell matrix."
        )
    print(DIAGNOSTIC_LIMIT)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
