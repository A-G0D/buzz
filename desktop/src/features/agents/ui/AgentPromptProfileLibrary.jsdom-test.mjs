import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import React from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
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

const { AgentPromptProfileLibrary } = await import(
  "./AgentPromptProfileLibrary.tsx"
);

afterEach(() => {
  cleanup();
  handlers.clear();
});

function profileFor(kind, targetId) {
  return {
    id: "profile-local",
    name: "Local profile",
    version: 3,
    schemaVersion: 1,
    target: { kind, targetId, modelId: null },
    prompt: "Be concise.",
    promptHash: "a".repeat(64),
    updatedAt: "2026-09-25T00:00:00Z",
  };
}

function mount(profile) {
  handlers.set("list_agent_prompt_profiles", () => [profile]);
  handlers.set("read_agent_prompt_profile", () => profile);
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
        React.createElement(AgentPromptProfileLibrary, { onBack() {} }),
      ),
    ),
  );
}

test("DSH ACP prompt profile discloses the overlay replacement and visible path", async () => {
  mount(profileFor("acp_harness", "dsh"));
  await waitFor(() => assert.ok(screen.getByLabelText("Prompt text")));
  assert.ok(screen.getByText(/replaces the effective/));
  assert.ok(
    screen.getByText(
      /unknown custom fields within that row are not preserved or verified/,
    ),
  );
  assert.ok(screen.getByText(/DSH’s saved profile files remain untouched/));
  assert.ok(
    screen.getByText(
      /\.agents\/dsh-prompt-overlays\/profile-local-v3\.patch\.yml/,
    ),
  );
});

test("unrelated ACP targets do not show DSH-specific overlay claims", async () => {
  mount(profileFor("acp_harness", "goose"));
  await waitFor(() => assert.ok(screen.getByLabelText("Prompt text")));
  assert.equal(screen.queryByText(/replaces the effective/), null);
  assert.equal(
    screen.queryByText(/DSH’s saved profile files remain untouched/),
    null,
  );
  assert.equal(screen.queryByText(/dsh-prompt-overlays/), null);
});
