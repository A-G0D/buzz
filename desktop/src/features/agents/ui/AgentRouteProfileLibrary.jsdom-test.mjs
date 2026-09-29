import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import React from "react";
import {
  fireEvent,
  cleanup,
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

const { AgentRouteProfileLibrary } = await import(
  "./AgentRouteProfileLibrary.tsx"
);

afterEach(() => {
  cleanup();
  handlers.clear();
});

function mount() {
  handlers.set("list_agent_route_profiles", () => []);
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
        React.createElement(AgentRouteProfileLibrary, { onBack() {} }),
      ),
    ),
  );
}

function mountSavedProfile(measurements, options = {}) {
  const candidate = {
    id: "local",
    provider: "openai",
    model: "gpt-test",
    data_location: "local",
    prompt_addendum: "",
    ...options.candidate,
  };
  const dataPolicy = options.dataPolicy ?? "local-only";
  const profile = {
    id: "local-first",
    name: "Local first",
    version: 3,
    dataPolicy,
    candidateCount: 1,
    documentHash: "a".repeat(64),
    updatedAt: "2026-09-26T00:00:00Z",
    schemaVersion: 1,
    document: {
      version: 1,
      data_policy: dataPolicy,
      preference_order: ["local"],
      strict_context_fit: false,
      candidates: [candidate],
    },
    ...options.profile,
  };
  handlers.set("list_agent_route_profiles", () => [profile]);
  handlers.set("read_agent_route_profile", () => profile);
  let measurementArgs;
  handlers.set("list_agent_route_throughput_summaries", (args) => {
    measurementArgs = args;
    return measurements;
  });
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
        React.createElement(AgentRouteProfileLibrary, { onBack() {} }),
      ),
    ),
  );
  return () => measurementArgs;
}

function candidateTestReceipt(overrides = {}) {
  return {
    profileId: "local-first",
    profileVersion: 3,
    profileDocumentHash: "a".repeat(64),
    resolvedProfileHash: "b".repeat(64),
    candidateId: "local",
    providerId: "openai",
    runtimeProviderId: "openai",
    requestedModelId: "gpt-test",
    dataLocation: "local",
    endpointOrigin: "http://127.0.0.1:1234",
    status: "responded",
    startedAt: "2026-09-27T00:00:00.000Z",
    elapsedMs: 125,
    outputTokenCap: 256,
    timeoutSeconds: 30,
    inputTokens: null,
    outputTokens: null,
    totalTokens: null,
    responseMarkerMatched: true,
    modelIdentityObserved: false,
    identityEvidence: "requested_configuration_only",
    targetPromptProfileIncluded: false,
    fallbackCount: 0,
    failureClass: null,
    ...overrides,
  };
}

function measuredGroup(overrides = {}) {
  return {
    profileHash: "a".repeat(64),
    endpointHash: "b".repeat(64),
    candidateId: "local",
    providerId: "openai",
    modelId: "gpt-test",
    thinkingEffort: "default",
    inputBucket: "small",
    freshSampleCount: 5,
    effectiveOutputTokensPerSecondMilli: 23_450,
    freshestSampleAtMs: Date.now() - 60 * 60 * 1000,
    ...overrides,
  };
}

test("new routing profiles default to local-only and keep candidate order explicit", async () => {
  mount();
  await waitFor(() =>
    assert.ok(screen.getByText("No routing profiles saved.")),
  );
  fireEvent.click(screen.getByRole("button", { name: /new profile/i }));

  await waitFor(() => assert.ok(screen.getByLabelText("Profile name")));
  assert.ok(screen.getByText(/does not retry another provider/));
  assert.ok(screen.getByText(/exact model profile takes precedence/));
  assert.match(
    screen.getByRole("button", { name: "Hosted data policy" }).textContent,
    /Local candidates only/,
  );
  assert.ok(screen.getByLabelText(/Preference order/));
  assert.equal(screen.getByLabelText(/Preference order/).value, "local");
  assert.equal(screen.getByLabelText("Candidate ID").value, "local");
  const strictFit = screen.getByRole("checkbox", {
    name: "Require strict context fit",
  });
  assert.equal(
    screen.getByLabelText(/Per-turn estimated cost ceiling \(USD\)/).value,
    "",
  );
  assert.equal(
    screen.getByLabelText("Input rate (USD per 1M tokens)").value,
    "",
  );
  assert.equal(
    screen.getByLabelText("Output rate (USD per 1M tokens)").value,
    "",
  );
  assert.equal(strictFit.getAttribute("aria-checked"), "false");
  assert.equal(
    screen.getByLabelText("Minimum effective output speed (tokens/s)").value,
    "",
  );
  assert.equal(
    screen
      .getByRole("checkbox", { name: "Prefer the fastest measured candidate" })
      .getAttribute("aria-checked"),
    "false",
  );
  assert.equal(
    screen
      .getByRole("checkbox", {
        name: "Allow temporary preference-order warm-up",
      })
      .getAttribute("aria-checked"),
    "false",
  );
  fireEvent.click(strictFit);
  await waitFor(() =>
    assert.equal(strictFit.getAttribute("aria-checked"), "true"),
  );
  const capacity = screen.getByLabelText(/Context window capacity \(tokens\)/);
  fireEvent.change(capacity, { target: { value: "32768" } });
  assert.equal(capacity.value, "32768");
  assert.ok(screen.getByText(/operator-declared context capacity/));

  fireEvent.click(screen.getByRole("button", { name: /add candidate/i }));
  await waitFor(() => {
    const candidateIds = screen.getAllByLabelText("Candidate ID");
    assert.equal(candidateIds.length, 2);
    assert.equal(candidateIds[1].value, "candidate-2");
  });
  assert.equal(
    screen.getByLabelText(/Preference order/).value,
    "local\ncandidate-2",
  );
});

test("hosted model-tier preset creates a draft without contacting a provider", async () => {
  mount();
  await waitFor(() =>
    assert.ok(screen.getByText("No routing profiles saved.")),
  );

  fireEvent.click(screen.getByText("Start from a model tier"));
  fireEvent.click(
    screen.getByRole("button", { name: "Fast · DeepSeek Flash" }),
  );

  await waitFor(() =>
    assert.equal(
      screen.getByLabelText("Profile name").value,
      "Fast hosted · DeepSeek Flash",
    ),
  );
  assert.equal(screen.getByLabelText("Exact model ID").value, "deepseek-flash");
  assert.equal(screen.getByLabelText("Candidate ID").value, "deepseek-flash");
  assert.equal(
    screen.getByLabelText(/Context window capacity \(tokens\)/).value,
    "1000000",
  );
  assert.equal(
    screen.getByRole("button", { name: "Hosted data policy" }).textContent,
    "Allow hosted candidates",
  );
  assert.ok(screen.getByText(/Hosted prompts leave this device/));
  assert.equal(handlers.has("test_agent_route_candidate"), false);
  assert.equal(handlers.has("save_agent_route_profile"), false);
});

test("local model-tier preset stays local and requires a real server model ID", async () => {
  mount();
  await waitFor(() =>
    assert.ok(screen.getByText("No routing profiles saved.")),
  );

  fireEvent.click(screen.getByText("Start from a model tier"));
  fireEvent.click(
    screen.getByRole("button", { name: "Local · OpenAI-compatible" }),
  );

  await waitFor(() =>
    assert.equal(
      screen.getByLabelText("Profile name").value,
      "Local · OpenAI-compatible",
    ),
  );
  assert.equal(screen.getByLabelText("Exact model ID").value, "");
  assert.equal(
    screen.getByRole("button", { name: "Hosted data policy" }).textContent,
    "Local candidates only",
  );
  assert.ok(screen.getByText(/running loopback-compatible server/));
  assert.equal(handlers.has("test_agent_route_candidate"), false);
  assert.equal(handlers.has("save_agent_route_profile"), false);
});

test("per-turn budgets and declared rates save as integer micro-USD", async () => {
  mount();
  await waitFor(() =>
    assert.ok(screen.getByText("No routing profiles saved.")),
  );
  fireEvent.click(screen.getByRole("button", { name: /new profile/i }));
  await waitFor(() => assert.ok(screen.getByLabelText("Profile name")));

  fireEvent.change(
    screen.getByLabelText(/Per-turn estimated cost ceiling \(USD\)/),
    {
      target: { value: "0.025" },
    },
  );
  fireEvent.change(screen.getByLabelText("Exact model ID"), {
    target: { value: "local-test" },
  });
  fireEvent.change(screen.getByLabelText("Input rate (USD per 1M tokens)"), {
    target: { value: "1.25" },
  });
  fireEvent.change(screen.getByLabelText("Output rate (USD per 1M tokens)"), {
    target: { value: "3.5" },
  });

  let savedInput;
  handlers.set("save_agent_route_profile", ({ input }) => {
    savedInput = input;
    return {
      id: input.id,
      name: input.name,
      version: 1,
      dataPolicy: input.document.data_policy,
      candidateCount: input.document.candidates.length,
      documentHash: "a".repeat(64),
      updatedAt: "2026-09-26T00:00:00Z",
      schemaVersion: 1,
      document: input.document,
    };
  });
  fireEvent.click(screen.getByRole("button", { name: "Save profile" }));

  await waitFor(() => assert.ok(savedInput));
  assert.equal(savedInput.document.max_turn_cost_microusd, 25_000);
  assert.equal(
    savedInput.document.candidates[0].input_cost_microusd_per_million_tokens,
    1_250_000,
  );
  assert.equal(
    savedInput.document.candidates[0].output_cost_microusd_per_million_tokens,
    3_500_000,
  );
});

test("saved routes show matching measured speed and sample freshness", async () => {
  const readArgs = mountSavedProfile([measuredGroup()]);
  const summary = await waitFor(() => {
    const card = screen.getByTestId("route-throughput-local");
    assert.match(card.textContent, /23\.5 effective output tokens\/s/);
    assert.match(card.textContent, /5 fresh/);
    assert.match(card.textContent, /Updated 1h ago/);
    return card;
  });
  assert.match(summary.textContent, /2k–8k input tokens/);
  await waitFor(() =>
    assert.deepEqual(readArgs(), {
      profileId: "local-first",
      profileVersion: 3,
    }),
  );
  assert.equal(
    screen.getByLabelText("Minimum effective output speed (tokens/s)").value,
    "",
  );
  assert.equal(
    screen
      .getByRole("checkbox", { name: "Prefer the fastest measured candidate" })
      .getAttribute("aria-checked"),
    "false",
  );
  assert.equal(
    screen
      .getByRole("checkbox", {
        name: "Allow temporary preference-order warm-up",
      })
      .getAttribute("aria-checked"),
    "false",
  );
});

test("speed routing choices save with their milli-token threshold", async () => {
  mount();
  await waitFor(() =>
    assert.ok(screen.getByText("No routing profiles saved.")),
  );
  fireEvent.click(screen.getByRole("button", { name: /new profile/i }));
  await waitFor(() => assert.ok(screen.getByLabelText("Profile name")));

  fireEvent.change(screen.getByLabelText("Exact model ID"), {
    target: { value: "gpt-test" },
  });
  fireEvent.change(
    screen.getByLabelText("Minimum effective output speed (tokens/s)"),
    { target: { value: "42.125" } },
  );
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: "Prefer the fastest measured candidate",
    }),
  );
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: "Allow temporary preference-order warm-up",
    }),
  );

  let savedDocument;
  handlers.set("save_agent_route_profile", ({ input }) => {
    savedDocument = input.document;
    return {
      id: input.id,
      name: input.name,
      version: 1,
      dataPolicy: input.document.data_policy,
      candidateCount: input.document.candidates.length,
      documentHash: "c".repeat(64),
      updatedAt: "2026-09-26T00:00:00Z",
      schemaVersion: 1,
      document: input.document,
    };
  });
  fireEvent.click(screen.getByRole("button", { name: "Save profile" }));

  await waitFor(() => assert.ok(savedDocument));
  assert.equal(
    savedDocument.min_effective_output_tokens_per_second_milli,
    42_125,
  );
  assert.equal(savedDocument.prefer_fastest_measured, true);
  assert.equal(savedDocument.allow_preference_order_warmup, true);
});

test("task-fit policy saves its supported thresholds and stays opt-in", async () => {
  mount();
  await waitFor(() =>
    assert.ok(screen.getByText("No routing profiles saved.")),
  );
  fireEvent.click(screen.getByRole("button", { name: /new profile/i }));
  await waitFor(() => assert.ok(screen.getByLabelText("Profile name")));
  assert.equal(
    screen
      .getByRole("checkbox", { name: "Require reviewed task-fit evidence" })
      .getAttribute("aria-checked"),
    "false",
  );

  fireEvent.change(screen.getByLabelText("Exact model ID"), {
    target: { value: "mock-coder" },
  });
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: "Require reviewed task-fit evidence",
    }),
  );
  const saveButton = screen.getByRole("button", { name: "Save profile" });
  assert.equal(saveButton.disabled, true);
  assert.match(
    screen.getByRole("status", { name: "Save profile requirements" })
      .textContent,
    /Task class ID must start with a lowercase letter/,
  );
  fireEvent.change(screen.getByLabelText("Task class ID"), {
    target: { value: "small-code-edit" },
  });
  assert.equal(saveButton.disabled, false);
  assert.equal(
    screen.queryByRole("status", { name: "Save profile requirements" }),
    null,
  );
  fireEvent.change(screen.getByLabelText("Minimum distinct tasks"), {
    target: { value: "12" },
  });
  fireEvent.change(screen.getByLabelText("Minimum confidence bound"), {
    target: { value: "0.75" },
  });
  fireEvent.change(screen.getByLabelText("Evidence freshness (days)"), {
    target: { value: "7" },
  });

  let savedDocument;
  handlers.set("save_agent_route_profile", ({ input }) => {
    savedDocument = input.document;
    return {
      id: input.id,
      name: input.name,
      version: 1,
      dataPolicy: input.document.data_policy,
      candidateCount: input.document.candidates.length,
      documentHash: "c".repeat(64),
      updatedAt: "2026-09-26T00:00:00Z",
      schemaVersion: 1,
      document: input.document,
    };
  });
  fireEvent.click(screen.getByRole("button", { name: "Save profile" }));

  await waitFor(() => assert.ok(savedDocument));
  assert.deepEqual(savedDocument.task_fit_policy, {
    taskClass: "small-code-edit",
    taskClassTaxonomyVersion: "operator-defined-v1",
    evaluationPolicyVersion: "task-fit-outcomes-v1",
    minimumDistinctTasks: 12,
    minimumWilsonLowerBound95: 0.75,
    maximumAgeSeconds: 7 * 86400,
    requireObservedModelIdentity: true,
  });
});

test("route speeds stay unknown until five fresh matching samples", async () => {
  mountSavedProfile([
    measuredGroup({
      freshSampleCount: 2,
      effectiveOutputTokensPerSecondMilli: null,
    }),
  ]);
  await waitFor(() =>
    assert.match(
      screen.getByTestId("route-throughput-local").textContent,
      /2\/5 fresh samples · speed unknown/,
    ),
  );
  assert.doesNotMatch(
    screen.getByTestId("route-throughput-local").textContent,
    /effective output tokens\/s/,
  );
});

test("task-fit report review loads only when its disclosure is opened", async () => {
  let evidenceListCalls = 0;
  handlers.set("list_agent_task_fit_reports", () => {
    evidenceListCalls += 1;
    return [];
  });
  mount();
  await waitFor(() =>
    assert.ok(screen.getByText("No routing profiles saved.")),
  );
  assert.equal(evidenceListCalls, 0);
  fireEvent.click(screen.getByText("Task-fit evidence review"));
  await waitFor(() =>
    assert.ok(screen.getByText("No task-fit reports imported.")),
  );
  assert.equal(evidenceListCalls, 1);
  assert.match(
    screen.getByText(/Reports are checked for internal consistency/)
      .textContent,
    /does not verify the benchmark or enable routing/,
  );
});

test("candidate test shows a redacted receipt for the saved profile identity", async () => {
  let callArgs;
  handlers.set("test_agent_route_candidate", (args) => {
    callArgs = args;
    return candidateTestReceipt();
  });
  mountSavedProfile([]);

  await waitFor(() => assert.ok(screen.getByTestId("route-candidate-tests")));
  fireEvent.click(screen.getByRole("button", { name: "Test candidate" }));
  await waitFor(() => assert.ok(screen.getByTestId("candidate-test-receipt")));
  assert.deepEqual(callArgs, {
    profileId: "local-first",
    candidateId: "local",
    expectedProfileVersion: 3,
    expectedProfileDocumentHash: "a".repeat(64),
    confirmHosted: false,
  });
  assert.match(
    screen.getByTestId("candidate-test-receipt").textContent,
    /provider response identity was not observed/,
  );
  assert.match(
    screen.getByTestId("candidate-test-receipt").textContent,
    /Synthetic prompt check: passed \(marker matched\)/,
  );
});

test("candidate test distinguishes a response from a passed synthetic check", async () => {
  handlers.set("test_agent_route_candidate", () =>
    candidateTestReceipt({ responseMarkerMatched: false }),
  );
  mountSavedProfile([]);

  await waitFor(() => assert.ok(screen.getByTestId("route-candidate-tests")));
  fireEvent.click(screen.getByRole("button", { name: "Test candidate" }));
  await waitFor(() =>
    assert.match(
      screen.getByTestId("candidate-test-receipt").textContent,
      /Connection responded/,
    ),
  );
  assert.match(
    screen.getByTestId("candidate-test-receipt").textContent,
    /Synthetic prompt check: did not match expected marker/,
  );
});

test("hosted candidate requires destination preview and a second confirmation", async () => {
  const callArgs = [];
  handlers.set("test_agent_route_candidate", (args) => {
    callArgs.push(args);
    return args.confirmHosted
      ? candidateTestReceipt({
          candidateId: "hosted",
          providerId: "deepseek",
          requestedModelId: "deepseek-chat",
          dataLocation: "hosted",
          endpointOrigin: "https://api.deepseek.com",
        })
      : candidateTestReceipt({
          candidateId: "hosted",
          providerId: "deepseek",
          requestedModelId: "deepseek-chat",
          dataLocation: "hosted",
          endpointOrigin: "https://api.deepseek.com",
          status: "confirmation_required",
          responseMarkerMatched: null,
        });
  });
  mountSavedProfile([], {
    candidate: {
      id: "hosted",
      provider: "deepseek",
      model: "deepseek-chat",
      data_location: "hosted",
    },
    dataPolicy: "allow-hosted",
  });

  await waitFor(() => assert.ok(screen.getByTestId("route-candidate-tests")));
  fireEvent.click(screen.getByRole("button", { name: "Test candidate" }));
  await waitFor(() =>
    assert.ok(
      screen.getByRole("dialog", { name: "Confirm hosted synthetic test" }),
    ),
  );
  assert.equal(callArgs.length, 1);
  assert.equal(callArgs[0].confirmHosted, false);
  assert.ok(screen.getByText(/https:\/\/api\.deepseek\.com/));

  fireEvent.click(screen.getByRole("button", { name: "Send synthetic test" }));
  await waitFor(() => assert.ok(screen.getByTestId("candidate-test-receipt")));
  assert.equal(callArgs.length, 2);
  assert.equal(callArgs[1].confirmHosted, true);
});

test("candidate test shows typed failure without provider response text", async () => {
  handlers.set("test_agent_route_candidate", () =>
    candidateTestReceipt({
      status: "failed",
      responseMarkerMatched: null,
      failureClass: "provider_error",
    }),
  );
  mountSavedProfile([]);

  await waitFor(() => assert.ok(screen.getByTestId("route-candidate-tests")));
  fireEvent.click(screen.getByRole("button", { name: "Test candidate" }));
  await waitFor(() =>
    assert.match(
      screen.getByTestId("candidate-test-receipt").textContent,
      /Connection failed · provider_error/,
    ),
  );
  assert.doesNotMatch(
    screen.getByTestId("candidate-test-receipt").textContent,
    /raw provider response|secret/i,
  );
});

test("candidate test prevents duplicate requests while one is in flight", async () => {
  let resolveCall;
  let calls = 0;
  handlers.set(
    "test_agent_route_candidate",
    () =>
      new Promise((resolve) => {
        calls += 1;
        resolveCall = resolve;
      }),
  );
  mountSavedProfile([]);

  await waitFor(() => assert.ok(screen.getByTestId("route-candidate-tests")));
  const button = screen.getByRole("button", { name: "Test candidate" });
  fireEvent.click(button);
  await waitFor(() => assert.equal(button.disabled, true));
  fireEvent.click(button);
  assert.equal(calls, 1);
  resolveCall(candidateTestReceipt());
  await waitFor(() => assert.ok(screen.getByTestId("candidate-test-receipt")));
});
