import assert from "node:assert/strict";
import test from "node:test";

import {
  allocateEstimatedCriticBudget,
  criticFailureLabel,
  formatEstimatedCriticBudgetUsd,
  parseEstimatedCriticBudgetUsd,
} from "./criticCostBudget.ts";

test("USD text converts exactly to micro-USD and rejects excess precision", () => {
  assert.equal(parseEstimatedCriticBudgetUsd(""), null);
  assert.equal(parseEstimatedCriticBudgetUsd(".25"), 250_000);
  assert.equal(parseEstimatedCriticBudgetUsd("1.000001"), 1_000_001);
  assert.equal(parseEstimatedCriticBudgetUsd("1000000"), 1_000_000_000_000);
  assert.equal(parseEstimatedCriticBudgetUsd("1000000.000001"), undefined);
  assert.equal(parseEstimatedCriticBudgetUsd("0.0000001"), undefined);
  assert.equal(parseEstimatedCriticBudgetUsd("-1"), undefined);
});

test("budget shares are stable and sum to the requested aggregate ceiling", () => {
  const shares = allocateEstimatedCriticBudget(10, [
    "security",
    "correctness",
    "architecture",
  ]);
  assert.deepEqual(shares, {
    architecture: 4,
    correctness: 3,
    security: 3,
  });
  assert.equal(
    Object.values(shares).reduce((sum, value) => sum + value, 0),
    10,
  );
  assert.deepEqual(allocateEstimatedCriticBudget(0, ["correctness"]), {
    correctness: 0,
  });
});

test("estimated USD formatting removes insignificant zeros", () => {
  assert.equal(formatEstimatedCriticBudgetUsd(250_000), "0.25");
  assert.equal(formatEstimatedCriticBudgetUsd(1_000_000), "1");
  assert.equal(formatEstimatedCriticBudgetUsd(1), "0.000001");
});

test("estimated budget failures get clear user-facing labels", () => {
  assert.match(
    criticFailureLabel("estimated_cost_ceiling"),
    /stopped this reviewer before another model request/,
  );
  assert.match(
    criticFailureLabel("estimated_cost_unavailable"),
    /could not estimate the next model request/,
  );
  assert.equal(
    criticFailureLabel("local_model_unavailable"),
    "local_model_unavailable",
  );
});
