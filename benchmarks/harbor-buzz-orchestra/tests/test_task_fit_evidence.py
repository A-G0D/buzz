"""Task-fit reports accept only complete, attributable Harbor outcomes."""

import copy
import hashlib
import json
from pathlib import Path

import pytest
import yaml

from harbor_buzz_orchestra.manifest import ExperimentManifest
from harbor_buzz_orchestra.task_fit_evidence import (
    REPORT_FILENAME,
    TaskFitEvidenceError,
    build_task_fit_evidence_report,
    validate_single_candidate,
    validate_task_class_id,
    write_task_fit_evidence_report,
)


@pytest.fixture
def evidence_inputs(tmp_path: Path) -> dict:
    prompt_path = tmp_path / "prompt.md"
    prompt_path.write_text("private prompt body", encoding="utf-8")
    prompt_hash = hashlib.sha256(prompt_path.read_bytes()).hexdigest()
    manifest_data = {
        "schema_version": "1",
        "condition": "solo-test",
        "roster": [
            {
                "id": "solo",
                "kind": "orchestrator",
                "role": "solo",
                "count": 1,
                "endpoint": "test-endpoint",
                "model_revision": "model-revision-1",
                "prompt": {"path": "prompt.md", "sha256": prompt_hash},
                "generation": {
                    "temperature": 0,
                    "max_output_tokens": 1024,
                    "context_window_tokens": 8192,
                },
            }
        ],
        "prices": {
            "test-endpoint": {
                "input_per_million_usd": 1,
                "cached_input_per_million_usd": 0,
                "output_per_million_usd": 2,
            }
        },
        "trial_budget": {"timeout_seconds": 300},
    }
    manifest_path = tmp_path / "manifest.yaml"
    manifest_path.write_text(yaml.safe_dump(manifest_data), encoding="utf-8")
    manifest = ExperimentManifest.load(manifest_path)

    endpoint_config_path = tmp_path / "endpoints.json"
    endpoint_config_path.write_text(
        json.dumps(
            {"test-endpoint": {"provider": "test-provider", "model": "model"}}
        ),
        encoding="utf-8",
    )
    job_dir = tmp_path / "job"
    job_dir.mkdir()
    trial_results = [
        {
            "trial_name": f"task-a__rep-{index}",
            "task_name": "task-a",
            "task_checksum": "a" * 64,
            "agent_info": {"model_info": {"name": "model-revision-1"}},
            "verifier_result": {"rewards": {"reward": reward}},
        }
        for index, reward in enumerate((1.0, 0.5), start=1)
    ]
    result_path = job_dir / "result.json"
    result_path.write_text(
        json.dumps(
            {
                "id": "job-123",
                "started_at": "2026-09-25T12:00:00Z",
                "finished_at": "2026-09-25T12:10:00Z",
                "n_total_trials": len(trial_results),
                "stats": {
                    "n_completed_trials": len(trial_results),
                    "n_errored_trials": 0,
                    "n_running_trials": 0,
                    "n_pending_trials": 0,
                    "n_cancelled_trials": 0,
                },
                "trial_results": trial_results,
            }
        ),
        encoding="utf-8",
    )
    binary = tmp_path / "buzz-agent"
    binary.write_bytes(b"test binary")
    return {
        "job_dir": job_dir,
        "manifest_path": manifest_path,
        "endpoint_config_path": endpoint_config_path,
        "artifact_root": tmp_path,
        "task_class": "coding",
        "dataset": "local/test-dataset",
        "attempts_per_task": 2,
        "runtime_binaries": {"buzz-agent": binary},
        "captured_manifest_sha256": manifest.sha256,
        "captured_endpoint_config_sha256": hashlib.sha256(
            endpoint_config_path.read_bytes()
        ).hexdigest(),
        "captured_prompt_sha256": prompt_hash,
        "captured_runtime_binary_sha256": {
            "buzz-agent": hashlib.sha256(binary.read_bytes()).hexdigest()
        },
        "manifest_data": manifest_data,
        "trial_results": trial_results,
    }


def test_reports_scoped_results_without_prompt_or_credentials(evidence_inputs):
    report = build_task_fit_evidence_report(**_report_args(evidence_inputs))

    assert report["routing_status"]["eligible_for_routing"] is False
    assert report["schema_version"] == 2
    assert report["task_class"] == {
        "id": "coding",
        "taxonomy_version": "operator-defined-v1",
        "classification_source": "operator_annotation",
    }
    assert report["candidate"]["model_id"] == "model-revision-1"
    assert report["evaluation"]["sample_count"] == 2
    assert report["evaluation"]["policy_version"] == "task-fit-outcomes-v1"
    assert report["evaluation"]["task_count"] == 1
    assert report["evaluation"]["trial_success_count"] == 1
    assert report["evaluation"]["task_success_count"] == 0
    assert report["evaluation"]["mean_reward"] == 0.75
    assert report["evaluation"]["task_wilson_lower_bound_95"] == 0.0
    assert len(report["source"]["job_result_sha256"]) == 64
    assert report["evaluator"]["reward_key"] == "verifier_result.rewards.reward"
    encoded = json.dumps(report)
    assert "private prompt body" not in encoded
    assert "api_key" not in encoded
    assert "secret" not in encoded


def test_partial_verifier_reward_is_scored_as_a_failure(evidence_inputs):
    report = build_task_fit_evidence_report(**_report_args(evidence_inputs))

    assert report["evaluation"]["pass_threshold"] == 1.0
    assert report["evaluation"]["trial_success_count"] == 1
    assert report["evaluation"]["task_success_count"] == 0
    assert report["evaluation"]["mean_reward"] == 0.75


def test_task_confidence_bound_counts_tasks_not_repeated_trials(evidence_inputs):
    evidence_inputs["trial_results"][1]["verifier_result"]["rewards"]["reward"] = 1.0
    _write_result(evidence_inputs)

    report = build_task_fit_evidence_report(**_report_args(evidence_inputs))

    evaluation = report["evaluation"]
    assert evaluation["sample_count"] == 2
    assert evaluation["task_count"] == 1
    assert evaluation["task_success_count"] == 1
    assert evaluation["task_wilson_lower_bound_95"] < 1.0


def test_duplicate_task_checksums_count_as_one_case(evidence_inputs):
    trials = copy.deepcopy(evidence_inputs["trial_results"])
    for index, trial in enumerate(trials, start=1):
        trial["trial_name"] = f"task-b__rep-{index}"
        trial["task_name"] = "task-b"
        trial["verifier_result"]["rewards"]["reward"] = 1.0
    evidence_inputs["trial_results"] = trials
    result = _read_result(evidence_inputs)
    result["n_total_trials"] = len(trials) * 2
    result["stats"]["n_completed_trials"] = len(trials) * 2
    result["trial_results"] = trials + [
        {
            **trial,
            "trial_name": trial["trial_name"].replace("task-b", "task-a"),
            "task_name": "task-a",
        }
        for trial in trials
    ]
    _write_result(evidence_inputs, result)

    report = build_task_fit_evidence_report(**_report_args(evidence_inputs))

    evaluation = report["evaluation"]
    assert evaluation["sample_count"] == 4
    assert evaluation["task_name_count"] == 2
    assert evaluation["task_count"] == 1
    assert evaluation["task_success_count"] == 1
    assert evaluation["task_checksums"] == ["a" * 64]
    assert len(evaluation["case_set_sha256"]) == 64


def test_missing_canonical_reward_fails_closed(evidence_inputs):
    evidence_inputs["trial_results"][0]["verifier_result"] = {"rewards": {}}
    _write_result(evidence_inputs)

    with pytest.raises(TaskFitEvidenceError, match="no numeric canonical verifier"):
        build_task_fit_evidence_report(**_report_args(evidence_inputs))


def test_incomplete_harbor_job_fails_closed(evidence_inputs):
    result = _read_result(evidence_inputs)
    result["stats"]["n_completed_trials"] = 1
    _write_result(evidence_inputs, result)

    with pytest.raises(TaskFitEvidenceError, match="incomplete"):
        build_task_fit_evidence_report(**_report_args(evidence_inputs))


def test_uneven_attempts_per_task_fail_closed(evidence_inputs):
    evidence_inputs["trial_results"][1]["task_name"] = "task-b"
    evidence_inputs["trial_results"][1]["task_checksum"] = "b" * 64
    _write_result(evidence_inputs)

    with pytest.raises(TaskFitEvidenceError, match="exactly the declared number"):
        build_task_fit_evidence_report(**_report_args(evidence_inputs))


def test_different_observed_model_fails_closed(evidence_inputs):
    evidence_inputs["trial_results"][0]["agent_info"]["model_info"]["name"] = (
        "unrelated-model"
    )
    _write_result(evidence_inputs)

    with pytest.raises(TaskFitEvidenceError, match="different model"):
        build_task_fit_evidence_report(**_report_args(evidence_inputs))


def test_changed_manifest_or_endpoint_config_fails_closed(evidence_inputs):
    evidence_inputs["manifest_data"]["metadata"] = {"changed": True}
    evidence_inputs["manifest_path"].write_text(
        yaml.safe_dump(evidence_inputs["manifest_data"]), encoding="utf-8"
    )
    with pytest.raises(TaskFitEvidenceError, match="manifest changed"):
        build_task_fit_evidence_report(**_report_args(evidence_inputs))

    manifest = ExperimentManifest.load(evidence_inputs["manifest_path"])
    evidence_inputs["captured_manifest_sha256"] = manifest.sha256
    evidence_inputs["endpoint_config_path"].write_text("{}", encoding="utf-8")
    with pytest.raises(TaskFitEvidenceError, match="endpoint configuration changed"):
        build_task_fit_evidence_report(**_report_args(evidence_inputs))


def test_prompt_and_runtime_binary_are_bound_to_the_run(evidence_inputs):
    prompt_path = evidence_inputs["artifact_root"] / "prompt.md"
    prompt_path.write_text("changed prompt", encoding="utf-8")
    with pytest.raises(TaskFitEvidenceError, match="prompt content"):
        build_task_fit_evidence_report(**_report_args(evidence_inputs))

    evidence_inputs["manifest_data"]["roster"][0]["prompt"]["sha256"] = hashlib.sha256(
        prompt_path.read_bytes()
    ).hexdigest()
    evidence_inputs["manifest_path"].write_text(
        yaml.safe_dump(evidence_inputs["manifest_data"]), encoding="utf-8"
    )
    evidence_inputs["captured_manifest_sha256"] = ExperimentManifest.load(
        evidence_inputs["manifest_path"]
    ).sha256
    evidence_inputs["captured_prompt_sha256"] = hashlib.sha256(
        prompt_path.read_bytes()
    ).hexdigest()
    evidence_inputs["captured_runtime_binary_sha256"] = {"buzz-agent": "0" * 64}
    with pytest.raises(TaskFitEvidenceError, match="runtime binaries changed"):
        build_task_fit_evidence_report(**_report_args(evidence_inputs))


def test_team_manifest_is_not_attributed_to_one_candidate(evidence_inputs):
    data = copy.deepcopy(evidence_inputs["manifest_data"])
    worker = copy.deepcopy(data["roster"][0])
    worker.update(
        {
            "id": "worker",
            "kind": "worker",
            "role": "worker",
            "endpoint": "test-endpoint",
        }
    )
    data["roster"].append(worker)
    with pytest.raises(TaskFitEvidenceError, match="single-agent manifest"):
        validate_single_candidate(ExperimentManifest.load(data))


@pytest.mark.parametrize(
    "task_class", ["Coding", "../coding", "", "has spaces", "x" * 65]
)
def test_task_class_identifier_is_validated(task_class):
    with pytest.raises(TaskFitEvidenceError, match="short lowercase ID"):
        validate_task_class_id(task_class)


def test_report_writer_publishes_a_complete_json_file(evidence_inputs):
    output = write_task_fit_evidence_report(**_report_args(evidence_inputs))

    report = json.loads(output.read_text(encoding="utf-8"))
    assert output.name == REPORT_FILENAME
    assert report["evaluation"]["sample_count"] == 2


def test_report_writer_does_not_overwrite(evidence_inputs):
    output = evidence_inputs["job_dir"] / REPORT_FILENAME
    output.write_text("existing", encoding="utf-8")

    with pytest.raises(TaskFitEvidenceError, match="refusing to overwrite"):
        write_task_fit_evidence_report(**_report_args(evidence_inputs))


def _report_args(inputs: dict) -> dict:
    return {
        key: value
        for key, value in inputs.items()
        if key
        in {
            "job_dir",
            "manifest_path",
            "endpoint_config_path",
            "artifact_root",
            "task_class",
            "dataset",
            "attempts_per_task",
            "runtime_binaries",
            "captured_manifest_sha256",
            "captured_endpoint_config_sha256",
            "captured_prompt_sha256",
            "captured_runtime_binary_sha256",
        }
    }


def _read_result(inputs: dict) -> dict:
    return json.loads((inputs["job_dir"] / "result.json").read_text(encoding="utf-8"))


def _write_result(inputs: dict, result: dict | None = None) -> None:
    if result is None:
        result = _read_result(inputs)
        result["trial_results"] = inputs["trial_results"]
    (inputs["job_dir"] / "result.json").write_text(
        json.dumps(result), encoding="utf-8"
    )
