import assert from "node:assert/strict";
import test from "node:test";

import { buildProjectResourcePreflight } from "./projectResourcePreflight.ts";

test("resource preflight scopes configured controls to new agents", () => {
  const result = buildProjectResourcePreflight({
    parallelism: "4",
    idleTimeoutSeconds: "240",
    maxTurnDurationSeconds: "",
  });

  assert.equal(result.valid, true);
  assert.equal(result.policy.schemaVersion, 1);
  assert.equal(result.policy.scope, "project");
  assert.equal(result.policy.appliesTo, "new_managed_agents");
  assert.deepEqual(
    result.policy.metrics.slice(0, 3).map((metric) => ({
      id: metric.id,
      value: metric.value,
      source: metric.source,
      scope: metric.scope,
      state: metric.state,
      enforcement: metric.enforcement,
    })),
    [
      {
        id: "agent.parallelism",
        value: 4,
        source: "project_local_default",
        scope: "agent",
        state: "configured",
        enforcement: "hard_runtime",
      },
      {
        id: "agent.idleTimeoutSeconds",
        value: 240,
        source: "project_local_default",
        scope: "agent",
        state: "configured",
        enforcement: "turn_boundary",
      },
      {
        id: "agent.maxTurnDurationSeconds",
        value: null,
        source: "runtime_default",
        scope: "agent",
        state: "unknown",
        enforcement: "unknown",
      },
    ],
  );
  assert.ok(
    result.policy.metrics.some(
      (metric) => metric.id === "run.token_usage" && metric.state === "unknown",
    ),
  );
  assert.ok(
    result.policy.metrics.some(
      (metric) => metric.id === "device.memory" && metric.value === null,
    ),
  );
});

test("invalid controls block the preflight instead of being normalized", () => {
  const result = buildProjectResourcePreflight({
    parallelism: "33",
    idleTimeoutSeconds: "",
    maxTurnDurationSeconds: "",
  });

  assert.deepEqual(result, { valid: false, policy: null });
});
