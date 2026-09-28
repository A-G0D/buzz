import assert from "node:assert/strict";
import test from "node:test";

import { deletionAcknowledgementMatches } from "./HostedCommunityDeleteDialog.tsx";

test("deletion acknowledgement is byte-exact", () => {
  const host = "Exact-Host.communities.buzz.xyz";
  assert.equal(deletionAcknowledgementMatches(host, host), true);
  assert.equal(deletionAcknowledgementMatches(host.toLowerCase(), host), false);
  assert.equal(deletionAcknowledgementMatches(` ${host}`, host), false);
  assert.equal(deletionAcknowledgementMatches(`${host} `, host), false);
});
