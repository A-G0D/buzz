#!/usr/bin/env python3
"""Summarize recorded verifier labels and evidenced telemetry in a receipt."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from validate_dataset import CONDITIONS
from validate_report import validate as validate_report


BASELINE = "no_skill"
COMPARE_CONDITIONS = tuple(condition for condition in CONDITIONS if condition != BASELINE)
STATUSES = ("pass", "fail", "unavailable")
TRANSITIONS = (
    "pass_to_fail",
    "fail_to_pass",
    "unchanged_pass",
    "unchanged_fail",
    "unavailable",
)
NUMERIC_TELEMETRY = (
    "added_context_tokens",
    "latency_ms",
    "cost_microusd",
    "tool_calls",
)
DIAGNOSTIC_LIMIT = (
    "Descriptive receipt analysis only: verifier labels are not independently checked, "
    "evidence references are not opened or authenticated, fixture gold is synthetic, "
    "and no result establishes skill efficacy or a grade."
)


def _status(cell: dict[str, Any]) -> str:
    value = cell["verifier_status"]["value"]
    return value if value is not None else "unavailable"


def _transition(before: str, after: str) -> str:
    if "unavailable" in (before, after):
        return "unavailable"
    if before == after == "pass":
        return "unchanged_pass"
    if before == after == "fail":
        return "unchanged_fail"
    if before == "pass" and after == "fail":
        return "pass_to_fail"
    return "fail_to_pass"


def analyze(
    report: Any,
    fixture: Any,
    *,
    expected_config: Any = None,
) -> dict[str, Any]:
    """Validate receipt structure, then describe raw statuses and evidenced deltas."""
    errors = validate_report(report, fixture=fixture, expected_config=expected_config)
    if errors:
        raise ValueError("receipt validation failed:\n- " + "\n- ".join(errors))

    cells = {(cell["case_id"], cell["condition"]): cell for cell in report["cells"]}
    condition_counts = {
        condition: {status: 0 for status in STATUSES} for condition in CONDITIONS
    }
    transition_counts = {
        condition: {transition: 0 for transition in TRANSITIONS}
        for condition in COMPARE_CONDITIONS
    }
    telemetry_pair_counts = {
        condition: {
            metric: {"measured": 0, "unavailable": 0}
            for metric in NUMERIC_TELEMETRY
        }
        for condition in COMPARE_CONDITIONS
    }
    case_rows = []
    telemetry_deltas = []

    for case in fixture["cases"]:
        case_id = case["id"]
        by_condition = {condition: cells[(case_id, condition)] for condition in CONDITIONS}
        statuses = {condition: _status(by_condition[condition]) for condition in CONDITIONS}
        evidence_refs = {
            condition: by_condition[condition]["verifier_status"]["evidence_ref"]
            for condition in CONDITIONS
        }
        for condition, status in statuses.items():
            condition_counts[condition][status] += 1

        transitions = {}
        baseline = by_condition[BASELINE]
        for condition in COMPARE_CONDITIONS:
            target = by_condition[condition]
            transition = _transition(statuses[BASELINE], statuses[condition])
            transitions[condition] = {
                "from": statuses[BASELINE],
                "to": statuses[condition],
                "transition": transition,
            }
            transition_counts[condition][transition] += 1

            for metric in NUMERIC_TELEMETRY:
                before = baseline["telemetry"][metric]
                after = target["telemetry"][metric]
                if (
                    before["value"] is None
                    or after["value"] is None
                    or not before["evidence_ref"]
                    or not after["evidence_ref"]
                ):
                    telemetry_pair_counts[condition][metric]["unavailable"] += 1
                    continue
                telemetry_pair_counts[condition][metric]["measured"] += 1
                telemetry_deltas.append(
                    {
                        "case_id": case_id,
                        "condition": condition,
                        "metric": metric,
                        "baseline_value": before["value"],
                        "condition_value": after["value"],
                        "delta": after["value"] - before["value"],
                        "baseline_evidence_ref": before["evidence_ref"],
                        "condition_evidence_ref": after["evidence_ref"],
                    }
                )

        case_rows.append(
            {
                "case_id": case_id,
                "verifier_statuses": statuses,
                "verifier_evidence_refs": evidence_refs,
                "transitions_vs_no_skill": transitions,
            }
        )

    return {
        "case_count": len(fixture["cases"]),
        "condition_counts": condition_counts,
        "transition_counts_vs_no_skill": transition_counts,
        "cases": case_rows,
        "telemetry_delta_counts": telemetry_pair_counts,
        "measured_telemetry_deltas": telemetry_deltas,
    }


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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path, help="receipt JSON to summarize")
    parser.add_argument(
        "--expected-config",
        type=Path,
        help="optional preregistered config JSON, validated before analysis",
    )
    args = parser.parse_args(argv)
    fixture_path = Path(__file__).with_name("dataset.json")
    try:
        report = _read_json(args.report)
        fixture = _read_json(fixture_path)
        expected_config = _read_json(args.expected_config) if args.expected_config else None
        result = analyze(report, fixture, expected_config=expected_config)
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        print(f"outcome analysis stopped: {exc}", file=sys.stderr)
        return 1

    print("Recorded verifier statuses by case (status order: no_skill / focused_skill / small_pack):")
    for case in result["cases"]:
        statuses = case["verifier_statuses"]
        transitions = case["transitions_vs_no_skill"]
        print(
            f"- {case['case_id']}: "
            f"{statuses['no_skill']} / {statuses['focused_skill']} / {statuses['small_pack']}; "
            f"focused_vs_no_skill={transitions['focused_skill']['transition']}; "
            f"small_pack_vs_no_skill={transitions['small_pack']['transition']}"
        )
    print("Descriptive condition counts:")
    for condition, counts in result["condition_counts"].items():
        print(f"- {condition}: " + ", ".join(f"{key}={value}" for key, value in counts.items()))
    print("Measured telemetry deltas (condition minus no_skill; receipt-referenced pairs only):")
    for delta in result["measured_telemetry_deltas"]:
        print(
            f"- case={delta['case_id']} condition={delta['condition']} "
            f"metric={delta['metric']} delta={delta['delta']} "
            f"evidence={delta['baseline_evidence_ref']}|{delta['condition_evidence_ref']}"
        )
    print("Telemetry pair counts (measured / unavailable):")
    for condition, metrics in result["telemetry_delta_counts"].items():
        for metric, counts in metrics.items():
            print(f"- {condition}/{metric}: {counts['measured']} / {counts['unavailable']}")
    print(DIAGNOSTIC_LIMIT)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
