//! Integration test: fake LLM HTTP server + buzz-agent subprocess.
//!
//! Drives the agent through the ACP wire protocol and verifies:
//!   - initialize / session/new responses
//!   - tool_call (pending) → request_permission → tool_call_update
//!   - session/prompt response with stopReason=end_turn
//!   - concurrent prompt rejection

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use buzz_agent::route_preview::{
    RouteProfileCandidate, RouteProfileDataPolicy, RouteProfileDocument, RouteProfileLocation,
};
use buzz_agent::task_fit_evidence::{
    validate_task_fit_report, TaskFitEligibilityPolicy, TaskFitEvidenceBinding,
    TaskFitRouteAttestationPayload, TASK_FIT_REVIEW_PUBLIC_KEY_ENV,
};
use nostr::{EventBuilder, Keys, Kind};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

mod common;
use common::approve_permission;

async fn spawn_fake_llm(responses: Vec<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let queue = queue.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                    if buf.len() > 1_000_000 {
                        return;
                    }
                }
                let body = queue
                    .lock()
                    .await
                    .pop_front()
                    .unwrap_or_else(|| json!({ "error": "no canned response" }));
                let body_s = serde_json::to_string(&body).unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_s.len(),
                    body_s,
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    url
}

struct CannedResponse {
    status: u16,
    body: Value,
}

/// Like `spawn_fake_llm` but also captures the full JSON request body from each
/// incoming HTTP request. Returns (url, captured_requests).
async fn spawn_capturing_fake_llm(responses: Vec<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
    spawn_capturing_fake_llm_with_statuses(
        responses
            .into_iter()
            .map(|body| CannedResponse { status: 200, body })
            .collect(),
    )
    .await
}

async fn spawn_capturing_fake_llm_with_statuses(
    responses: Vec<CannedResponse>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let captures: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let url = spawn_capturing_fake_llm_core(responses, captures.clone(), None).await;
    (url, captures)
}

/// Shared connection loop for the capturing fake LLM: reads each request,
/// records its JSON body into `captures`, and replies with the next canned
/// response. When `gate` is `Some`, the FIRST request's response is withheld
/// until the gate fires; when `None`, every response is served immediately.
async fn spawn_capturing_fake_llm_core(
    responses: Vec<CannedResponse>,
    captures: Arc<Mutex<Vec<Value>>>,
    gate: Option<Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
    tokio::spawn(async move {
        let mut request_num = 0usize;
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let queue = queue.clone();
            let captures = captures.clone();
            let gate = gate.clone();
            request_num += 1;
            let req_num = request_num;
            tokio::spawn(async move {
                // Read headers.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                    if buf.len() > 2_000_000 {
                        return;
                    }
                }
                // Parse Content-Length from headers to read the body.
                let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let header_str = String::from_utf8_lossy(&buf[..header_end]);
                let content_length: usize = header_str
                    .lines()
                    .find_map(|line| {
                        let lower = line.to_lowercase();
                        if lower.starts_with("content-length:") {
                            lower
                                .trim_start_matches("content-length:")
                                .trim()
                                .parse()
                                .ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);

                // Collect body bytes (some may already be in buf after headers).
                let mut body_buf = buf[header_end..].to_vec();
                while body_buf.len() < content_length {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body_buf.extend_from_slice(&tmp[..n]),
                    }
                }

                // Parse and store the request body.
                if let Ok(parsed) =
                    serde_json::from_slice::<Value>(&body_buf[..content_length.min(body_buf.len())])
                {
                    captures.lock().await.push(parsed);
                }

                // Hold the first request's response until the gate opens.
                if req_num == 1 {
                    if let Some(gate) = &gate {
                        if let Some(rx) = gate.lock().await.take() {
                            let _ = rx.await;
                        }
                    }
                }

                // Send canned response.
                let response = queue.lock().await.pop_front().unwrap_or(CannedResponse {
                    status: 500,
                    body: json!({ "error": "no canned response" }),
                });
                let body_s = serde_json::to_string(&response.body).unwrap();
                let reason = if response.status == 200 {
                    "OK"
                } else {
                    "Error"
                };
                let resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.status,
                    reason,
                    body_s.len(),
                    body_s,
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    url
}

/// A capturing fake LLM whose FIRST provider response is withheld until
/// `gate` fires. Later responses are served immediately. Used to make
/// round-boundary races deterministic: hold round 1 open until a client action
/// (e.g. a steer) is confirmed, so the second round observes it. Request bodies
/// are recorded into `captures` exactly as `spawn_capturing_fake_llm` does.
async fn spawn_gated_capturing_fake_llm(
    responses: Vec<CannedResponse>,
    captures: Arc<Mutex<Vec<Value>>>,
    gate: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let url = spawn_capturing_fake_llm_core(responses, captures.clone(), Some(gate)).await;
    (url, captures)
}

struct Harness {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: i64,
}

struct TaskFitEnv {
    nest_dir: PathBuf,
    reviewer_public_key: String,
    profile_id: String,
    profile_version: u32,
    profile_hash: String,
}

impl Harness {
    async fn spawn(base_url: &str) -> Self {
        Self::spawn_with_route(base_url, None).await
    }

    async fn spawn_routed(
        openai_base_url: &str,
        deepseek_base_url: &str,
        route_profile: Value,
    ) -> Self {
        Self::spawn_with_route(
            openai_base_url,
            Some((deepseek_base_url.to_owned(), route_profile)),
        )
        .await
    }

    async fn spawn_routed_with_task_fit(
        openai_base_url: &str,
        deepseek_base_url: &str,
        route_profile: Value,
        task_fit: TaskFitEnv,
    ) -> Self {
        Self::spawn_with_route_and_task_fit(
            openai_base_url,
            Some((deepseek_base_url.to_owned(), route_profile)),
            Some(task_fit),
        )
        .await
    }

    async fn spawn_with_route(base_url: &str, route: Option<(String, Value)>) -> Self {
        Self::spawn_with_route_and_task_fit(base_url, route, None).await
    }

    async fn spawn_with_route_and_task_fit(
        base_url: &str,
        route: Option<(String, Value)>,
        task_fit: Option<TaskFitEnv>,
    ) -> Self {
        Self::spawn_with_options(base_url, route, task_fit, false).await
    }

    async fn spawn_review_only(base_url: &str) -> Self {
        Self::spawn_with_options(base_url, None, None, true).await
    }

    async fn spawn_with_options(
        base_url: &str,
        route: Option<(String, Value)>,
        task_fit: Option<TaskFitEnv>,
        review_only: bool,
    ) -> Self {
        Self::spawn_with_options_and_barrier(base_url, route, task_fit, review_only, None).await
    }

    async fn spawn_with_options_and_barrier(
        base_url: &str,
        route: Option<(String, Value)>,
        task_fit: Option<TaskFitEnv>,
        review_only: bool,
        barrier_dir: Option<&Path>,
    ) -> Self {
        let bin = env!("CARGO_BIN_EXE_buzz-agent");
        let mut cmd = tokio::process::Command::new(bin);
        cmd.env("BUZZ_AGENT_PROVIDER", "openai")
            .env("OPENAI_COMPAT_API_KEY", "test")
            .env("OPENAI_COMPAT_MODEL", "fake-model")
            .env("OPENAI_COMPAT_BASE_URL", base_url)
            .env("BUZZ_AGENT_LLM_TIMEOUT_SECS", "5")
            .env("BUZZ_AGENT_TOOL_TIMEOUT_SECS", "5")
            .env("BUZZ_AGENT_MAX_ROUNDS", "4")
            .env_remove("BUZZ_AGENT_REVIEW_ONLY")
            .env_remove("BUZZ_NEST_DIR")
            .env_remove(TASK_FIT_REVIEW_PUBLIC_KEY_ENV)
            .env_remove("BUZZ_ACP_ROUTE_PROFILE_ID")
            .env_remove("BUZZ_ACP_ROUTE_PROFILE_VERSION")
            .env_remove("BUZZ_ACP_ROUTE_PROFILE_HASH")
            .env_remove("BUZZ_AGENT_TEST_ROUTE_PREFLIGHT_BARRIER_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if review_only {
            cmd.env("BUZZ_AGENT_REVIEW_ONLY", "1");
        }
        if let Some((deepseek_base_url, route_profile)) = route {
            cmd.env("DEEPSEEK_API_KEY", "local-test-key")
                .env("DEEPSEEK_MODEL", "deepseek-chat")
                .env("DEEPSEEK_BASE_URL", deepseek_base_url)
                .env("BUZZ_AGENT_ROUTE_PROFILE_JSON", route_profile.to_string());
        }
        if let Some(task_fit) = task_fit {
            cmd.env("BUZZ_NEST_DIR", task_fit.nest_dir)
                .env(TASK_FIT_REVIEW_PUBLIC_KEY_ENV, task_fit.reviewer_public_key)
                .env("BUZZ_ACP_ROUTE_PROFILE_ID", task_fit.profile_id)
                .env(
                    "BUZZ_ACP_ROUTE_PROFILE_VERSION",
                    task_fit.profile_version.to_string(),
                )
                .env("BUZZ_ACP_ROUTE_PROFILE_HASH", task_fit.profile_hash);
        }
        if let Some(barrier_dir) = barrier_dir {
            cmd.env("BUZZ_AGENT_TEST_ROUTE_PREFLIGHT_BARRIER_DIR", barrier_dir);
        }
        let mut child = cmd.spawn().expect("spawn buzz-agent");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    async fn send(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        id
    }

    async fn write(&mut self, msg: Value) {
        let mut s = serde_json::to_string(&msg).unwrap();
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(10), self.stdout.read_line(&mut line))
            .await
            .expect("recv timeout")
            .expect("read line");
        assert!(n > 0, "agent EOF");
        serde_json::from_str(&line).expect("non-JSON line")
    }

    /// Read messages until one matches `pred`.
    async fn recv_until<F: FnMut(&Value) -> bool>(&mut self, mut pred: F) -> Value {
        loop {
            let v = self.recv().await;
            if pred(&v) {
                return v;
            }
        }
    }

    async fn shutdown(mut self) {
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        let _ = self.child.start_kill();
    }
}

async fn wait_for_route_preflight_snapshot(directory: &Path, request_id: i64) {
    let ready = directory.join(format!("{request_id}.ready"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("prompt reached route-preflight snapshot barrier");
}

fn release_route_preflight_snapshot(directory: &Path, request_id: i64) {
    fs::write(directory.join(format!("{request_id}.release")), b"release")
        .expect("release route-preflight snapshot barrier");
}

async fn wait_for_captured_request_count(captures: &Arc<Mutex<Vec<Value>>>, expected: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if captures.lock().await.len() >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fake provider captured expected request count");
}

fn openai_text(content: &str) -> Value {
    json!({
        "id": "cc-1", "object": "chat.completion", "model": "fake-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop",
        }],
    })
}

fn openai_tool_call(id: &str, name: &str, args: Value) -> Value {
    json!({
        "id": "cc-2", "object": "chat.completion", "model": "fake-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant", "content": null,
                "tool_calls": [{
                    "id": id, "type": "function",
                    "function": { "name": name, "arguments": args.to_string() },
                }],
            },
            "finish_reason": "tool_calls",
        }],
    })
}

async fn init_session(h: &mut Harness) -> String {
    h.send(
        "initialize",
        json!({"protocolVersion":2,"clientCapabilities":{}}),
    )
    .await;
    let r = h.recv().await;
    assert_eq!(r["result"]["protocolVersion"], 2);
    assert_eq!(r["result"]["agentInfo"]["name"], "buzz-agent");
    h.send("session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;
    let r = h.recv().await;
    let sid = r["result"]["sessionId"].as_str().unwrap().to_owned();
    assert!(sid.starts_with("ses_"));
    sid
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_only_end_turn() {
    let url = spawn_fake_llm(vec![openai_text("done")]).await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;
    let p_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "hi" }],
            }),
        )
        .await;
    let v = h.recv_until(|v| v["id"] == json!(p_id)).await;
    assert_eq!(v["result"]["stopReason"], "end_turn");
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_only_session_rejects_tools_model_changes_and_reuse() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text(
        "{\"findings\":[],\"summary\":\"No issues found in the supplied snapshot.\"}",
    )])
    .await;
    let mut h = Harness::spawn_review_only(&url).await;
    h.send(
        "initialize",
        json!({"protocolVersion":2,"clientCapabilities":{}}),
    )
    .await;
    let init = h.recv().await;
    assert_eq!(init["result"]["protocolVersion"], 2);

    let rejected_id = h
        .send(
            "session/new",
            json!({
                "cwd":"/tmp",
                "mcpServers":[{"name":"inert","command":"/usr/bin/env","args":[]}]
            }),
        )
        .await;
    let rejected = h.recv_until(|v| v["id"] == json!(rejected_id)).await;
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("review-only sessions cannot start MCP tools"));

    h.send("session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;
    let created = h.recv().await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();

    let set_model_id = h
        .send(
            "session/set_model",
            json!({"sessionId":sid,"modelId":"different-model"}),
        )
        .await;
    let set_model = h.recv_until(|v| v["id"] == json!(set_model_id)).await;
    assert!(set_model["error"]["message"]
        .as_str()
        .unwrap()
        .contains("model identity is pinned at launch"));

    let first_prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId":sid,
                "prompt":[{"type":"text","text":"Review this frozen input and return the requested JSON."}]
            }),
        )
        .await;
    let first_prompt = h.recv_until(|v| v["id"] == json!(first_prompt_id)).await;
    assert_eq!(first_prompt["result"]["stopReason"], "end_turn");

    let second_prompt_id = h
        .send(
            "session/prompt",
            json!({"sessionId":sid,"prompt":[{"type":"text","text":"Change the task."}]}),
        )
        .await;
    let second_prompt = h.recv_until(|v| v["id"] == json!(second_prompt_id)).await;
    assert!(second_prompt["error"]["message"]
        .as_str()
        .unwrap()
        .contains("review-only session accepts one prompt"));

    let captured = requests.lock().await;
    assert_eq!(
        captured.len(),
        1,
        "only the first prompt may call the model"
    );
    assert!(captured[0].get("tools").is_none());
    assert!(captured[0].get("functions").is_none());
    drop(captured);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_only_rejects_hosted_route_profiles_before_any_request() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("must not be called")]).await;
    let route_profile = json!({
        "version": 1,
        "data_policy": "allow-hosted",
        "preference_order": ["openai"],
        "candidates": [{
            "id": "openai",
            "provider": "openai",
            "model": "fixture-model-r1",
            "data_location": "hosted"
        }]
    });
    let mut h =
        Harness::spawn_with_options(&url, Some((url.clone(), route_profile)), None, true).await;
    let status = tokio::time::timeout(Duration::from_secs(3), h.child.wait())
        .await
        .expect("review-only process rejects a hosted candidate promptly")
        .expect("child exit status");
    assert!(!status.success());
    assert!(requests.lock().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_only_rejects_non_loopback_default_endpoint_at_startup() {
    let mut h = Harness::spawn_with_options("https://api.openai.com", None, None, true).await;
    let status = tokio::time::timeout(Duration::from_secs(3), h.child.wait())
        .await
        .expect("review-only process rejects a hosted default endpoint promptly")
        .expect("child exit status");
    assert!(!status.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_route_profile_env_dispatches_only_to_selected_provider() {
    let (openai_url, openai_requests) =
        spawn_capturing_fake_llm(vec![openai_text("wrong provider")]).await;
    let (deepseek_url, deepseek_requests) =
        spawn_capturing_fake_llm(vec![openai_text("selected provider")]).await;
    let route_profile = json!({
        "version": 1,
        "data_policy": "allow-hosted",
        "preference_order": ["deepseek", "openai"],
        "candidates": [
            {
                "id": "openai",
                "provider": "openai",
                "model": "gpt-route-model",
                "data_location": "hosted",
                "prompt_addendum": "OpenAI-only prompt."
            },
            {
                "id": "deepseek",
                "provider": "deepseek",
                "model": "deepseek-chat",
                "data_location": "hosted",
                "prompt_addendum": "DeepSeek-only prompt."
            }
        ]
    });

    let mut h = Harness::spawn_routed(&openai_url, &deepseek_url, route_profile).await;
    let sid = init_session(&mut h).await;
    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "route this locally" }],
            }),
        )
        .await;
    let reply = h
        .recv_until(|message| message["id"] == json!(prompt_id))
        .await;

    assert_eq!(reply["result"]["stopReason"], "end_turn");
    assert_eq!(openai_requests.lock().await.len(), 0);
    let deepseek_requests = deepseek_requests.lock().await;
    assert_eq!(deepseek_requests.len(), 1);
    assert_eq!(deepseek_requests[0]["model"], "deepseek-chat");
    let system_prompt = deepseek_requests[0]["messages"][0]["content"]
        .as_str()
        .expect("system prompt is text");
    assert!(system_prompt.contains("DeepSeek-only prompt."));
    assert!(!system_prompt.contains("OpenAI-only prompt."));
    drop(deepseek_requests);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_model_override_passes_profile_selection_before_provider_request() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("override passed")]).await;
    let route_profile = json!({
        "version": 1,
        "data_policy": "allow-hosted",
        "preference_order": ["selected", "other"],
        "candidates": [
            {
                "id": "selected",
                "provider": "openai",
                "model": "selected-model",
                "data_location": "hosted",
                "prompt_addendum": "Selected profile candidate."
            },
            {
                "id": "other",
                "provider": "openai",
                "model": "other-model",
                "data_location": "hosted",
                "prompt_addendum": "Other profile candidate."
            }
        ]
    });
    let mut h = Harness::spawn_with_route(&url, Some((url.clone(), route_profile))).await;
    let sid = init_session(&mut h).await;
    let set_model_id = h
        .send(
            "session/set_model",
            json!({"sessionId": sid, "modelId": "selected-model"}),
        )
        .await;
    h.recv_until(|message| message["id"] == json!(set_model_id))
        .await;
    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "use the selected profile route" }],
            }),
        )
        .await;
    let reply = h
        .recv_until(|message| message["id"] == json!(prompt_id))
        .await;
    assert_eq!(reply["result"]["stopReason"], "end_turn");
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], "selected-model");
    let system_prompt = requests[0]["messages"][0]["content"]
        .as_str()
        .expect("system prompt is text");
    assert!(system_prompt.contains("Selected profile candidate."));
    assert!(!system_prompt.contains("Other profile candidate."));
    drop(requests);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_model_override_rejects_ambiguous_profile_matches() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("must not be called")]).await;
    let route_profile = json!({
        "version": 1,
        "candidates": [
            {
                "id": "first",
                "provider": "openai",
                "model": "shared-model",
                "data_location": "local"
            },
            {
                "id": "second",
                "provider": "openai",
                "model": "shared-model",
                "data_location": "local"
            }
        ]
    });
    let mut h = Harness::spawn_with_route(&url, Some((url.clone(), route_profile))).await;
    let sid = init_session(&mut h).await;
    let set_model_id = h
        .send(
            "session/set_model",
            json!({"sessionId": sid, "modelId": "shared-model"}),
        )
        .await;
    h.recv_until(|message| message["id"] == json!(set_model_id))
        .await;
    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "reject the ambiguous override" }],
            }),
        )
        .await;
    let reply = h
        .recv_until(|message| message["id"] == json!(prompt_id))
        .await;
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("matches multiple active route profile candidates"));
    assert!(requests.lock().await.is_empty());
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_model_override_rejects_context_overflow_before_provider_request() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("must not be called")]).await;
    let route_profile = json!({
        "version": 1,
        "preference_order": ["small-context"],
        "strict_context_fit": true,
        "candidates": [{
            "id": "small-context",
            "provider": "openai",
            "model": "small-context-model",
            "data_location": "local",
            "context_capacity_tokens": 1
        }]
    });
    let mut h = Harness::spawn_with_route(&url, Some((url.clone(), route_profile))).await;
    let sid = init_session(&mut h).await;
    let unlisted_model_id = h
        .send(
            "session/set_model",
            json!({"sessionId": sid, "modelId": "not-in-profile"}),
        )
        .await;
    h.recv_until(|message| message["id"] == json!(unlisted_model_id))
        .await;
    let unlisted_prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "reject unlisted override" }],
            }),
        )
        .await;
    let unlisted_reply = h
        .recv_until(|message| message["id"] == json!(unlisted_prompt_id))
        .await;
    assert!(unlisted_reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not listed in the active route profile"));
    assert!(requests.lock().await.is_empty());

    let set_model_id = h
        .send(
            "session/set_model",
            json!({"sessionId": sid, "modelId": "small-context-model"}),
        )
        .await;
    h.recv_until(|message| message["id"] == json!(set_model_id))
        .await;
    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "this request cannot fit" }],
            }),
        )
        .await;
    let reply = h
        .recv_until(|message| message["id"] == json!(prompt_id))
        .await;
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no eligible route"));
    assert!(requests.lock().await.is_empty());
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_model_override_rejects_hosted_candidate_under_local_only_policy() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("must not be called")]).await;
    let route_profile = json!({
        "version": 1,
        "data_policy": "local-only",
        "preference_order": ["hosted"],
        "candidates": [{
            "id": "hosted",
            "provider": "openai",
            "model": "hosted-model",
            "data_location": "hosted"
        }]
    });
    let mut h = Harness::spawn_with_route(&url, Some((url.clone(), route_profile))).await;
    let sid = init_session(&mut h).await;
    let set_model_id = h
        .send(
            "session/set_model",
            json!({"sessionId": sid, "modelId": "hosted-model"}),
        )
        .await;
    h.recv_until(|message| message["id"] == json!(set_model_id))
        .await;
    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "stay within local-only policy" }],
            }),
        )
        .await;
    let reply = h
        .recv_until(|message| message["id"] == json!(prompt_id))
        .await;
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no eligible route"));
    assert!(requests.lock().await.is_empty());
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_model_override_without_route_profile_remains_supported() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("override worked")]).await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;
    let set_model_id = h
        .send(
            "session/set_model",
            json!({"sessionId": sid, "modelId": "legacy-override-model"}),
        )
        .await;
    let set_model = h
        .recv_until(|message| message["id"] == json!(set_model_id))
        .await;
    assert_eq!(set_model["result"]["modelId"], "legacy-override-model");

    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "use the legacy override" }],
            }),
        )
        .await;
    let reply = h
        .recv_until(|message| message["id"] == json!(prompt_id))
        .await;
    assert_eq!(reply["result"]["stopReason"], "end_turn");
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], "legacy-override-model");
    drop(requests);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_only_route_abstention_keeps_prompt_capacity_retryable() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("retry succeeded")]).await;
    let route_profile = json!({
        "version": 1,
        "data_policy": "local-only",
        "strict_context_fit": true,
        "preference_order": ["local"],
        "candidates": [{
            "id": "local",
            "provider": "openai",
            "model": "fake-model",
            "data_location": "local",
            "context_capacity_tokens": 70_000
        }]
    });
    let mut h =
        Harness::spawn_with_options(&url, Some((url.clone(), route_profile)), None, true).await;
    let sid = init_session(&mut h).await;

    let rejected_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "x".repeat(8_000) }],
            }),
        )
        .await;
    let rejected = h
        .recv_until(|message| message["id"] == json!(rejected_id))
        .await;
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no eligible route"));
    assert!(requests.lock().await.is_empty());

    let retry_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "retry after the abstention" }],
            }),
        )
        .await;
    let retry = h
        .recv_until(|message| message["id"] == json!(retry_id))
        .await;
    assert_eq!(retry["result"]["stopReason"], "end_turn");
    assert_eq!(requests.lock().await.len(), 1);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn route_preflight_set_model_change_rejects_stale_attempt() {
    let (url, requests) =
        spawn_capturing_fake_llm(vec![openai_text("current model succeeded")]).await;
    let barrier = tempfile::tempdir().expect("barrier directory");
    let route_profile = json!({
        "version": 1,
        "data_policy": "local-only",
        "preference_order": ["first", "second"],
        "candidates": [
            {
                "id": "first",
                "provider": "openai",
                "model": "first-model",
                "data_location": "local"
            },
            {
                "id": "second",
                "provider": "openai",
                "model": "second-model",
                "data_location": "local"
            }
        ]
    });
    let mut h = Harness::spawn_with_options_and_barrier(
        &url,
        Some((url.clone(), route_profile)),
        None,
        false,
        Some(barrier.path()),
    )
    .await;
    let sid = init_session(&mut h).await;

    let stale_prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "preflight before model change" }],
            }),
        )
        .await;
    wait_for_route_preflight_snapshot(barrier.path(), stale_prompt_id).await;

    let set_model_id = h
        .send(
            "session/set_model",
            json!({ "sessionId": sid, "modelId": "second-model" }),
        )
        .await;
    let set_model = h
        .recv_until(|message| message["id"] == json!(set_model_id))
        .await;
    assert_eq!(set_model["result"]["modelId"], "second-model");
    release_route_preflight_snapshot(barrier.path(), stale_prompt_id);

    let stale_reply = h
        .recv_until(|message| message["id"] == json!(stale_prompt_id))
        .await;
    assert!(stale_reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("session changed during route preflight; retry prompt"));
    assert!(
        requests.lock().await.is_empty(),
        "stale preflight must not call provider"
    );

    let retry_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "retry against current model" }],
            }),
        )
        .await;
    wait_for_route_preflight_snapshot(barrier.path(), retry_id).await;
    release_route_preflight_snapshot(barrier.path(), retry_id);
    let retry = h
        .recv_until(|message| message["id"] == json!(retry_id))
        .await;
    assert_eq!(retry["result"]["stopReason"], "end_turn");
    let captured = requests.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0]["model"], "second-model");
    drop(captured);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn route_preflight_cancel_invalidates_snapshot_and_keeps_review_capacity() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("retry succeeded")]).await;
    let barrier = tempfile::tempdir().expect("barrier directory");
    let route_profile = json!({
        "version": 1,
        "data_policy": "local-only",
        "preference_order": ["local"],
        "candidates": [{
            "id": "local",
            "provider": "openai",
            "model": "fake-model",
            "data_location": "local"
        }]
    });
    let mut h = Harness::spawn_with_options_and_barrier(
        &url,
        Some((url.clone(), route_profile)),
        None,
        true,
        Some(barrier.path()),
    )
    .await;
    let sid = init_session(&mut h).await;

    let cancelled_prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "cancel during route preflight" }],
            }),
        )
        .await;
    wait_for_route_preflight_snapshot(barrier.path(), cancelled_prompt_id).await;

    let cancel_id = h.send("session/cancel", json!({ "sessionId": sid })).await;
    let cancel_ack = h
        .recv_until(|message| message["id"] == json!(cancel_id))
        .await;
    assert_eq!(cancel_ack.get("result"), Some(&Value::Null));
    release_route_preflight_snapshot(barrier.path(), cancelled_prompt_id);

    let cancelled = h
        .recv_until(|message| message["id"] == json!(cancelled_prompt_id))
        .await;
    assert!(cancelled["error"]["message"]
        .as_str()
        .unwrap()
        .contains("session changed during route preflight; retry prompt"));
    assert!(requests.lock().await.is_empty());

    let retry_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "retry after preflight cancel" }],
            }),
        )
        .await;
    wait_for_route_preflight_snapshot(barrier.path(), retry_id).await;
    release_route_preflight_snapshot(barrier.path(), retry_id);
    let retry = h
        .recv_until(|message| message["id"] == json!(retry_id))
        .await;
    assert_eq!(retry["result"]["stopReason"], "end_turn");
    assert_eq!(requests.lock().await.len(), 1);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_preflight_snapshots_reserve_review_only_capacity_once() {
    use tokio::sync::oneshot;

    let (gate_tx, gate_rx) = oneshot::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(gate_rx)));
    let captures = Arc::new(Mutex::new(Vec::new()));
    let (url, _) = spawn_gated_capturing_fake_llm(
        vec![CannedResponse {
            status: 200,
            body: openai_text("one prompt admitted"),
        }],
        captures.clone(),
        gate,
    )
    .await;
    let barrier = tempfile::tempdir().expect("barrier directory");
    let route_profile = json!({
        "version": 1,
        "data_policy": "local-only",
        "preference_order": ["local"],
        "candidates": [{
            "id": "local",
            "provider": "openai",
            "model": "fake-model",
            "data_location": "local"
        }]
    });
    let mut h = Harness::spawn_with_options_and_barrier(
        &url,
        Some((url.clone(), route_profile)),
        None,
        true,
        Some(barrier.path()),
    )
    .await;
    let sid = init_session(&mut h).await;
    let first_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "first candidate" }],
            }),
        )
        .await;
    wait_for_route_preflight_snapshot(barrier.path(), first_id).await;
    let second_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "second candidate" }],
            }),
        )
        .await;
    wait_for_route_preflight_snapshot(barrier.path(), second_id).await;

    // Both requests have captured the same idle, unused session revision.
    // Releasing both proves only one can win the atomic admission step.
    release_route_preflight_snapshot(barrier.path(), first_id);
    release_route_preflight_snapshot(barrier.path(), second_id);
    let losing_reply = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let message = h.recv().await;
            if (message["id"] == json!(first_id) || message["id"] == json!(second_id))
                && message.get("error").is_some()
            {
                break message;
            }
        }
    })
    .await
    .expect("one preflight loses atomic session admission");
    assert!(losing_reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("prompt already in flight"));
    let winner_id = if losing_reply["id"] == json!(first_id) {
        second_id
    } else {
        first_id
    };
    wait_for_captured_request_count(&captures, 1).await;
    assert_eq!(captures.lock().await.len(), 1);
    gate_tx.send(()).expect("release admitted fake response");
    let winner_reply = h
        .recv_until(|message| message["id"] == json!(winner_id))
        .await;
    assert_eq!(winner_reply["result"]["stopReason"], "end_turn");

    let third_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "review-only reuse must fail" }],
            }),
        )
        .await;
    let third_reply = h
        .recv_until(|message| message["id"] == json!(third_id))
        .await;
    assert!(third_reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("review-only session accepts one prompt"));
    assert_eq!(captures.lock().await.len(), 1);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_task_fit_gate_uses_only_current_identity_route_attestations() {
    let (openai_url, openai_requests) =
        spawn_capturing_fake_llm(vec![openai_text("qualified model")]).await;
    let (deepseek_url, deepseek_requests) =
        spawn_capturing_fake_llm(vec![openai_text("unreviewed model")]).await;
    let reviewer = Keys::generate();
    let route_profile = RouteProfileDocument {
        version: 1,
        data_policy: RouteProfileDataPolicy::AllowHosted,
        preference_order: vec!["deepseek".into(), "openai".into()],
        strict_context_fit: false,
        max_turn_cost_microusd: None,
        prefer_fastest_measured: false,
        min_effective_output_tokens_per_second_milli: None,
        allow_preference_order_warmup: false,
        task_fit_policy: Some(TaskFitEligibilityPolicy {
            task_class: "coding".into(),
            task_class_taxonomy_version: "operator-defined-v1".into(),
            evaluation_policy_version: "task-fit-outcomes-v1".into(),
            minimum_distinct_tasks: 1,
            minimum_wilson_lower_bound_95: 0.2,
            maximum_age_seconds: 31_536_000,
            require_observed_model_identity: true,
        }),
        candidates: vec![
            RouteProfileCandidate {
                id: "deepseek".into(),
                provider: "deepseek".into(),
                model: "deepseek-chat".into(),
                data_location: RouteProfileLocation::Hosted,
                context_capacity_tokens: None,
                input_cost_microusd_per_million_tokens: None,
                output_cost_microusd_per_million_tokens: None,
                prompt_addendum: "DeepSeek candidate.".into(),
                prompt_profile: None,
            },
            RouteProfileCandidate {
                id: "openai".into(),
                provider: "openai".into(),
                model: "fixture-model-r1".into(),
                data_location: RouteProfileLocation::Hosted,
                context_capacity_tokens: None,
                input_cost_microusd_per_million_tokens: None,
                output_cost_microusd_per_million_tokens: None,
                prompt_addendum: "Reviewed OpenAI candidate.".into(),
                prompt_profile: None,
            },
        ],
        profile_id: Some("coding-route".into()),
        profile_version: Some(2),
        profile_hash: Some("7".repeat(64)),
    };
    route_profile.validate().expect("valid task-fit profile");
    let serialized_profile = serde_json::to_string(&route_profile).expect("route profile JSON");
    let profile_hash = hex::encode(Sha256::digest(serialized_profile.as_bytes()));

    let nest_dir = tempfile::tempdir().expect("temporary Buzz nest");
    let report_bytes = include_bytes!("../testdata/harbor-task-fit-v2.json");
    let report = validate_task_fit_report(report_bytes).expect("fixture report validates");
    let report_hash = report.report_sha256().to_owned();
    let report_dir = nest_dir.path().join(".agents/task-fit-evidence");
    let route_attestation_dir = report_dir.join("attestations/routes").join(&report_hash);
    fs::create_dir_all(&route_attestation_dir).expect("create local evidence store");
    fs::write(report_dir.join(format!("{report_hash}.json")), report_bytes)
        .expect("write imported report");
    let binding = TaskFitEvidenceBinding {
        report_sha256: report_hash.clone(),
        profile_id: "coding-route".into(),
        profile_version: 2,
        profile_hash: profile_hash.clone(),
        candidate_id: "openai".into(),
    };
    let payload = TaskFitRouteAttestationPayload {
        schema_version: 1,
        report_sha256: report_hash.clone(),
        action: "reviewed_for_local_route_candidate".into(),
        task_class: report.task_class().into(),
        task_class_taxonomy_version: report.task_class_taxonomy_version().into(),
        binding,
    };
    let event = EventBuilder::new(
        Kind::Custom(30078),
        serde_json::to_string(&payload).expect("attestation payload"),
    )
    .sign_with_keys(&reviewer)
    .expect("sign local route review");
    let reviewer_public_key = reviewer.public_key().to_hex();
    fs::write(
        route_attestation_dir.join(format!(
            "coding-route-v2-{}-openai-{reviewer_public_key}.json",
            profile_hash
        )),
        serde_json::to_vec(&event).expect("attestation event JSON"),
    )
    .expect("write signed route review");

    let mut h = Harness::spawn_routed_with_task_fit(
        &openai_url,
        &deepseek_url,
        serde_json::from_str(&serialized_profile).expect("route profile value"),
        TaskFitEnv {
            nest_dir: nest_dir.path().to_path_buf(),
            reviewer_public_key,
            profile_id: "coding-route".into(),
            profile_version: 2,
            profile_hash,
        },
    )
    .await;
    let sid = init_session(&mut h).await;
    let missing_class_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "missing explicit task class" }],
            }),
        )
        .await;
    let missing_class = h
        .recv_until(|message| message["id"] == json!(missing_class_id))
        .await;
    assert!(missing_class["error"]["message"]
        .as_str()
        .unwrap()
        .contains("strict_task_fit_task_class_unknown"));
    assert!(openai_requests.lock().await.is_empty());
    assert!(deepseek_requests.lock().await.is_empty());

    let mismatched_class_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "wrong explicit task class" }],
                "_meta": { "buzz": { "taskClass": {
                    "version": 1,
                    "taskClass": "code_review",
                    "taxonomyVersion": "operator-defined-v1",
                    "source": "desktop_ui"
                } } },
            }),
        )
        .await;
    let mismatched_class = h
        .recv_until(|message| message["id"] == json!(mismatched_class_id))
        .await;
    assert!(mismatched_class["error"]["message"]
        .as_str()
        .unwrap()
        .contains("strict_task_fit_task_class_mismatch"));
    assert!(openai_requests.lock().await.is_empty());
    assert!(deepseek_requests.lock().await.is_empty());

    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{ "type": "text", "text": "route this coding task" }],
                "_meta": { "buzz": { "taskClass": {
                    "version": 1,
                    "taskClass": "coding",
                    "taxonomyVersion": "operator-defined-v1",
                    "source": "desktop_ui"
                } } },
            }),
        )
        .await;
    let reply = h
        .recv_until(|message| message["id"] == json!(prompt_id))
        .await;

    assert_eq!(reply["result"]["stopReason"], "end_turn");
    assert_eq!(deepseek_requests.lock().await.len(), 0);
    let openai_requests = openai_requests.lock().await;
    assert_eq!(openai_requests.len(), 1);
    assert_eq!(openai_requests[0]["model"], "fixture-model-r1");
    assert!(openai_requests[0]["messages"][0]["content"]
        .as_str()
        .expect("system prompt is text")
        .contains("Reviewed OpenAI candidate."));
    drop(openai_requests);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_call_then_end_turn() {
    // Round 1: tool call (will fail with "unknown tool" since no MCP registered).
    // Round 2: text response → end_turn.
    let url = spawn_fake_llm(vec![
        openai_tool_call("call_xyz", "fake__do_thing", json!({"foo": "bar"})),
        openai_text("ok"),
    ])
    .await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;
    let p_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{"type":"text","text":"do something"}],
            }),
        )
        .await;

    // Tool unknown: agent emits failed tool_call_update directly (no permission ask).
    let v = h
        .recv_until(|v| {
            v.get("method") == Some(&json!("session/update"))
                && v["params"]["update"]["sessionUpdate"] == "tool_call_update"
                && v["params"]["update"]["status"] == "failed"
        })
        .await;
    assert_eq!(v["params"]["update"]["toolCallId"], "call_xyz");

    // Final response.
    let v = h.recv_until(|v| v["id"] == json!(p_id)).await;
    assert_eq!(v["result"]["stopReason"], "end_turn");
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_image_response_recovers_without_replaying_image() {
    let responses = vec![
        CannedResponse {
            status: 200,
            body: openai_tool_call("call_image", "fake__tool_0", json!({})),
        },
        CannedResponse {
            status: 404,
            body: json!({
                "error": { "message": "No endpoints found that support image input" }
            }),
        },
        CannedResponse {
            status: 200,
            body: openai_text("recovered"),
        },
    ];
    let (url, captures) = spawn_capturing_fake_llm_with_statuses(responses).await;
    let mut h = Harness::spawn(&url).await;

    h.send(
        "initialize",
        json!({"protocolVersion":2,"clientCapabilities":{}}),
    )
    .await;
    let _ = h.recv().await;
    let session_id = h
        .send(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [{
                    "name": "fake",
                    "command": env!("CARGO_BIN_EXE_fake-mcp"),
                    "args": [],
                    "env": [{ "name": "FAKE_MCP_IMAGE_RESULT", "value": "1" }],
                }],
            }),
        )
        .await;
    let session = h.recv_until(|v| v["id"] == json!(session_id)).await;
    let sid = session["result"]["sessionId"].as_str().unwrap();

    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{"type":"text","text":"inspect the image"}],
            }),
        )
        .await;
    loop {
        let message = h.recv().await;
        if message.get("method") == Some(&json!("session/request_permission")) {
            h.write(approve_permission(&message)).await;
        } else if message["id"] == json!(prompt_id) {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
    }

    let requests = captures.lock().await;
    assert_eq!(
        requests.len(),
        3,
        "expected tool, rejection, recovery requests"
    );
    let rejected = requests[1].to_string();
    assert!(
        rejected.contains("data:image/png;base64,aW1n"),
        "second request must contain the MCP image: {rejected}"
    );
    let recovered = requests[2].to_string();
    assert!(
        !recovered.contains("image_url") && !recovered.contains("data:image"),
        "recovery request must not replay image input: {recovered}"
    );
    assert!(
        recovered.contains("does not support image input")
            && recovered.contains("text-based inspection"),
        "recovery request must give the model actionable guidance: {recovered}"
    );
    assert!(
        recovered.contains("call_image") && recovered.contains("tool_call_id"),
        "recovery must preserve tool-call/result pairing: {recovered}"
    );
    drop(requests);
    h.shutdown().await;
}

/// The recovery path must only fire when it actually removed an image. If the
/// provider emits the unsupported-image phrase while history holds no image
/// (a misclassification, or a provider that returns the phrase for an
/// unrelated reason), mutating nothing and continuing would spin the turn loop
/// forever — `max_rounds` defaults to 0 (unlimited) in production, so nothing
/// downstream bounds it. The turn must fail with the typed error instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_image_without_image_in_history_fails_instead_of_looping() {
    // Five rejections but MAX_ROUNDS=4: if the guard is removed the loop
    // re-requests without ever mutating history and drains the queue.
    let responses = (0..5)
        .map(|_| CannedResponse {
            status: 404,
            body: json!({
                "error": { "message": "No endpoints found that support image input" }
            }),
        })
        .collect();
    let (url, captures) = spawn_capturing_fake_llm_with_statuses(responses).await;
    let mut h = Harness::spawn(&url).await;

    h.send(
        "initialize",
        json!({"protocolVersion":2,"clientCapabilities":{}}),
    )
    .await;
    let _ = h.recv().await;
    let session_id = h
        .send("session/new", json!({ "cwd": "/tmp", "mcpServers": [] }))
        .await;
    let session = h.recv_until(|v| v["id"] == json!(session_id)).await;
    let sid = session["result"]["sessionId"].as_str().unwrap();

    let prompt_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{"type":"text","text":"no image here"}],
            }),
        )
        .await;
    let reply = h.recv_until(|v| v["id"] == json!(prompt_id)).await;

    assert!(
        reply.get("result").is_none(),
        "an unrecoverable image rejection must not complete the turn: {reply}"
    );
    let message = reply["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("image input unsupported"),
        "the typed error must surface to the caller: {reply}"
    );
    assert_eq!(
        captures.lock().await.len(),
        1,
        "the loop must not re-request after a rejection it could not repair"
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_concurrent_prompts() {
    // Slow first response so the second prompt arrives mid-flight.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let body = openai_text("done").to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
    });

    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;
    let p1 = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid, "prompt": [{"type":"text","text":"go"}],
            }),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let p2 = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid, "prompt": [{"type":"text","text":"go again"}],
            }),
        )
        .await;

    let mut saw_p2_err = false;
    let mut saw_p1_ok = false;
    for _ in 0..10 {
        let v = h.recv().await;
        if v["id"] == json!(p2) {
            assert_eq!(v["error"]["code"], -32602);
            saw_p2_err = true;
        } else if v["id"] == json!(p1) {
            assert_eq!(v["result"]["stopReason"], "end_turn");
            saw_p1_ok = true;
        }
        if saw_p1_ok && saw_p2_err {
            break;
        }
    }
    assert!(saw_p2_err, "expected concurrent prompt rejection");
    assert!(saw_p1_ok, "first prompt didn't complete");
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_oversized_line() {
    // Set a tiny max line and send something larger; agent must abort with an
    // io error and not OOM.
    let url = spawn_fake_llm(vec![]).await;
    let bin = env!("CARGO_BIN_EXE_buzz-agent");
    let mut cmd = tokio::process::Command::new(bin);
    cmd.env("BUZZ_AGENT_PROVIDER", "openai")
        .env("OPENAI_COMPAT_API_KEY", "test")
        .env("OPENAI_COMPAT_MODEL", "fake-model")
        .env("OPENAI_COMPAT_BASE_URL", &url)
        .env("BUZZ_AGENT_MAX_LINE_BYTES", "256")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // 1024-byte line — agent should reject and exit.
    let big = "x".repeat(1024);
    let _ = stdin.write_all(big.as_bytes()).await;
    let _ = stdin.write_all(b"\n").await;
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("agent didn't exit after oversized line");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_new_rejects_oversized_system_prompt() {
    // A systemPrompt exceeding 512KB must produce a JSON-RPC error, not a panic.
    let url = spawn_fake_llm(vec![]).await;
    let mut h = Harness::spawn(&url).await;
    h.send(
        "initialize",
        json!({"protocolVersion":2,"clientCapabilities":{}}),
    )
    .await;
    let r = h.recv().await;
    assert_eq!(r["result"]["protocolVersion"], 2);

    // 600KB payload — exceeds the 512KB limit.
    let big_prompt = "x".repeat(600 * 1024);
    let id = h
        .send(
            "session/new",
            json!({"cwd":"/tmp","mcpServers":[],"systemPrompt": big_prompt}),
        )
        .await;
    let r = h.recv_until(|v| v["id"] == json!(id)).await;
    assert!(
        r.get("error").is_some(),
        "expected JSON-RPC error for oversized systemPrompt, got: {r}"
    );
    let err_msg = r["error"]["message"].as_str().unwrap_or("");
    assert!(
        err_msg.contains("512KB limit"),
        "error message should mention 512KB limit, got: {err_msg}"
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_prompt_reaches_llm_system_role() {
    // Proves the full contract: systemPrompt sent via session/new → agent appends
    // it to the effective system prompt → LLM receives it in the system role.
    let canary = "CANARY_E2E_TEST_MARKER_7f3a9b";
    let (url, captures) = spawn_capturing_fake_llm(vec![openai_text("done")]).await;
    let mut h = Harness::spawn(&url).await;

    // initialize.
    h.send(
        "initialize",
        json!({"protocolVersion":2,"clientCapabilities":{}}),
    )
    .await;
    let r = h.recv().await;
    assert_eq!(r["result"]["protocolVersion"], 2);

    // session/new with systemPrompt containing the canary.
    let sn_id = h
        .send(
            "session/new",
            json!({"cwd":"/tmp","mcpServers":[],"systemPrompt": canary}),
        )
        .await;
    let r = h.recv_until(|v| v["id"] == json!(sn_id)).await;
    let sid = r["result"]["sessionId"].as_str().unwrap().to_owned();
    assert!(sid.starts_with("ses_"));

    // session/prompt — triggers the LLM call.
    let p_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{"type":"text","text":"hello"}],
            }),
        )
        .await;
    let _ = h.recv_until(|v| v["id"] == json!(p_id)).await;

    // Inspect the captured LLM request.
    let reqs = captures.lock().await;
    assert!(!reqs.is_empty(), "expected at least one LLM request");
    let llm_req = &reqs[0];
    let messages = llm_req["messages"].as_array().expect("messages array");

    // First message should be the system role.
    let system_msg = &messages[0];
    assert_eq!(
        system_msg["role"], "system",
        "first message must be system role"
    );
    let system_content = system_msg["content"].as_str().unwrap_or("");

    // Canary must appear in the system message (proves systemPrompt was used as base).
    assert!(
        system_content.contains(canary),
        "system message must contain the canary string.\nGot: {system_content}"
    );

    // The agent's default prompt must NOT appear — it is suppressed when
    // the harness provides a systemPrompt.
    let default_prompt = "You are buzz-agent";
    assert!(
        !system_content.contains(default_prompt),
        "system message must NOT contain the default prompt when systemPrompt is provided.\nGot: {system_content}"
    );

    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_prompt_absent_no_canary() {
    // Negative case: when systemPrompt is NOT sent in session/new, the canary
    // must NOT appear in the LLM system message.
    let canary = "CANARY_E2E_TEST_MARKER_7f3a9b";
    let (url, captures) = spawn_capturing_fake_llm(vec![openai_text("done")]).await;
    let mut h = Harness::spawn(&url).await;

    // initialize.
    h.send(
        "initialize",
        json!({"protocolVersion":2,"clientCapabilities":{}}),
    )
    .await;
    let _ = h.recv().await;

    // session/new WITHOUT systemPrompt field.
    let sn_id = h
        .send("session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;
    let r = h.recv_until(|v| v["id"] == json!(sn_id)).await;
    let sid = r["result"]["sessionId"].as_str().unwrap().to_owned();

    // session/prompt — triggers the LLM call.
    let p_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{"type":"text","text":"hello"}],
            }),
        )
        .await;
    let _ = h.recv_until(|v| v["id"] == json!(p_id)).await;

    // Inspect the captured LLM request.
    let reqs = captures.lock().await;
    assert!(!reqs.is_empty(), "expected at least one LLM request");
    let llm_req = &reqs[0];
    let messages = llm_req["messages"].as_array().expect("messages array");
    let system_msg = &messages[0];
    assert_eq!(system_msg["role"], "system");
    let system_content = system_msg["content"].as_str().unwrap_or("");

    // Canary must NOT appear (it was never sent).
    assert!(
        !system_content.contains(canary),
        "system message must NOT contain canary when systemPrompt is absent.\nGot: {system_content}"
    );

    // But the agent's default prompt should still be there.
    assert!(
        system_content.contains("You are buzz-agent"),
        "system message must still contain the agent's default prompt"
    );

    h.shutdown().await;
}

// ─── Steering (_goose/unstable/session/steer) ───────────────────────────────

/// Wait for the `activeRunId` advert buzz-agent emits at prompt start and
/// return the run id, so a steer can target the live turn.
async fn recv_active_run_id(h: &mut Harness) -> String {
    let v = h
        .recv_until(|v| {
            v.get("method") == Some(&json!("session/update"))
                && v["params"]["update"]["_meta"]["goose"]["activeRunId"].is_string()
        })
        .await;
    v["params"]["update"]["_meta"]["goose"]["activeRunId"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steer_folds_into_active_turn_without_cancelling() {
    use tokio::sync::oneshot;

    // A two-round turn (tool call → text). A steer sent once the run is live
    // must (a) be accepted with the matching runId, (b) NOT cancel the turn —
    // it still ends with end_turn — and (c) reach the provider as a user turn.
    //
    // The steer is drained only at a round boundary (before the next provider
    // request), so it must be enqueued before round 2 begins. Without
    // synchronization a fast worker can complete round 1, drain an empty steer
    // queue at the round-2 boundary, and dispatch round 2 before the steer is
    // even sent — the steer then lands after the turn ends and never reaches
    // the provider. To make this deterministic, the FIRST provider response is
    // gated: it is withheld until the steer has been sent AND observed
    // accepted, so round 1 cannot complete (and round 2 cannot start its drain)
    // until the steer is already queued.
    let (gate_tx, gate_rx) = oneshot::channel::<()>();
    let gate_rx = Arc::new(Mutex::new(Some(gate_rx)));

    let responses = vec![
        CannedResponse {
            status: 200,
            body: openai_tool_call("call_steer", "fake__noop", json!({})),
        },
        CannedResponse {
            status: 200,
            body: openai_text("acknowledged the steer"),
        },
    ];
    let captures: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let (url, _) = spawn_gated_capturing_fake_llm(responses, captures.clone(), gate_rx).await;

    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    let p_id = h
        .send(
            "session/prompt",
            json!({
                "sessionId": sid,
                "prompt": [{"type":"text","text":"work on the original task"}],
            }),
        )
        .await;

    // Learn the run id (advertised before the gated round-1 request), then steer
    // into the live turn while round 1 is still held.
    let run_id = recv_active_run_id(&mut h).await;
    let steer_text = "STEER-CANARY: also consider the edge case";
    let s_id = h
        .send(
            "_goose/unstable/session/steer",
            json!({
                "sessionId": sid,
                "expectedRunId": run_id,
                "prompt": [{"type":"text","text": steer_text}],
            }),
        )
        .await;

    // Steer is accepted and echoes the run id it landed in. Only after this
    // confirmation do we release the gate, so the steer is guaranteed queued
    // before round 2's boundary drains it.
    let mut steer_ok = false;
    let mut end_turn = false;
    let mut gate = Some(gate_tx);
    for _ in 0..40 {
        let v = h.recv().await;
        if v["id"] == json!(s_id) {
            assert_eq!(
                v["result"]["runId"],
                json!(run_id),
                "steer ran into the live turn"
            );
            assert!(
                v["result"]["messageId"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("steer_")),
                "steer reply carries a messageId"
            );
            steer_ok = true;
            // Steer accepted — release round 1 so the turn proceeds to round 2,
            // whose boundary now drains the queued steer.
            if let Some(tx) = gate.take() {
                let _ = tx.send(());
            }
        } else if v["id"] == json!(p_id) {
            // The turn was NOT cancelled — it completed normally.
            assert_eq!(v["result"]["stopReason"], "end_turn");
            end_turn = true;
        }
        if steer_ok && end_turn {
            break;
        }
    }
    assert!(steer_ok, "steer request was not accepted");
    assert!(end_turn, "turn did not complete with end_turn after steer");

    // The steered text reached the provider as a user message in some round.
    let reqs = captures.lock().await;
    let saw_steer = reqs.iter().any(|req| {
        req["messages"].as_array().is_some_and(|msgs| {
            msgs.iter().any(|m| {
                m["role"] == "user"
                    && m["content"]
                        .as_str()
                        .is_some_and(|c| c.contains(steer_text))
            })
        })
    });
    assert!(
        saw_steer,
        "steered text never reached the provider; captured requests: {reqs:#?}"
    );
    drop(reqs);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steer_rejected_when_no_active_run() {
    // No prompt in flight → no active run → invalid_params.
    let url = spawn_fake_llm(vec![]).await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    let s_id = h
        .send(
            "_goose/unstable/session/steer",
            json!({
                "sessionId": sid,
                "expectedRunId": "run_does_not_exist",
                "prompt": [{"type":"text","text":"hello?"}],
            }),
        )
        .await;
    let v = h.recv_until(|v| v["id"] == json!(s_id)).await;
    assert_eq!(v["error"]["code"], -32602, "expected invalid_params");
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steer_rejected_on_run_id_mismatch() {
    // A live run, but the caller targets a stale/wrong run id → invalid_params,
    // so the client falls back to cancel+merge instead of injecting blind.
    let (url, _captures) = spawn_capturing_fake_llm(vec![
        openai_tool_call("call_x", "fake__noop", json!({})),
        openai_text("done"),
    ])
    .await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    let p_id = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"go"}]}),
        )
        .await;
    let _live_run = recv_active_run_id(&mut h).await;

    let s_id = h
        .send(
            "_goose/unstable/session/steer",
            json!({
                "sessionId": sid,
                "expectedRunId": "run_stale_mismatch",
                "prompt": [{"type":"text","text":"too late"}],
            }),
        )
        .await;

    let mut saw_reject = false;
    for _ in 0..40 {
        let v = h.recv().await;
        if v["id"] == json!(s_id) {
            assert_eq!(
                v["error"]["code"], -32602,
                "mismatched runId must be rejected"
            );
            saw_reject = true;
        } else if v["id"] == json!(p_id) {
            // Turn finishes normally regardless of the rejected steer.
            break;
        }
    }
    assert!(saw_reject, "run-id mismatch was not rejected");
    h.shutdown().await;
}

// ─── Usage notification (_goose/unstable/session/update usage_update) ───────

/// An OpenAI chat completion response with a `usage` block (prompt_tokens +
/// completion_tokens). buzz-agent maps these to `accumulatedInputTokens` /
/// `accumulatedOutputTokens` in the `_goose/unstable/session/update` notification.
fn openai_text_with_usage(content: &str, input_tokens: u64, output_tokens: u64) -> Value {
    json!({
        "id": "cc-u", "object": "chat.completion", "model": "fake-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        },
    })
}

/// An OpenAI chat completion response WITH i/o usage but WITHOUT `total_tokens`.
/// Simulates a provider that omits the genuine total from its usage block.
/// buzz-agent must treat this turn's total as Unknown and poison the cumulative.
fn openai_text_with_usage_no_total(content: &str, input_tokens: u64, output_tokens: u64) -> Value {
    json!({
        "id": "cc-nt", "object": "chat.completion", "model": "fake-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            // total_tokens deliberately absent — simulates Anthropic or any
            // provider that does not report a genuine total.
        },
    })
}

/// Returns true when `v` is a `_goose/unstable/session/update` usage_update
/// notification.
fn is_usage_update(v: &Value) -> bool {
    v.get("method") == Some(&json!("_goose/unstable/session/update"))
        && v["params"]["update"]["sessionUpdate"] == "usage_update"
}

/// Collect every frame that arrives BEFORE the message matching `until_pred`,
/// then return (frames_before, matching_frame).
async fn recv_until_with_drain<F>(h: &mut Harness, mut until_pred: F) -> (Vec<Value>, Value)
where
    F: FnMut(&Value) -> bool,
{
    let mut before = Vec::new();
    loop {
        let v = h.recv().await;
        if until_pred(&v) {
            return (before, v);
        }
        before.push(v);
    }
}

/// buzz-agent must emit `_goose/unstable/session/update` with `sessionUpdate:
/// "usage_update"` **before** the `session/prompt` response on each turn, and
/// must accumulate counters across turns (turn 2 reports turn1+turn2 sums).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_notification_emitted_before_prompt_response() {
    let url = spawn_fake_llm(vec![
        openai_text_with_usage("turn one reply", 10, 5),
        openai_text_with_usage("turn two reply", 20, 8),
    ])
    .await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    // ── Turn 1 ──────────────────────────────────────────────────────────────
    let p1 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"turn 1"}]}),
        )
        .await;

    let (frames_before_t1, response_t1) = recv_until_with_drain(&mut h, |v| v["id"] == p1).await;
    assert_eq!(
        response_t1["result"]["stopReason"], "end_turn",
        "turn 1 must complete with end_turn"
    );

    // A usage_update notification must appear in the frames before the response.
    let usage_t1 = frames_before_t1
        .iter()
        .find(|v| is_usage_update(v))
        .unwrap_or_else(|| {
            panic!(
                "expected _goose/unstable/session/update usage_update before turn-1 response; frames: {frames_before_t1:#?}"
            )
        });
    assert_eq!(
        usage_t1["params"]["update"]["sessionUpdate"], "usage_update",
        "sessionUpdate field must be 'usage_update'"
    );
    assert_eq!(
        usage_t1["params"]["update"]["accumulatedInputTokens"],
        json!(10u64),
        "turn 1 accumulated input tokens"
    );
    assert_eq!(
        usage_t1["params"]["update"]["accumulatedOutputTokens"],
        json!(5u64),
        "turn 1 accumulated output tokens"
    );

    // ── Turn 2 ──────────────────────────────────────────────────────────────
    let p2 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"turn 2"}]}),
        )
        .await;

    let (frames_before_t2, response_t2) = recv_until_with_drain(&mut h, |v| v["id"] == p2).await;
    assert_eq!(
        response_t2["result"]["stopReason"], "end_turn",
        "turn 2 must complete with end_turn"
    );

    // Notification arrives before the response, with cumulative sums (10+20, 5+8).
    let usage_t2 = frames_before_t2
        .iter()
        .find(|v| is_usage_update(v))
        .unwrap_or_else(|| {
            panic!(
                "expected _goose/unstable/session/update usage_update before turn-2 response; frames: {frames_before_t2:#?}"
            )
        });
    assert_eq!(
        usage_t2["params"]["update"]["accumulatedInputTokens"],
        json!(30u64),
        "turn 2 accumulated input tokens must be 10+20=30"
    );
    assert_eq!(
        usage_t2["params"]["update"]["accumulatedOutputTokens"],
        json!(13u64),
        "turn 2 accumulated output tokens must be 5+8=13"
    );

    h.shutdown().await;
}

/// When the provider returns a response with no `usage` block, buzz-agent must
/// NOT emit a `_goose/unstable/session/update` notification for that turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_usage_turn_emits_no_usage_notification() {
    let url = spawn_fake_llm(vec![openai_text("no usage here")]).await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    let p_id = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"go"}]}),
        )
        .await;

    let (frames_before, response) = recv_until_with_drain(&mut h, |v| v["id"] == p_id).await;
    assert_eq!(
        response["result"]["stopReason"], "end_turn",
        "turn must complete with end_turn"
    );

    // No usage notification must appear in the frames before the response.
    let found = frames_before.iter().any(is_usage_update);
    assert!(
        !found,
        "expected NO usage_update notification when provider reports no usage; frames: {frames_before:#?}"
    );

    h.shutdown().await;
}

/// Usage must be reported after EVERY provider round, not only once the turn
/// returns.
///
/// A turn is many provider round-trips over many minutes. While the only report
/// was the one `session/prompt` sends after the turn returns, a turn whose
/// process was killed mid-flight reported nothing at all: its counters lived in
/// the prompt task's stack frame, the provider had already billed them, and no
/// consumer ever saw them. That is not a corner case for a long-horizon
/// benchmark — every phase of a `continue_until_timeout` run is terminated
/// mid-turn by design, which under-reported one measured run's cost several-fold.
///
/// Two rounds with distinct usage. The assertion that matters is the FIRST
/// notification: it must carry round 1's counts alone, proving it was sent
/// before round 2 had returned, so a kill between the rounds would still have
/// left round 1 on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_is_reported_after_each_round_not_only_at_turn_end() {
    let url = spawn_fake_llm(vec![
        openai_tool_call_with_usage("call_round1", "fake__noop", json!({}), 15, 6),
        openai_text_with_usage("done", 20, 8),
    ])
    .await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    let p_id = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"go"}]}),
        )
        .await;

    let (frames_before, response) = recv_until_with_drain(&mut h, |v| v["id"] == p_id).await;
    assert_eq!(
        response["result"]["stopReason"], "end_turn",
        "turn must complete with end_turn"
    );

    let usage: Vec<&Value> = frames_before
        .iter()
        .filter(|v| is_usage_update(v))
        .collect();
    assert!(
        usage.len() >= 2,
        "expected a usage_update per round (2 rounds), got {}; frames: {frames_before:#?}",
        usage.len()
    );

    // Round 1 alone — emitted while round 2 was still outstanding.
    assert_eq!(
        usage[0]["params"]["update"]["accumulatedInputTokens"],
        json!(15u64),
        "first notification must carry round 1's input tokens only"
    );
    assert_eq!(
        usage[0]["params"]["update"]["accumulatedOutputTokens"],
        json!(6u64),
        "first notification must carry round 1's output tokens only"
    );

    // The last one is the turn total and is what a high-water-mark consumer keeps.
    let last = usage[usage.len() - 1];
    assert_eq!(
        last["params"]["update"]["accumulatedInputTokens"],
        json!(35u64),
        "final notification must carry the turn total 15+20=35"
    );
    assert_eq!(
        last["params"]["update"]["accumulatedOutputTokens"],
        json!(14u64),
        "final notification must carry the turn total 6+8=14"
    );

    h.shutdown().await;
}

/// A mid-turn report must be SESSION-cumulative, not turn-local.
///
/// The baseline handed to the run loop is a snapshot taken when the turn began;
/// if it were dropped, a consumer taking the high-water mark per session would
/// see turn 2's first round (a small number) arrive after turn 1's total and
/// discard it, silently losing turn 2 for any turn that never completed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mid_turn_usage_includes_earlier_turns() {
    let url = spawn_fake_llm(vec![
        openai_text_with_usage("turn one", 10, 5),
        openai_tool_call_with_usage("call_t2", "fake__noop", json!({}), 20, 8),
        openai_text_with_usage("turn two done", 30, 9),
    ])
    .await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    let p1 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"turn 1"}]}),
        )
        .await;
    let (_, _) = recv_until_with_drain(&mut h, |v| v["id"] == p1).await;

    let p2 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"turn 2"}]}),
        )
        .await;
    let (frames_before, _) = recv_until_with_drain(&mut h, |v| v["id"] == p2).await;

    let first = frames_before
        .iter()
        .find(|v| is_usage_update(v))
        .unwrap_or_else(|| {
            panic!("expected a usage_update during turn 2; frames: {frames_before:#?}")
        });
    assert_eq!(
        first["params"]["update"]["accumulatedInputTokens"],
        json!(30u64),
        "turn 2 round 1 must report 10 (turn 1) + 20 (this round), not 20"
    );
    assert_eq!(
        first["params"]["update"]["accumulatedOutputTokens"],
        json!(13u64),
        "turn 2 round 1 must report 5 (turn 1) + 8 (this round), not 8"
    );

    h.shutdown().await;
}

/// When a turn is cancelled AFTER the provider has already returned a response
/// (so token counts are observed), buzz-agent must still emit the usage
/// notification before the cancelled `session/prompt` response.
///
/// Setup: round 1 is a tool call WITH usage (tokens are captured). After the
/// tool_call_update notification (proving round 1 is fully processed), we gate
/// the round-2 LLM response behind a `oneshot` barrier that only releases after
/// cancel is acknowledged. This guarantees the turn exits with `stopReason: "cancelled"`
/// deterministically, even on a slow CI worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_turn_with_usage_emits_notification_before_response() {
    use tokio::sync::oneshot;

    // Gate: the second LLM request (round 2) is held until we explicitly release it.
    let (gate_tx, gate_rx) = oneshot::channel::<()>();
    let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));

    // Round 1: tool call with usage — sets turn_input/output_tokens.
    // Round 2: gated — blocked until cancel fires, then released so the
    // in-flight TCP request can resolve. The queue is empty for round 2, so the
    // agent receives the fallback "no canned response" body which it treats as
    // an LLM error; the cancel check at the round boundary fires first because
    // the gate is only released after cancel is acknowledged.
    let responses = vec![openai_tool_call_with_usage(
        "call_cancel_test",
        "fake__noop",
        json!({}),
        15,
        6,
    )];
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
    let gate_rx_clone = gate_rx.clone();
    tokio::spawn(async move {
        let mut request_num = 0usize;
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let queue = queue.clone();
            let gate = gate_rx_clone.clone();
            request_num += 1;
            let req_num = request_num;
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                    if buf.len() > 1_000_000 {
                        return;
                    }
                }
                // For request 2+ (round 2), wait for the gate to open before
                // responding. This ensures cancel is processed before round 2 resolves,
                // making stopReason: cancelled deterministic.
                if req_num >= 2 {
                    let rx = gate.lock().await.take();
                    if let Some(rx) = rx {
                        let _ = rx.await;
                    }
                }
                let body = queue
                    .lock()
                    .await
                    .pop_front()
                    .unwrap_or_else(|| json!({ "error": "no canned response" }));
                let body_s = serde_json::to_string(&body).unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_s.len(),
                    body_s,
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });

    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    let p_id = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"start work"}]}),
        )
        .await;

    // Wait for the activeRunId advert (agent is live).
    let _run_id = recv_active_run_id(&mut h).await;
    // Wait for tool_call_update — proves round 1 LLM response is fully processed
    // and tokens are captured before we send cancel.
    h.recv_until(|v| {
        v.get("method") == Some(&json!("session/update"))
            && v["params"]["update"]["sessionUpdate"] == "tool_call_update"
    })
    .await;

    // Writing to stdin does not prove the agent processed cancel. Keep the
    // provider blocked until the acknowledgement, retaining all earlier frames
    // so usage/prompt ordering is checked even if the turn finishes before ACK.
    let c_id = h.send("session/cancel", json!({"sessionId": sid})).await;
    let (frames_before_cancel_ack, cancel_ack) =
        recv_until_with_drain(&mut h, |v| v["id"] == json!(c_id)).await;
    assert_eq!(cancel_ack.get("result"), Some(&Value::Null), "{cancel_ack}");
    assert!(cancel_ack.get("error").is_none(), "{cancel_ack}");
    let _ = gate_tx.send(()); // unblock round 2

    let mut saw_usage_before_prompt_response = false;
    let mut saw_usage = false;
    let mut saw_prompt_response = false;
    let mut pending_frames = VecDeque::from(frames_before_cancel_ack);
    let frame_budget = 40 + pending_frames.len();
    for _ in 0..frame_budget {
        let v = match pending_frames.pop_front() {
            Some(v) => v,
            None => h.recv().await,
        };
        if is_usage_update(&v) {
            saw_usage = true;
            if !saw_prompt_response {
                saw_usage_before_prompt_response = true;
            }
        } else if v["id"] == json!(p_id) {
            saw_prompt_response = true;
            // The gate guarantees stopReason: cancelled — not a race-driven error.
            assert_eq!(
                v["result"]["stopReason"], "cancelled",
                "turn must end with stopReason: cancelled"
            );
        }
        if saw_usage && saw_prompt_response {
            break;
        }
    }
    assert!(
        saw_prompt_response,
        "session/prompt did not finish after cancel"
    );
    assert!(
        saw_usage,
        "expected usage_update notification for cancelled turn with observed tokens"
    );
    assert!(
        saw_usage_before_prompt_response,
        "usage_update must arrive before the session/prompt response"
    );

    h.shutdown().await;
}

/// A tool-call OpenAI response with a `usage` block. Used to capture tokens in
/// round 1 before a cancel fires at the round boundary.
fn openai_tool_call_with_usage(
    id: &str,
    name: &str,
    args: Value,
    input_tokens: u64,
    output_tokens: u64,
) -> Value {
    json!({
        "id": "cc-u2", "object": "chat.completion", "model": "fake-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant", "content": null,
                "tool_calls": [{
                    "id": id, "type": "function",
                    "function": { "name": name, "arguments": args.to_string() },
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        },
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steer_rejected_on_empty_prompt() {
    let (url, _captures) = spawn_capturing_fake_llm(vec![
        openai_tool_call("call_x", "fake__noop", json!({})),
        openai_text("done"),
    ])
    .await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;
    let p_id = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"go"}]}),
        )
        .await;
    let run_id = recv_active_run_id(&mut h).await;
    let s_id = h
        .send(
            "_goose/unstable/session/steer",
            json!({"sessionId": sid, "expectedRunId": run_id, "prompt": []}),
        )
        .await;
    let mut saw_reject = false;
    for _ in 0..40 {
        let v = h.recv().await;
        if v["id"] == json!(s_id) {
            assert_eq!(v["error"]["code"], -32602, "empty prompt must be rejected");
            saw_reject = true;
        } else if v["id"] == json!(p_id) {
            break;
        }
    }
    assert!(saw_reject, "empty steer prompt was not rejected");
    h.shutdown().await;
}

// ─── Session-boundary total accumulation ────────────────────────────────────

/// Once a usage-bearing turn lacks a provider total, the session cumulative
/// becomes Unknown and `accumulatedTotalTokens` must be absent from subsequent
/// `usage_update` notifications — even if later turns supply a total.
///
/// Sequence: turn 1 has total, turn 2 lacks total → session poisoned, turn 3
/// has total → still poisoned. Only turn 1 must carry `accumulatedTotalTokens`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_total_poisoned_by_missing_total_and_stays_poisoned() {
    let url = spawn_fake_llm(vec![
        openai_text_with_usage("t1", 10, 5), // total present → Exact(15)
        openai_text_with_usage_no_total("t2", 20, 8), // total absent  → Unknown
        openai_text_with_usage("t3", 15, 6), // total present → still Unknown
    ])
    .await;
    let mut h = Harness::spawn(&url).await;
    let sid = init_session(&mut h).await;

    // ── Turn 1: total present ───────────────────────────────────────────────
    let p1 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"t1"}]}),
        )
        .await;
    let (frames1, _) = recv_until_with_drain(&mut h, |v| v["id"] == p1).await;
    let usage1 = frames1
        .iter()
        .find(|v| is_usage_update(v))
        .expect("usage_update for turn 1");
    assert_eq!(
        usage1["params"]["update"]["accumulatedTotalTokens"],
        json!(15u64),
        "turn 1 has genuine total; accumulatedTotalTokens must be 15"
    );

    // ── Turn 2: total absent — session is now poisoned ──────────────────────
    let p2 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"t2"}]}),
        )
        .await;
    let (frames2, _) = recv_until_with_drain(&mut h, |v| v["id"] == p2).await;
    let usage2 = frames2
        .iter()
        .find(|v| is_usage_update(v))
        .expect("usage_update for turn 2");
    assert!(
        usage2["params"]["update"]["accumulatedTotalTokens"].is_null()
            || usage2["params"]["update"]
                .get("accumulatedTotalTokens")
                .is_none(),
        "turn 2 lacked total; accumulatedTotalTokens must be absent/null; got: {usage2:#?}"
    );

    // ── Turn 3: total present, but session is still poisoned ─────────────────
    let p3 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"t3"}]}),
        )
        .await;
    let (frames3, _) = recv_until_with_drain(&mut h, |v| v["id"] == p3).await;
    let usage3 = frames3
        .iter()
        .find(|v| is_usage_update(v))
        .expect("usage_update for turn 3");
    assert!(
        usage3["params"]["update"]["accumulatedTotalTokens"].is_null()
            || usage3["params"]["update"]
                .get("accumulatedTotalTokens")
                .is_none(),
        "session is poisoned; accumulatedTotalTokens must remain absent even after a total-bearing turn; got: {usage3:#?}"
    );

    // i/o counters are unaffected by total poisoning.
    assert_eq!(
        usage3["params"]["update"]["accumulatedInputTokens"],
        json!(45u64),
        "poisoned total must not discard input accumulation"
    );
    assert_eq!(
        usage3["params"]["update"]["accumulatedOutputTokens"],
        json!(19u64),
        "poisoned total must not discard output accumulation"
    );

    h.shutdown().await;
}

/// A new session starts fresh and can accumulate an exact total independently
/// of any previous session. This verifies `accumulated_total_state` is reset
/// to `Unseen` on `session/new`, not inherited from a prior session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_session_resets_total_accumulation() {
    // Session A: two turns both with totals → Exact should accumulate.
    // Session B (new session/new call): starts fresh.
    let url = spawn_fake_llm(vec![
        // Session A, turn 1
        openai_text_with_usage("s1t1", 10, 5),
        // Session A, turn 2
        openai_text_with_usage("s1t2", 20, 8),
        // Session B, turn 1
        openai_text_with_usage("s2t1", 30, 10),
    ])
    .await;
    let mut h = Harness::spawn(&url).await;
    let sid_a = init_session(&mut h).await;

    // Session A, turn 1
    let p1 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid_a, "prompt": [{"type":"text","text":"s1t1"}]}),
        )
        .await;
    let (frames1, _) = recv_until_with_drain(&mut h, |v| v["id"] == p1).await;
    let u1 = frames1.iter().find(|v| is_usage_update(v)).expect("usage1");
    assert_eq!(
        u1["params"]["update"]["accumulatedTotalTokens"],
        json!(15u64),
        "session A turn 1 accumulated total"
    );

    // Session A, turn 2 — cumulative total is 15+28=43
    let p2 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid_a, "prompt": [{"type":"text","text":"s1t2"}]}),
        )
        .await;
    let (frames2, _) = recv_until_with_drain(&mut h, |v| v["id"] == p2).await;
    let u2 = frames2.iter().find(|v| is_usage_update(v)).expect("usage2");
    assert_eq!(
        u2["params"]["update"]["accumulatedTotalTokens"],
        json!(43u64),
        "session A turn 2 cumulative total must be 15+28=43"
    );

    // Start a new session — must reset accumulated_total_state to Unseen.
    let sid_b = init_session(&mut h).await;
    assert_ne!(sid_a, sid_b, "sessions must have distinct IDs");

    // Session B, turn 1 — total 30+10=40. Must NOT start from 43.
    let p3 = h
        .send(
            "session/prompt",
            json!({"sessionId": sid_b, "prompt": [{"type":"text","text":"s2t1"}]}),
        )
        .await;
    let (frames3, _) = recv_until_with_drain(&mut h, |v| v["id"] == p3).await;
    let u3 = frames3.iter().find(|v| is_usage_update(v)).expect("usage3");
    assert_eq!(
        u3["params"]["update"]["accumulatedTotalTokens"],
        json!(40u64),
        "new session must start fresh — accumulated total must be 40, not 83"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn synthetic_probe_subcommand_sends_one_fixed_bounded_request_and_redacts_receipt() {
    let (url, requests) = spawn_capturing_fake_llm(vec![openai_text("BUZZ_PROBE_OK")]).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_buzz-agent"))
        .arg("synthetic-probe")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BUZZ_AGENT_PROVIDER", "openai")
        .env("BUZZ_AGENT_MODEL", "probe-only-model")
        .env("OPENAI_COMPAT_API_KEY", "PROBE_SECRET_SHOULD_NOT_ECHO")
        .env("OPENAI_COMPAT_BASE_URL", &url)
        .env("OPENAI_COMPAT_API", "chat")
        .env("BUZZ_AGENT_PROBE_LOCAL", "1")
        .output()
        .await
        .expect("run synthetic probe");

    assert!(
        output.status.success(),
        "probe subprocess should return a JSON receipt: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).expect("JSON receipt");
    assert_eq!(receipt["status"], "responded");
    assert_eq!(receipt["provider"], "openai");
    assert_eq!(receipt["requestedModel"], "probe-only-model");
    assert_eq!(receipt["responseMarkerMatched"], true);
    assert!(
        receipt.get("text").is_none(),
        "model output is not retained"
    );
    let output_text = String::from_utf8_lossy(&output.stdout);
    assert!(!output_text.contains("PROBE_SECRET_SHOULD_NOT_ECHO"));

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 1, "one candidate produces one request");
    assert_eq!(requests[0]["model"], "probe-only-model");
    assert_eq!(requests[0]["max_completion_tokens"], 256);
    assert!(requests[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| { message["content"] == "Reply exactly with BUZZ_PROBE_OK." }));
}
