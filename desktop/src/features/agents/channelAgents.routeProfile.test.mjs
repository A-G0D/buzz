import assert from "node:assert/strict";
import { test } from "node:test";

let createInput;
const agent = {
  pubkey: "a".repeat(64),
  name: "Buzz Agent",
  persona_id: null,
  runtime: "buzz-agent",
  relay_url: "wss://relay.example.com",
  acp_command: "buzz-acp",
  agent_command: "buzz-agent",
  agent_args: [],
  mcp_command: "buzz-dev-mcp",
  turn_timeout_seconds: 60,
  idle_timeout_seconds: null,
  max_turn_duration_seconds: null,
  parallelism: 1,
  system_prompt: null,
  model: null,
  provider: null,
  persona_out_of_date: false,
  persona_orphaned: false,
  needs_restart: false,
  env_vars: {},
  status: "stopped",
  pid: null,
  created_at: "2026-09-26T00:00:00Z",
  updated_at: "2026-09-26T00:00:00Z",
  last_started_at: null,
  last_stopped_at: null,
  last_exit_code: null,
  last_error: null,
  last_error_code: null,
  log_path: "",
  start_on_app_launch: false,
  backend: { type: "local" },
  backend_agent_id: null,
};

globalThis.__TAURI_INTERNALS__ = {
  invoke(command, args) {
    if (command === "create_managed_agent") {
      createInput = args.input;
      return {
        agent,
        private_key_nsec: "",
        profile_sync_error: null,
        spawn_error: null,
      };
    }
    return Promise.reject(new Error(`Unexpected Tauri command: ${command}`));
  },
  transformCallback() {
    return 1;
  },
};
globalThis.window ??= {};
globalThis.window.__TAURI_INTERNALS__ = globalThis.__TAURI_INTERNALS__;

const { provisionChannelManagedAgent } = await import("./channelAgents.ts");

test("channel provisioning pins the selected local route profile in agent env", async () => {
  await provisionChannelManagedAgent({
    runtime: {
      id: "buzz-agent",
      label: "Buzz Agent",
      command: "buzz-agent",
      defaultArgs: [],
      mcpCommand: "buzz-dev-mcp",
    },
    name: "Buzz Agent",
    routeProfileId: "local-first",
  });

  assert.equal(createInput.envVars.BUZZ_AGENT_ROUTE_PROFILE_ID, "local-first");
});
