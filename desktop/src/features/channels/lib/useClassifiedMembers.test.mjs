import assert from "node:assert/strict";
import test from "node:test";

import { isVerifiedAgentMember } from "./useClassifiedMembers.ts";

const PUBKEY = "A".repeat(64);
const member = {
  pubkey: PUBKEY,
  displayName: null,
  role: "admin",
  isAgent: false,
};

test("classifies an admin from the uncapped verified profile cache as an agent", () => {
  assert.equal(
    isVerifiedAgentMember(
      member,
      new Set([PUBKEY.toLowerCase()]),
      new Set(),
      new Set(),
    ),
    true,
  );
});

test("does not infer agent identity from an admin channel role", () => {
  assert.equal(
    isVerifiedAgentMember(member, new Set(), new Set(), new Set()),
    false,
  );
});
