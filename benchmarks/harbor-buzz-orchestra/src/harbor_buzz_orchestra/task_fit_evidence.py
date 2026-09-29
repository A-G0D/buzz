"""Build bounded, condition-scoped evidence from completed Harbor jobs.

The report describes one exact single-agent condition. It is deliberately
advisory: Buzz routing does not consume benchmark reports yet, and a benchmark
score is not a general rating of a provider or model.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .manifest import AgentClass, ExperimentManifest, ManifestError

HARBOR_LOCKED_VERSION = "0.16.1"
TASK_CLASS_TAXONOMY_VERSION = "operator-defined-v1"
TASK_FIT_POLICY_VERSION = "task-fit-outcomes-v1"
TASK_CLASS_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{0,63}$")
REPORT_FILENAME = "route-fit-evidence.json"


class TaskFitEvidenceError(ValueError):
    """Raised when a Harbor job cannot support a complete scoped report."""


def validate_task_class_id(task_class: str) -> str:
    """Validate an operator-provided task class identifier."""
    if not isinstance(task_class, str) or not TASK_CLASS_RE.fullmatch(task_class):
        raise TaskFitEvidenceError("task class must use a short lowercase ID")
    return task_class


def validate_single_candidate(manifest: ExperimentManifest) -> AgentClass:
    """Return the sole candidate and its declared provider, rejecting teams."""
    if len(manifest.roster) != 1:
        raise TaskFitEvidenceError(
            "task-fit reports require a single-agent manifest; team outcomes "
            "cannot be attributed to one model"
        )
    candidate = manifest.roster[0]
    if candidate.kind != "orchestrator" or candidate.count != 1:
        raise TaskFitEvidenceError(
            "task-fit reports require one solo orchestrator agent"
        )

    return candidate


def verified_prompt_sha256(
    manifest: ExperimentManifest, artifact_root: Path
) -> str:
    """Resolve and verify the sole candidate's prompt within the harness root."""
    candidate = validate_single_candidate(manifest)
    try:
        root = artifact_root.resolve(strict=True)
        prompt_path = (root / candidate.prompt.path).resolve(strict=True)
    except OSError as error:
        raise TaskFitEvidenceError(
            f"cannot resolve manifest prompt: {error}"
        ) from error
    if not prompt_path.is_relative_to(root) or not prompt_path.is_file():
        raise TaskFitEvidenceError(
            "manifest prompt must be a file within the harness root"
        )
    try:
        actual = _sha256_file(prompt_path)
    except OSError as error:
        raise TaskFitEvidenceError(f"cannot hash manifest prompt: {error}") from error
    if actual != candidate.prompt.sha256:
        raise TaskFitEvidenceError("manifest prompt content does not match its SHA-256")
    return actual


def lower_wilson_95(successes: int, samples: int) -> float | None:
    """One-sided 95% Wilson lower bound for binary full-reward success."""
    if samples <= 0 or successes < 0 or successes > samples:
        return None
    z = 1.6448536269514722
    rate = successes / samples
    z2 = z * z
    denominator = 1 + z2 / samples
    center = rate + z2 / (2 * samples)
    margin = z * math.sqrt(rate * (1 - rate) / samples + z2 / (4 * samples**2))
    return round(max(0.0, (center - margin) / denominator), 6)


def _read_json_bytes(contents: bytes, label: str) -> dict[str, Any]:
    try:
        value = json.loads(contents)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise TaskFitEvidenceError(f"cannot parse {label}: {error}") from error
    if not isinstance(value, dict):
        raise TaskFitEvidenceError(f"{label} must be a JSON object")
    return value


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _parse_utc_timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str) or not value:
        raise TaskFitEvidenceError(f"Harbor job {label} timestamp is missing")
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError as error:
        raise TaskFitEvidenceError(
            f"Harbor job {label} timestamp is invalid"
        ) from error
    if parsed.tzinfo is None:
        raise TaskFitEvidenceError(
            f"Harbor job {label} timestamp must include a timezone"
        )
    return parsed.astimezone(UTC)


def _result_reward(result: dict[str, Any], trial_name: str) -> float:
    if result.get("exception_info") is not None:
        raise TaskFitEvidenceError(
            f"trial {trial_name!r} has an execution exception; infrastructure "
            "and model failures must not be blended into one score"
        )
    verifier = result.get("verifier_result")
    rewards = verifier.get("rewards") if isinstance(verifier, dict) else None
    reward = rewards.get("reward") if isinstance(rewards, dict) else None
    if isinstance(reward, bool) or not isinstance(reward, int | float):
        raise TaskFitEvidenceError(
            f"trial {trial_name!r} has no numeric canonical verifier reward"
        )
    numeric = float(reward)
    if not math.isfinite(numeric) or not 0.0 <= numeric <= 1.0:
        raise TaskFitEvidenceError(
            f"trial {trial_name!r} reward must be finite and between 0 and 1"
        )
    return numeric


def build_task_fit_evidence_report(
    *,
    job_dir: Path,
    manifest_path: Path,
    endpoint_config_path: Path,
    task_class: str,
    dataset: str,
    attempts_per_task: int,
    artifact_root: Path,
    runtime_binaries: dict[str, Path],
    captured_manifest_sha256: str,
    captured_endpoint_config_sha256: str,
    captured_prompt_sha256: str,
    captured_runtime_binary_sha256: dict[str, str],
    pass_threshold: float = 1.0,
) -> dict[str, Any]:
    """Validate one completed job and return a privacy-bounded evidence report.

    Hashes captured immediately before dispatch must still match. This prevents
    the report from describing edited inputs different from the run inputs.
    """
    validate_task_class_id(task_class)
    if not 0.0 <= pass_threshold <= 1.0 or not math.isfinite(pass_threshold):
        raise TaskFitEvidenceError("pass threshold must be finite and in [0, 1]")
    if attempts_per_task <= 0:
        raise TaskFitEvidenceError("attempts per task must be positive")

    try:
        manifest = ExperimentManifest.load(manifest_path)
    except ManifestError as error:
        raise TaskFitEvidenceError(f"invalid experiment manifest: {error}") from error
    if manifest.sha256 != captured_manifest_sha256:
        raise TaskFitEvidenceError("manifest changed after the benchmark started")
    try:
        endpoint_config_bytes = endpoint_config_path.read_bytes()
    except OSError as error:
        raise TaskFitEvidenceError(
            f"cannot read endpoint configuration: {error}"
        ) from error
    endpoint_config_sha256 = hashlib.sha256(endpoint_config_bytes).hexdigest()
    if endpoint_config_sha256 != captured_endpoint_config_sha256:
        raise TaskFitEvidenceError("endpoint configuration changed during the run")

    candidate = validate_single_candidate(manifest)
    prompt_sha256 = verified_prompt_sha256(manifest, artifact_root)
    if prompt_sha256 != captured_prompt_sha256:
        raise TaskFitEvidenceError("manifest prompt changed during the run")
    endpoint_config = _read_json_bytes(endpoint_config_bytes, "endpoint configuration")
    endpoint_entry = endpoint_config.get(candidate.endpoint)
    provider = (
        endpoint_entry.get("provider") if isinstance(endpoint_entry, dict) else None
    )
    if not isinstance(provider, str) or not provider.strip():
        raise TaskFitEvidenceError(
            f"endpoint {candidate.endpoint!r} has no declared provider"
        )

    job_result_path = job_dir / "result.json"
    try:
        job_result_bytes = job_result_path.read_bytes()
    except OSError as error:
        raise TaskFitEvidenceError(f"cannot read Harbor job result: {error}") from error
    job_result = _read_json_bytes(job_result_bytes, "Harbor job result")
    job_result_sha256 = hashlib.sha256(job_result_bytes).hexdigest()
    stats = job_result.get("stats")
    trial_results = job_result.get("trial_results")
    total = job_result.get("n_total_trials")
    if not isinstance(stats, dict) or not isinstance(trial_results, list):
        raise TaskFitEvidenceError("Harbor job result is missing trial stats")
    if isinstance(total, bool) or not isinstance(total, int) or total <= 0:
        raise TaskFitEvidenceError("Harbor job result has no positive trial count")
    job_id = job_result.get("id")
    if not isinstance(job_id, str) or not job_id:
        raise TaskFitEvidenceError("Harbor job ID is missing")
    started_at = _parse_utc_timestamp(job_result.get("started_at"), "start")
    finished_at = _parse_utc_timestamp(job_result.get("finished_at"), "finish")
    if finished_at < started_at:
        raise TaskFitEvidenceError("Harbor job finished before it started")
    counts = {}
    for key in (
        "n_completed_trials",
        "n_errored_trials",
        "n_running_trials",
        "n_pending_trials",
        "n_cancelled_trials",
    ):
        value = stats.get(key)
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise TaskFitEvidenceError(f"Harbor job result has invalid {key}")
        counts[key] = value
    if (
        counts["n_completed_trials"] != total
        or counts["n_errored_trials"] != 0
        or counts["n_running_trials"] != 0
        or counts["n_pending_trials"] != 0
        or counts["n_cancelled_trials"] != 0
        or len(trial_results) != total
    ):
        raise TaskFitEvidenceError(
            "Harbor job is incomplete or contains errored/cancelled trials"
        )

    trials: list[dict[str, Any]] = []
    seen_names: set[str] = set()
    observed_models: set[str] = set()
    task_checksums: dict[str, str] = {}
    task_sample_counts: dict[str, int] = {}
    rewards_by_case: dict[str, list[float]] = {}
    for result in trial_results:
        if not isinstance(result, dict):
            raise TaskFitEvidenceError("Harbor trial result is not an object")
        trial_name = result.get("trial_name")
        task_name = result.get("task_name")
        task_checksum = result.get("task_checksum")
        if (
            not isinstance(trial_name, str)
            or not trial_name
            or trial_name in seen_names
            or not isinstance(task_name, str)
            or not task_name
            or not isinstance(task_checksum, str)
            or not task_checksum
        ):
            raise TaskFitEvidenceError("trial identity/checksum is missing or repeated")
        seen_names.add(trial_name)
        previous_checksum = task_checksums.setdefault(task_name, task_checksum)
        if previous_checksum != task_checksum:
            raise TaskFitEvidenceError(
                f"task {task_name!r} has inconsistent checksums in one job"
            )
        task_sample_counts[task_name] = task_sample_counts.get(task_name, 0) + 1
        reward = _result_reward(result, trial_name)
        rewards_by_case.setdefault(task_checksum, []).append(reward)
        agent_info = result.get("agent_info")
        model_info = (
            agent_info.get("model_info") if isinstance(agent_info, dict) else None
        )
        observed_model = (
            model_info.get("name") if isinstance(model_info, dict) else None
        )
        if observed_model is not None:
            if observed_model not in {candidate.model_revision, candidate.endpoint}:
                raise TaskFitEvidenceError(
                    f"trial {trial_name!r} reports a different model than the manifest"
                )
            observed_models.add(observed_model)
        trials.append(
            {
                "trial_name": trial_name,
                "task_name": task_name,
                "task_checksum": task_checksum,
                "reward": reward,
            }
        )

    uneven_tasks = {
        name: count
        for name, count in task_sample_counts.items()
        if count != attempts_per_task
    }
    if uneven_tasks:
        raise TaskFitEvidenceError(
            "each task must have exactly the declared number of attempts; "
            f"observed {uneven_tasks}, expected {attempts_per_task}"
        )

    rewards = [trial["reward"] for trial in trials]
    trial_successes = sum(reward >= pass_threshold for reward in rewards)
    task_successes = sum(
        all(reward >= pass_threshold for reward in case_rewards)
        for case_rewards in rewards_by_case.values()
    )
    case_checksums = sorted(rewards_by_case)
    case_set_sha256 = hashlib.sha256(
        json.dumps(case_checksums, ensure_ascii=False, separators=(",", ":")).encode(
            "utf-8"
        )
    ).hexdigest()
    agent_version_digests = {
        name: _sha256_file(path) for name, path in sorted(runtime_binaries.items())
    }
    if not agent_version_digests:
        raise TaskFitEvidenceError("runtime binary identities are required")
    if agent_version_digests != captured_runtime_binary_sha256:
        raise TaskFitEvidenceError("runtime binaries changed during the run")

    return {
        "schema_version": 2,
        "evidence_kind": "harbor_verifier_outcomes",
        "evidence_scope": "exact_single_agent_benchmark_condition",
        "created_at_utc": datetime.now(UTC).isoformat(),
        "task_class": {
            "id": task_class,
            "taxonomy_version": TASK_CLASS_TAXONOMY_VERSION,
            "classification_source": "operator_annotation",
        },
        "source": {
            "harness": "Harbor",
            "version": HARBOR_LOCKED_VERSION,
            "job_id": job_id,
            "job_result_sha256": job_result_sha256,
            "dataset": dataset,
        },
        "evaluator": {
            "name": "harbor_canonical_verifier_reward",
            "version": HARBOR_LOCKED_VERSION,
            "reward_key": "verifier_result.rewards.reward",
        },
        "candidate": {
            "provider_id": provider,
            "model_id": candidate.model_revision,
            "endpoint_id": candidate.endpoint,
            "condition_id": manifest.condition,
            "condition_sha256": manifest.sha256,
            "prompt_sha256": prompt_sha256,
            "generation": candidate.generation.model_dump(mode="json"),
            "runtime_binary_sha256": agent_version_digests,
            "endpoint_config_sha256": captured_endpoint_config_sha256,
            "observed_model_ids": sorted(observed_models),
            "model_identity_source": (
                "harbor_trial_result"
                if observed_models
                else "manifest_configuration_only"
            ),
        },
        "evaluation": {
            "started_at": started_at.isoformat(),
            "finished_at": finished_at.isoformat(),
            "attempts_per_task": attempts_per_task,
            "policy_version": TASK_FIT_POLICY_VERSION,
            "sample_count": len(rewards),
            "task_name_count": len(task_checksums),
            "task_count": len(case_checksums),
            "case_set_sha256": case_set_sha256,
            "pass_threshold": pass_threshold,
            "trial_success_rule": "reward_at_or_above_threshold",
            "trial_success_count": trial_successes,
            "trial_success_rate": round(trial_successes / len(rewards), 6),
            "task_success_rule": "all_repeats_for_unique_checksum_meet_threshold",
            "task_success_count": task_successes,
            "task_success_rate": round(task_successes / len(case_checksums), 6),
            "task_wilson_lower_bound_95": lower_wilson_95(
                task_successes, len(case_checksums)
            ),
            "mean_reward": round(sum(rewards) / len(rewards), 6),
            "task_checksums": case_checksums,
            "trials": sorted(trials, key=lambda trial: trial["trial_name"]),
        },
        "routing_status": {
            "eligible_for_routing": False,
            "reason": (
                "report is advisory until Buzz enforces exact task, prompt, "
                "runtime, freshness, and sample requirements"
            ),
        },
    }


def write_task_fit_evidence_report(**kwargs: Any) -> Path:
    """Write the validated report atomically without overwriting an artifact."""
    job_dir = kwargs["job_dir"]
    output = job_dir / REPORT_FILENAME
    if output.exists():
        raise TaskFitEvidenceError(f"refusing to overwrite existing report {output}")
    report = build_task_fit_evidence_report(**kwargs)
    temporary = job_dir / f".{REPORT_FILENAME}.{os.getpid()}.tmp"
    try:
        with temporary.open("x", encoding="utf-8") as destination:
            destination.write(
                json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + "\n"
            )
            destination.flush()
            os.fsync(destination.fileno())
        # A hard link publishes the complete file atomically and fails if a
        # concurrent writer created the destination after our first check.
        os.link(temporary, output)
    except FileExistsError as error:
        raise TaskFitEvidenceError(
            f"refusing to overwrite existing report {output}"
        ) from error
    finally:
        temporary.unlink(missing_ok=True)
    return output
