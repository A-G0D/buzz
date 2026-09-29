import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import React from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ThemeProvider } from "@/shared/theme/ThemeProvider";

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
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};
globalThis.window.ResizeObserver ??= globalThis.ResizeObserver;

const { AgentCriticRunDialog } = await import("./AgentCriticRunDialog.tsx");
const { buildThreadCriticSnapshot } = await import(
  "@/features/messages/lib/threadCriticSnapshot.ts"
);

const selectedProfile = {
  id: "local-critic",
  version: 2,
  hash: "c".repeat(64),
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
const guidePreview = {
  path: "AGENT_GUIDES/CRITICS.md",
  sha256: "b".repeat(64),
  byteLength: 36,
  text: "Review as the human coordinator.\n",
};

function installRouteHandlers() {
  handlers.set("list_agent_route_profiles", () => [
    {
      id: "local-critic",
      name: "Local critic",
      version: 2,
      dataPolicy: "local-only",
      candidateCount: 1,
      documentHash: "c".repeat(64),
      updatedAt: "2026-09-26T00:00:00Z",
    },
    {
      id: "hosted-route",
      name: "Hosted route",
      version: 1,
      dataPolicy: "allow-hosted",
      candidateCount: 1,
      documentHash: "e".repeat(64),
      updatedAt: "2026-09-26T00:00:00Z",
    },
  ]);
  handlers.set("preview_critic_route_profile", ({ profileId }) => {
    assert.equal(profileId, selectedProfile.id);
    return routePreview;
  });
}

afterEach(() => {
  cleanup();
  handlers.clear();
  routePreview.candidates[0].costPricingAvailable = true;
  routePreview.estimatedCostLimitMicrousd = 500_000;
});

function mount({ threadContext = null } = {}) {
  if (!handlers.has("preview_critic_coordinator_guide")) {
    handlers.set("preview_critic_coordinator_guide", () => guidePreview);
  }
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  render(
    React.createElement(
      ThemeProvider,
      null,
      React.createElement(
        QueryClientProvider,
        { client: queryClient },
        React.createElement(AgentCriticRunDialog, {
          onOpenChange() {},
          open: true,
          threadContext,
        }),
      ),
    ),
  );
}

test("shows a frozen thread prefill and disclosure without dispatching", async () => {
  let callCount = 0;
  handlers.set("run_critic_round", () => {
    callCount += 1;
    return completedRunResult();
  });
  const sourceId = "f".repeat(64);
  const threadContext = buildThreadCriticSnapshot({
    head: {
      id: sourceId,
      author: "Ari",
      body: "Loaded thread question",
      createdAt: 1,
      depth: 0,
      time: "now",
    },
    replies: [],
    repliesPending: true,
  });

  mount({ threadContext });

  assert.ok(await screen.findByText("Coordinator guide for you"));
  assert.ok(screen.getByText(/shown to you as the human coordinator/));
  assert.ok(screen.getByText(/does not send it to the reviewer workers/));
  assert.ok(await screen.findByText(guidePreview.path));
  assert.ok(await screen.findByText(`SHA-256: ${guidePreview.sha256}`));
  fireEvent.click(screen.getByText("Preview guide text"));
  assert.ok(await screen.findByText("Review as the human coordinator."));

  await waitFor(() =>
    assert.match(
      screen.getByLabelText("Frozen review text").value,
      new RegExp(sourceId),
    ),
  );
  assert.match(
    screen.getByLabelText("Frozen review text").value,
    /Loaded thread question/,
  );
  assert.ok(screen.getByTestId("critic-thread-disclosure"));
  assert.ok(screen.getByText(/Replies are still loading/));
  assert.ok(
    screen.getByText(/captured snapshot includes 1 stable source event ID/i),
  );
  assert.match(screen.getByLabelText("Original goal").value, /original intent/);
  assert.match(
    screen.getByLabelText("Review focus").value,
    /unresolved blockers/,
  );
  assert.equal(callCount, 0);
  assert.equal(
    screen.getByRole("button", { name: "Run review" }).disabled,
    true,
  );
});

test("blocks review when the canonical coordinator guide cannot be previewed", async () => {
  let callCount = 0;
  handlers.set("preview_critic_coordinator_guide", () =>
    Promise.reject(new Error("The coordinator guide is missing.")),
  );
  handlers.set("run_critic_round", () => {
    callCount += 1;
    return completedRunResult();
  });
  installRouteHandlers();
  mount();

  assert.ok(await screen.findByText("The coordinator guide is missing."));
  assert.equal(
    screen.getByRole("button", { name: "Run review" }).disabled,
    true,
  );
  assert.equal(callCount, 0);
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
      outputTokensPerReviewer: 512,
      timeLimitSecondsPerReviewer: 30,
      thinkingEffortRequested: "high",
      thinkingEffortNote: "Requested setting; provider support may differ.",
      estimatedRoundCostBudgetMicrousd: 120_000,
    },
    reviewers: [
      {
        role: "correctness",
        status: "completed",
        output: "Confirmed defect in the frozen patch.",
        outputTruncated: false,
        stopReason: "end_turn",
        candidateId: "local-review",
        providerId: "openai",
        modelId: "local-model",
        routeProfile: selectedProfile,
        estimatedCostLimitMicrousd: 60_000,
        elapsedMs: 1500,
        errorCode: null,
      },
    ],
  };
}

test("shows a saved route ceiling when it lowers the requested reviewer share", async () => {
  routePreview.estimatedCostLimitMicrousd = 30_000;
  installRouteHandlers();
  mount();

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

  await screen.findAllByText(/effective estimate ceiling \$0\.03/);
  assert.equal(
    screen.getAllByText(
      /\$0\.06 requested share; effective estimate ceiling \$0\.03/,
    ).length,
    2,
  );
});

test("runs only after explicit submit and presents the bounded local result", async () => {
  let request;
  let callCount = 0;
  handlers.set("run_critic_round", (args) => {
    request = args;
    callCount += 1;
    return completedRunResult();
  });
  handlers.set("cancel_critic_round", () => true);
  installRouteHandlers();

  mount();

  assert.equal(callCount, 0);
  assert.equal(
    screen.getByRole("button", { name: "Run review" }).disabled,
    true,
  );
  const hostedOptions = await screen.findAllByRole("option", {
    name: /Hosted route/,
  });
  assert.equal(hostedOptions.length, 2);
  assert.ok(hostedOptions.every((option) => option.disabled));
  assert.ok(
    screen.getByText(/The local inference service may forward data elsewhere/),
  );
  fireEvent.change(screen.getByLabelText("Original goal"), {
    target: { value: "Protect the editor from data loss" },
  });
  fireEvent.change(screen.getByLabelText("Review focus"), {
    target: { value: "Check saved-draft and error behavior" },
  });
  fireEvent.change(screen.getByLabelText("Frozen review text"), {
    target: { value: "diff --git a/editor.ts b/editor.ts\n+saveDraft()" },
  });
  fireEvent.change(screen.getByLabelText("Output cap per critic"), {
    target: { value: "512" },
  });
  fireEvent.change(screen.getByLabelText("Time cap per critic"), {
    target: { value: "30" },
  });
  fireEvent.change(
    screen.getByLabelText("Requested round estimate ceiling in USD"),
    { target: { value: "0.1234567" } },
  );
  fireEvent.change(screen.getByLabelText("Requested reasoning effort"), {
    target: { value: "high" },
  });
  await screen.findAllByRole("option", { name: /Local critic/ });
  fireEvent.change(screen.getByLabelText("Local route for Correctness"), {
    target: { value: selectedProfile.id },
  });
  fireEvent.change(screen.getByLabelText("Local route for Security"), {
    target: { value: selectedProfile.id },
  });
  assert.ok(screen.getByText(/no more than six decimal places/));
  assert.equal(
    screen.getByRole("button", { name: "Run review" }).disabled,
    true,
  );
  fireEvent.change(
    screen.getByLabelText("Requested round estimate ceiling in USD"),
    { target: { value: "0.12" } },
  );
  await waitFor(() =>
    assert.equal(
      screen.getByRole("button", { name: "Run review" }).disabled,
      false,
    ),
  );

  fireEvent.click(screen.getByRole("button", { name: "Run review" }));

  await screen.findByText("Confirmed defect in the frozen patch.");
  assert.equal(callCount, 1);
  assert.match(request.requestId, /^[0-9a-f-]{36}$/);
  assert.deepEqual(request.params, {
    objective: "Protect the editor from data loss",
    scope: "Check saved-draft and error behavior",
    snapshot: "diff --git a/editor.ts b/editor.ts\n+saveDraft()",
    roles: ["correctness", "security"],
    maxOutputTokens: 512,
    timeLimitSeconds: 30,
    thinkingEffort: "high",
    routeProfiles: {
      correctness: selectedProfile,
      security: selectedProfile,
    },
    estimatedRoundCostBudgetMicrousd: 120_000,
    coordinatorGuideSha256: guidePreview.sha256,
  });
  assert.ok(screen.getByText("Saved to local history"));
  assert.equal(
    screen.getAllByText(/Requested round estimate ceiling/).length,
    2,
  );
  assert.equal(
    screen.getAllByText(/requested share; effective estimate ceiling/).length,
    2,
  );
  assert.ok(screen.getByText(/Snapshot SHA-256/));

  const staleMessage =
    "The review setup changed after this run. These results use the previous settings.";
  const changeAndRestore = (control, changedValue, originalValue) => {
    fireEvent.change(control, { target: { value: changedValue } });
    assert.equal(control.value, changedValue);
    assert.ok(screen.getByText(staleMessage));
    fireEvent.change(control, { target: { value: originalValue } });
    assert.equal(screen.queryByText(staleMessage), null);
  };
  changeAndRestore(
    screen.getByLabelText("Original goal"),
    "Different goal",
    "Protect the editor from data loss",
  );
  changeAndRestore(
    screen.getByLabelText("Review focus"),
    "Different focus",
    "Check saved-draft and error behavior",
  );
  changeAndRestore(
    screen.getByLabelText("Frozen review text"),
    "different frozen text",
    "diff --git a/editor.ts b/editor.ts\n+saveDraft()",
  );
  changeAndRestore(
    screen.getByLabelText("Requested round estimate ceiling in USD"),
    "0.13",
    "0.12",
  );
  changeAndRestore(
    screen.getByLabelText("Output cap per critic"),
    "1024",
    "512",
  );
  changeAndRestore(screen.getByLabelText("Time cap per critic"), "60", "30");
  changeAndRestore(
    screen.getByLabelText("Requested reasoning effort"),
    "medium",
    "high",
  );
  const securityRoute = screen.getByLabelText("Local route for Security");
  fireEvent.change(securityRoute, { target: { value: "" } });
  assert.equal(
    screen.queryByText("Confirmed defect in the frozen patch."),
    null,
  );
  fireEvent.change(securityRoute, { target: { value: selectedProfile.id } });
  await waitFor(() =>
    assert.equal(
      screen.getByRole("button", { name: "Run review" }).disabled,
      false,
    ),
  );
  fireEvent.click(screen.getByRole("button", { name: "Run review" }));
  await screen.findByText("Confirmed defect in the frozen patch.");

  const performanceRole = screen.getByRole("checkbox", {
    name: "Performance",
  });
  fireEvent.click(performanceRole);
  assert.equal(
    screen.queryByText("Confirmed defect in the frozen patch."),
    null,
  );
});

test("offers cancellation while the native review is active", async () => {
  let rejectRun;
  let canceledRequestId;
  handlers.set(
    "run_critic_round",
    () =>
      new Promise((_resolve, reject) => {
        rejectRun = reject;
      }),
  );
  handlers.set("cancel_critic_round", ({ requestId }) => {
    canceledRequestId = requestId;
    rejectRun(new Error("Critic round was canceled."));
    return true;
  });
  installRouteHandlers();

  mount();
  fireEvent.change(screen.getByLabelText("Original goal"), {
    target: { value: "Check the patch" },
  });
  fireEvent.change(screen.getByLabelText("Review focus"), {
    target: { value: "Look for concrete defects" },
  });
  fireEvent.change(screen.getByLabelText("Frozen review text"), {
    target: { value: "small frozen diff" },
  });
  await screen.findAllByRole("option", { name: /Local critic/ });
  fireEvent.change(screen.getByLabelText("Local route for Correctness"), {
    target: { value: selectedProfile.id },
  });
  fireEvent.change(screen.getByLabelText("Local route for Security"), {
    target: { value: selectedProfile.id },
  });
  await waitFor(() =>
    assert.equal(
      screen.getByRole("button", { name: "Run review" }).disabled,
      false,
    ),
  );
  fireEvent.click(screen.getByRole("button", { name: "Run review" }));
  fireEvent.click(await screen.findByRole("button", { name: "Cancel review" }));

  await waitFor(() => assert.ok(canceledRequestId));
  await screen.findByText("Critic round was canceled.");
  assert.match(canceledRequestId, /^[0-9a-f-]{36}$/);
});

test("marks a result stale when setup changes during an active review", async () => {
  let resolveRun;
  handlers.set(
    "run_critic_round",
    () =>
      new Promise((resolve) => {
        resolveRun = resolve;
      }),
  );
  installRouteHandlers();

  mount();
  fireEvent.change(screen.getByLabelText("Original goal"), {
    target: { value: "Check the patch" },
  });
  fireEvent.change(screen.getByLabelText("Review focus"), {
    target: { value: "Look for concrete defects" },
  });
  fireEvent.change(screen.getByLabelText("Frozen review text"), {
    target: { value: "small frozen diff" },
  });
  await screen.findAllByRole("option", { name: /Local critic/ });
  fireEvent.change(screen.getByLabelText("Local route for Correctness"), {
    target: { value: selectedProfile.id },
  });
  fireEvent.change(screen.getByLabelText("Local route for Security"), {
    target: { value: selectedProfile.id },
  });
  await waitFor(() =>
    assert.equal(
      screen.getByRole("button", { name: "Run review" }).disabled,
      false,
    ),
  );
  fireEvent.click(screen.getByRole("button", { name: "Run review" }));
  await screen.findByRole("button", { name: "Cancel review" });

  fireEvent.change(screen.getByLabelText("Original goal"), {
    target: { value: "Updated while reviewing" },
  });
  resolveRun(completedRunResult());

  await screen.findByText("Confirmed defect in the frozen patch.");
  assert.ok(
    screen.getByText(
      "The review setup changed after this run. These results use the previous settings.",
    ),
  );
});

test("requires configured input and output prices for a round ceiling", async () => {
  routePreview.candidates[0].costPricingAvailable = false;
  installRouteHandlers();
  mount();
  fireEvent.change(screen.getByLabelText("Original goal"), {
    target: { value: "Check the patch" },
  });
  fireEvent.change(screen.getByLabelText("Review focus"), {
    target: { value: "Look for concrete defects" },
  });
  fireEvent.change(screen.getByLabelText("Frozen review text"), {
    target: { value: "small frozen diff" },
  });
  fireEvent.change(
    screen.getByLabelText("Requested round estimate ceiling in USD"),
    { target: { value: "1.00" } },
  );
  await screen.findAllByRole("option", { name: /Local critic/ });
  fireEvent.change(screen.getByLabelText("Local route for Correctness"), {
    target: { value: selectedProfile.id },
  });
  fireEvent.change(screen.getByLabelText("Local route for Security"), {
    target: { value: selectedProfile.id },
  });

  assert.ok(
    await screen.findByText(
      /Add input and output prices to a configured Local candidate/,
    ),
  );
  assert.equal(
    screen.getByRole("button", { name: "Run review" }).disabled,
    true,
  );
});
