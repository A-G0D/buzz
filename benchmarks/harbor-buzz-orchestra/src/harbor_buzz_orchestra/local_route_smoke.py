"""Run fixed synthetic tasks through a single loopback Buzz Agent route."""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from .local_route_report import build_local_route_report, write_local_route_report
from .local_route_suite import LOCAL_ROUTE_TASKS, grade_local_route_task

MAX_MODEL_LIST_BYTES = 1_048_576
MAX_ACP_FRAME_BYTES = 4 * 1024 * 1024
MAX_OUTPUT_CHARACTERS = 65_536
MAX_FRAMES_PER_TASK = 512
MAX_OUTPUT_TOKENS = 4096
MAX_TIMEOUT_SECONDS = 120
MAX_REPETITIONS = 5
MODEL_ID_RE = re.compile(r"^[^\x00-\x20\x7f]{1,256}$")
ROUTE_CANDIDATE_ID = "local-smoke"


class LocalRouteSmokeError(RuntimeError):
    """A bounded, non-sensitive failure classification for the local runner."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def validate_loopback_base_url(value: str) -> str:
    """Accept only a plain HTTP API base ending in /v1 on a loopback host."""
    if not isinstance(value, str) or len(value) > 2048:
        raise ValueError("base URL is missing or too long")
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise ValueError("base URL is malformed") from error
    host = (parsed.hostname or "").lower().rstrip(".")
    is_loopback = host == "localhost"
    if not is_loopback:
        try:
            is_loopback = ipaddress.ip_address(host).is_loopback
        except ValueError:
            is_loopback = False
    if (
        parsed.scheme != "http"
        or not is_loopback
        or port is None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path.rstrip("/").split("/")[-1] != "v1"
    ):
        raise ValueError("base URL must be an HTTP loopback API URL ending in /v1")
    normalized_path = parsed.path.rstrip("/")
    return urlunsplit(("http", parsed.netloc, normalized_path, "", ""))


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, file_pointer, code, message, headers, new_url):
        return None


def fetch_model_ids(base_url: str, timeout_seconds: int = 5) -> list[str]:
    """Read one bounded, proxy-free, non-redirecting local model catalog."""
    safe_base = validate_loopback_base_url(base_url)
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), _NoRedirect()
    )
    request = urllib.request.Request(
        f"{safe_base}/models", headers={"Accept": "application/json"}
    )
    try:
        with opener.open(request, timeout=timeout_seconds) as response:
            if response.status != 200:
                raise LocalRouteSmokeError("endpoint_error", "model catalog was not available")
            raw = response.read(MAX_MODEL_LIST_BYTES + 1)
    except LocalRouteSmokeError:
        raise
    except (OSError, urllib.error.URLError, TimeoutError) as error:
        raise LocalRouteSmokeError("endpoint_error", "loopback model catalog request failed") from error
    if len(raw) > MAX_MODEL_LIST_BYTES:
        raise LocalRouteSmokeError("endpoint_error", "model catalog exceeded the size limit")
    try:
        payload = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise LocalRouteSmokeError("endpoint_error", "model catalog was not valid JSON") from error
    rows = payload.get("data") if isinstance(payload, dict) else None
    if not isinstance(rows, list) or not 1 <= len(rows) <= 256:
        raise LocalRouteSmokeError("endpoint_error", "model catalog must contain 1 to 256 entries")
    ids = [row.get("id") for row in rows if isinstance(row, dict)]
    if len(ids) != len(rows) or any(not isinstance(item, str) or not MODEL_ID_RE.fullmatch(item) for item in ids):
        raise LocalRouteSmokeError("endpoint_error", "model catalog contains an invalid ID")
    return ids


def select_model_id(model_ids: list[str], requested: str | None = None) -> str:
    """Select an exact endpoint-reported model, requiring explicit choice if ambiguous."""
    if requested is None:
        if len(model_ids) != 1:
            raise LocalRouteSmokeError("model_not_listed", "choose a model because the endpoint lists more than one")
        return model_ids[0]
    if requested not in model_ids:
        raise LocalRouteSmokeError("model_not_listed", "requested model is not in the local endpoint catalog")
    return requested


def prompt_sha256(prompt: str) -> str:
    return hashlib.sha256(prompt.encode("utf-8")).hexdigest()


def validate_repetitions(value: int) -> int:
    """Bound repeated local calls before any endpoint or model request."""
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= MAX_REPETITIONS:
        raise ValueError(f"repetitions must be between 1 and {MAX_REPETITIONS}")
    return value


def attempt_task_id(task_id: str, attempt: int, repetitions: int) -> str:
    """Keep single-run IDs stable and make repeated attempt IDs unique."""
    if repetitions == 1:
        return task_id
    return f"{task_id}_r{attempt}"


def iter_local_route_attempts(repetitions: int):
    """Yield every task in repeat order; each is dispatched in a fresh process."""
    for attempt in range(1, repetitions + 1):
        for task in LOCAL_ROUTE_TASKS:
            yield task, attempt


@dataclass
class _Capture:
    output_parts: list[str] = field(default_factory=list)
    output_characters: int = 0
    latest_usage: dict[str, int] | None = None
    frame_count: int = 0

    def observe(self, frame: dict[str, Any]) -> None:
        self.frame_count += 1
        if self.frame_count > MAX_FRAMES_PER_TASK:
            raise LocalRouteSmokeError("protocol_error", "agent exceeded the frame limit")
        method = frame.get("method")
        params = frame.get("params")
        if not isinstance(params, dict):
            return
        update = params.get("update")
        if not isinstance(update, dict):
            return
        kind = update.get("sessionUpdate")
        if kind in ("tool_call", "tool_call_update"):
            raise LocalRouteSmokeError("unexpected_tool_call", "synthetic route tried to call a tool")
        if kind == "agent_message_chunk":
            content = update.get("content")
            text = content.get("text") if isinstance(content, dict) else None
            if isinstance(text, str):
                self.output_characters += len(text)
                if self.output_characters > MAX_OUTPUT_CHARACTERS:
                    raise LocalRouteSmokeError("protocol_error", "visible output exceeded the size limit")
                self.output_parts.append(text)
        if method == "_goose/unstable/session/update" and kind == "usage_update":
            input_tokens = update.get("accumulatedInputTokens")
            output_tokens = update.get("accumulatedOutputTokens")
            total_tokens = update.get("accumulatedTotalTokens")
            usage = {
                key: value
                for key, value in (
                    ("input_tokens", input_tokens),
                    ("output_tokens", output_tokens),
                    ("total_tokens", total_tokens),
                )
                if isinstance(value, int) and not isinstance(value, bool) and value >= 0
            }
            if "total_tokens" not in usage and {"input_tokens", "output_tokens"} <= usage.keys():
                usage["total_tokens"] = usage["input_tokens"] + usage["output_tokens"]
            if usage:
                self.latest_usage = usage


def _read_frames(process: subprocess.Popen[bytes], output_queue: queue.Queue[bytes | None | Exception]) -> None:
    assert process.stdout is not None
    try:
        while True:
            line = process.stdout.readline(MAX_ACP_FRAME_BYTES + 2)
            if not line:
                output_queue.put(None)
                return
            if len(line) > MAX_ACP_FRAME_BYTES + 1 or not line.endswith(b"\n"):
                output_queue.put(LocalRouteSmokeError("protocol_error", "agent frame exceeded the size limit"))
                return
            output_queue.put(line)
    except OSError as error:
        output_queue.put(error)


def _send(process: subprocess.Popen[bytes], message: dict[str, Any]) -> None:
    if process.stdin is None:
        raise LocalRouteSmokeError("process_exit", "agent input was unavailable")
    try:
        frame = json.dumps(message, separators=(",", ":")).encode("utf-8") + b"\n"
        process.stdin.write(frame)
        process.stdin.flush()
    except (BrokenPipeError, OSError) as error:
        raise LocalRouteSmokeError("process_exit", "agent closed its input") from error


def _receive_response(
    process: subprocess.Popen[bytes],
    output_queue: queue.Queue[bytes | None | Exception],
    request_id: int,
    deadline: float,
    capture: _Capture | None = None,
) -> dict[str, Any]:
    frames = 0
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise LocalRouteSmokeError("process_timeout", "agent exceeded the bounded run time")
        try:
            item = output_queue.get(timeout=remaining)
        except queue.Empty as error:
            raise LocalRouteSmokeError("process_timeout", "agent exceeded the bounded run time") from error
        if item is None:
            raise LocalRouteSmokeError("process_exit", "agent exited before replying")
        if isinstance(item, Exception):
            if isinstance(item, LocalRouteSmokeError):
                raise item
            raise LocalRouteSmokeError("protocol_error", "agent output could not be read") from item
        frames += 1
        if frames > MAX_FRAMES_PER_TASK:
            raise LocalRouteSmokeError("protocol_error", "agent exceeded the frame limit")
        try:
            frame = json.loads(item)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise LocalRouteSmokeError("protocol_error", "agent emitted invalid JSON-RPC") from error
        if not isinstance(frame, dict) or frame.get("jsonrpc") != "2.0":
            raise LocalRouteSmokeError("protocol_error", "agent emitted an invalid JSON-RPC frame")
        if capture is not None:
            capture.observe(frame)
        if frame.get("method") and "id" in frame:
            raise LocalRouteSmokeError("unexpected_tool_call", "agent requested an unexpected client action")
        if frame.get("id") != request_id:
            continue
        if "error" in frame:
            raise LocalRouteSmokeError("provider_error", "Buzz Agent returned a failed request")
        result = frame.get("result")
        if not isinstance(result, dict):
            raise LocalRouteSmokeError("protocol_error", "agent response did not contain a result")
        return result


def _run_one_task(
    *,
    binary: Path,
    base_url: str,
    model_id: str,
    output_tokens: int,
    timeout_seconds: int,
    max_token_recoveries: int,
    prompt: str,
) -> tuple[str, dict[str, int] | None, int, str | None]:
    """Run one fixed prompt in a fresh, no-tools, one-candidate ACP process."""
    candidate = {
        "id": ROUTE_CANDIDATE_ID,
        "provider": "openai",
        "model": model_id,
        "data_location": "local",
        "prompt_addendum": "",
    }
    profile = {
        "version": 1,
        "data_policy": "local-only",
        "preference_order": [ROUTE_CANDIDATE_ID],
        "candidates": [candidate],
    }
    with tempfile.TemporaryDirectory(prefix="buzz-local-route-") as temporary:
        isolated_home = Path(temporary)
        env = {
            "PATH": os.environ.get("PATH", os.defpath),
            "HOME": str(isolated_home),
            "XDG_CONFIG_HOME": str(isolated_home / ".config"),
            "TMPDIR": str(isolated_home),
            "LANG": "C.UTF-8",
            "BUZZ_AGENT_PROVIDER": "openai",
            "OPENAI_COMPAT_API_KEY": "local-probe",
            "OPENAI_COMPAT_MODEL": model_id,
            "OPENAI_COMPAT_BASE_URL": base_url,
            "OPENAI_COMPAT_API": "chat",
            "BUZZ_AGENT_ROUTE_PROFILE_JSON": json.dumps(profile, separators=(",", ":")),
            "BUZZ_AGENT_MAX_OUTPUT_TOKENS": str(output_tokens),
            "BUZZ_AGENT_MAX_CONTEXT_TOKENS": str(max(8192, output_tokens + 1)),
            "BUZZ_AGENT_MAX_TOKEN_RECOVERIES": str(max_token_recoveries),
            "BUZZ_AGENT_LLM_TIMEOUT_SECS": str(timeout_seconds),
            "BUZZ_AGENT_MAX_SESSIONS": "1",
            "BUZZ_AGENT_MAX_PARALLEL_TOOLS": "1",
            "BUZZ_AGENT_NO_HINTS": "1",
            "BUZZ_AGENT_PROMPT_CACHING": "0",
            "BUZZ_AGENT_REVIEW_ONLY": "1",
        }
        try:
            process = subprocess.Popen(
                [str(binary)],
                cwd=isolated_home,
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                bufsize=0,
            )
        except OSError as error:
            raise LocalRouteSmokeError("process_exit", "Buzz Agent could not be started") from error
        frames: queue.Queue[bytes | None | Exception] = queue.Queue(maxsize=64)
        threading.Thread(target=_read_frames, args=(process, frames), daemon=True).start()
        capture = _Capture()
        process_timeout = timeout_seconds * (max_token_recoveries + 1) + 15
        deadline = time.monotonic() + process_timeout
        try:
            _send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {"protocolVersion": 2, "clientCapabilities": {}},
                },
            )
            _receive_response(process, frames, 1, deadline)
            _send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/new",
                    "params": {
                        "cwd": str(isolated_home),
                        "mcpServers": [],
                        "systemPrompt": "Answer the fixed synthetic evaluation directly. Do not use tools.",
                    },
                },
            )
            new_session = _receive_response(process, frames, 2, deadline)
            session_id = new_session.get("sessionId")
            if not isinstance(session_id, str) or not session_id:
                raise LocalRouteSmokeError("protocol_error", "Buzz Agent did not create a session")
            _send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": prompt}],
                    },
                },
            )
            result = _receive_response(process, frames, 3, deadline, capture)
            stop_reason = result.get("stopReason") if isinstance(result.get("stopReason"), str) else "unknown"
            output = "".join(capture.output_parts)
            return output, capture.latest_usage, capture.output_characters, stop_reason
        finally:
            if process.stdin is not None:
                try:
                    process.stdin.close()
                except OSError:
                    pass
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=2)


def run_local_route_smoke(
    *,
    binary: Path,
    base_url: str,
    model: str | None,
    output_tokens: int = 512,
    timeout_seconds: int = 30,
    max_token_recoveries: int = 3,
    repetitions: int = 1,
) -> dict[str, Any]:
    """Run the fixed suite through one endpoint-reported local model."""
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise LocalRouteSmokeError("process_exit", "Buzz Agent binary is missing or not executable")
    if not 1 <= output_tokens <= MAX_OUTPUT_TOKENS:
        raise ValueError(f"output token cap must be between 1 and {MAX_OUTPUT_TOKENS}")
    if not 1 <= timeout_seconds <= MAX_TIMEOUT_SECONDS:
        raise ValueError(f"timeout must be between 1 and {MAX_TIMEOUT_SECONDS} seconds")
    if not 0 <= max_token_recoveries <= 5:
        raise ValueError("token recoveries must be between 0 and 5")
    repetitions = validate_repetitions(repetitions)
    safe_base_url = validate_loopback_base_url(base_url)
    selected_model = select_model_id(fetch_model_ids(safe_base_url), model)
    started_at = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    report_tasks: list[dict[str, Any]] = []
    for task, attempt in iter_local_route_attempts(repetitions):
        task_started = time.monotonic()
        outcome = "error"
        passed = False
        stop_reason = "error"
        output_digest = None
        output_chars = 0
        usage = None
        error_code = None
        try:
            output, usage, output_chars, stop_reason = _run_one_task(
                binary=binary,
                base_url=safe_base_url,
                model_id=selected_model,
                output_tokens=output_tokens,
                timeout_seconds=timeout_seconds,
                max_token_recoveries=max_token_recoveries,
                prompt=task.prompt,
            )
            if output:
                output_digest = hashlib.sha256(output.encode("utf-8")).hexdigest()
            if stop_reason == "end_turn" and output:
                passed = grade_local_route_task(task.task_id, output)
                outcome = "passed" if passed else "failed"
            else:
                outcome = "failed"
                error_code = "missing_output" if not output else None
        except LocalRouteSmokeError as error:
            error_code = error.code
            outcome = "timed_out" if error.code == "process_timeout" else "error"
            stop_reason = "error"
        report_tasks.append(
            {
                "task_id": attempt_task_id(task.task_id, attempt, repetitions),
                "prompt_sha256": prompt_sha256(task.prompt),
                "outcome": outcome,
                "passed": passed,
                "stop_reason": stop_reason,
                "output_sha256": output_digest,
                "output_characters": output_chars,
                "duration_ms": round((time.monotonic() - task_started) * 1000),
                "usage": usage,
                "error_code": error_code,
            }
        )
    finished_at = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    return build_local_route_report(
        model_id=selected_model,
        started_at=started_at,
        finished_at=finished_at,
        tasks=report_tasks,
        candidate_count=1,
        fallback_count=0,
        profile_persisted=False,
    )


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="path to a built buzz-agent executable")
    parser.add_argument("--base-url", required=True, help="loopback HTTP OpenAI-compatible API base ending in /v1")
    parser.add_argument("--model", help="exact model ID listed by GET /v1/models; auto-selected only if exactly one is listed")
    parser.add_argument("--output", type=Path, required=True, help="destination for the redacted JSON report")
    parser.add_argument("--max-output-tokens", type=int, default=512, help="per-provider-call output cap (1-4096)")
    parser.add_argument("--timeout-seconds", type=int, default=30, help="per-provider-call timeout (1-120)")
    parser.add_argument("--max-token-recoveries", type=int, default=3, help="same-candidate recoveries (0-5)")
    parser.add_argument("--repetitions", type=int, default=1, help="sequential full-suite repeats (1-5)")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        report = run_local_route_smoke(
            binary=args.binary.expanduser().resolve(),
            base_url=args.base_url,
            model=args.model,
            output_tokens=args.max_output_tokens,
            timeout_seconds=args.timeout_seconds,
            max_token_recoveries=args.max_token_recoveries,
            repetitions=args.repetitions,
        )
        write_local_route_report(report, args.output.expanduser())
    except (LocalRouteSmokeError, ValueError) as error:
        code = error.code if isinstance(error, LocalRouteSmokeError) else "invalid_configuration"
        print(f"local route smoke failed: {code}", file=sys.stderr)
        return 2
    summary = report["summary"]
    print(
        f"Local route sample: {summary['passed']}/{summary['task_count']} passed; "
        f"report: {args.output}"
    )
    return 0 if summary["passed"] == summary["task_count"] else 1
