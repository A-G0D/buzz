import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import React from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

globalThis.__TAURI_INTERNALS__ = {
  invoke(command) {
    if (command === "list_agent_archetypes") return [];
    return Promise.reject(new Error(`Unexpected Tauri command: ${command}`));
  },
  transformCallback() {
    return 1;
  },
};
globalThis.window.__TAURI_INTERNALS__ = globalThis.__TAURI_INTERNALS__;

const { CreateProjectFormSettings } = await import(
  "./CreateProjectFormSettings.tsx"
);

afterEach(() => cleanup());

test("project creation exposes saved route choices for new local Buzz Agents", async () => {
  const setRouteProfileId = (value) => values.push(value);
  const values = [];
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  const props = {
    agentPersonaId: "persona-1",
    channelVisibility: "open",
    disabled: false,
    executionProfileId: "",
    handleTemplateChange() {},
    handleTemplateCreated() {},
    personas: [
      {
        id: "persona-1",
        displayName: "Builder",
      },
    ],
    projectVisibility: "listed",
    resourceDefaultsDraft: {
      parallelism: "",
      idleTimeoutSeconds: "",
      maxTurnDurationSeconds: "",
    },
    routeProfileId: "",
    routeProfiles: [
      {
        id: "local-first",
        name: "Local first",
        version: 2,
        dataPolicy: "local-only",
        candidateCount: 1,
        documentHash: "a".repeat(64),
        updatedAt: "2026-09-26T00:00:00Z",
      },
    ],
    routeProfilesError: false,
    routeProfilesLoading: false,
    runtimesAvailable: true,
    setAgentPersonaId() {},
    setChannelVisibility() {},
    setExecutionProfileId() {},
    setProjectVisibility() {},
    setResourceDefaultsDraft() {},
    setRouteProfileId,
    setTeamId() {},
    teamId: "",
    teams: [],
    templateId: "project-home",
    templates: [{ id: "project-home", name: "Project home" }],
  };

  render(
    React.createElement(
      QueryClientProvider,
      { client: queryClient },
      React.createElement(CreateProjectFormSettings, props),
    ),
  );

  fireEvent.click(screen.getByText("Provider route for new local Buzz Agents"));
  fireEvent.pointerDown(
    screen.getByRole("button", {
      name: /off · use configured provider\/model/i,
    }),
    { button: 0, ctrlKey: false, pointerType: "mouse" },
  );
  fireEvent.click(
    await screen.findByRole("menuitemradio", {
      name: /Local first · local-only · v2/,
    }),
  );

  assert.deepEqual(values, ["local-first"]);
  assert.match(
    screen.getByText(/Pins a saved route to initial local Buzz Agents/)
      .textContent,
    /future local Buzz Agents added to this project/,
  );
});
