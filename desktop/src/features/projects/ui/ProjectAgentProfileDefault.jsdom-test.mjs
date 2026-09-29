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
import { readProjectAgentRouteProfileDefault } from "@/features/projects/projectAgentProfileDefault";

const summaries = [
  {
    id: "local-first",
    name: "Local first",
    version: 2,
    dataPolicy: "local-only",
    candidateCount: 1,
    documentHash: "a".repeat(64),
    updatedAt: "2026-09-26T00:00:00Z",
  },
];

globalThis.__TAURI_INTERNALS__ = {
  invoke(command) {
    if (command === "list_agent_route_profiles") return summaries;
    if (command === "list_agent_archetypes") return [];
    return Promise.reject(new Error(`Unexpected Tauri command: ${command}`));
  },
  transformCallback() {
    return 1;
  },
};
globalThis.window.__TAURI_INTERNALS__ = globalThis.__TAURI_INTERNALS__;

const { ProjectAgentProfileDefault } = await import(
  "./ProjectAgentProfileDefault.tsx"
);

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

test("Project Home saves a route default scoped to its local Buzz Agent additions", async () => {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  render(
    React.createElement(
      QueryClientProvider,
      { client: queryClient },
      React.createElement(ProjectAgentProfileDefault, {
        channelId: "11111111-1111-4111-8111-111111111111",
        identityPubkey: "a".repeat(64),
        relayUrl: "wss://relay.example.com",
      }),
    ),
  );

  fireEvent.click(screen.getByText("New agent style"));
  fireEvent.click(screen.getByText("Provider route for new local Buzz Agents"));
  fireEvent.pointerDown(
    screen.getByRole("button", { name: /off · use configured provider/i }),
    { button: 0, ctrlKey: false, pointerType: "mouse" },
  );
  fireEvent.click(
    await screen.findByRole("menuitemradio", { name: /Local first/ }),
  );

  await waitFor(() => {
    assert.equal(
      readProjectAgentRouteProfileDefault(
        "wss://relay.example.com",
        "a".repeat(64),
        "11111111-1111-4111-8111-111111111111",
      ),
      "local-first",
    );
  });
  assert.match(
    screen.getByText(/pinned when a new local Buzz Agent is created/)
      .textContent,
    /Saved locally/,
  );
});
