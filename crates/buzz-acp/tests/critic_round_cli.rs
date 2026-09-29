#[cfg(unix)]
use std::{
    os::unix::fs::PermissionsExt as _,
    process::{Command, Stdio},
};

#[cfg(unix)]
use sha2::{Digest, Sha256};

#[cfg(unix)]
#[test]
fn critic_round_subcommand_runs_one_tool_free_worker_and_returns_the_bound_snapshot() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    let fake_agent = temp.path().join("buzz-agent");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/critic_agent.py"
        ),
        &fake_agent,
    )
    .expect("copy the deterministic ACP peer");
    std::fs::set_permissions(&fake_agent, std::fs::Permissions::from_mode(0o700))
        .expect("mark the peer executable");
    let path = std::env::join_paths(std::iter::once(temp.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("build test PATH");

    let snapshot = "fn value() -> bool { true }\n";
    let digest = hex::encode(Sha256::digest(snapshot.as_bytes()));
    let request = serde_json::json!({
        "version": 1,
        "objective": "Review the behavior",
        "scope": "Check the frozen function",
        "snapshot": snapshot,
        "roles": ["correctness"],
        "max_output_tokens": 64,
        "time_limit_seconds": 15,
        "thinking_effort": null,
        "estimated_round_cost_budget_microusd": 100,
        "reviewer_cost_limits": [{
            "role": "correctness",
            "estimated_cost_limit_microusd": 100,
        }],
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_buzz-acp"))
        .arg("critic-round")
        .current_dir(temp.path())
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the bundled ACP binary");
    let mut stdin = child.stdin.take().expect("round stdin");
    serde_json::to_writer(&mut stdin, &request).expect("write the bounded critic request");
    drop(stdin);

    let output = child.wait_with_output().expect("wait for critic round");
    assert!(
        output.status.success(),
        "critic-round exited unsuccessfully"
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON response on stdout");
    assert_eq!(response["version"], 1);
    assert_eq!(response["snapshot_sha256"], digest);
    assert_eq!(response["reviewers"].as_array().unwrap().len(), 1);
    assert_eq!(response["reviewers"][0]["role"], "correctness");
    assert_eq!(
        response["reviewers"][0]["status"],
        "completed",
        "{response}; child stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(response["reviewers"][0]["output"], "No verified defects.");
    assert_eq!(response["reviewers"][0]["model_id"], "mock-local-model");
    assert_eq!(response["limits"]["output_tokens_per_reviewer"], 64);
    assert_eq!(
        response["limits"]["estimated_round_cost_budget_microusd"],
        100
    );
    assert_eq!(
        response["reviewers"][0]["estimated_cost_limit_microusd"],
        100
    );
}
