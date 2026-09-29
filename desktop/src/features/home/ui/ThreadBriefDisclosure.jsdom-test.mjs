import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import React from "react";

const handlers = new Map();
const tauriMock = {
  invoke(command, args) {
    const handler = handlers.get(command);
    if (handler) return handler(args);
    return Promise.reject(new Error(`Unexpected Tauri command: ${command}`));
  },
  transformCallback() {
    return 1;
  },
};
globalThis.__TAURI_INTERNALS__ = tauriMock;
globalThis.window.__TAURI_INTERNALS__ = tauriMock;
globalThis.window.matchMedia ??= () => ({
  matches: false,
  addEventListener() {},
  removeEventListener() {},
});

const { act, cleanup, fireEvent, render, screen, waitFor } = await import(
  "@testing-library/react"
);
const { ThreadBriefDisclosure } = await import("./ThreadBriefDisclosure.tsx");

afterEach(() => {
  cleanup();
  handlers.clear();
});

test("thread brief shows local attempt evidence without implying completion", async () => {
  const sourceId = "b".repeat(64);
  const initialBrief = {
    thread_root_id: "a".repeat(64),
    original_intent: {
      id: "a".repeat(64),
      kind: 45001,
      pubkey: "c".repeat(64),
      content: "Implement the feature and report status.",
      created_at: 1_780_000_000,
      tags: [],
    },
    summary: {
      text: "One reply event observed. Task completion is unknown.",
      method: "deterministic",
      task_completion: "unknown",
    },
    progress_events: [],
    auxiliary_events: [],
    status: {
      task_state: "unknown",
      observed_at_ms: 1_780_000_002_000,
      reply_event_count: 0,
      latest_activity: null,
      next_cursor: { created_at: 1_780_000_000, event_id: sourceId },
      requested_depth_limit: null,
      applied_depth_limit: 64,
      depth_limit_may_truncate: true,
      possibly_truncated: true,
      steering_controls: [
        {
          turn_id: "123e4567-e89b-12d3-a456-426614174000",
          agent_index: 0,
          source_event_id: sourceId,
          state: "adapter_acknowledged",
          submission_recorded: false,
          submitted_at_ms: null,
          adapter_outcome: "adapter_acknowledged",
          outcome_at_ms: 1_780_000_001_000,
          agent_observed: "unknown",
          event_history_may_be_truncated: false,
        },
      ],
      managed_turns: [
        {
          turn: {
            turn_id: "123e4567-e89b-12d3-a456-426614174000",
            agent_index: 0,
            status: "returned",
            liveness: "unknown",
            task_state: "unknown",
            updated_at_ms: 1_780_000_001_000,
          },
          recent_events: [
            {
              sequence: 1,
              kind: "turn_started",
              occurred_at_ms: 1_780_000_000_000,
              details: {
                effective_controls: {
                  configured_worker_pool_slots: 3,
                  idle_timeout_secs: 1_500,
                  max_turn_duration_secs: 7_200,
                },
                agent_profile_v1: {
                  schema_version: 1,
                  source: "managed_agent_runtime_config",
                  harness_id: "goose",
                  provider_id: "anthropic",
                  model_id: "claude-opus",
                  agent_prompt_sha256: "d".repeat(64),
                  prompt_content_stored: false,
                },
                resource_policy_v1: {
                  schema_version: 1,
                  scope: "run",
                  applies_to: "current_attempt",
                  metrics: [
                    {
                      id: "run.token_usage",
                      label: "Token usage",
                      state: "unknown",
                      source: "unavailable",
                    },
                    {
                      id: "device.memory",
                      label: "RAM and VRAM usage",
                      state: "unknown",
                      source: "unavailable",
                    },
                    {
                      id: "model.throughput",
                      label: "Model throughput",
                      state: "unknown",
                      source: "unavailable",
                    },
                  ],
                },
              },
            },
            {
              sequence: 3,
              kind: "steer_outcome",
              occurred_at_ms: 1_780_000_001_000,
              details: {
                source_event_id: sourceId,
                outcome: "adapter_acknowledged",
              },
            },
          ],
          event_history_may_be_truncated: false,
        },
      ],
      coordinator_runs: [
        {
          run_id: "323e4567-e89b-12d3-a456-426614174000",
          channel_id: "123e4567-e89b-12d3-a456-426614174001",
          session_scope: "thread",
          thread_root_event_id: "a".repeat(64),
          original_intent_event_id: sourceId,
          project_coordinate: `30621:${"c".repeat(64)}:buzz`,
          project_link_conflict: true,
          attempt_turns: [
            {
              turn_id: "123e4567-e89b-12d3-a456-426614174000",
              agent_index: 0,
              status: "returned",
              liveness: "unknown",
              task_state: "unknown",
              updated_at_ms: 1_780_000_001_000,
            },
          ],
          attempt_history_may_be_truncated: false,
          recent_events: [
            {
              sequence: 1,
              run_id: "323e4567-e89b-12d3-a456-426614174000",
              event_key: "created",
              kind: "run_created",
              occurred_at_ms: 1_780_000_000_000,
              details: { original_intent_event_id: sourceId },
            },
          ],
          event_history_may_be_truncated: false,
          created_at_ms: 1_780_000_000_000,
          updated_at_ms: 1_780_000_001_000,
          task_state: "unknown",
        },
      ],
      managed_turn_lookup: {
        source: "local_acp_attempt_journal",
        scope: "caller-readable exact thread",
        has_more_turns: false,
        capture_reliability: "best_effort",
        capture_gap_count: 2,
        capture_gap_scope: "current_relay_owner_journal",
        task_state: "unknown",
        liveness: "unknown",
      },
      coordinator_run_lookup: {
        source: "local_coordinator_run_journal",
        scope: "caller-readable exact thread",
        has_more_runs: false,
        capture_reliability: "best_effort",
        capture_gap_count: 2,
        capture_gap_scope: "current_relay_owner_journal",
        task_state: "unknown",
      },
    },
    source_event_ids: ["a".repeat(64)],
  };
  const selectedTurn = initialBrief.status.managed_turns[0];
  const routeEvent = (sequence, details) => ({
    sequence,
    kind: "route_decision_v1",
    occurred_at_ms: 1_780_000_001_000,
    details,
  });
  selectedTurn.recent_events.push(
    routeEvent(4, {
      outcome: "selected",
      candidateId: "local-fast",
      providerId: "openai",
      modelId: "qwen-local",
      profileId: "local-first",
      profileVersion: 3,
      profileHash: "f".repeat(64),
      contextFit: {
        estimateMethod: "utf8_bytes_plus_framing_and_output_reserve_v1",
        capacitySource: "operator_declared",
        inputTokensUpperBound: 1536,
        capacityTokens: 4096,
      },
    }),
  );
  for (const [index, outcome, reasonCode] of [
    [1, "abstained", "no_eligible_candidate"],
    [2, "refused", "provider_client_initialization_failed"],
    [3, "overridden", "explicit_session_model_override"],
    [4, "abstained", "toString"],
    [5, "abstained", "context_capacity_insufficient"],
  ]) {
    initialBrief.status.managed_turns.push({
      turn: {
        ...selectedTurn.turn,
        turn_id: `223e4567-e89b-12d3-a456-42661417400${index}`,
        agent_index: index,
      },
      recent_events: [
        routeEvent(index + 1, {
          outcome,
          reasonCode,
          profileId: "local-first",
          profileVersion: 3,
          profileHash: "e".repeat(64),
          ...(outcome === "overridden"
            ? {
                candidateId: "local-fast",
                providerId: "openai",
                modelId: "qwen-local",
              }
            : {}),
        }),
      ],
      event_history_may_be_truncated: false,
    });
  }
  initialBrief.status.managed_turns.push({
    turn: {
      ...selectedTurn.turn,
      turn_id: "223e4567-e89b-12d3-a456-426614174007",
      agent_index: 7,
    },
    recent_events: [
      routeEvent(7, {
        outcome: "overridden",
        reasonCode: "explicit_session_model_override",
        profileId: "local-first",
        profileVersion: 3,
        profileHash: "e".repeat(64),
      }),
    ],
    event_history_may_be_truncated: false,
  });
  for (const [index, reasonCode] of [
    [8, "manual_override_model_not_listed"],
    [9, "manual_override_model_ambiguous"],
  ]) {
    initialBrief.status.managed_turns.push({
      turn: {
        ...selectedTurn.turn,
        turn_id: `423e4567-e89b-12d3-a456-42661417400${index}`,
        agent_index: index,
      },
      recent_events: [
        routeEvent(index, {
          outcome: "refused",
          reasonCode,
          profileId: "local-first",
          profileVersion: 3,
          profileHash: "e".repeat(64),
        }),
      ],
      event_history_may_be_truncated: false,
    });
  }
  for (const [index, details] of [
    [5, null],
    [6, { outcome: "future_unknown_outcome" }],
  ]) {
    initialBrief.status.managed_turns.push({
      turn: {
        ...selectedTurn.turn,
        turn_id: `323e4567-e89b-12d3-a456-42661417400${index}`,
        agent_index: index,
      },
      recent_events: [routeEvent(index + 1, details)],
      event_history_may_be_truncated: false,
    });
  }
  let calls = 0;
  handlers.set("get_thread_brief", () => {
    calls += 1;
    if (calls === 1) return initialBrief;
    return {
      ...initialBrief,
      status: {
        ...initialBrief.status,
        next_cursor: null,
        managed_turns: [],
        coordinator_runs: [],
      },
    };
  });

  render(
    React.createElement(ThreadBriefDisclosure, {
      channelId: "123e4567-e89b-12d3-a456-426614174001",
      rootEventId: "a".repeat(64),
    }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Brief" }));

  await waitFor(() =>
    assert.ok(screen.getByText(/Implement the feature and report status/)),
  );
  assert.match(
    screen.getByTestId("thread-brief-observed-at").textContent,
    /^Evidence observed at /,
  );
  assert.ok(screen.getByText(/Agent slot 1 · last recorded: returned/));
  assert.ok(screen.getByText(/adapter acknowledged/));
  assert.ok(screen.getByText(/model observation unknown/));
  assert.ok(screen.getByText(/does not prove the model saw the steer/));
  assert.ok(screen.getAllByText(/worker liveness unknown/).length >= 1);
  assert.ok(
    screen.getByText(/ACP pool 3 slots · idle limit 1500s · turn limit 7200s/),
  );
  assert.ok(
    screen.getByText(/Unknown at dispatch: Token usage, RAM and VRAM usage/),
  );
  assert.ok(
    screen.getByText(
      /Profile at dispatch: goose · anthropic · claude-opus · prompt fingerprint d{12}…/,
    ),
  );
  const routeSummaries = screen
    .getAllByTestId("thread-brief-route-decision")
    .map((element) => element.textContent);
  assert.ok(
    routeSummaries.includes(
      "Route: Selected before request · openai · qwen-local · conservative UTF-8 upper-bound estimate 1536 tokens; operator-declared capacity 4096 tokens · profile local-first v3",
    ),
  );
  assert.ok(
    routeSummaries.includes(
      "Route: Abstained · No candidate met the route requirements · profile local-first v3",
    ),
  );
  assert.ok(
    routeSummaries.includes(
      "Route: Refused · Provider connection could not initialize · profile local-first v3",
    ),
  );
  assert.ok(
    routeSummaries.includes(
      "Route: Pinned before request · reported candidate local-fast · openai · qwen-local · Manual model choice passed profile gates; provider request outcome unknown · profile local-first v3",
    ),
  );
  assert.ok(
    routeSummaries.includes(
      "Route: Manual model override recorded · profile gate result unknown; provider request outcome unknown · profile local-first v3",
    ),
  );
  assert.ok(
    routeSummaries.includes(
      "Route: Refused · Manual model is not listed in the active route profile · profile local-first v3",
    ),
  );
  assert.ok(
    routeSummaries.includes(
      "Route: Refused · Manual model matches multiple active route candidates · profile local-first v3",
    ),
  );
  assert.ok(
    routeSummaries.includes(
      "Route: Abstained · Reason unavailable · profile local-first v3",
    ),
  );
  assert.ok(
    routeSummaries.includes(
      "Route: Abstained · No candidate fits the conservative UTF-8 upper-bound estimate · profile local-first v3",
    ),
  );
  assert.equal(
    routeSummaries.filter(
      (summary) => summary === "Route: Route decision unavailable.",
    ).length,
    2,
  );
  assert.ok(
    !routeSummaries.some((summary) => summary.includes("f".repeat(64))),
  );
  assert.ok(screen.getByText(/Prompt text is not stored/));
  assert.ok(screen.getByText(/run 323e4567-e89b-12d3-a456-426614174000/));
  assert.ok(screen.getByText(/project 30621:/));
  assert.ok(screen.getByText(/Later attempts resolved a different project/));
  assert.ok(
    screen.getByText(/IDs bind a source request to recorded ACP attempts/),
  );
  assert.ok(
    screen.getByText(/2 local journal write failure\(s\) were recorded/),
  );
  assert.equal(screen.queryByText(/No ACP attempts are recorded/), null);

  await act(async () => {
    fireEvent.click(screen.getByTestId("thread-brief-load-more"));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  assert.equal(screen.queryByTestId("thread-brief-load-more"), null);
  assert.ok(screen.getByText(/run 323e4567-e89b-12d3-a456-426614174000/));
  assert.ok(screen.getByText(/Agent slot 1 · last recorded: returned/));
});
