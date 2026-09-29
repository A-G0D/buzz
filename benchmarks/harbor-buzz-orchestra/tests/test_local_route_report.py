"""Privacy and schema checks for local route smoke evidence."""

import json
from copy import deepcopy
from pathlib import Path

import pytest

from harbor_buzz_orchestra.local_route_report import (
    LocalRouteReportError,
    build_local_route_report,
    validate_local_route_report,
    write_local_route_report,
)


def _task(**overrides):
    task = {
        "task_id": "exact_json",
        "prompt_sha256": "a" * 64,
        "outcome": "passed",
        "passed": True,
        "stop_reason": "end_turn",
        "output_sha256": "b" * 64,
        "output_characters": 14,
        "duration_ms": 920,
        "usage": {"input_tokens": 10, "output_tokens": 7, "total_tokens": 17},
        "error_code": None,
    }
    task.update(overrides)
    return task


def _report(**overrides):
    arguments = {
        "model_id": "/Users/private-user/models/fast",
        "started_at": "2026-09-27T10:00:00-05:00",
        "finished_at": "2026-09-27T10:00:01-05:00",
        "tasks": [_task()],
    }
    arguments.update(overrides)
    return build_local_route_report(**arguments)


def test_report_is_allowlisted_and_redacts_model_path(tmp_path: Path):
    report = _report()
    encoded = json.dumps(report)

    assert report["model"] == {"id": "fast", "id_source": "endpoint_self_reported"}
    assert report["route"] == {
        "endpoint_scope": "loopback",
        "candidate_count": 1,
        "fallback_count": 0,
        "profile": "ephemeral",
        "profile_persisted": False,
        "concurrency": 1,
    }
    assert report["tasks"][0]["prompt_sha256"] == "a" * 64
    assert "private-user" not in encoded
    assert "prompt" not in report["tasks"][0]
    assert "output" not in report["tasks"][0]
    assert validate_local_route_report(report) == report

    path = tmp_path / "route-report.json"
    write_local_route_report(report, path)
    assert json.loads(path.read_text(encoding="utf-8")) == report


def test_rows_reject_raw_text_and_uncontrolled_exception_details():
    with pytest.raises(LocalRouteReportError, match="unexpected fields"):
        _report(tasks=[_task(prompt="private synthetic prompt")])

    with pytest.raises(LocalRouteReportError, match="sanitized error code"):
        _report(
            tasks=[
                _task(
                    outcome="error",
                    passed=False,
                    error_code="provider said api_key=secret-value",
                )
            ]
        )


def test_report_rejects_private_endpoint_and_inconsistent_outcomes():
    report = _report()
    report["route"]["endpoint_url"] = "http://127.0.0.1:8000/v1"
    with pytest.raises(LocalRouteReportError, match="unexpected fields"):
        validate_local_route_report(report)

    with pytest.raises(LocalRouteReportError, match="pass flag"):
        _report(tasks=[_task(outcome="failed", passed=True)])

    with pytest.raises(LocalRouteReportError, match="unsupported characters"):
        _report(model_id="sk-12345678901234567890")


def test_fractional_timestamps_produce_a_consistent_millisecond_duration():
    report = _report(
        started_at="2026-09-27T10:00:00.0009Z",
        finished_at="2026-09-27T10:00:00.0011Z",
    )

    assert report["started_at"] == "2026-09-27T10:00:00.000Z"
    assert report["finished_at"] == "2026-09-27T10:00:00.001Z"
    assert report["duration_ms"] == 1


def test_errors_require_only_a_sanitized_code_and_usage_can_be_missing():
    report = _report(
        tasks=[
            _task(
                outcome="error",
                passed=False,
                stop_reason="error",
                output_sha256=None,
                output_characters=0,
                usage=None,
                error_code="provider_error",
            )
        ]
    )
    assert report["tasks"][0]["usage"] is None
    assert report["summary"] == {
        "task_count": 1,
        "passed": 0,
        "failed": 1,
        "pass_rate": 0.0,
    }


def test_runner_hash_and_character_count_aliases_are_normalized():
    runner_task = _task()
    runner_task["prompt_hash"] = runner_task.pop("prompt_sha256")
    runner_task["character_count"] = runner_task.pop("output_characters")
    report = _report(tasks=[runner_task])

    assert report["tasks"][0]["prompt_sha256"] == "a" * 64
    assert report["tasks"][0]["output_characters"] == 14


def test_write_validates_before_creating_report(tmp_path: Path):
    report = _report()
    broken = deepcopy(report)
    broken["tasks"][0]["output_characters"] = -1
    path = tmp_path / "broken.json"

    with pytest.raises(LocalRouteReportError):
        write_local_route_report(broken, path)
    assert not path.exists()
