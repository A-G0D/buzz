import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import React from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { ProjectAgentRunActivity } from "./ProjectAgentRunActivity.tsx";

const calls = [];
let pageHandler;
const tauriMock = {
  invoke(command, args) {
    calls.push({ command, args });
    if (pageHandler) return pageHandler(command, args);
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

afterEach(() => {
  cleanup();
  calls.length = 0;
  pageHandler = undefined;
});

const projectCoordinate = `30621:${"a".repeat(64)}:demo`;
const homeChannelId = "123e4567-e89b-12d3-a456-426614174001";
const firstRun = {
  run_id: "123e4567-e89b-12d3-a456-426614174000",
  channel_id: homeChannelId,
  session_scope: "thread",
  thread_root_event_id: "b".repeat(64),
  original_intent_event_id: "c".repeat(64),
  project_coordinate: projectCoordinate,
  project_link_conflict: false,
  attempt_turns: [
    {
      turn_id: "223e4567-e89b-12d3-a456-426614174000",
      channel_id: homeChannelId,
      session_scope: "thread",
      thread_root_event_id: "b".repeat(64),
      batch_trigger_event_ids: ["c".repeat(64)],
      merged_cancelled_event_ids: [],
      agent_index: 0,
      status: "returned",
      liveness: "unknown",
      task_state: "unknown",
      runtime_session_match: "unknown",
      control_target_available: false,
      started_at_ms: 90,
      updated_at_ms: 100,
    },
  ],
  attempt_history_may_be_truncated: false,
  recent_events: [],
  event_history_may_be_truncated: false,
  created_at_ms: 90,
  updated_at_ms: 100,
  task_state: "unknown",
  history_reliability: "best_effort",
  history_completeness: "unknown",
};

const defaultScope = {
  homeChannelId,
  identityPubkey: "d".repeat(64),
  projectCoordinate,
  relayUrl: "wss://relay.example.test",
};

function mount(onOpenThread = () => {}, scope = {}) {
  return render(
    React.createElement(ProjectAgentRunActivity, {
      ...defaultScope,
      ...scope,
      onOpenThread,
    }),
  );
}

test("project agent activity loads only when opened and marks completion unverified", async () => {
  const rootId = firstRun.thread_root_event_id;
  const intentId = firstRun.original_intent_event_id;
  pageHandler = async (command) => {
    if (command === "get_thread_brief") {
      return {
        thread_root_id: rootId,
        original_intent: {
          id: rootId,
          kind: 9,
          pubkey: "d".repeat(64),
          content: "Implement the deployment fix and report results.",
          created_at: 1_780_000_000,
          tags: [],
        },
        summary: {
          text: "One reply event observed. Task completion is unknown.",
          method: "deterministic",
          task_completion: "unknown",
        },
        progress_events: [
          {
            id: intentId,
            kind: 9,
            pubkey: "e".repeat(64),
            content: "The change is ready for review.",
            created_at: 1_780_000_001,
            tags: [],
          },
        ],
        auxiliary_events: [],
        status: {
          task_state: "unknown",
          reply_event_count: 1,
          latest_activity: null,
          next_cursor: null,
          requested_depth_limit: null,
          applied_depth_limit: 64,
          depth_limit_may_truncate: false,
          possibly_truncated: false,
          managed_turns: [],
          coordinator_runs: [],
          managed_turn_lookup: {
            source: "local_acp_attempt_journal",
            scope: "caller-readable exact thread",
            has_more_turns: false,
            task_state: "unknown",
            liveness: "unknown",
          },
          coordinator_run_lookup: {
            source: "local_coordinator_run_journal",
            scope: "caller-readable exact thread",
            has_more_runs: false,
            task_state: "unknown",
          },
        },
        source_event_ids: [rootId, intentId],
      };
    }
    return {
      project_coordinate: projectCoordinate,
      runs: [firstRun],
      attempt_evidence_may_be_truncated: false,
      has_more_candidates: false,
      next_cursor: null,
    };
  };
  const openedThreads = [];
  mount((channelId, rootEventId) =>
    openedThreads.push({ channelId, rootEventId }),
  );

  assert.equal(calls.length, 0);
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));
  assert.ok(await screen.findByText("Run 123e4567"));
  assert.ok(screen.getByText("Completion unverified"));
  assert.ok(screen.getByText("1 readable attempt"));
  assert.ok(
    screen.getByText(
      "Attempt history is best effort; completeness is unknown.",
    ),
  );
  assert.ok(
    screen.getByText("Worker liveness and task completion are unknown."),
  );
  assert.equal(calls[0].command, "get_project_coordinator_runs");
  assert.equal(calls[0].args.projectCoordinate, projectCoordinate);
  assert.equal(calls[0].args.homeChannelId, homeChannelId);

  fireEvent.click(screen.getByRole("button", { name: "Brief" }));
  assert.ok(
    await screen.findByText("Implement the deployment fix and report results."),
  );
  assert.ok(await screen.findByText("The change is ready for review."));
  assert.equal(calls.at(-1).command, "get_thread_brief");

  fireEvent.click(screen.getByRole("button", { name: "Open conversation" }));
  assert.deepEqual(openedThreads, [
    { channelId: homeChannelId, rootEventId: rootId },
  ]);
});

test("project agent activity discloses known attempt or event truncation", async () => {
  pageHandler = async () => ({
    project_coordinate: projectCoordinate,
    runs: [
      {
        ...firstRun,
        attempt_history_may_be_truncated: true,
        event_history_may_be_truncated: false,
      },
    ],
    attempt_evidence_may_be_truncated: false,
    has_more_candidates: false,
    next_cursor: null,
  });
  mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));

  assert.ok(await screen.findByText("Run 123e4567"));
  assert.ok(
    screen.getByText(
      "Some retained attempt or event history is known to be truncated.",
    ),
  );
});

test("project agent activity discloses evidence omitted by page safety limits", async () => {
  pageHandler = async () => ({
    project_coordinate: projectCoordinate,
    runs: [firstRun],
    attempt_evidence_may_be_truncated: true,
    has_more_candidates: false,
    next_cursor: null,
  });
  mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));

  assert.ok(await screen.findByText("Run 123e4567"));
  assert.ok(
    screen.getByText(
      "Some attempt evidence was omitted by page safety limits.",
    ),
  );
});

test("empty candidate batch explains that more runs are available", async () => {
  pageHandler = async () => ({
    project_coordinate: projectCoordinate,
    runs: [],
    attempt_evidence_may_be_truncated: false,
    has_more_candidates: true,
    next_cursor: { updated_at_ms: 80, run_id: "0".repeat(64) },
  });
  mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));

  assert.ok(
    await screen.findByText(
      "No readable runs in this batch. Another batch is available.",
    ),
  );
  assert.equal(
    screen.getByRole("button", { name: "Load more agent activity" }).disabled,
    false,
  );
});

test("project agent activity follows the returned candidate cursor", async () => {
  const secondRun = {
    ...firstRun,
    run_id: "323e4567-e89b-12d3-a456-426614174000",
  };
  pageHandler = async (_command, { cursor }) =>
    cursor
      ? {
          project_coordinate: projectCoordinate,
          runs: [secondRun],
          attempt_evidence_may_be_truncated: false,
          has_more_candidates: false,
          next_cursor: null,
        }
      : {
          project_coordinate: projectCoordinate,
          runs: [firstRun],
          attempt_evidence_may_be_truncated: false,
          has_more_candidates: true,
          next_cursor: { updated_at_ms: 100, run_id: firstRun.run_id },
        };
  mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));
  await screen.findByText("Run 123e4567");
  fireEvent.click(
    screen.getByRole("button", { name: "Load more agent activity" }),
  );
  await screen.findByText("Run 323e4567");
  assert.ok(screen.getByText("Run 123e4567"));
  await waitFor(() => assert.equal(calls.length, 2));
  assert.deepEqual(calls[1].args.cursor, {
    updated_at_ms: 100,
    run_id: firstRun.run_id,
  });
});

test("scope changes reset activity and ignore late pages from the old scope", async () => {
  const scopeChanges = [
    ["projectCoordinate", `30621:${"b".repeat(64)}:other`],
    ["homeChannelId", "123e4567-e89b-12d3-a456-426614174099"],
    ["relayUrl", "wss://other-relay.example.test"],
    ["identityPubkey", "e".repeat(64)],
  ];

  for (const [changedField, changedValue] of scopeChanges) {
    cleanup();
    calls.length = 0;
    let resolveOldPage;
    let pageReads = 0;
    const freshRun = {
      ...firstRun,
      run_id: "423e4567-e89b-12d3-a456-426614174000",
    };
    const staleRun = {
      ...firstRun,
      run_id: "523e4567-e89b-12d3-a456-426614174000",
    };
    pageHandler = async () => {
      pageReads += 1;
      if (pageReads === 1) {
        return {
          project_coordinate: projectCoordinate,
          runs: [firstRun],
          attempt_evidence_may_be_truncated: false,
          has_more_candidates: true,
          next_cursor: { updated_at_ms: 100, run_id: firstRun.run_id },
        };
      }
      if (pageReads === 2) {
        return new Promise((resolve) => {
          resolveOldPage = resolve;
        });
      }
      return {
        project_coordinate: projectCoordinate,
        runs: [freshRun],
        attempt_evidence_may_be_truncated: false,
        has_more_candidates: false,
        next_cursor: null,
      };
    };

    const view = mount();
    fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));
    await screen.findByText("Run 123e4567");
    fireEvent.click(
      screen.getByRole("button", { name: "Load more agent activity" }),
    );
    await waitFor(() => assert.equal(pageReads, 2));

    const newScope = { ...defaultScope, [changedField]: changedValue };
    view.rerender(
      React.createElement(ProjectAgentRunActivity, {
        ...newScope,
        onOpenThread: () => {},
      }),
    );
    assert.equal(screen.queryByText("Run 123e4567"), null);
    await screen.findByText("Run 423e4567");
    assert.equal(calls[2].args.cursor, null);

    await act(async () => {
      resolveOldPage({
        project_coordinate: projectCoordinate,
        runs: [staleRun],
        attempt_evidence_may_be_truncated: false,
        has_more_candidates: false,
        next_cursor: null,
      });
    });
    assert.equal(screen.queryByText("Run 523e4567"), null);
    assert.ok(screen.getByText("Run 423e4567"));
  }
});

test("scope changes clear errors and reload from the first candidate", async () => {
  let pageReads = 0;
  pageHandler = async () => {
    pageReads += 1;
    if (pageReads === 1) throw new Error("old scope read failed");
    return {
      project_coordinate: projectCoordinate,
      runs: [],
      attempt_evidence_may_be_truncated: false,
      has_more_candidates: false,
      next_cursor: null,
    };
  };
  const view = mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));
  assert.ok(await screen.findByRole("alert"));

  view.rerender(
    React.createElement(ProjectAgentRunActivity, {
      ...defaultScope,
      identityPubkey: "e".repeat(64),
      onOpenThread: () => {},
    }),
  );
  await waitFor(() => assert.equal(pageReads, 2));
  await waitFor(() => assert.equal(screen.queryByRole("alert"), null));
  assert.equal(calls[1].args.cursor, null);
});

test("a successful first-page refresh replaces cached pages with current results", async () => {
  const secondRun = {
    ...firstRun,
    run_id: "323e4567-e89b-12d3-a456-426614174000",
  };
  let firstPageReads = 0;
  pageHandler = async (_command, { cursor }) => {
    if (cursor) {
      return {
        project_coordinate: projectCoordinate,
        runs: [secondRun],
        attempt_evidence_may_be_truncated: false,
        has_more_candidates: false,
        next_cursor: null,
      };
    }
    firstPageReads += 1;
    return {
      project_coordinate: projectCoordinate,
      runs: firstPageReads === 1 ? [firstRun] : [],
      attempt_evidence_may_be_truncated: false,
      has_more_candidates: firstPageReads === 1,
      next_cursor:
        firstPageReads === 1
          ? { updated_at_ms: 100, run_id: firstRun.run_id }
          : null,
    };
  };
  mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));
  await screen.findByText("Run 123e4567");
  fireEvent.click(
    screen.getByRole("button", { name: "Load more agent activity" }),
  );
  await screen.findByText("Run 323e4567");

  fireEvent.click(
    screen.getByRole("button", { name: "Refresh agent activity" }),
  );
  await screen.findByText("No readable agent runs were found.");

  assert.equal(screen.queryByText("Run 323e4567"), null);
  assert.equal(screen.queryByText("Run 123e4567"), null);
  assert.equal(firstPageReads, 2);
  assert.equal(calls.at(-1).args.cursor, null);
  assert.match(
    screen.getByTestId("project-agent-activity-first-page-checked").textContent,
    /^First page last checked at /,
  );
});

test("a failed refresh preserves cached rows and the prior check time", async () => {
  let reads = 0;
  pageHandler = async () => {
    reads += 1;
    if (reads > 1) throw new Error("Refresh failed");
    return {
      project_coordinate: projectCoordinate,
      runs: [firstRun],
      attempt_evidence_may_be_truncated: false,
      has_more_candidates: false,
      next_cursor: null,
    };
  };
  mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));
  await screen.findByText("Run 123e4567");
  const checkedAt = screen.getByTestId(
    "project-agent-activity-first-page-checked",
  ).textContent;

  fireEvent.click(
    screen.getByRole("button", { name: "Refresh agent activity" }),
  );
  await screen.findByRole("alert");

  assert.equal(
    screen.getByTestId("project-agent-activity-first-page-checked").textContent,
    checkedAt,
  );
  assert.ok(screen.getByText("Run 123e4567"));
});

test("refreshes project agent activity without losing the loaded run", async () => {
  let reads = 0;
  pageHandler = async () => {
    reads += 1;
    return {
      project_coordinate: projectCoordinate,
      runs: [
        reads === 1
          ? firstRun
          : {
              ...firstRun,
              attempt_turns: [
                ...firstRun.attempt_turns,
                {
                  ...firstRun.attempt_turns[0],
                  turn_id: "323e4567-e89b-12d3-a456-426614174000",
                  status: "prompt_started",
                  updated_at_ms: 200,
                },
              ],
              updated_at_ms: 200,
            },
      ],
      attempt_evidence_may_be_truncated: false,
      has_more_candidates: false,
      next_cursor: null,
    };
  };
  mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));
  await screen.findByText("Run 123e4567");
  assert.ok(screen.getByText("1 readable attempt"));

  fireEvent.click(
    screen.getByRole("button", { name: "Refresh agent activity" }),
  );
  await screen.findByText("2 readable attempts");
  assert.equal(reads, 2);
  assert.equal(calls[1].command, "get_project_coordinator_runs");
  assert.equal(calls[1].args.cursor, null);
});

test("guidance posts to the exact run thread under the captured relay and signer", async () => {
  pageHandler = async (command) => {
    if (command === "send_channel_message") {
      return {
        event_id: "f".repeat(64),
        parent_event_id: firstRun.thread_root_event_id,
        root_event_id: firstRun.thread_root_event_id,
        depth: 1,
        created_at: 1_780_000_002,
      };
    }
    if (command === "get_thread_brief") {
      return {
        thread_root_id: firstRun.thread_root_event_id,
        original_intent: null,
        summary: {
          text: "",
          method: "deterministic",
          task_completion: "unknown",
        },
        progress_events: [],
        auxiliary_events: [],
        status: {
          task_state: "unknown",
          reply_event_count: 0,
          coordinator_runs: [],
        },
        source_event_ids: [firstRun.thread_root_event_id],
      };
    }
    return {
      project_coordinate: projectCoordinate,
      runs: [firstRun],
      attempt_evidence_may_be_truncated: false,
      has_more_candidates: false,
      next_cursor: null,
    };
  };
  mount();
  fireEvent.click(screen.getByRole("button", { name: /Agent activity/i }));
  await screen.findByText("Run 123e4567");
  fireEvent.click(screen.getByRole("button", { name: "Guide run" }));
  fireEvent.change(screen.getByRole("textbox", { name: /Guidance for run/ }), {
    target: { value: "Focus on the failing test and report the result." },
  });
  const sendButton = screen.getByRole("button", { name: "Send guidance" });
  fireEvent.click(sendButton);
  fireEvent.click(sendButton);

  await screen.findByText(/Guidance posted to the thread/);
  const send = calls.find((call) => call.command === "send_channel_message");
  assert.equal(
    calls.filter((call) => call.command === "send_channel_message").length,
    1,
  );
  assert.deepEqual(send.args, {
    channelId: homeChannelId,
    content: "Focus on the failing test and report the result.",
    parentEventId: firstRun.thread_root_event_id,
    rootEventId: firstRun.thread_root_event_id,
    mediaTags: null,
    emojiTags: null,
    mentionTags: null,
    linkPreviewTags: undefined,
    sentFromThreadTag: null,
    mentionPubkeys: null,
    kind: null,
    taskClass: null,
    expectedRelayUrl: "wss://relay.example.test",
    expectedSignerPubkey: "d".repeat(64),
  });
});
