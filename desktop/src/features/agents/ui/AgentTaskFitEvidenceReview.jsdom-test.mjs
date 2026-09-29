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
globalThis.__TAURI_INTERNALS__ = {
  invoke(command, args) {
    const handler = handlers.get(command);
    if (handler) return handler(args);
    return Promise.reject(new Error(`Unexpected Tauri command: ${command}`));
  },
  transformCallback() {
    return 1;
  },
};
globalThis.window.__TAURI_INTERNALS__ = globalThis.__TAURI_INTERNALS__;
globalThis.window.matchMedia ??= () => ({
  matches: false,
  addEventListener() {},
  removeEventListener() {},
});

const { AgentTaskFitEvidenceReview } = await import(
  "./AgentTaskFitEvidenceReview.tsx"
);

afterEach(() => {
  cleanup();
  handlers.clear();
});

function mount() {
  if (!handlers.has("list_agent_task_fit_reports")) {
    handlers.set("list_agent_task_fit_reports", () => []);
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
        React.createElement(AgentTaskFitEvidenceReview),
      ),
    ),
  );
}

function reportSummary(overrides = {}) {
  return {
    reportSha256: "a".repeat(64),
    taskClass: "small-code-edit",
    taskClassTaxonomyVersion: "operator-defined-v1",
    evaluationPolicyVersion: "task-fit-outcomes-v1",
    dataset: "local-fixture",
    jobId: "job-fixture",
    providerId: "openai",
    modelId: "mock-coder",
    endpointId: "mock-local",
    conditionId: "condition-fixture",
    manifestSha256: "b".repeat(64),
    generation: { temperature: 0 },
    endpointConfigSha256: "c".repeat(64),
    promptSha256: "d".repeat(64),
    runtimeBinarySha256: { "mock-runtime": "e".repeat(64) },
    caseSetSha256: "f".repeat(64),
    taskCount: 2,
    taskSuccessCount: 2,
    taskSuccessRate: 1,
    wilsonLowerBound95: 0.3424,
    createdAt: "2026-09-26T00:00:00Z",
    finishedAt: "2026-09-26T00:01:00Z",
    modelIdentityObserved: false,
    localAttestation: null,
    routeAttestations: [],
    ...overrides,
  };
}

test("reviews and imports a report while keeping it out of route selection", async () => {
  const summary = reportSummary();
  const fileBytes = Array.from(new TextEncoder().encode("{}"));
  let storedReports = [];
  let importedArgs;
  handlers.set("preview_agent_task_fit_report", (args) => {
    assert.deepEqual(args.fileBytes, fileBytes);
    return summary;
  });
  handlers.set("import_agent_task_fit_report", (args) => {
    importedArgs = args;
    storedReports = [summary];
    return summary;
  });
  handlers.set("list_agent_task_fit_reports", () => storedReports);
  mount();

  await waitFor(() =>
    assert.ok(screen.getByText("No task-fit reports imported.")),
  );
  assert.match(
    screen.getByText(/Reports are checked for internal consistency/)
      .textContent,
    /does not verify the benchmark or enable routing/,
  );
  fireEvent.change(screen.getByTestId("task-fit-report-input"), {
    target: {
      files: [new window.File([new Uint8Array(fileBytes)], "report.json")],
    },
  });
  await waitFor(() => assert.ok(screen.getByText("Review before importing")));
  fireEvent.click(
    screen.getByRole("button", { name: "Import reviewed report" }),
  );
  await waitFor(() => assert.ok(screen.getByText("Imported local reports")));
  assert.deepEqual(importedArgs, {
    fileBytes,
    expectedReportSha256: summary.reportSha256,
  });
  assert.ok(screen.getByText("Unverified report"));
});

test("attests an imported report with the current local Buzz identity", async () => {
  const summary = reportSummary();
  const attested = {
    publicKey: "1".repeat(64),
    eventId: "2".repeat(64),
    createdAt: 1790380860,
  };
  let storedReports = [summary];
  let attestedHash;
  handlers.set("list_agent_task_fit_reports", () => storedReports);
  handlers.set("attest_agent_task_fit_report", ({ reportSha256 }) => {
    attestedHash = reportSha256;
    storedReports = [{ ...summary, localAttestation: attested }];
    return attested;
  });
  mount();

  await waitFor(() => assert.ok(screen.getByText("Unverified report")));
  fireEvent.click(screen.getByRole("button", { name: "Attest local review" }));
  await waitFor(() => assert.ok(screen.getByText("Locally attested")));
  assert.equal(attestedHash, summary.reportSha256);
  assert.match(
    screen.getByText(/Harbor and its source artifacts are not authenticated/)
      .textContent,
    /does not verify the benchmark or enable routing/,
  );
});

test("binds a report only to a matching saved profile and candidate", async () => {
  const summary = reportSummary();
  const profile = {
    id: "coding-route",
    name: "Coding route",
    version: 2,
    dataPolicy: "allow-hosted",
    candidateCount: 1,
    documentHash: "9".repeat(64),
    updatedAt: "2026-09-26T00:00:00Z",
    schemaVersion: 1,
    document: {
      version: 1,
      data_policy: "allow-hosted",
      preference_order: ["openai"],
      task_fit_policy: {
        taskClass: summary.taskClass,
        taskClassTaxonomyVersion: summary.taskClassTaxonomyVersion,
        evaluationPolicyVersion: summary.evaluationPolicyVersion,
        minimumDistinctTasks: 1,
        minimumWilsonLowerBound95: 0,
        maximumAgeSeconds: 86400,
        requireObservedModelIdentity: true,
      },
      candidates: [
        {
          id: "openai",
          provider: summary.providerId,
          model: summary.modelId,
          data_location: "hosted",
          prompt_addendum: "",
        },
      ],
    },
  };
  let storedReports = [summary];
  let bindingArgs;
  handlers.set("list_agent_task_fit_reports", () => storedReports);
  handlers.set("list_agent_route_profiles", () => [profile]);
  handlers.set("read_agent_route_profile", () => profile);
  handlers.set("attest_agent_task_fit_report_for_route", (args) => {
    bindingArgs = args;
    const attestation = {
      publicKey: "1".repeat(64),
      eventId: "2".repeat(64),
      createdAt: 1790380860,
      profileId: profile.id,
      profileVersion: profile.version,
      profileHash: profile.documentHash,
      candidateId: "openai",
    };
    storedReports = [{ ...summary, routeAttestations: [attestation] }];
    return attestation;
  });
  mount();

  await waitFor(() => assert.ok(screen.getByText("Unverified report")));
  await waitFor(() =>
    assert.ok(screen.getByRole("option", { name: /Coding route/ })),
  );
  fireEvent.change(screen.getByLabelText("Task-fit route profile"), {
    target: { value: profile.id },
  });
  await waitFor(() =>
    assert.equal(
      screen.getByLabelText("Task-fit route candidate").disabled,
      false,
    ),
  );
  fireEvent.change(screen.getByLabelText("Task-fit route candidate"), {
    target: { value: "openai" },
  });
  await waitFor(() => assert.ok(screen.getByText(/gate will remain closed/)));
  await waitFor(() =>
    assert.equal(
      screen.getByRole("button", { name: "Bind report to route" }).disabled,
      false,
    ),
  );
  fireEvent.click(screen.getByRole("button", { name: "Bind report to route" }));
  await waitFor(() => assert.ok(screen.getByText(/Bound to Coding route/)));
  assert.deepEqual(bindingArgs, {
    reportSha256: summary.reportSha256,
    profileId: profile.id,
    candidateId: "openai",
  });
});

test("explains why route binding is disabled when no saved profile exists", async () => {
  handlers.set("list_agent_task_fit_reports", () => [reportSummary()]);
  handlers.set("list_agent_route_profiles", () => []);
  mount();

  const bindButton = await screen.findByRole("button", {
    name: "Bind report to route",
  });
  assert.equal(bindButton.disabled, true);
  await screen.findByText("Create a route profile before binding this report.");
});

test("rejects an oversized report before sending bytes to Tauri", async () => {
  let previewCalled = false;
  handlers.set("preview_agent_task_fit_report", () => {
    previewCalled = true;
    return reportSummary();
  });
  mount();
  fireEvent.change(screen.getByTestId("task-fit-report-input"), {
    target: {
      files: [new window.File([new Uint8Array(1024 * 1024 + 1)], "large.json")],
    },
  });
  await waitFor(() =>
    assert.ok(screen.getByText("Task-fit reports must be 1 MiB or smaller.")),
  );
  assert.equal(previewCalled, false);
});
