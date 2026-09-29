#!/usr/bin/env python3
"""Validate the synthetic skill-use fixture without running a model."""

from __future__ import annotations

import json
import hashlib
import re
import sys
from pathlib import Path
from pathlib import PureWindowsPath
from typing import Any


CONDITIONS = ("no_skill", "focused_skill", "small_pack")
ROUTE_ACTIONS = {
    "none",
    "use_existing",
    "propose_create",
    "propose_merge",
    "propose_split",
    "defer_review",
    "import_approved",
}
CASE_ID = re.compile(r"^[a-z][a-z0-9-]*$")
SKILL_ID = re.compile(r"^[a-z][a-z0-9_]*$")
LABEL = re.compile(r"^[a-z][a-z0-9-]*$")
GOLD_LIST_FIELDS = ("selected_skill_ids", "review_subject_ids", "target_skill_ids")
EXPECTED_CONDITIONS = {
    "no_skill": "Task plus catalog metadata; no candidate skill body.",
    "focused_skill": "Task plus exactly focused_skill_id body (a plausible distractor for route_action=none).",
    "small_pack": "Task plus all small_pack_skill_ids bodies; the agent must select only what fits.",
}
V1_CASE_IDS = (
    "one-off-addition",
    "paid-total-exact-cents",
    "missing-record-field",
    "merge-overlapping-guides",
    "split-mixed-guide",
    "repeated-uncovered-procedure",
    "unknown-provenance-skill",
    "approved-local-skill-import",
)
APPROVED_IMPORT_CASE_ID = "approved-local-skill-import"
APPROVED_IMPORT_SKILL_ID = "timeline_normalize"
V1_PAYLOAD_SHA256 = "b963720d46f529d677043da68be613944618096aa2aad15d91b5fe36b73cac60"


def safe_relative_path(value: Any) -> bool:
    """Accept normalized repo-relative POSIX paths; reject traversal/absolute forms."""
    if not isinstance(value, str) or not value or value != value.strip():
        return False
    if "\x00" in value or "\\" in value or value.startswith("/"):
        return False
    if PureWindowsPath(value).drive or PureWindowsPath(value).is_absolute():
        return False
    return all(part not in {"", ".", ".."} for part in value.split("/"))


def validate(data: Any) -> list[str]:
    """Return structural or protocol errors; never infer evaluation results."""
    errors: list[str] = []
    if not isinstance(data, dict):
        return ["dataset root must be an object"]
    try:
        canonical = json.dumps(data, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        digest = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
        if digest != V1_PAYLOAD_SHA256:
            errors.append("dataset payload SHA-256 differs from the pinned v1 content")
    except (TypeError, ValueError):
        errors.append("dataset cannot be encoded as canonical JSON for the v1 content pin")

    expected_root_keys = {
        "schema_version",
        "claim_boundary",
        "required_output",
        "conditions",
        "skills",
        "cases",
    }
    if set(data) != expected_root_keys:
        errors.append(f"root keys must be exactly {sorted(expected_root_keys)}")
    if data.get("schema_version") != "buzz-skill-use-eval-v1":
        errors.append("schema_version must be buzz-skill-use-eval-v1")
    claim = data.get("claim_boundary")
    normalized_claim = claim.lower() if isinstance(claim, str) else ""
    if not all(
        phrase in normalized_claim
        for phrase in ("synthetic paired fixture", "contains no model runs", "or efficacy evidence")
    ):
        errors.append("claim_boundary must state that this is synthetic and has no runs or efficacy evidence")

    required_output = data.get("required_output")
    required_fields = {
        "route_action",
        "selected_skill_ids",
        "review_subject_ids",
        "target_skill_ids",
        "ingest_decision",
        "answer",
    }
    if not isinstance(required_output, dict) or not required_fields <= set(required_output):
        errors.append("required_output must define all six documented response fields")
    elif any(not isinstance(required_output[field], str) for field in required_fields):
        errors.append("required_output field descriptions must be strings")

    conditions = data.get("conditions")
    if not isinstance(conditions, dict) or set(conditions) != set(CONDITIONS):
        errors.append(f"conditions must define exactly {list(CONDITIONS)} for complete pairing")
    elif conditions != EXPECTED_CONDITIONS:
        errors.append("v1 condition descriptions must match the exact documented body-injection protocol")

    skills = data.get("skills")
    if not isinstance(skills, dict):
        errors.append("skills must be an object keyed by skill ID")
        skills = {}
    for skill_id, skill in skills.items():
        if not isinstance(skill_id, str) or not SKILL_ID.fullmatch(skill_id):
            errors.append(f"invalid skill ID: {skill_id!r}")
            continue
        if not isinstance(skill, dict):
            errors.append(f"skill {skill_id} must be an object")
            continue
        for label in ("version", "description"):
            if not isinstance(skill.get(label), str) or not skill[label].strip():
                errors.append(f"skill {skill_id} needs a nonempty {label}")
        availability = skill.get("availability", "installed")
        if not isinstance(availability, str) or availability not in {
            "installed",
            "staged_unimported",
            "unreviewed_listing",
        }:
            errors.append(f"skill {skill_id} has unknown availability {availability!r}")
        if availability == "unreviewed_listing" and "body" in skill:
            errors.append(f"unreviewed listing {skill_id} must not include a skill body")
        if availability != "unreviewed_listing" and (
            not isinstance(skill.get("body"), str) or not skill["body"].strip()
        ):
            errors.append(f"skill {skill_id} needs a nonempty body")
        if availability == "staged_unimported" and not safe_relative_path(skill.get("staged_path")):
            errors.append(f"staged skill {skill_id} needs a safe, nonempty repo-relative staged_path")

    cases = data.get("cases")
    if not isinstance(cases, list) or not cases:
        errors.append("cases must be a nonempty array")
        return errors
    if len(cases) != 8:
        errors.append("v1 fixture must retain the eight documented cases")
    case_ids = [case.get("id") if isinstance(case, dict) else None for case in cases]
    if tuple(case_ids) != V1_CASE_IDS:
        errors.append("v1 case IDs and order must match the pinned case list")

    seen_case_ids: set[str] = set()
    for index, case in enumerate(cases):
        where = f"case[{index}]"
        if not isinstance(case, dict):
            errors.append(f"{where} must be an object")
            continue
        case_id = case.get("id")
        if not isinstance(case_id, str) or not CASE_ID.fullmatch(case_id):
            errors.append(f"{where} has an invalid case ID")
        elif case_id in seen_case_ids:
            errors.append(f"duplicate case ID {case_id}")
        else:
            seen_case_ids.add(case_id)
            where = f"case {case_id}"
        category = case.get("category")
        if not isinstance(category, str) or not LABEL.fullmatch(category):
            errors.append(f"{where} needs a valid category label")
        if not isinstance(case.get("task"), str) or not case["task"].strip():
            errors.append(f"{where} needs task text")
        if not isinstance(case.get("answer_contract"), dict) or not case["answer_contract"]:
            errors.append(f"{where} needs a nonempty answer_contract")

        focused_id = case.get("focused_skill_id")
        if not isinstance(focused_id, str) or focused_id not in skills:
            errors.append(f"{where} focused_skill_id must reference a known skill")
        elif not isinstance(skills[focused_id], dict) or not isinstance(skills[focused_id].get("body"), str):
            errors.append(f"{where} focused skill must have a body")

        pack = case.get("small_pack_skill_ids")
        if (
            not isinstance(pack, list)
            or len(pack) != 3
            or any(not isinstance(skill_id, str) for skill_id in pack)
            or len(set(pack)) != 3
        ):
            errors.append(f"{where} small_pack_skill_ids must contain exactly three distinct skills")
        else:
            for skill_id in pack:
                if not isinstance(skill_id, str) or skill_id not in skills:
                    errors.append(f"{where} small pack references unknown skill {skill_id!r}")
                elif not isinstance(skills[skill_id], dict) or not isinstance(skills[skill_id].get("body"), str):
                    errors.append(f"{where} small pack skill {skill_id} has no body")

        gold = case.get("route_gold")
        if not isinstance(gold, dict):
            errors.append(f"{where} needs route_gold")
            continue
        action = gold.get("route_action")
        if not isinstance(action, str) or action not in ROUTE_ACTIONS:
            errors.append(f"{where} has unknown route_action {action!r}")
        if not isinstance(gold.get("ingest_decision"), bool):
            errors.append(f"{where} ingest_decision must be boolean")
        elif gold["ingest_decision"] != (action == "import_approved"):
            errors.append(f"{where} ingest_decision must be true only for import_approved")
        for field in GOLD_LIST_FIELDS:
            values = gold.get(field)
            if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
                errors.append(f"{where} {field} must be an array of skill IDs")
                continue
            if len(values) != len(set(values)):
                errors.append(f"{where} {field} must not contain duplicate IDs")
            for skill_id in values:
                if skill_id not in skills:
                    errors.append(f"{where} {field} references unknown skill {skill_id!r}")

    approved_cases = [
        case
        for case in cases
        if isinstance(case, dict)
        and isinstance(case.get("route_gold"), dict)
        and case["route_gold"].get("ingest_decision") is True
    ]
    if (
        len(approved_cases) != 1
        or approved_cases[0].get("id") != APPROVED_IMPORT_CASE_ID
    ):
        errors.append("fixture must contain exactly the pinned approved import-gate case")
    else:
        approved = approved_cases[0]
        skill = skills.get(APPROVED_IMPORT_SKILL_ID)
        answer = approved.get("answer_contract")
        if not isinstance(skill, dict) or skill.get("availability") != "staged_unimported":
            errors.append("approved import case must target the pinned staged skill")
        if approved["route_gold"].get("target_skill_ids") != [APPROVED_IMPORT_SKILL_ID]:
            errors.append("approved import case must target exactly timeline_normalize")
        if not isinstance(answer, dict) or answer.get("skill_id") != APPROVED_IMPORT_SKILL_ID:
            errors.append("approved import answer contract must name timeline_normalize")
        elif not isinstance(skill, dict) or answer.get("path") != skill.get("staged_path"):
            errors.append("approved import answer path must match the staged skill path")
    return errors


def main() -> int:
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).with_name("dataset.json")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"invalid fixture: {exc}", file=sys.stderr)
        return 1
    errors = validate(data)
    if errors:
        print("fixture validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(
        f"fixture valid: {len(data['cases'])} cases × {len(CONDITIONS)} conditions "
        f"= {len(data['cases']) * len(CONDITIONS)} planned cells; no runs performed"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
