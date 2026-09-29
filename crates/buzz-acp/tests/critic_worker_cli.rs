use std::process::{Command, Stdio};

#[test]
fn critic_worker_subcommand_keeps_its_json_contract_on_stdout() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_buzz-acp"))
        .arg("critic-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the bundled ACP binary");
    let request = serde_json::json!({
        "version": 2,
        "role": "correctness",
        "objective": "Check the patch",
        "scope": "Input validation",
        "snapshot": "snapshot bytes",
        "snapshot_sha256": "0".repeat(64),
        "max_output_tokens": 2048,
        "time_limit_seconds": 120,
        "thinking_effort": null,
    });
    serde_json::to_writer(child.stdin.as_mut().expect("worker stdin"), &request)
        .expect("write bounded request");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("wait for worker");
    assert!(output.status.success());
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON response on stdout");
    assert_eq!(response["ok"], false);
    assert_eq!(response["role"], "correctness");
    assert_eq!(response["snapshot_sha256"], "0".repeat(64));
    assert_eq!(response["error_code"], "snapshot_hash_mismatch");
}
