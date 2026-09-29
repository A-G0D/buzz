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

const { AgentCriticHistoryDialog } = await import(
  "./AgentCriticHistoryDialog.tsx"
);

afterEach(() => {
  cleanup();
  handlers.clear();
});

function mount() {
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
        React.createElement(AgentCriticHistoryDialog, {
          onOpenChange() {},
          open: true,
        }),
      ),
    ),
  );
}

test("critic history loads findings only after selecting a round", async () => {
  const roundId = "123e4567-e89b-12d3-a456-426614174005";
  let detailCalls = 0;
  handlers.set("get_recent_critic_rounds", () => [
    {
      roundId,
      createdAtMs: 1_790_000_000_000,
      snapshotSha256: "a".repeat(64),
      objectiveSha256: "b".repeat(64),
      scopeSha256: "c".repeat(64),
      settings: {
        maxOutputTokens: 512,
        timeLimitSeconds: 45,
        thinkingEffortRequested: "medium",
        estimatedRoundCostBudgetMicrousd: 120_000,
        coordinatorGuide: {
          path: "AGENT_GUIDES/CRITICS.md",
          sha256: "f".repeat(64),
          text: "PRIVATE_GUIDE_TEXT_SENTINEL",
        },
      },
      reviewers: [
        {
          role: "security",
          status: "completed",
          outputTruncated: false,
          candidateId: "local-review",
          providerId: "openai",
          modelId: "local-model",
          routeProfile: {
            id: "local-security",
            version: 4,
            hash: "e".repeat(64),
          },
          elapsedMs: 1_500,
          estimatedCostLimitMicrousd: 60_000,
          errorCode: null,
        },
      ],
    },
  ]);
  handlers.set("get_critic_round", ({ roundId: requestedId }) => {
    detailCalls += 1;
    assert.equal(requestedId, roundId);
    return {
      roundId,
      createdAtMs: 1_790_000_000_000,
      snapshotSha256: "a".repeat(64),
      objectiveSha256: "b".repeat(64),
      scopeSha256: "c".repeat(64),
      settings: {
        maxOutputTokens: 512,
        timeLimitSeconds: 45,
        thinkingEffortRequested: "medium",
        estimatedRoundCostBudgetMicrousd: 120_000,
        coordinatorGuide: {
          path: "AGENT_GUIDES/CRITICS.md",
          sha256: "f".repeat(64),
          text: "PRIVATE_GUIDE_TEXT_SENTINEL",
        },
      },
      reviewers: [
        {
          role: "security",
          status: "completed",
          output: "Review found one concrete issue.",
          outputTruncated: false,
          stopReason: "end_turn",
          candidateId: "local-review",
          providerId: "openai",
          modelId: "local-model",
          routeProfile: {
            id: "local-security",
            version: 4,
            hash: "e".repeat(64),
          },
          elapsedMs: 1_500,
          estimatedCostLimitMicrousd: 60_000,
          errorCode: null,
        },
      ],
    };
  });

  mount();

  await screen.findByText("Recent rounds");
  await waitFor(() => assert.equal(detailCalls, 0));
  const scrollRegion = screen.getByTestId("critic-history-scroll-region");
  assert.match(scrollRegion.className, /min-h-0/);
  assert.match(scrollRegion.className, /overflow-y-auto/);
  fireEvent.click(screen.getByRole("button", { name: /security.*completed/i }));
  await screen.findByText("Review found one concrete issue.");
  assert.equal(detailCalls, 1);
  const roundList = screen.getByTestId("critic-history-round-list");
  assert.match(roundList.className, /max-h-none/);
  assert.match(roundList.className, /overflow-visible/);
  assert.match(roundList.className, /md:max-h-\[55vh\]/);
  assert.match(roundList.className, /md:overflow-y-auto/);
  const reviewerOutput = screen
    .getByText("Review found one concrete issue.")
    .closest("pre");
  assert.ok(reviewerOutput);
  assert.match(reviewerOutput.className, /max-h-none/);
  assert.match(reviewerOutput.className, /overflow-visible/);
  assert.match(reviewerOutput.className, /md:max-h-64/);
  assert.match(reviewerOutput.className, /md:overflow-auto/);
  assert.ok(screen.getByText(/Route local-security v4/));
  assert.ok(screen.getByText("Requested round estimate ceiling"));
  assert.ok(screen.getByText("$0.12"));
  assert.ok(
    screen.getByText(
      /Estimated per-reviewer ceiling: \$0\.06\. Estimate only; provider usage or billing may differ\./,
    ),
  );
  assert.equal(screen.queryByRole("script"), null);
  assert.equal(screen.queryByText("PRIVATE_GUIDE_TEXT_SENTINEL"), null);
  assert.ok(screen.getByText(/Source fingerprints/));
  assert.ok(
    screen.getByText(`AGENT_GUIDES/CRITICS.md · SHA-256 ${"f".repeat(64)}`),
  );
  assert.ok(
    screen.getByText(
      "This stored reference does not indicate that an AI coordinator used the guide.",
    ),
  );
});

test("older critic history reports guide provenance as not recorded", async () => {
  const roundId = "123e4567-e89b-12d3-a456-426614174006";
  const round = {
    roundId,
    createdAtMs: 1_790_000_000_000,
    snapshotSha256: "a".repeat(64),
    objectiveSha256: "b".repeat(64),
    scopeSha256: "c".repeat(64),
    settings: {
      maxOutputTokens: 512,
      timeLimitSeconds: 45,
      thinkingEffortRequested: null,
    },
    reviewers: [
      {
        role: "security",
        status: "completed",
        outputTruncated: false,
        providerId: null,
        modelId: null,
        elapsedMs: null,
      },
    ],
  };
  handlers.set("get_recent_critic_rounds", () => [round]);
  handlers.set("get_critic_round", () => round);

  mount();

  await screen.findByText("Recent rounds");
  fireEvent.click(
    await screen.findByRole("button", { name: /security.*1 completed/i }),
  );
  await screen.findByText("Not recorded");
  assert.ok(
    screen.getByText(
      "This stored reference does not indicate that an AI coordinator used the guide.",
    ),
  );
});

test("critic history shows an empty state without a failed result panel", async () => {
  handlers.set("get_recent_critic_rounds", () => []);

  mount();

  await screen.findByText("No critic rounds have been saved yet.");
  assert.ok(screen.getByText("Select a saved round to inspect its findings."));
});
