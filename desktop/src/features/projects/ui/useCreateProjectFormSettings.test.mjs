import assert from "node:assert/strict";
import { test } from "node:test";

import { buildCreateProjectAgents } from "./useCreateProjectFormSettings.ts";

const runtime = {
  id: "buzz-agent",
  label: "Buzz Agent",
  availability: "available",
  command: "buzz-agent",
  binaryPath: "/bin/buzz-agent",
};

function persona(id, displayName) {
  return {
    id,
    displayName,
    avatarUrl: null,
    systemPrompt: `${displayName} instructions`,
    runtime: null,
    model: null,
  };
}

test("project creation expands a team and deduplicates the separately selected persona", () => {
  const alpha = persona("alpha", "Alpha");
  const beta = persona("beta", "Beta");
  const agents = buildCreateProjectAgents({
    agentPersonaId: "beta",
    personas: [alpha, beta],
    runtimes: [runtime],
    teamId: "builders",
    teams: [{ id: "builders", personaIds: ["alpha", "beta"] }],
  });

  assert.deepEqual(
    agents.map(({ personaId, teamId }) => ({ personaId, teamId })),
    [
      { personaId: "alpha", teamId: "builders" },
      { personaId: "beta", teamId: "builders" },
    ],
  );
  assert.equal(agents[0].runtime, runtime);
});

test("project creation applies the selected archetype to each initial agent", () => {
  const agents = buildCreateProjectAgents({
    agentPersonaId: "",
    executionProfileId: "critic",
    resourceDefaults: {
      parallelism: 3,
      idleTimeoutSeconds: 240,
      maxTurnDurationSeconds: 1800,
    },
    personas: [persona("alpha", "Alpha"), persona("beta", "Beta")],
    runtimes: [runtime],
    teamId: "builders",
    teams: [{ id: "builders", personaIds: ["alpha", "beta"] }],
  });

  assert.deepEqual(
    agents.map(
      ({
        executionProfileId,
        parallelism,
        idleTimeoutSeconds,
        maxTurnDurationSeconds,
        forceNewInstance,
      }) => ({
        executionProfileId,
        parallelism,
        idleTimeoutSeconds,
        maxTurnDurationSeconds,
        forceNewInstance,
      }),
    ),
    [
      {
        executionProfileId: "critic",
        parallelism: 3,
        idleTimeoutSeconds: 240,
        maxTurnDurationSeconds: 1800,
        forceNewInstance: true,
      },
      {
        executionProfileId: "critic",
        parallelism: 3,
        idleTimeoutSeconds: 240,
        maxTurnDurationSeconds: 1800,
        forceNewInstance: true,
      },
    ],
  );
});

test("project creation pins the selected route only to local Buzz Agents", () => {
  const claudeRuntime = { ...runtime, id: "claude", label: "Claude Code" };
  const agents = buildCreateProjectAgents({
    agentPersonaId: "beta",
    routeProfileId: "local-first",
    personas: [
      { ...persona("alpha", "Alpha"), runtime: "claude" },
      persona("beta", "Beta"),
    ],
    runtimes: [runtime, claudeRuntime],
    teamId: "builders",
    teams: [{ id: "builders", personaIds: ["alpha", "beta"] }],
  });

  assert.equal(agents[0].runtime.id, "claude");
  assert.equal(agents[0].routeProfileId, undefined);
  assert.equal(agents[0].forceNewInstance, false);
  assert.equal(agents[1].runtime.id, "buzz-agent");
  assert.equal(agents[1].routeProfileId, "local-first");
  assert.equal(agents[1].forceNewInstance, true);
});

test("project limits create fresh initial agents without changing style", () => {
  const agents = buildCreateProjectAgents({
    agentPersonaId: "alpha",
    resourceDefaults: { parallelism: 2 },
    personas: [persona("alpha", "Alpha")],
    runtimes: [runtime],
    teamId: "",
    teams: [],
  });
  assert.equal(agents[0].executionProfileId, undefined);
  assert.equal(agents[0].parallelism, 2);
  assert.equal(agents[0].forceNewInstance, true);
});

test("project creation leaves agent reuse unchanged without an archetype", () => {
  const agents = buildCreateProjectAgents({
    agentPersonaId: "alpha",
    personas: [persona("alpha", "Alpha")],
    runtimes: [runtime],
    teamId: "",
    teams: [],
  });

  assert.equal(agents[0].executionProfileId, undefined);
  assert.equal(agents[0].forceNewInstance, false);
});
