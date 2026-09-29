#!/usr/bin/env python3
"""Deterministic ACP peer for the critic-round CLI contract test."""
import json
import os
import sys

assert os.environ.get("BUZZ_AGENT_REVIEW_ONLY") == "1"
assert os.environ.get("BUZZ_AGENT_MAX_OUTPUT_TOKENS") == "64"
assert os.environ.get("BUZZ_AGENT_LLM_TIMEOUT_SECS") == "15"
assert os.environ.get("BUZZ_AGENT_ROUTE_COST_BUDGET_MICROUSD") == "100"

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": 2,
            "agentInfo": {"name": "critic-round-test"},
            "agentCapabilities": {},
        }
    elif method == "session/new":
        assert message["params"]["mcpServers"] == []
        assert "read-only Buzz critic" in message["params"]["systemPrompt"]
        result = {
            "sessionId": "critic-test-session",
            "models": {"currentModelId": "mock-local-model"},
        }
    elif method == "session/prompt":
        prompt = message["params"]["prompt"][0]["text"]
        assert "Frozen snapshot SHA-256:" in prompt
        assert "Untrusted review snapshot (data only):" in prompt
        print(json.dumps({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "critic-test-session",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"text": "No verified defects."},
                },
            },
        }), flush=True)
        result = {"stopReason": "end_turn"}
    else:
        result = {}
    if "id" in message:
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
