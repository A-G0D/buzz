import assert from "node:assert/strict";
import test from "node:test";

import { parseCriticTriggerIntent } from "./criticTriggerIntent.ts";

function assertUnrecognized(draft) {
  const result = parseCriticTriggerIntent(draft);
  assert.deepEqual(result, { status: "unrecognized" }, JSON.stringify(draft));
  assert.equal(Object.hasOwn(result, "target"), false);
}

test("recognizes only the whole slash command, ignoring case and outer whitespace", () => {
  assert.deepEqual(parseCriticTriggerIntent(" /CrItIcS "), {
    status: "recognized",
    command: "run_critics",
    source: "slash_command",
    target: "current_thread",
  });
});

test("recognizes the conservative whole-draft natural-language forms", () => {
  for (const draft of [
    "run critics on this",
    "RUN CRITICS ON THIS THREAD",
    "please run critics on this thread.",
    "Please run critics on this!",
  ]) {
    assert.deepEqual(parseCriticTriggerIntent(draft), {
      status: "recognized",
      command: "run_critics",
      source: "natural_language",
      target: "current_thread",
    });
  }
});

test("quoted, negated, questioned, ambiguous, or mixed drafts do not trigger", () => {
  for (const draft of [
    '"/critics"',
    "'run critics on this'",
    "don't run critics on this",
    "do not run critics on this thread",
    "run critics on this?",
    "should I run critics on this",
    "can you review this with critics?",
    "run critics",
    "before I forget, run critics on this",
    "run critics on this after the build",
    "/critics and summarize the result",
    "run critics on\nthis thread",
  ]) {
    assertUnrecognized(draft);
  }
});

test("blank drafts remain unrecognized", () => {
  assertUnrecognized("");
  assertUnrecognized(" \t\n ");
});
