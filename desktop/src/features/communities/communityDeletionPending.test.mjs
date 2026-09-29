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

function transport(http_status, body) {
  return { http_status, body };
}

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

test("persistence boundary never overwrites an existing envelope", () => {
  const target = storage();
  assert.equal(persistPendingCommunityDeletion(envelope, target), true);
  const second = {
    ...envelope,
    community_id: "33333333-3333-4333-8333-333333333333",
    request_id: "44444444-4444-4444-8444-444444444444",
  };
  assert.equal(persistPendingCommunityDeletion(second, target), false);
  assert.deepEqual(loadPendingCommunityDeletion(target), envelope);
});

test("ambiguous receipt and same-UUID resubmit misses retain the envelope", () => {
  for (const attempt of ["receipt", "resubmit"]) {
    assert.equal(
      deletionResponseDisposition(
        transport(404, { error: { code: "not_owner" } }),
        envelope,
        attempt,
      ),
      "retain",
    );
  }
  assert.equal(
    deletionResponseDisposition(
      transport(503, { error: { code: "acceptance_unknown" } }),
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
      transport(202, { ...tuple, status: "accepted" }),
      envelope,
      "receipt",
    ),
    "accept",
  );
  assert.equal(
    deletionResponseDisposition(
      transport(409, { ...tuple, error: { code: "deletion_aborted" } }),
      envelope,
      "receipt",
    ),
    "abort",
  );
  assert.equal(
    deletionResponseDisposition(
      transport(409, {
        ...tuple,
        request_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        error: { code: "deletion_aborted" },
      }),
      envelope,
      "receipt",
    ),
    "retain",
  );
});

test("fresh admission clears only the established exact code and native-status pairs", () => {
  const terminalPairs = [
    ["missing_mapping", 400],
    ["invalid_request", 400],
    ["confirmation_mismatch", 400],
    ["unsupported_acknowledgement_version", 400],
    ["not_owner", 404],
    ["must_archive", 409],
    ["protected_target", 409],
    ["deletion_conflict", 409],
  ];
  for (const [code, httpStatus] of terminalPairs) {
    assert.equal(
      deletionResponseDisposition(
        transport(httpStatus, { error: { code } }),
        envelope,
        "initial",
      ),
      "clear",
      `${code} requires HTTP ${httpStatus}`,
    );
    assert.equal(
      deletionResponseDisposition(
        transport(httpStatus + 1, { error: { code } }),
        envelope,
        "initial",
      ),
      "retain",
      `${code} with the wrong native status is ambiguous`,
    );
  }

  for (const code of [
    "acceptance_unknown",
    "unauthorized",
    "relay_unavailable",
    "unknown",
    "future_code",
  ]) {
    assert.equal(
      deletionResponseDisposition(
        transport(400, { error: { code } }),
        envelope,
        "initial",
      ),
      "retain",
      `${code} cannot clear the envelope`,
    );
  }
});

test("native status, never a body-claimed status, binds acceptance and abort", () => {
  const tuple = {
    request_id: envelope.request_id,
    community_id: envelope.community_id,
    host: envelope.host,
    acknowledgement_version: envelope.acknowledgement_version,
  };
  assert.equal(
    deletionResponseDisposition(
      transport(202, { ...tuple, status: "accepted" }),
      envelope,
      "initial",
    ),
    "accept",
  );
  assert.equal(
    deletionResponseDisposition(
      transport(503, {
        ...tuple,
        status: "accepted",
        http_status: 202,
      }),
      envelope,
      "initial",
    ),
    "retain",
  );
  assert.equal(
    deletionResponseDisposition(
      transport(409, { ...tuple, error: { code: "deletion_aborted" } }),
      envelope,
      "receipt",
    ),
    "abort",
  );
  assert.equal(
    deletionResponseDisposition(
      transport(200, {
        ...tuple,
        error: { code: "deletion_aborted" },
        http_status: 409,
      }),
      envelope,
      "receipt",
    ),
    "retain",
  );
});
