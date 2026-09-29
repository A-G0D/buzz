import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import { afterEach, test } from "node:test";

import React, { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";

globalThis.__BUZZ_TEST_REACT__ = React;

// Keep the production panel and review dialog mounted while replacing only the
// TipTap editor with a small submit adapter and omitting unrelated message rows.
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (context.parentURL?.endsWith("/MessageThreadPanel.tsx")) {
      if (specifier === "./MessageComposer") {
        return { shortCircuit: true, url: "buzz-thread-review-stub:composer" };
      }
      if (specifier === "./MessageThreadRow") {
        return { shortCircuit: true, url: "buzz-thread-review-stub:row" };
      }
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    if (url === "buzz-thread-review-stub:composer") {
      return {
        format: "module",
        shortCircuit: true,
        source: `const React = globalThis.__BUZZ_TEST_REACT__;
export const MessageComposer = ({ disabled, onConsumeSubmit, onSend, channelId }) => {
  const [draft, setDraft] = React.useState("");
  const submit = (event) => {
    event.preventDefault();
    const content = draft.trim();
    if (onConsumeSubmit?.(content)) {
      setDraft("");
      return;
    }
    void onSend(content, [], undefined, channelId);
    setDraft("");
  };
  return React.createElement("form", {
    "data-testid": "thread-composer-stub",
    onSubmit: submit,
  },
    React.createElement("input", {
      "aria-label": "Thread reply draft",
      disabled,
      onChange: (event) => setDraft(event.target.value),
      value: draft,
    }),
    React.createElement("button", { disabled, type: "submit" }, "Send thread reply"),
  );
};
`,
      };
    }
    if (url === "buzz-thread-review-stub:row") {
      return {
        format: "module",
        shortCircuit: true,
        source: "export const MessageThreadRow = () => null;\n",
      };
    }
    return nextLoad(url, context);
  },
});

const handlers = new Map();
const selectedProfile = {
  id: "local-critic",
  version: 2,
  hash: "c".repeat(64),
};
const guidePreview = {
  path: "AGENT_GUIDES/CRITICS.md",
  sha256: "b".repeat(64),
  byteLength: 36,
  text: "Review as the human coordinator.\n",
};
const routePreview = {
  profile: selectedProfile,
  candidates: [
    {
      id: "loopback",
      provider: "openai",
      model: "local-model",
      configured: true,
      costPricingAvailable: true,
      promptProfile: {
        id: "local-coding",
        version: 1,
        promptHash: "d".repeat(64),
      },
    },
  ],
  estimatedCostLimitMicrousd: 500_000,
};
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
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};
globalThis.window.ResizeObserver ??= globalThis.ResizeObserver;
globalThis.requestAnimationFrame ??= (callback) => setTimeout(callback, 0);
globalThis.cancelAnimationFrame ??= (id) => clearTimeout(id);
globalThis.window.requestAnimationFrame ??= globalThis.requestAnimationFrame;
globalThis.window.cancelAnimationFrame ??= globalThis.cancelAnimationFrame;
globalThis.HTMLElement.prototype.scrollTo ??= function scrollTo(options) {
  if (typeof options === "number") {
    this.scrollLeft = options;
    return;
  }
  if (options.top !== undefined) this.scrollTop = options.top;
  if (options.left !== undefined) this.scrollLeft = options.left;
};

const { MessageThreadPanel, THREAD_BRIEF_LOAD_TIMEOUT_MS } = await import(
  "./MessageThreadPanel.tsx"
);
const { ThemeProvider } = await import("@/shared/theme/ThemeProvider");

afterEach(() => {
  cleanup();
  handlers.clear();
  routePreview.candidates[0].costPricingAvailable = true;
  routePreview.estimatedCostLimitMicrousd = 500_000;
});

function installRouteHandlers() {
  handlers.set("preview_critic_coordinator_guide", () => guidePreview);
  handlers.set("list_agent_route_profiles", () => [
    {
      id: selectedProfile.id,
      name: "Local critic",
      version: selectedProfile.version,
      dataPolicy: "local-only",
      candidateCount: 1,
      documentHash: selectedProfile.hash,
      updatedAt: "2026-09-26T00:00:00Z",
    },
  ]);
  handlers.set("preview_critic_route_profile", ({ profileId }) => {
    assert.equal(profileId, selectedProfile.id);
    return routePreview;
  });
}

function message(id, author, body, createdAt) {
  return {
    id,
    author,
    body,
    createdAt,
    depth: 0,
    time: "now",
    tags: [],
  };
}

function panelProps({
  channelId = "channel-id",
  threadHead,
  threadReplies = [],
  isHuddleTranscript = false,
  onSend = async () => {},
}) {
  return {
    activityAccessoryVisible: false,
    channel: null,
    channelId,
    channelName: "general",
    isFocusMode: false,
    isHuddleTranscript,
    isSending: false,
    onCancelReply() {},
    onClose() {},
    onExpandReplies() {},
    onScrollTargetResolved() {},
    onSelectReplyTarget() {},
    onSend,
    replyTargetMessage: null,
    scrollTargetId: null,
    threadHead,
    threadReplies,
    threadTypingPubkeys: [],
    widthPx: 480,
  };
}

function threadBrief(rootEventId, originalIntent = "Original request") {
  return {
    thread_root_id: rootEventId,
    original_intent: { id: rootEventId, content: originalIntent },
    summary: {
      text: "1 reply observed; task completion is unknown.",
      method: "deterministic",
      task_completion: "unknown",
    },
    progress_events: [
      { id: "c".repeat(64), content: "Source-backed progress update" },
    ],
    auxiliary_events: [],
    source_event_ids: [rootEventId, "c".repeat(64)],
    status: {
      task_state: "unknown",
      reply_event_count: 1,
      latest_activity: null,
      next_cursor: null,
      requested_depth_limit: 64,
      applied_depth_limit: 64,
      depth_limit_may_truncate: true,
      possibly_truncated: true,
      managed_turns: [],
      steering_controls: [
        {
          turn_id: "turn-1",
          agent_index: 0,
          source_event_id: "d".repeat(64),
          state: "adapter_acknowledged",
          submission_recorded: true,
          submitted_at_ms: 1,
          adapter_outcome: "adapter_acknowledged",
          outcome_at_ms: 2,
          agent_observed: "unknown",
          event_history_may_be_truncated: false,
        },
      ],
      coordinator_runs: [],
      managed_turn_lookup: {
        source: "local_acp_attempt_journal",
        scope: "caller-readable exact thread",
        has_more_turns: false,
        capture_reliability: "best_effort",
        capture_gap_count: 0,
        capture_gap_scope: "current_relay_owner_journal",
        task_state: "unknown",
        liveness: "unknown",
      },
      coordinator_run_lookup: {
        source: "local_coordinator_run_journal",
        scope: "caller-readable exact thread",
        has_more_runs: false,
        capture_reliability: "best_effort",
        capture_gap_count: 0,
        capture_gap_scope: "current_relay_owner_journal",
        task_state: "unknown",
      },
    },
  };
}

function panelElement(props, queryClient) {
  return React.createElement(
    ThemeProvider,
    null,
    React.createElement(
      QueryClientProvider,
      { client: queryClient },
      React.createElement(MessageThreadPanel, props),
    ),
  );
}

test("thread review action opens a frozen snapshot without dispatching", async () => {
  let criticCalls = 0;
  let request;
  let briefArgs;
  installRouteHandlers();
  handlers.set("get_thread_brief", (args) => {
    briefArgs = args;
    return threadBrief(args.rootEventId);
  });
  handlers.set("run_critic_round", (args) => {
    criticCalls += 1;
    request = args;
    return completedRunResult();
  });

  const initialHead = message("a".repeat(64), "Ari", "Original request", 10);
  const initialReply = message(
    "b".repeat(64),
    "Sam",
    "Current progress update",
    11,
  );
  const initialReplies = [{ message: initialReply, summary: null }];
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  const view = render(
    panelElement(
      panelProps({ threadHead: initialHead, threadReplies: initialReplies }),
      queryClient,
    ),
  );

  fireEvent.click(screen.getByRole("button", { name: "Review this thread" }));

  const snapshot = await screen.findByLabelText("Frozen review text");
  await waitFor(() => assert.match(snapshot.value, /Current progress update/));
  assert.deepEqual(briefArgs, {
    rootEventId: initialHead.id,
    channelId: "channel-id",
    limit: 30,
    depthLimit: null,
    cursor: null,
  });
  assert.match(snapshot.value, /Original request/);
  assert.match(snapshot.value, /Source-backed progress update/);
  assert.match(snapshot.value, /Steering source event ID: d{64}/);
  assert.match(
    snapshot.value,
    /Task completion: unknown\. Worker liveness: unknown\./,
  );
  assert.match(snapshot.value, new RegExp(initialHead.id));
  assert.match(snapshot.value, new RegExp(initialReply.id));
  assert.match(snapshot.value, new RegExp("c".repeat(64)));
  assert.ok(screen.getByRole("button", { name: "Run review" }).disabled);
  assert.equal(criticCalls, 0);

  fireEvent.change(snapshot, {
    target: { value: `${snapshot.value}\nUser-added review note` },
  });
  const editedSnapshot = snapshot.value;

  view.rerender(
    panelElement(
      panelProps({
        threadHead: message("c".repeat(64), "Ari", "Later request edit", 12),
        threadReplies: [
          {
            message: message("d".repeat(64), "Sam", "Later progress", 13),
            summary: null,
          },
        ],
      }),
      queryClient,
    ),
  );

  assert.equal(snapshot.value, editedSnapshot);
  assert.match(snapshot.value, /Original request/);
  assert.match(snapshot.value, /Current progress update/);
  assert.doesNotMatch(snapshot.value, /Later request edit|Later progress/);

  await screen.findAllByRole("option", { name: /Local critic/ });
  fireEvent.change(screen.getByLabelText("Local route for Correctness"), {
    target: { value: selectedProfile.id },
  });
  fireEvent.change(screen.getByLabelText("Local route for Security"), {
    target: { value: selectedProfile.id },
  });
  fireEvent.change(
    screen.getByLabelText("Requested round estimate ceiling in USD"),
    { target: { value: "0.12" } },
  );
  const runButton = screen.getByRole("button", { name: "Run review" });
  await waitFor(() => assert.equal(runButton.disabled, false));
  assert.equal(criticCalls, 0);

  fireEvent.click(runButton);

  await screen.findByText("Confirmed review dispatch from the thread panel.");
  assert.equal(criticCalls, 1);
  assert.equal(request.params.routeProfiles.correctness.id, selectedProfile.id);
  assert.equal(request.params.routeProfiles.security.id, selectedProfile.id);
  assert.match(request.params.snapshot, /Original request/);
  assert.match(request.params.snapshot, /Current progress update/);
  assert.match(request.params.snapshot, /Source-backed progress update/);
  assert.match(request.params.snapshot, /User-added review note/);
  assert.doesNotMatch(
    request.params.snapshot,
    /Later request edit|Later progress/,
  );
});

test("whole-draft critic phrase opens review and is consumed before send", async () => {
  const sent = [];
  let briefArgs;
  let criticCalls = 0;
  installRouteHandlers();
  handlers.set("get_thread_brief", (args) => {
    briefArgs = args;
    return threadBrief(args.rootEventId);
  });
  handlers.set("run_critic_round", () => {
    criticCalls += 1;
    return completedRunResult();
  });

  const head = message("9".repeat(64), "Ari", "Review this work", 10);
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  render(
    panelElement(
      panelProps({
        threadHead: head,
        onSend: async (...args) => sent.push(args),
      }),
      queryClient,
    ),
  );

  const draft = screen.getByRole("textbox", { name: "Thread reply draft" });
  fireEvent.change(draft, { target: { value: "run critics on this thread" } });
  fireEvent.submit(screen.getByTestId("thread-composer-stub"));

  const snapshot = await screen.findByLabelText("Frozen review text");
  assert.deepEqual(briefArgs, {
    rootEventId: head.id,
    channelId: "channel-id",
    limit: 30,
    depthLimit: null,
    cursor: null,
  });
  assert.match(snapshot.value, /Review this work/);
  assert.ok(screen.getByRole("button", { name: "Run review" }).disabled);
  assert.equal(draft.value, "");
  assert.deepEqual(sent, []);
  assert.equal(criticCalls, 0);
});

test("ambiguous and huddle drafts continue through ordinary send", async () => {
  const ambiguousSends = [];
  const huddleSends = [];
  let briefCalls = 0;
  handlers.set("get_thread_brief", () => {
    briefCalls += 1;
    return threadBrief("a".repeat(64));
  });
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  const view = render(
    panelElement(
      panelProps({
        threadHead: message("a".repeat(64), "Ari", "Thread", 10),
        onSend: async (...args) => ambiguousSends.push(args),
      }),
      queryClient,
    ),
  );

  const ambiguousDraft = screen.getByRole("textbox", {
    name: "Thread reply draft",
  });
  fireEvent.change(ambiguousDraft, {
    target: { value: "before the build, run critics on this" },
  });
  fireEvent.submit(screen.getByTestId("thread-composer-stub"));
  await waitFor(() => assert.equal(ambiguousSends.length, 1));
  assert.equal(ambiguousSends[0][0], "before the build, run critics on this");
  assert.equal(briefCalls, 0);
  assert.equal(screen.queryByLabelText("Frozen review text"), null);

  view.rerender(
    panelElement(
      panelProps({
        isHuddleTranscript: true,
        threadHead: message("b".repeat(64), "Sam", "Huddle thread", 11),
        onSend: async (...args) => huddleSends.push(args),
      }),
      queryClient,
    ),
  );
  const huddleDraft = screen.getByRole("textbox", {
    name: "Thread reply draft",
  });
  fireEvent.change(huddleDraft, { target: { value: "/critics" } });
  fireEvent.submit(screen.getByTestId("thread-composer-stub"));
  await waitFor(() => assert.equal(huddleSends.length, 1));
  assert.equal(huddleSends[0][0], "/critics");
  assert.equal(briefCalls, 0);
  assert.equal(screen.queryByLabelText("Frozen review text"), null);
});

test("failed brief lookup is disclosed and falls back to loaded messages only", async () => {
  installRouteHandlers();
  let criticCalls = 0;
  handlers.set("get_thread_brief", () => Promise.reject(new Error("offline")));
  handlers.set("run_critic_round", () => {
    criticCalls += 1;
    return completedRunResult();
  });

  const initialHead = message(
    "e".repeat(64),
    "Ari",
    "Loaded fallback intent",
    10,
  );
  const initialReply = message(
    "f".repeat(64),
    "Sam",
    "Loaded fallback progress",
    11,
  );
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  render(
    panelElement(
      panelProps({
        threadHead: initialHead,
        threadReplies: [{ message: initialReply, summary: null }],
      }),
      queryClient,
    ),
  );

  fireEvent.click(screen.getByRole("button", { name: "Review this thread" }));
  const snapshot = await screen.findByLabelText("Frozen review text");
  assert.match(
    screen.getByTestId("critic-thread-disclosure").textContent,
    /only messages already loaded in the panel are included/i,
  );
  assert.match(snapshot.value, /Loaded fallback intent/);
  assert.match(snapshot.value, /Loaded fallback progress/);
  assert.match(
    snapshot.value,
    /Could not load a matching source-linked thread brief/,
  );
  assert.doesNotMatch(snapshot.value, /Source-backed progress update/);
  assert.equal(criticCalls, 0);
});

test("thread change while brief is pending discards the stale review request", async () => {
  const deferred = () => {
    let resolve;
    const promise = new Promise((done) => {
      resolve = done;
    });
    return { promise, resolve };
  };
  const staleBrief = deferred();
  const currentBrief = deferred();
  const firstHead = message("1".repeat(64), "Ari", "First thread", 10);
  const nextHead = message("2".repeat(64), "Sam", "Selected thread", 11);
  const briefRequests = [];
  let criticCalls = 0;
  installRouteHandlers();
  handlers.set("get_thread_brief", (args) => {
    briefRequests.push(args);
    return args.rootEventId === firstHead.id
      ? staleBrief.promise
      : currentBrief.promise;
  });
  handlers.set("run_critic_round", () => {
    criticCalls += 1;
    return completedRunResult();
  });

  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  const view = render(
    panelElement(
      panelProps({ channelId: "channel-first", threadHead: firstHead }),
      queryClient,
    ),
  );

  fireEvent.click(screen.getByRole("button", { name: "Review this thread" }));
  assert.equal(briefRequests.length, 1);

  view.rerender(
    panelElement(
      panelProps({ channelId: "channel-next", threadHead: nextHead }),
      queryClient,
    ),
  );
  fireEvent.click(
    await screen.findByRole("button", { name: "Review this thread" }),
  );
  assert.equal(briefRequests.length, 2);
  assert.deepEqual(
    briefRequests.map(({ rootEventId, channelId }) => ({
      rootEventId,
      channelId,
    })),
    [
      { rootEventId: firstHead.id, channelId: "channel-first" },
      { rootEventId: nextHead.id, channelId: "channel-next" },
    ],
  );

  await act(async () => {
    staleBrief.resolve(threadBrief(firstHead.id, "Stale first-thread brief"));
    await staleBrief.promise;
  });
  assert.equal(
    screen.queryByLabelText("Frozen review text"),
    null,
    "the late first-thread brief must not open a review dialog",
  );

  await act(async () => {
    currentBrief.resolve(
      threadBrief(nextHead.id, "Current selected-thread brief"),
    );
    await currentBrief.promise;
  });
  const snapshot = await screen.findByLabelText("Frozen review text");
  assert.match(snapshot.value, /Current selected-thread brief/);
  assert.match(snapshot.value, new RegExp(nextHead.id));
  assert.doesNotMatch(snapshot.value, /Stale first-thread brief/);
  assert.doesNotMatch(snapshot.value, new RegExp(firstHead.id));
  assert.equal(criticCalls, 0);
});

test("brief lookup timeout opens a disclosed loaded-messages-only review", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  installRouteHandlers();
  handlers.set("get_thread_brief", () => new Promise(() => {}));
  let criticCalls = 0;
  handlers.set("run_critic_round", () => {
    criticCalls += 1;
    return completedRunResult();
  });

  const head = message("7".repeat(64), "Ari", "Loaded-only intent", 10);
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  render(
    panelElement(
      panelProps({
        threadHead: head,
        threadReplies: [
          {
            message: message("8".repeat(64), "Sam", "Loaded-only progress", 11),
            summary: null,
          },
        ],
      }),
      queryClient,
    ),
  );

  fireEvent.click(screen.getByRole("button", { name: "Review this thread" }));
  assert.ok(
    screen.getByRole("button", { name: "Loading thread review context" })
      .disabled,
  );

  await act(async () => {
    t.mock.timers.tick(THREAD_BRIEF_LOAD_TIMEOUT_MS);
    await Promise.resolve();
    await Promise.resolve();
  });

  const snapshot = screen.getByLabelText("Frozen review text");
  assert.match(snapshot.value, /Loaded-only intent/);
  assert.match(snapshot.value, /Loaded-only progress/);
  assert.doesNotMatch(snapshot.value, /Source-backed progress update/);
  assert.match(
    screen.getByTestId("critic-thread-disclosure").textContent,
    /only messages already loaded in the panel are included/i,
  );
  assert.ok(screen.getByRole("button", { name: "Run review" }).disabled);
  assert.equal(criticCalls, 0);
  assert.ok(
    screen.queryByRole("button", { name: "Loading thread review context" }) ===
      null,
  );
});

function completedRunResult() {
  return {
    roundId: "123e4567-e89b-42d3-a456-426614174005",
    ledgerStatus: "saved",
    ledgerErrorCode: null,
    snapshotSha256: "a".repeat(64),
    routeProfiles: {
      correctness: selectedProfile,
      security: selectedProfile,
    },
    execution: "loopback-only Buzz Agent review mode",
    dataBoundary:
      "The snapshot is sent to Buzz Agent's configured loopback endpoint.",
    independence: "Each role has an independently selected route.",
    limits: {
      maximumReviewers: 3,
      outputTokensPerReviewer: 1024,
      timeLimitSecondsPerReviewer: 60,
      thinkingEffortRequested: null,
      thinkingEffortNote: "Provider default.",
      estimatedRoundCostBudgetMicrousd: 120_000,
    },
    reviewers: [
      {
        role: "correctness",
        status: "completed",
        output: "Confirmed review dispatch from the thread panel.",
        outputTruncated: false,
        stopReason: "end_turn",
        candidateId: "local-review",
        providerId: "openai",
        modelId: "local-model",
        routeProfile: selectedProfile,
        estimatedCostLimitMicrousd: 60_000,
        elapsedMs: 100,
        errorCode: null,
      },
    ],
  };
}

test("huddle transcript mode does not expose thread review", () => {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  render(
    panelElement(
      panelProps({
        threadHead: message("e".repeat(64), "Ari", "Huddle message", 10),
        isHuddleTranscript: true,
      }),
      queryClient,
    ),
  );

  assert.equal(
    screen.queryByRole("button", { name: "Review this thread" }),
    null,
  );
});
