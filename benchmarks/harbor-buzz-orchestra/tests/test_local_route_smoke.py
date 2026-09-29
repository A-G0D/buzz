"""Pure safety and model-selection checks for the local ACP runner."""

import pytest

from harbor_buzz_orchestra.local_route_smoke import (
    LocalRouteSmokeError,
    _Capture,
    attempt_task_id,
    iter_local_route_attempts,
    select_model_id,
    validate_loopback_base_url,
    validate_repetitions,
)
from harbor_buzz_orchestra.local_route_suite import LOCAL_ROUTE_TASKS


@pytest.mark.parametrize(
    ("url", "expected"),
    [
        ("http://127.0.0.1:8000/v1", "http://127.0.0.1:8000/v1"),
        ("http://localhost:1234/api/v1/", "http://localhost:1234/api/v1"),
        ("http://[::1]:8000/v1", "http://[::1]:8000/v1"),
    ],
)
def test_loopback_base_url_is_normalized(url, expected):
    assert validate_loopback_base_url(url) == expected


@pytest.mark.parametrize(
    "url",
    [
        "https://127.0.0.1:8000/v1",
        "http://example.com:8000/v1",
        "http://127.0.0.1.nip.io:8000/v1",
        "http://127.0.0.1/v1",
        "http://127.0.0.1:8000/v1?key=secret",
        "http://user:pass@127.0.0.1:8000/v1",
        "http://127.0.0.1:8000/v1/chat/completions",
    ],
)
def test_nonlocal_or_ambiguous_base_url_is_rejected(url):
    with pytest.raises(ValueError):
        validate_loopback_base_url(url)


def test_single_reported_model_is_selected_but_ambiguous_list_requires_choice():
    assert select_model_id(["local-model"]) == "local-model"
    assert select_model_id(["first", "second"], "second") == "second"
    with pytest.raises(LocalRouteSmokeError, match="choose a model"):
        select_model_id(["first", "second"])
    with pytest.raises(LocalRouteSmokeError, match="not in"):
        select_model_id(["first"], "unreported")


def test_capture_ignores_thoughts_and_keeps_only_visible_text_and_usage():
    capture = _Capture()
    capture.observe(
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"type": "text", "text": "private reasoning"},
                }
            },
        }
    )
    capture.observe(
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "323"},
                }
            },
        }
    )
    capture.observe(
        {
            "jsonrpc": "2.0",
            "method": "_goose/unstable/session/update",
            "params": {
                "update": {
                    "sessionUpdate": "usage_update",
                    "accumulatedInputTokens": 7,
                    "accumulatedOutputTokens": 3,
                },
            },
        }
    )
    assert "".join(capture.output_parts) == "323"
    assert capture.output_characters == 3
    assert capture.latest_usage == {
        "input_tokens": 7,
        "output_tokens": 3,
        "total_tokens": 10,
    }


def test_capture_rejects_unexpected_tool_call():
    with pytest.raises(LocalRouteSmokeError, match="tool"):
        _Capture().observe(
            {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"update": {"sessionUpdate": "tool_call"}},
            }
        )


@pytest.mark.parametrize("count", [1, 2, 5])
def test_repetition_limit_accepts_bounded_counts(count):
    assert validate_repetitions(count) == count


@pytest.mark.parametrize("count", [0, 6, -1, True, "2"])
def test_repetition_limit_rejects_unbounded_or_wrong_types(count):
    with pytest.raises(ValueError, match="repetitions"):
        validate_repetitions(count)


def test_repeated_attempts_are_sequential_and_have_unique_report_ids():
    attempts = list(iter_local_route_attempts(2))
    assert len(attempts) == 2 * len(LOCAL_ROUTE_TASKS)
    assert [attempt for _, attempt in attempts] == [1] * len(LOCAL_ROUTE_TASKS) + [
        2
    ] * len(LOCAL_ROUTE_TASKS)
    report_ids = [
        attempt_task_id(task.task_id, attempt, 2) for task, attempt in attempts
    ]
    assert len(set(report_ids)) == len(report_ids)
    assert attempt_task_id("arithmetic", 1, 1) == "arithmetic"
    assert attempt_task_id("arithmetic", 2, 2) == "arithmetic_r2"
