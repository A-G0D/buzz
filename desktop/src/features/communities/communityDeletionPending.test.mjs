import assert from "node:assert/strict";
import test from "node:test";

import {
  clearPendingCommunityDeletion,
  deletionResponseDisposition,
  loadPendingCommunityDeletion,
  persistPendingCommunityDeletion,
  pendingCommunityDeletionMatchesAccount,
} from "./communityDeletionPending.ts";

function storage() {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
    values,
  };
}

const envelope = {
  community_id: "4efb8c89-b9cb-4a26-863d-cf2bd5f9d5c1",
  host: "Exact-Host.communities.buzz.xyz",
  request_id: "b2456816-eea0-4f74-9c54-531645dbaec9",
  acknowledgement_version: 1,
  bound_owner_pubkey: "a".repeat(64),
  backend_origin: "https://app.builderlab.xyz",
};

test("pending deletion round-trips exact host bytes and account binding", () => {
  const target = storage();
  assert.equal(persistPendingCommunityDeletion(envelope, target), true);
  assert.deepEqual(loadPendingCommunityDeletion(target), envelope);
  assert.equal(
    pendingCommunityDeletionMatchesAccount(
      envelope,
      "a".repeat(64),
      "https://app.builderlab.xyz",
    ),
    true,
  );
  assert.equal(
    pendingCommunityDeletionMatchesAccount(
      envelope,
      "b".repeat(64),
      "https://app.builderlab.xyz",
    ),
    false,
  );
});

test("pending deletion rejects repaired hosts, malformed UUIDs, and unknown fields", () => {
  const target = storage();
  for (const invalid of [
    { ...envelope, host: ` ${envelope.host}` },
    { ...envelope, request_id: envelope.request_id.toUpperCase() },
    { ...envelope, acknowledgement_version: 2 },
    { ...envelope, extra: true },
  ]) {
    target.setItem(
      "buzz:hosted-community-delete-pending:v1",
      JSON.stringify(invalid),
    );
    assert.equal(loadPendingCommunityDeletion(target), null);
    assert.equal(target.values.size, 0, "invalid envelopes are discarded");
  }
});

test("persistence failure is observable and clear is bounded to the deletion key", () => {
  const throwing = {
    getItem: () => null,
    setItem: () => {
      throw new Error("denied");
    },
    removeItem: () => {},
  };
  assert.equal(persistPendingCommunityDeletion(envelope, throwing), false);

  const target = storage();
  target.setItem("unrelated", "keep");
  assert.equal(persistPendingCommunityDeletion(envelope, target), true);
  clearPendingCommunityDeletion(target);
  assert.equal(target.getItem("unrelated"), "keep");
});

test("ambiguous receipt and same-UUID resubmit misses retain the envelope", () => {
  for (const attempt of ["receipt", "resubmit"]) {
    assert.equal(
      deletionResponseDisposition(
        { error: { code: "not_owner" } },
        envelope,
        attempt,
      ),
      "retain",
    );
  }
  assert.equal(
    deletionResponseDisposition(
      { error: { code: "acceptance_unknown" } },
      envelope,
      "initial",
    ),
    "retain",
  );
});

test("only tuple-bound acceptance or abort terminates ambiguous recovery", () => {
  const tuple = {
    request_id: envelope.request_id,
    community_id: envelope.community_id,
    host: envelope.host,
    acknowledgement_version: envelope.acknowledgement_version,
  };
  assert.equal(
    deletionResponseDisposition(
      { ...tuple, status: "accepted" },
      envelope,
      "receipt",
    ),
    "accept",
  );
  assert.equal(
    deletionResponseDisposition(
      { ...tuple, error: { code: "deletion_aborted" } },
      envelope,
      "receipt",
    ),
    "abort",
  );
  assert.equal(
    deletionResponseDisposition(
      {
        ...tuple,
        request_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        error: { code: "deletion_aborted" },
      },
      envelope,
      "receipt",
    ),
    "retain",
  );
});
