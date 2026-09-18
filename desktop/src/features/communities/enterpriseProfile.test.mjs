import assert from "node:assert/strict";
import test from "node:test";

const { authoritativeEnterpriseProfile } = await import(
  "./enterpriseProfile.ts"
);

test("authoritativeEnterpriseProfile requires independent username and display name", () => {
  assert.deepEqual(
    authoritativeEnterpriseProfile({
      corporateUsername: " seiler ",
      corporateDisplayName: " Brad Seiler ",
      expiresAt: "2026-09-18T21:00:00Z",
    }),
    { username: "seiler", displayName: "Brad Seiler" },
  );
  assert.equal(
    authoritativeEnterpriseProfile({
      corporateUsername: "seiler",
      expiresAt: "2026-09-18T21:00:00Z",
    }),
    null,
  );
  assert.equal(
    authoritativeEnterpriseProfile({
      corporateDisplayName: "Brad Seiler",
      expiresAt: "2026-09-18T21:00:00Z",
    }),
    null,
  );
  assert.equal(
    authoritativeEnterpriseProfile({
      email: "seiler@example.com",
      expiresAt: "2026-09-18T21:00:00Z",
    }),
    null,
  );
});
