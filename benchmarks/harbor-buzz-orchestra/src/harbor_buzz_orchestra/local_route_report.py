"""Build privacy-bounded evidence for fixed local route smoke runs."""

from __future__ import annotations

import json
import math
import re
from collections.abc import Mapping, Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

LOCAL_ROUTE_REPORT_SCHEMA_VERSION = 1
OUTCOMES = frozenset({"passed", "failed", "error", "timed_out", "skipped"})
STOP_REASONS = frozenset({"end_turn", "max_tokens", "tool_use", "unknown", "error"})
ERROR_CODES = frozenset(
    {
        "endpoint_error",
        "missing_output",
        "model_not_listed",
        "process_exit",
        "process_timeout",
        "protocol_error",
        "provider_error",
        "unexpected_tool_call",
    }
)
_HEX_SHA256 = re.compile(r"^[0-9a-f]{64}$")
_TASK_ID = re.compile(r"^[a-z0-9][a-z0-9_-]{0,63}$")
_MODEL_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:+/-]{0,127}$")
_SECRET_LIKE_MODEL_ID = re.compile(
    r"(?i)(?:api[_-]?key|password|secret|bearer\s|token=|"
    r"sk-[a-z0-9_-]{12,}|hf_[a-z0-9]{16,}|"
    r"gh[pousr]_[a-z0-9]{20,}|xox[baprs]-[a-z0-9-]{15,})"
)
_SCHEMA_KEYS = {
    "schema_version",
    "started_at",
    "finished_at",
    "duration_ms",
    "model",
    "route",
    "tasks",
    "summary",
}


class LocalRouteReportError(ValueError):
    """Raised when a local route report contains invalid or unsafe evidence."""


def _timestamp(value: object, name: str) -> tuple[str, datetime]:
    if not isinstance(value, str):
        raise LocalRouteReportError(f"{name} must be an ISO timestamp")
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError as error:
        raise LocalRouteReportError(f"{name} must be an ISO timestamp") from error
    if parsed.tzinfo is None:
        raise LocalRouteReportError(f"{name} must include a timezone")
    normalized = parsed.astimezone(UTC)
    normalized = normalized.replace(microsecond=(normalized.microsecond // 1000) * 1000)
    return (
        normalized.isoformat(timespec="milliseconds").replace("+00:00", "Z"),
        normalized,
    )


def _nonnegative_int(value: object, name: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise LocalRouteReportError(f"{name} must be a nonnegative integer")
    return value


def _safe_model_id(value: object) -> str:
    if not isinstance(value, str) or not value or len(value) > 512:
        raise LocalRouteReportError("model ID must be a nonempty string")
    # Local endpoints sometimes report an absolute model directory. Keep only
    # its leaf name so the user's home path never enters the evidence artifact.
    if (
        value.startswith("/")
        or re.match(r"^[A-Za-z]:[\\/]", value)
        or value.count("/") > 1
        or "\\" in value
    ):
        value = value.replace("\\", "/").rstrip("/").rsplit("/", 1)[-1]
    if (
        not _MODEL_ID.fullmatch(value)
        or value in {".", ".."}
        or "://" in value
        or _SECRET_LIKE_MODEL_ID.search(value)
    ):
        raise LocalRouteReportError("model ID contains unsupported characters")
    return value


def _usage(value: object) -> dict[str, int | None] | None:
    if value is None:
        return None
    if not isinstance(value, Mapping):
        raise LocalRouteReportError("task usage must be an object")
    expected = {"input_tokens", "output_tokens", "total_tokens"}
    if set(value) != expected:
        raise LocalRouteReportError("task usage has unexpected fields")
    result: dict[str, int | None] = {}
    for key in expected:
        count = value[key]
        result[key] = None if count is None else _nonnegative_int(count, key)
    input_tokens = result["input_tokens"]
    output_tokens = result["output_tokens"]
    total_tokens = result["total_tokens"]
    if (
        total_tokens is not None
        and input_tokens is not None
        and output_tokens is not None
        and total_tokens != input_tokens + output_tokens
    ):
        raise LocalRouteReportError("total_tokens must equal input plus output tokens")
    return result


def _task_report(task: object, *, allow_runner_aliases: bool = False) -> dict[str, Any]:
    if not isinstance(task, Mapping):
        raise LocalRouteReportError("each task must be an object")
    required = {
        "task_id",
        "outcome",
        "passed",
        "stop_reason",
        "output_sha256",
        "duration_ms",
        "usage",
    }
    keys = set(task)
    if allow_runner_aliases:
        prompt_key = "prompt_sha256" if "prompt_sha256" in keys else "prompt_hash"
        count_key = (
            "output_characters" if "output_characters" in keys else "character_count"
        )
    else:
        prompt_key = "prompt_sha256"
        count_key = "output_characters"
    expected = required | {prompt_key, count_key}
    if "error_code" in keys or not allow_runner_aliases:
        expected.add("error_code")
    if set(task) != expected:
        raise LocalRouteReportError("task has missing or unexpected fields")

    task_id = task["task_id"]
    if not isinstance(task_id, str) or not _TASK_ID.fullmatch(task_id):
        raise LocalRouteReportError("task_id must be a short lowercase identifier")
    prompt_sha256 = task[prompt_key]
    if not isinstance(prompt_sha256, str) or not _HEX_SHA256.fullmatch(prompt_sha256):
        raise LocalRouteReportError("prompt_sha256 must be a lowercase SHA-256 digest")
    output_sha256 = task["output_sha256"]
    if output_sha256 is not None and (
        not isinstance(output_sha256, str) or not _HEX_SHA256.fullmatch(output_sha256)
    ):
        raise LocalRouteReportError("output_sha256 must be a digest or null")
    output_characters = _nonnegative_int(task[count_key], "output_characters")
    if output_sha256 is None and output_characters != 0:
        raise LocalRouteReportError("missing output digest requires zero characters")
    outcome = task["outcome"]
    if not isinstance(outcome, str) or outcome not in OUTCOMES:
        raise LocalRouteReportError("unsupported task outcome")
    passed = task["passed"]
    if not isinstance(passed, bool) or passed is not (outcome == "passed"):
        raise LocalRouteReportError("task pass flag does not match its outcome")
    stop_reason = task["stop_reason"]
    if not isinstance(stop_reason, str) or stop_reason not in STOP_REASONS:
        raise LocalRouteReportError("unsupported stop reason")
    error_code = task.get("error_code")
    if error_code is not None and (
        not isinstance(error_code, str) or error_code not in ERROR_CODES
    ):
        raise LocalRouteReportError("unsupported sanitized error code")
    if outcome in {"error", "timed_out"} and error_code is None:
        raise LocalRouteReportError("error and timeout outcomes require an error code")

    return {
        "task_id": task_id,
        "prompt_sha256": prompt_sha256,
        "outcome": outcome,
        "passed": passed,
        "stop_reason": stop_reason,
        "output_sha256": output_sha256,
        "output_characters": output_characters,
        "duration_ms": _nonnegative_int(task["duration_ms"], "task duration_ms"),
        "usage": _usage(task["usage"]),
        "error_code": error_code,
    }


def build_local_route_report(
    *,
    model_id: str,
    started_at: str,
    finished_at: str,
    tasks: Sequence[Mapping[str, Any]],
    candidate_count: int = 1,
    fallback_count: int = 0,
    profile_persisted: bool = False,
) -> dict[str, Any]:
    """Return an allowlisted report containing only digests and safe metadata.

    Task rows accept digests and counts, never prompt/output text or exception
    messages. The endpoint URL is omitted; only its verified scope is recorded.
    """
    start_text, start = _timestamp(started_at, "started_at")
    finish_text, finish = _timestamp(finished_at, "finished_at")
    if finish < start:
        raise LocalRouteReportError("finished_at must not precede started_at")
    if isinstance(tasks, (str, bytes)) or not isinstance(tasks, Sequence) or not tasks:
        raise LocalRouteReportError("tasks must be a nonempty sequence")
    if not isinstance(profile_persisted, bool):
        raise LocalRouteReportError("profile_persisted must be boolean")

    report_tasks = [_task_report(task, allow_runner_aliases=True) for task in tasks]
    passed = sum(task["passed"] for task in report_tasks)
    report = {
        "schema_version": LOCAL_ROUTE_REPORT_SCHEMA_VERSION,
        "started_at": start_text,
        "finished_at": finish_text,
        "duration_ms": int((finish - start).total_seconds() * 1000),
        "model": {
            "id": _safe_model_id(model_id),
            "id_source": "endpoint_self_reported",
        },
        "route": {
            "endpoint_scope": "loopback",
            "candidate_count": _nonnegative_int(candidate_count, "candidate_count"),
            "fallback_count": _nonnegative_int(fallback_count, "fallback_count"),
            "profile": "ephemeral",
            "profile_persisted": profile_persisted,
            "concurrency": 1,
        },
        "tasks": report_tasks,
        "summary": {
            "task_count": len(report_tasks),
            "passed": passed,
            "failed": len(report_tasks) - passed,
            "pass_rate": round(passed / len(report_tasks), 4),
        },
    }
    return validate_local_route_report(report)


def validate_local_route_report(report: object) -> dict[str, Any]:
    """Validate report structure, strict allowlists, and all privacy-safe fields."""
    if not isinstance(report, Mapping) or set(report) != _SCHEMA_KEYS:
        raise LocalRouteReportError("report has missing or unexpected fields")
    if (
        isinstance(report["schema_version"], bool)
        or report["schema_version"] != LOCAL_ROUTE_REPORT_SCHEMA_VERSION
    ):
        raise LocalRouteReportError("unsupported report schema version")
    _, started = _timestamp(report["started_at"], "started_at")
    _, finished = _timestamp(report["finished_at"], "finished_at")
    expected_duration = int((finished - started).total_seconds() * 1000)
    actual_duration = _nonnegative_int(report["duration_ms"], "duration_ms")
    if finished < started or actual_duration != expected_duration:
        raise LocalRouteReportError("report duration does not match its timestamps")

    model = report["model"]
    if not isinstance(model, Mapping) or set(model) != {"id", "id_source"}:
        raise LocalRouteReportError("model has missing or unexpected fields")
    if _safe_model_id(model["id"]) != model["id"]:
        raise LocalRouteReportError("model ID is not in redacted canonical form")
    if model["id_source"] != "endpoint_self_reported":
        raise LocalRouteReportError("model ID source must be endpoint self-reported")

    route = report["route"]
    route_keys = {
        "endpoint_scope",
        "candidate_count",
        "fallback_count",
        "profile",
        "profile_persisted",
        "concurrency",
    }
    if not isinstance(route, Mapping) or set(route) != route_keys:
        raise LocalRouteReportError("route has missing or unexpected fields")
    if route["endpoint_scope"] != "loopback":
        raise LocalRouteReportError("endpoint scope must be loopback")
    if _nonnegative_int(route["candidate_count"], "candidate_count") < 1:
        raise LocalRouteReportError("local route smoke requires at least one candidate")
    _nonnegative_int(route["fallback_count"], "fallback_count")
    if route["profile"] != "ephemeral" or not isinstance(
        route["profile_persisted"], bool
    ):
        raise LocalRouteReportError("route profile must be marked ephemeral")
    if route["profile_persisted"]:
        raise LocalRouteReportError("ephemeral profile cannot be marked persisted")
    if _nonnegative_int(route["concurrency"], "concurrency") != 1:
        raise LocalRouteReportError("local route smoke concurrency must be one")

    tasks = report["tasks"]
    if not isinstance(tasks, list) or not tasks:
        raise LocalRouteReportError("report must contain a nonempty task list")
    normalized_tasks = [_task_report(task) for task in tasks]
    task_ids = [task["task_id"] for task in normalized_tasks]
    if len(set(task_ids)) != len(task_ids):
        raise LocalRouteReportError("task IDs must be unique")

    summary = report["summary"]
    if not isinstance(summary, Mapping) or set(summary) != {
        "task_count",
        "passed",
        "failed",
        "pass_rate",
    }:
        raise LocalRouteReportError("summary has missing or unexpected fields")
    count = len(normalized_tasks)
    passed = sum(task["passed"] for task in normalized_tasks)
    _nonnegative_int(summary["task_count"], "summary task_count")
    _nonnegative_int(summary["passed"], "summary passed")
    _nonnegative_int(summary["failed"], "summary failed")
    pass_rate = summary["pass_rate"]
    if (
        summary["task_count"] != count
        or summary["passed"] != passed
        or summary["failed"] != count - passed
        or isinstance(pass_rate, bool)
        or not isinstance(pass_rate, int | float)
        or not math.isfinite(pass_rate)
        or pass_rate != round(passed / count, 4)
    ):
        raise LocalRouteReportError("summary does not match task outcomes")
    try:
        json.dumps(report, allow_nan=False)
    except (TypeError, ValueError) as error:
        raise LocalRouteReportError("report must contain JSON-safe values") from error
    normalized_report = dict(report)
    normalized_report["tasks"] = normalized_tasks
    return normalized_report


def write_local_route_report(report: object, path: Path) -> None:
    """Validate and write pretty JSON without creating unexpected directories."""
    validated = validate_local_route_report(report)
    path.write_text(
        json.dumps(validated, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
