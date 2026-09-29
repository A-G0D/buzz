import assert from "node:assert/strict";
import test from "node:test";

import {
  applyProjectAgentProfileDefault,
  applyProjectAgentRouteProfileDefault,
  applyProjectAgentResourceDefaults,
  applyStoredProjectAgentProfileDefault,
  applyStoredProjectAgentRouteProfileDefault,
  parseProjectAgentResourceDefaultsDraft,
  readProjectAgentProfileDefault,
  readProjectAgentRouteProfileDefault,
  readProjectAgentResourceDefaults,
  writeProjectAgentProfileDefault,
  writeProjectAgentRouteProfileDefault,
  writeProjectAgentResourceDefaults,
  toProjectAgentResourceDefaultsDraft,
  projectAgentRouteProfileEnvVars,
} from "./projectAgentProfileDefault.ts";

const store = new Map();
globalThis.window = {
  localStorage: {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => store.set(key, value),
    removeItem: (key) => store.delete(key),
  },
};

const RELAY = "wss://relay.example.com";
const OWNER = "a".repeat(64);
const CHANNEL = "11111111-1111-4111-8111-111111111111";

test.beforeEach(() => store.clear());

test("project agent profile defaults stay scoped to relay, identity, and channel", () => {
  assert.equal(
    writeProjectAgentProfileDefault(RELAY, OWNER, CHANNEL, "critic"),
    true,
  );
  assert.equal(readProjectAgentProfileDefault(RELAY, OWNER, CHANNEL), "critic");
  assert.equal(
    readProjectAgentProfileDefault("wss://other.example.com", OWNER, CHANNEL),
    null,
  );
  assert.equal(
    readProjectAgentProfileDefault(RELAY, "b".repeat(64), CHANNEL),
    null,
  );
  assert.equal(
    readProjectAgentProfileDefault(
      RELAY,
      OWNER,
      "22222222-2222-4222-8222-222222222222",
    ),
    null,
  );
});

test("clearing removes a project default and malformed values are ignored", () => {
  writeProjectAgentProfileDefault(RELAY, OWNER, CHANNEL, "fast");
  assert.equal(
    writeProjectAgentProfileDefault(RELAY, OWNER, CHANNEL, null),
    true,
  );
  assert.equal(readProjectAgentProfileDefault(RELAY, OWNER, CHANNEL), null);

  const key = `buzz-project-agent-profile.v1:${RELAY}:${OWNER}:${CHANNEL}`;
  store.set(key, "not-json");
  assert.equal(readProjectAgentProfileDefault(RELAY, OWNER, CHANNEL), null);
});

test("project route defaults are scoped, validated, and independently clearable", () => {
  assert.equal(
    writeProjectAgentRouteProfileDefault(RELAY, OWNER, CHANNEL, "local-first"),
    true,
  );
  assert.equal(
    readProjectAgentRouteProfileDefault(RELAY, OWNER, CHANNEL),
    "local-first",
  );
  assert.equal(
    readProjectAgentRouteProfileDefault(
      "wss://other.example.com",
      OWNER,
      CHANNEL,
    ),
    null,
  );
  assert.equal(
    writeProjectAgentRouteProfileDefault(RELAY, OWNER, CHANNEL, "../outside"),
    false,
  );
  assert.equal(
    writeProjectAgentRouteProfileDefault(RELAY, OWNER, CHANNEL, null),
    true,
  );
  assert.equal(
    readProjectAgentRouteProfileDefault(RELAY, OWNER, CHANNEL),
    null,
  );
});

test("resource defaults are scoped, validated, and independent of the style default", () => {
  writeProjectAgentProfileDefault(RELAY, OWNER, CHANNEL, "critic");
  assert.equal(
    writeProjectAgentResourceDefaults(RELAY, OWNER, CHANNEL, {
      parallelism: 4,
      idleTimeoutSeconds: 240,
    }),
    true,
  );
  assert.deepEqual(readProjectAgentResourceDefaults(RELAY, OWNER, CHANNEL), {
    parallelism: 4,
    idleTimeoutSeconds: 240,
  });
  assert.equal(readProjectAgentProfileDefault(RELAY, OWNER, CHANNEL), "critic");
  assert.equal(
    readProjectAgentResourceDefaults("wss://other.example.com", OWNER, CHANNEL),
    null,
  );
  assert.equal(
    writeProjectAgentResourceDefaults(RELAY, OWNER, CHANNEL, {
      parallelism: 33,
    }),
    false,
  );
  assert.equal(
    writeProjectAgentResourceDefaults(RELAY, OWNER, CHANNEL, {
      idleTimeoutSeconds: Number.MAX_SAFE_INTEGER + 1,
    }),
    false,
  );
  assert.equal(
    writeProjectAgentResourceDefaults(RELAY, OWNER, CHANNEL, null),
    true,
  );
  assert.equal(readProjectAgentResourceDefaults(RELAY, OWNER, CHANNEL), null);
  assert.equal(readProjectAgentProfileDefault(RELAY, OWNER, CHANNEL), "critic");
});

test("resource limit form accepts blank or whole-number defaults only", () => {
  assert.deepEqual(
    toProjectAgentResourceDefaultsDraft({
      parallelism: 2,
      maxTurnDurationSeconds: 900,
    }),
    {
      parallelism: "2",
      idleTimeoutSeconds: "",
      maxTurnDurationSeconds: "900",
    },
  );
  assert.deepEqual(
    parseProjectAgentResourceDefaultsDraft({
      parallelism: "",
      idleTimeoutSeconds: "",
      maxTurnDurationSeconds: "",
    }),
    {},
  );
  assert.deepEqual(
    parseProjectAgentResourceDefaultsDraft({
      parallelism: "3",
      idleTimeoutSeconds: "120",
      maxTurnDurationSeconds: "",
    }),
    { parallelism: 3, idleTimeoutSeconds: 120 },
  );
  assert.equal(
    parseProjectAgentResourceDefaultsDraft({
      parallelism: "33",
      idleTimeoutSeconds: "1",
      maxTurnDurationSeconds: "",
    }),
    null,
  );
  assert.equal(
    parseProjectAgentResourceDefaultsDraft({
      parallelism: "2.5",
      idleTimeoutSeconds: "1",
      maxTurnDurationSeconds: "",
    }),
    null,
  );
});

test("project defaults fill unconfigured agents and pin profiled instances", () => {
  const inputs = [
    { name: "new", executionProfileId: undefined, forceNewInstance: false },
    { name: "explicit", executionProfileId: "critic", forceNewInstance: false },
    { name: "plain", forceNewInstance: false },
  ];
  assert.deepEqual(applyProjectAgentProfileDefault(inputs, "fast"), [
    { name: "new", executionProfileId: "fast", forceNewInstance: true },
    { name: "explicit", executionProfileId: "critic", forceNewInstance: true },
    { name: "plain", executionProfileId: "fast", forceNewInstance: true },
  ]);
  assert.deepEqual(applyProjectAgentProfileDefault(inputs, null), [
    { name: "new", executionProfileId: undefined, forceNewInstance: false },
    { name: "explicit", executionProfileId: "critic", forceNewInstance: true },
    { name: "plain", forceNewInstance: false },
  ]);
});

test("route defaults only pin local Buzz Agents and preserve explicit choices", () => {
  const inputs = [
    { name: "buzz", runtime: { id: "buzz-agent" } },
    {
      name: "explicit",
      runtime: { id: "buzz-agent" },
      routeProfileId: "manual",
    },
    {
      name: "provider backend",
      runtime: { id: "buzz-agent" },
      backend: { type: "provider" },
    },
    { name: "other harness", runtime: { id: "claude" } },
  ];
  assert.deepEqual(
    applyProjectAgentRouteProfileDefault(inputs, "local-first"),
    [
      {
        name: "buzz",
        runtime: { id: "buzz-agent" },
        routeProfileId: "local-first",
        forceNewInstance: true,
      },
      {
        name: "explicit",
        runtime: { id: "buzz-agent" },
        routeProfileId: "manual",
        forceNewInstance: true,
      },
      inputs[2],
      inputs[3],
    ],
  );
  assert.deepEqual(
    projectAgentRouteProfileEnvVars(
      inputs[0] && {
        ...inputs[0],
        routeProfileId: "local-first",
      },
    ),
    { BUZZ_AGENT_ROUTE_PROFILE_ID: "local-first" },
  );
  assert.equal(
    projectAgentRouteProfileEnvVars({
      ...inputs[2],
      routeProfileId: "local-first",
    }),
    undefined,
  );
  assert.equal(
    projectAgentRouteProfileEnvVars({
      ...inputs[3],
      routeProfileId: "local-first",
    }),
    undefined,
  );
});

test("resource defaults fill only missing controls and prevent silent reuse", () => {
  const inputs = [
    { name: "new", forceNewInstance: false },
    {
      name: "partially configured",
      parallelism: 8,
      forceNewInstance: false,
    },
    {
      name: "fully configured",
      parallelism: 2,
      idleTimeoutSeconds: 30,
      maxTurnDurationSeconds: 600,
      forceNewInstance: false,
    },
  ];
  assert.deepEqual(
    applyProjectAgentResourceDefaults(inputs, {
      parallelism: 4,
      idleTimeoutSeconds: 240,
      maxTurnDurationSeconds: 1200,
    }),
    [
      {
        name: "new",
        parallelism: 4,
        idleTimeoutSeconds: 240,
        maxTurnDurationSeconds: 1200,
        forceNewInstance: true,
      },
      {
        name: "partially configured",
        parallelism: 8,
        idleTimeoutSeconds: 240,
        maxTurnDurationSeconds: 1200,
        forceNewInstance: true,
      },
      inputs[2],
    ],
  );
  assert.deepEqual(applyProjectAgentResourceDefaults(inputs, null), inputs);
});

test("stored defaults are catalog-checked before the production input projection", async () => {
  writeProjectAgentProfileDefault(RELAY, OWNER, CHANNEL, "critic");
  let catalogReads = 0;
  const resolved = await applyStoredProjectAgentProfileDefault(
    { relayUrl: RELAY, ownerPubkey: OWNER, channelId: CHANNEL },
    [{ name: "new agent" }],
    async () => {
      catalogReads += 1;
      return [{ id: "critic" }];
    },
  );
  assert.equal(catalogReads, 1);
  assert.deepEqual(resolved, [
    { name: "new agent", executionProfileId: "critic", forceNewInstance: true },
  ]);

  const stale = await applyStoredProjectAgentProfileDefault(
    { relayUrl: RELAY, ownerPubkey: OWNER, channelId: CHANNEL },
    [{ name: "another" }],
    async () => [{ id: "fast" }],
  );
  assert.deepEqual(stale, [{ name: "another" }]);
  assert.equal(readProjectAgentProfileDefault(RELAY, OWNER, CHANNEL), null);
});

test("stored resource defaults reach channel provisioning without a profile lookup", async () => {
  writeProjectAgentResourceDefaults(RELAY, OWNER, CHANNEL, {
    parallelism: 3,
    maxTurnDurationSeconds: 1800,
  });
  let catalogReads = 0;
  const resolved = await applyStoredProjectAgentProfileDefault(
    { relayUrl: RELAY, ownerPubkey: OWNER, channelId: CHANNEL },
    [{ name: "new agent", forceNewInstance: false }],
    async () => {
      catalogReads += 1;
      return [];
    },
  );
  assert.equal(catalogReads, 0);
  assert.deepEqual(resolved, [
    {
      name: "new agent",
      forceNewInstance: true,
      parallelism: 3,
      maxTurnDurationSeconds: 1800,
    },
  ]);
});

test("stored route defaults are catalog-checked only for eligible new instances", async () => {
  writeProjectAgentRouteProfileDefault(RELAY, OWNER, CHANNEL, "local-first");
  let catalogReads = 0;
  const localAgent = {
    name: "new local agent",
    runtime: { id: "buzz-agent" },
  };
  const resolved = await applyStoredProjectAgentRouteProfileDefault(
    { relayUrl: RELAY, ownerPubkey: OWNER, channelId: CHANNEL },
    [localAgent, { name: "other", runtime: { id: "claude" } }],
    async () => {
      catalogReads += 1;
      return [{ id: "local-first" }];
    },
  );
  assert.equal(catalogReads, 1);
  assert.equal(resolved[0].routeProfileId, "local-first");
  assert.equal(resolved[0].forceNewInstance, true);
  assert.equal(resolved[1].routeProfileId, undefined);

  const ineligible = await applyStoredProjectAgentRouteProfileDefault(
    { relayUrl: RELAY, ownerPubkey: OWNER, channelId: CHANNEL },
    [
      {
        name: "remote",
        runtime: { id: "buzz-agent" },
        backend: { type: "provider" },
      },
    ],
    async () => {
      catalogReads += 1;
      return [];
    },
  );
  assert.equal(catalogReads, 1);
  assert.equal(ineligible[0].routeProfileId, undefined);

  const stale = await applyStoredProjectAgentRouteProfileDefault(
    { relayUrl: RELAY, ownerPubkey: OWNER, channelId: CHANNEL },
    [localAgent],
    async () => [{ id: "other" }],
  );
  assert.deepEqual(stale, [localAgent]);
  assert.equal(
    readProjectAgentRouteProfileDefault(RELAY, OWNER, CHANNEL),
    null,
  );
});

test("explicit per-agent settings take precedence over project defaults", async () => {
  writeProjectAgentProfileDefault(RELAY, OWNER, CHANNEL, "critic");
  writeProjectAgentResourceDefaults(RELAY, OWNER, CHANNEL, {
    parallelism: 3,
    idleTimeoutSeconds: 240,
  });
  const resolved = await applyStoredProjectAgentProfileDefault(
    { relayUrl: RELAY, ownerPubkey: OWNER, channelId: CHANNEL },
    [
      {
        name: "configured",
        executionProfileId: "fast",
        parallelism: 8,
        idleTimeoutSeconds: 30,
        forceNewInstance: false,
      },
      { name: "inherited" },
    ],
    async () => [{ id: "critic" }, { id: "fast" }],
  );
  assert.deepEqual(resolved, [
    {
      name: "configured",
      executionProfileId: "fast",
      parallelism: 8,
      idleTimeoutSeconds: 30,
      forceNewInstance: true,
    },
    {
      name: "inherited",
      executionProfileId: "critic",
      parallelism: 3,
      idleTimeoutSeconds: 240,
      forceNewInstance: true,
    },
  ]);
});
