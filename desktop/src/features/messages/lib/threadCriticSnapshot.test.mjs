import assert from "node:assert/strict";
import { test } from "node:test";

const { buildThreadCriticSnapshot, THREAD_CRITIC_SNAPSHOT_MAX_BYTES } =
  await import("./threadCriticSnapshot.ts");

function message(id, body, author = "Ari") {
  return {
    id,
    author,
    body,
    createdAt: 1,
    depth: 0,
    time: "now",
  };
}

function entry(value) {
  return { message: value, summary: null };
}

function brief(
  rootId,
  { intent = "Original intent", progress = "Latest progress" } = {},
) {
  return {
    thread_root_id: rootId,
    original_intent: { id: rootId, content: intent },
    progress_events: [{ id: "c".repeat(64), content: progress }],
    auxiliary_events: [],
    summary: {
      text: "1 reply event(s) observed.",
      method: "deterministic",
      task_completion: "unknown",
    },
    source_event_ids: [rootId, "c".repeat(64)],
    status: {
      task_state: "unknown",
      reply_event_count: 1,
      latest_activity: null,
      next_cursor: { created_at: 2, event_id: "c".repeat(64) },
      requested_depth_limit: 64,
      applied_depth_limit: 64,
      depth_limit_may_truncate: true,
      possibly_truncated: true,
      managed_turns: [],
      steering_controls: [
        {
          source_event_id: "d".repeat(64),
          state: "adapter_acknowledged",
          agent_observed: "unknown",
        },
      ],
      coordinator_runs: [],
      managed_turn_lookup: { capture_gap_count: 1 },
      coordinator_run_lookup: { capture_gap_count: 1 },
    },
  };
}

test("captures loaded messages with stable source IDs and honest partial state", () => {
  const head = message("a".repeat(64), "Original thread question");
  const reply = {
    ...message("b".repeat(64), "Loaded reply", "Bea"),
    author: "Bea\r\nInjected metadata",
    createdAt: 1_700_000_000,
    edited: true,
  };
  const unstable = message(
    "optimistic:local-1",
    "Unsynced visible draft",
    "Cal",
  );
  const result = buildThreadCriticSnapshot({
    head,
    replies: [entry(reply), entry(unstable), entry(reply)],
    repliesPending: true,
    repliesError: true,
  });

  assert.deepEqual(result.sourceIds, ["a".repeat(64), "b".repeat(64)]);
  assert.equal(result.includedMessageCount, 3);
  assert.equal(result.omittedMessageCount, 0);
  assert.match(result.snapshot, /Source event ID: a{64}/);
  assert.match(
    result.snapshot,
    /Original event timestamp \(UTC\): 1970-01-01T00:00:01\.000Z\nAuthor: Ari/,
  );
  assert.match(
    result.snapshot,
    /Original event timestamp \(UTC\): 2023-11-14T22:13:20\.000Z \(edited\)\nAuthor: Bea Injected metadata/,
  );
  assert.doesNotMatch(result.snapshot, /Author: Bea\r?\nInjected metadata/);
  assert.match(
    result.snapshot,
    /Displayed bodies may reflect Buzz edit overlays; they are not necessarily raw signed event content/,
  );
  assert.match(
    result.snapshot,
    /Source event ID: unavailable \(no stable event ID\)/,
  );
  assert.match(result.snapshot, /Unsynced visible draft/);
  assert.doesNotMatch(result.snapshot, /optimistic:local-1/);
  assert.match(result.disclosure, /currently loaded/);
  assert.match(result.disclosure, /incomplete or stale/);
  assert.match(result.disclosure, /still loading/);
  assert.match(result.disclosure, /could not load all replies/);
});

test("truncates only between messages and stays within 64 KiB", () => {
  const head = message("a".repeat(64), "Keep this complete message");
  const tooLarge = message("b".repeat(64), "😀".repeat(40_000));
  const afterLarge = message(
    "c".repeat(64),
    "This later message is omitted too",
  );
  const result = buildThreadCriticSnapshot({
    head,
    replies: [entry(tooLarge), entry(afterLarge)],
  });

  assert.ok(
    new TextEncoder().encode(result.snapshot).byteLength <=
      THREAD_CRITIC_SNAPSHOT_MAX_BYTES,
  );
  assert.match(result.snapshot, /Keep this complete message/);
  assert.doesNotMatch(result.snapshot, /😀/);
  assert.doesNotMatch(result.snapshot, /This later message is omitted too/);
  assert.match(result.snapshot, /omitted at a message boundary/);
  assert.deepEqual(result.sourceIds, ["a".repeat(64)]);
  assert.equal(result.includedMessageCount, 1);
  assert.equal(result.omittedMessageCount, 2);
});

test("adds source-linked brief intent, progress, steering, and honest status limits", () => {
  const head = message("a".repeat(64), "Loaded thread root");
  const result = buildThreadCriticSnapshot({
    head,
    replies: [entry(message("b".repeat(64), "Loaded reply"))],
    brief: brief(head.id),
  });

  assert.match(result.snapshot, /## Source-linked thread brief/);
  assert.match(
    result.snapshot,
    /### Original intent\nSource event ID: a{64}\nOriginal intent/,
  );
  assert.match(
    result.snapshot,
    /Progress event\nSource event ID: c{64}\nLatest progress/,
  );
  assert.match(
    result.snapshot,
    /Steering source event ID: d{64}; recorded state: adapter_acknowledged; agent observation: unknown/,
  );
  assert.match(
    result.snapshot,
    /Task completion: unknown\. Worker liveness: unknown\./,
  );
  assert.match(
    result.snapshot,
    /another reply page is available and was not loaded/,
  );
  assert.match(
    result.snapshot,
    /nested replies beyond depth 64 may be omitted/,
  );
  assert.match(result.snapshot, /best-effort with 1 recorded journal gap/);
  assert.deepEqual(result.sourceIds, [
    "a".repeat(64),
    "b".repeat(64),
    "c".repeat(64),
    "d".repeat(64),
  ]);
  assert.match(result.disclosure, /fetched when review opened/);
  assert.match(
    result.disclosure,
    /Task completion and worker liveness remain unknown/,
  );
  assert.ok(
    new TextEncoder().encode(result.snapshot).byteLength <=
      THREAD_CRITIC_SNAPSHOT_MAX_BYTES,
  );
});

test("brief and loaded messages share the same 64 KiB cap and disclose failed lookup", () => {
  const head = message("a".repeat(64), "😀".repeat(40_000));
  const result = buildThreadCriticSnapshot({
    head,
    replies: [],
    brief: brief(head.id, {
      intent: "💡".repeat(20_000),
      progress: "🧭".repeat(20_000),
    }),
  });
  assert.ok(
    new TextEncoder().encode(result.snapshot).byteLength <=
      THREAD_CRITIC_SNAPSHOT_MAX_BYTES,
  );
  assert.match(result.snapshot, /Text shortened to fit the review snapshot/);
  assert.match(result.disclosure, /may be truncated/);

  const failed = buildThreadCriticSnapshot({
    head,
    replies: [],
    briefError: true,
  });
  assert.match(
    failed.snapshot,
    /Could not load a matching source-linked thread brief/,
  );
  assert.match(
    failed.snapshot,
    /Only messages already loaded in the thread panel are included/,
  );
  assert.match(
    failed.disclosure,
    /only messages already loaded in the panel are included/,
  );
  assert.ok(
    new TextEncoder().encode(failed.snapshot).byteLength <=
      THREAD_CRITIC_SNAPSHOT_MAX_BYTES,
  );
});
