import { expect, test, type Page } from "@playwright/test";
import { npubEncode } from "nostr-tools/nip19";

import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";
import { openSettings } from "../helpers/settings";

const OUTDIR = "test-results/hosted-communities";
const DEFAULT_MOCK_PUBKEY = "deadbeef".repeat(8);
/** A second valid identity key, used only as a contradictory hosted npub. */
const OTHER_HEX = "b".repeat(64);
const DELETION_COMMUNITIES = [
  {
    id: "11111111-1111-4111-8111-111111111111",
    name: "Active team",
    normalized_host: "active.communities.buzz.xyz",
  },
  {
    id: "22222222-2222-4222-8222-222222222222",
    name: "Archived team",
    normalized_host: "Exact-Host.communities.buzz.xyz",
    archived_at: "2026-09-28T00:00:00Z",
  },
];

/**
 * Install the default hosted-communities fixture and open its settings
 * section. `builderlabIdentity` overrides the bound account identity so a
 * spec can drive independent — even contradictory — `pubkey_hex`/`npub`
 * fields; the mock bridge passes both through verbatim, like the native
 * command.
 */
async function openHostedCommunitiesSettings(
  page: Page,
  builderlabIdentity?: { npub?: string; pubkey_hex?: string } | null,
) {
  await installMockBridge(page, {
    builderlabAuth: {
      email: "owner@example.com",
      expiresAt: "2099-01-01T00:00:00Z",
    },
    builderlabIdentity: builderlabIdentity ?? {
      pubkey_hex: DEFAULT_MOCK_PUBKEY,
    },
    builderlabCommunities: [
      {
        id: "active-community",
        name: "E2E Test",
        normalized_host: "localhost:3000",
      },
      {
        id: "other-community",
        name: "Design studio",
        normalized_host: "design-studio.communities.buzz.xyz",
      },
    ],
  });
  await page.goto("/");
  await openSettings(page, "hosted-communities");
}

test.beforeEach(async ({ page }) => {
  await openHostedCommunitiesSettings(page);
});

async function openDeletionFixture(
  page: Page,
  options: {
    capability?: boolean;
    mismatch?: boolean;
    errorCode?: string;
    errorSequence?: Array<{ code: string; message?: string } | null>;
    capabilitySequence?: boolean[];
    communitiesSequence?: Array<typeof DELETION_COMMUNITIES>;
    communities?: Array<(typeof DELETION_COMMUNITIES)[number]>;
    httpStatusSequence?: number[];
    bodyStatus?: number;
    identityResponseSequence?: Array<
      | { identity: { npub?: string; pubkey_hex?: string } }
      | { error: { code: string; setup_needed?: boolean } }
    >;
    deferDeletion?: boolean;
  } = {},
) {
  await installMockBridge(page, {
    builderlabAuth: {
      email: "owner@example.com",
      expiresAt: "2099-01-01T00:00:00Z",
      canDeleteBuzzCommunities: options.capability,
    },
    builderlabIdentity: {
      pubkey_hex: options.mismatch ? "f".repeat(64) : DEFAULT_MOCK_PUBKEY,
    },
    builderlabCommunities: options.communities ?? DELETION_COMMUNITIES,
    builderlabCommunitiesSequence: options.communitiesSequence,
    builderlabQuota: { used: 2, limit: 5, canCreate: true },
    builderlabDeletionError: options.errorCode
      ? { code: options.errorCode, message: "mock deletion error" }
      : undefined,
    builderlabDeletionErrorSequence: options.errorSequence,
    builderlabDeletionHttpStatusSequence: options.httpStatusSequence,
    builderlabDeletionBodyStatus: options.bodyStatus,
    builderlabIdentityResponseSequence: options.identityResponseSequence,
    builderlabDeferDeletion: options.deferDeletion,
    builderlabAuthSequence: options.capabilitySequence?.map((capability) => ({
      email: "owner@example.com",
      expiresAt: "2099-01-01T00:00:00Z",
      canDeleteBuzzCommunities: capability,
    })),
  });
  await page.goto("/");
  await openSettings(page, "hosted-communities");
}

async function startArchivedDeletion(page: Page) {
  const exactHost = "Exact-Host.communities.buzz.xyz";
  await page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Archived team" })
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await page
    .getByLabel(`Type the exact host to continue: ${exactHost}`)
    .fill(exactHost);
  await page.getByRole("button", { name: "Continue" }).click();
  await page
    .getByRole("button", { name: "Delete community permanently" })
    .click();
}

async function storedDeletionRequestId(page: Page) {
  return page.evaluate(() => {
    const raw = window.localStorage.getItem(
      "buzz:hosted-community-delete-pending:v1",
    );
    return raw ? JSON.parse(raw).request_id : null;
  });
}

test("deletion is default-off and identity mismatch preserves the gate", async ({
  page,
}) => {
  await openDeletionFixture(page);
  await expect(
    page.getByRole("button", { name: "Delete", exact: true }),
  ).toHaveCount(0);

  await openDeletionFixture(page, { capability: true, mismatch: true });
  await expect(
    page.getByRole("button", { name: "Delete", exact: true }),
  ).toHaveCount(0);
});

test("archived deletion requires exact host and two confirmations, then removes the row", async ({
  page,
}) => {
  await openDeletionFixture(page, { capability: true });
  const active = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Active team" });
  const archived = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Archived team" });
  await expect(
    active.getByRole("button", { name: "Delete", exact: true }),
  ).toHaveCount(0);

  await archived.getByRole("button", { name: "Delete", exact: true }).click();
  const exactHost = "Exact-Host.communities.buzz.xyz";
  const input = page.getByLabel(
    `Type the exact host to continue: ${exactHost}`,
  );
  await input.fill(exactHost.toLowerCase());
  await expect(page.getByRole("button", { name: "Continue" })).toBeDisabled();
  await input.fill(` ${exactHost}`);
  await expect(page.getByRole("button", { name: "Continue" })).toBeDisabled();
  await page.getByRole("button", { name: "Cancel" }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__BUZZ_E2E_COMMANDS__?.filter(
            (command) => command === "delete_builderlab_community",
          ).length ?? 0,
      ),
    )
    .toBe(0);

  await archived.getByRole("button", { name: "Delete", exact: true }).click();
  await page
    .getByLabel(`Type the exact host to continue: ${exactHost}`)
    .fill(exactHost);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(
    page.getByRole("button", { name: "Delete community permanently" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Cancel" }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__BUZZ_E2E_COMMANDS__?.filter(
            (command) => command === "delete_builderlab_community",
          ).length ?? 0,
      ),
    )
    .toBe(0);

  await archived.getByRole("button", { name: "Delete", exact: true }).click();
  await page
    .getByLabel(`Type the exact host to continue: ${exactHost}`)
    .fill(exactHost);
  await page.getByRole("button", { name: "Continue" }).click();
  await page
    .getByRole("button", { name: "Delete community permanently" })
    .dblclick();
  await expect(
    page.getByText("Deletion started", { exact: true }),
  ).toBeVisible();
  await expect(archived).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__BUZZ_E2E_COMMANDS__?.filter(
            (command) => command === "delete_builderlab_community",
          ).length ?? 0,
      ),
    )
    .toBe(1);
});

test("ambiguous deletion keeps the same pending request and exposes receipt lookup", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    errorCode: "acceptance_unknown",
  });
  const archived = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Archived team" });
  const exactHost = "Exact-Host.communities.buzz.xyz";
  await archived.getByRole("button", { name: "Delete", exact: true }).click();
  await page
    .getByLabel(`Type the exact host to continue: ${exactHost}`)
    .fill(exactHost);
  await page.getByRole("button", { name: "Continue" }).click();
  await page
    .getByRole("button", { name: "Delete community permanently" })
    .click();
  await expect(
    page.getByRole("button", { name: "Check deletion status" }),
  ).toBeVisible();
  await expect(
    page.getByText(/Correlation ID: mock-delete-correlation/),
  ).toBeVisible();
  await expect(archived).toBeVisible();
});

test("transient identity loss hides but retains an ambiguous envelope and restores it for the same owner", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    errorCode: "acceptance_unknown",
    identityResponseSequence: [
      { identity: { pubkey_hex: DEFAULT_MOCK_PUBKEY } },
      { error: { code: "unauthorized" } },
      { identity: { pubkey_hex: DEFAULT_MOCK_PUBKEY } },
    ],
  });
  await startArchivedDeletion(page);
  const requestId = await storedDeletionRequestId(page);
  expect(requestId).not.toBeNull();

  await page.getByRole("button", { name: "Refresh" }).click();
  await expect(page.getByText(requestId, { exact: true })).toHaveCount(0);
  await expect.poll(() => storedDeletionRequestId(page)).toBe(requestId);

  await page.getByRole("button", { name: "Refresh" }).click();
  await expect(page.getByText(requestId, { exact: true })).toBeVisible();
  await expect.poll(() => storedDeletionRequestId(page)).toBe(requestId);
});

test("a valid different bound owner discards the prior owner's envelope", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    errorCode: "acceptance_unknown",
    identityResponseSequence: [
      { identity: { pubkey_hex: DEFAULT_MOCK_PUBKEY } },
      { identity: { pubkey_hex: OTHER_HEX } },
    ],
  });
  await startArchivedDeletion(page);
  const requestId = await storedDeletionRequestId(page);

  await page.getByRole("button", { name: "Refresh" }).click();
  await expect(page.getByText(requestId, { exact: true })).toHaveCount(0);
  await expect.poll(() => storedDeletionRequestId(page)).toBeNull();
});

test("sign out hides but retains an ambiguous envelope for same-owner reauthentication", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    errorCode: "acceptance_unknown",
  });
  await startArchivedDeletion(page);
  const requestId = await storedDeletionRequestId(page);

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByText(requestId, { exact: true })).toHaveCount(0);
  await expect.poll(() => storedDeletionRequestId(page)).toBe(requestId);

  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByText(requestId, { exact: true })).toBeVisible();
  await expect.poll(() => storedDeletionRequestId(page)).toBe(requestId);
});

test("late A response cannot settle after a valid A-B-A owner transition", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    deferDeletion: true,
  });
  await startArchivedDeletion(page);
  await expect.poll(() => storedDeletionRequestId(page)).not.toBeNull();

  await page.keyboard.press("Escape");
  await expect(page.getByTestId("settings-view")).toHaveCount(0);
  await page.evaluate((pubkey) => {
    if (window.__BUZZ_E2E__?.mock) {
      window.__BUZZ_E2E__.mock.builderlabIdentity = { pubkey_hex: pubkey };
    }
  }, OTHER_HEX);
  await openSettings(page, "hosted-communities");
  await expect.poll(() => storedDeletionRequestId(page)).toBeNull();
  await expect(
    page.getByText("This account is connected to a different Buzz identity"),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("settings-view")).toHaveCount(0);
  await page.evaluate((pubkey) => {
    if (window.__BUZZ_E2E__?.mock) {
      window.__BUZZ_E2E__.mock.builderlabIdentity = { pubkey_hex: pubkey };
    }
  }, DEFAULT_MOCK_PUBKEY);
  await openSettings(page, "hosted-communities");
  await expect(
    page.getByText("This account is connected to a different Buzz identity"),
  ).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(() => window.__BUZZ_E2E_RELEASE_BUILDERLAB_DELETIONS__?.()),
    )
    .toBe(1);
  await expect(page.getByText("Deletion started", { exact: true })).toHaveCount(
    0,
  );
  await expect.poll(() => storedDeletionRequestId(page)).toBeNull();
});

test("one pending envelope blocks a second mounted deletion without overwriting or dispatching", async ({
  page,
}) => {
  const secondArchived = {
    id: "33333333-3333-4333-8333-333333333333",
    name: "Second archived team",
    normalized_host: "second.communities.buzz.xyz",
    archived_at: "2026-09-28T00:00:00Z",
  };
  await openDeletionFixture(page, {
    capability: true,
    errorCode: "acceptance_unknown",
    communities: [...DELETION_COMMUNITIES, secondArchived],
  });
  const first = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Archived team" })
    .filter({ hasNotText: "Second archived team" });
  await first.getByRole("button", { name: "Delete", exact: true }).click();
  await page
    .getByLabel(
      "Type the exact host to continue: Exact-Host.communities.buzz.xyz",
    )
    .fill("Exact-Host.communities.buzz.xyz");
  await page.getByRole("button", { name: "Continue" }).click();
  await page
    .getByRole("button", { name: "Delete community permanently" })
    .click();

  const firstRequestId = await page.evaluate(() => {
    const raw = window.localStorage.getItem(
      "buzz:hosted-community-delete-pending:v1",
    );
    return raw ? JSON.parse(raw).request_id : null;
  });
  expect(firstRequestId).not.toBeNull();

  const secondDelete = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Second archived team" })
    .getByRole("button", { name: "Delete", exact: true });
  await secondDelete.evaluate((button: HTMLButtonElement) => button.click());
  const secondHostInput = page.getByLabel(
    "Type the exact host to continue: second.communities.buzz.xyz",
  );
  if (await secondHostInput.isVisible().catch(() => false)) {
    await secondHostInput.fill("second.communities.buzz.xyz");
    await page.getByRole("button", { name: "Continue" }).click();
    await page
      .getByRole("button", { name: "Delete community permanently" })
      .click();
  }

  await expect(secondDelete).toBeDisabled();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__BUZZ_E2E_COMMANDS__?.filter(
            (command) => command === "delete_builderlab_community",
          ).length ?? 0,
      ),
    )
    .toBe(1);
  await expect
    .poll(() =>
      page.evaluate(() => {
        const raw = window.localStorage.getItem(
          "buzz:hosted-community-delete-pending:v1",
        );
        return raw ? JSON.parse(raw).request_id : null;
      }),
    )
    .toBe(firstRequestId);
});

test("native HTTP status wins over a contradictory body claim in the mounted flow", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    errorCode: "not_owner",
    httpStatusSequence: [503],
    bodyStatus: 404,
  });
  const archived = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Archived team" });
  const exactHost = "Exact-Host.communities.buzz.xyz";
  await archived.getByRole("button", { name: "Delete", exact: true }).click();
  await page
    .getByLabel(`Type the exact host to continue: ${exactHost}`)
    .fill(exactHost);
  await page.getByRole("button", { name: "Continue" }).click();
  await page
    .getByRole("button", { name: "Delete community permanently" })
    .click();

  const requestId = await page.evaluate(() => {
    const raw = window.localStorage.getItem(
      "buzz:hosted-community-delete-pending:v1",
    );
    return raw ? JSON.parse(raw).request_id : null;
  });
  expect(requestId).not.toBeNull();
  await expect(page.getByText(requestId, { exact: true })).toBeVisible();
  await expect(archived).toBeVisible();
});

test("ambiguous resubmit not_owner retains the same request UUID", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    errorSequence: [{ code: "acceptance_unknown" }, { code: "not_owner" }],
    capabilitySequence: [true, true, true],
    communitiesSequence: [DELETION_COMMUNITIES, DELETION_COMMUNITIES],
  });
  const archived = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Archived team" });
  const exactHost = "Exact-Host.communities.buzz.xyz";
  await archived.getByRole("button", { name: "Delete", exact: true }).click();
  await page
    .getByLabel(`Type the exact host to continue: ${exactHost}`)
    .fill(exactHost);
  await page.getByRole("button", { name: "Continue" }).click();
  await page
    .getByRole("button", { name: "Delete community permanently" })
    .click();
  const requestId = await page.evaluate(() => {
    const raw = window.localStorage.getItem(
      "buzz:hosted-community-delete-pending:v1",
    );
    return raw ? JSON.parse(raw).request_id : null;
  });
  await expect(page.getByText(requestId, { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Resubmit same request" }).click();
  await expect(page.getByText(/Only the community owner/)).toBeVisible();
  await expect(page.getByText(requestId, { exact: true })).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() => {
        const raw = window.localStorage.getItem(
          "buzz:hosted-community-delete-pending:v1",
        );
        return raw ? JSON.parse(raw).request_id : null;
      }),
    )
    .toBe(requestId);
});

test("resubmit rechecks capability after the fresh owner list", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    errorSequence: [{ code: "acceptance_unknown" }],
    capabilitySequence: [true, true, false],
    communitiesSequence: [DELETION_COMMUNITIES, DELETION_COMMUNITIES],
  });
  const exactHost = "Exact-Host.communities.buzz.xyz";
  await page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Archived team" })
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await page
    .getByLabel(`Type the exact host to continue: ${exactHost}`)
    .fill(exactHost);
  await page.getByRole("button", { name: "Continue" }).click();
  await page
    .getByRole("button", { name: "Delete community permanently" })
    .click();
  await page.getByRole("button", { name: "Resubmit same request" }).click();
  await expect(
    page.getByText(/Community deletion is no longer enabled/),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Resubmit same request" }),
  ).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__BUZZ_E2E_COMMANDS__?.filter(
            (command) => command === "delete_builderlab_community",
          ).length ?? 0,
      ),
    )
    .toBe(1);
});

test("resubmit fails closed when the fresh owner list misses", async ({
  page,
}) => {
  await openDeletionFixture(page, {
    capability: true,
    errorSequence: [{ code: "acceptance_unknown" }],
    capabilitySequence: [true, true, true],
    communitiesSequence: [DELETION_COMMUNITIES, []],
  });
  const exactHost = "Exact-Host.communities.buzz.xyz";
  await page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Archived team" })
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await page
    .getByLabel(`Type the exact host to continue: ${exactHost}`)
    .fill(exactHost);
  await page.getByRole("button", { name: "Continue" }).click();
  await page
    .getByRole("button", { name: "Delete community permanently" })
    .click();
  await page.getByRole("button", { name: "Resubmit same request" }).click();
  await expect(
    page.getByText(/not present in the fresh owner list/),
  ).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__BUZZ_E2E_COMMANDS__?.filter(
            (command) => command === "delete_builderlab_community",
          ).length ?? 0,
      ),
    )
    .toBe(1);
});

test("identity: mismatch rows follow pubkey_hex, never the hosted npub or raw hex", async ({
  page,
}) => {
  await openHostedCommunitiesSettings(page, {
    pubkey_hex: "f".repeat(64),
    npub: npubEncode(OTHER_HEX),
  });

  await expect(
    page.getByText("This account is connected to a different Buzz identity"),
  ).toBeVisible();
  const settingsView = page.getByTestId("settings-view");
  await expect(
    page
      .getByText("Account uses", { exact: true })
      .locator("xpath=following-sibling::dd[1]"),
  ).toHaveText(npubEncode("f".repeat(64)));
  await expect(
    page
      .getByText("This device", { exact: true })
      .locator("xpath=following-sibling::dd[1]"),
  ).toHaveText(npubEncode(DEFAULT_MOCK_PUBKEY));
  // The independently valid but contradictory hosted npub, and the raw hex
  // it would stand in for, must never render.
  await expect(settingsView.getByText(npubEncode(OTHER_HEX))).toHaveCount(0);
  await expect(settingsView.getByText("f".repeat(64))).toHaveCount(0);
});

test("identity: connected row follows pubkey_hex when the hosted npub encodes another key", async ({
  page,
}) => {
  await openHostedCommunitiesSettings(page, {
    pubkey_hex: DEFAULT_MOCK_PUBKEY,
    npub: npubEncode(OTHER_HEX),
  });

  const connectedNpub = page
    .getByText("Buzz identity connected")
    .locator("span.font-mono");
  await expect(connectedNpub).toHaveText(npubEncode(DEFAULT_MOCK_PUBKEY));
  await expect(
    page.getByTestId("settings-view").getByText(npubEncode(OTHER_HEX)),
  ).toHaveCount(0);
});

test("identity: unusable bound hex renders the neutral label, not the hosted npub or raw hex", async ({
  page,
}) => {
  await openHostedCommunitiesSettings(page, {
    // Valid hex alphabet, wrong length — unusable as an identity key.
    pubkey_hex: "f".repeat(63),
    npub: npubEncode(OTHER_HEX),
  });

  await expect(
    page.getByText("This account is connected to a different Buzz identity"),
  ).toBeVisible();
  const settingsView = page.getByTestId("settings-view");
  await expect(
    page
      .getByText("Account uses", { exact: true })
      .locator("xpath=following-sibling::dd[1]"),
  ).toHaveText("Unavailable");
  await expect(settingsView.getByText(npubEncode(OTHER_HEX))).toHaveCount(0);
  await expect(settingsView.getByText("f".repeat(63))).toHaveCount(0);
});

test("identity: consistent hosted identity renders its canonical npub", async ({
  page,
}) => {
  await openHostedCommunitiesSettings(page, {
    pubkey_hex: DEFAULT_MOCK_PUBKEY,
    npub: npubEncode(DEFAULT_MOCK_PUBKEY),
  });

  const connectedNpub = page
    .getByText("Buzz identity connected")
    .locator("span.font-mono");
  await expect(connectedNpub).toHaveText(npubEncode(DEFAULT_MOCK_PUBKEY));
});

test("identity: unlinked account offers linking, never a connected claim", async ({
  page,
}) => {
  await installMockBridge(page, {
    builderlabAuth: {
      email: "owner@example.com",
      expiresAt: "2099-01-01T00:00:00Z",
    },
    // No identity object at all: the account has not linked a Buzz key.
    builderlabIdentity: null,
    builderlabCommunities: [
      {
        id: "active-community",
        name: "E2E Test",
        normalized_host: "localhost:3000",
      },
    ],
  });
  await page.goto("/");
  await openSettings(page, "hosted-communities");

  await expect(
    page.getByText("Link this account to your Buzz identity"),
  ).toBeVisible();
  await expect(page.getByText("Buzz identity connected")).toHaveCount(0);
  // The seeded owned community still lists — every affordance that does
  // not act on the binding stays available — but Connect is an action on
  // the binding and cannot occur without a usable bound key: no row
  // affordance, and no onboarding it could start.
  await expect(page.getByTestId("hosted-community-row")).toHaveCount(1);
  await expect(
    page.getByRole("button", { name: "Connect", exact: true }),
  ).toHaveCount(0);
  // Creation stays unavailable: there is no authoritative key to bind the
  // new community to.
  await expect(page.getByLabel("Community address")).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "Create and connect", exact: true }),
  ).toBeDisabled();
});

test("identity: padded or mixed-case bound hex is the same key, not a mismatch", async ({
  page,
}) => {
  await openHostedCommunitiesSettings(page, {
    // The same key this device signs with, padded and uppercased. It
    // normalizes to the local key on both sides of the comparison — never
    // a mismatch demanding delete/rebind of an identity the device already
    // holds.
    pubkey_hex: `  ${DEFAULT_MOCK_PUBKEY.toUpperCase()}  `,
  });

  await expect(
    page.getByText("This account is connected to a different Buzz identity"),
  ).toHaveCount(0);
  const connectedNpub = page
    .getByText("Buzz identity connected")
    .locator("span.font-mono");
  await expect(connectedNpub).toHaveText(npubEncode(DEFAULT_MOCK_PUBKEY));
  // A binding that is the same key after normalization keeps its Connect
  // affordance — recovery is reserved for a binding that actually differs.
  await expect(
    page.getByRole("button", { name: "Connect", exact: true }).first(),
  ).toBeVisible();
});

/**
 * Identity payloads whose authoritative `pubkey_hex` cannot act as a key.
 * Each is an identity *object* — so presence alone must never read as a
 * connected, ready account — yet none carries a key the app can use.
 */
const UNUSABLE_BOUND_KEY_PAYLOADS: Array<
  [label: string, payload: { npub?: string; pubkey_hex?: string }]
> = [
  ["missing key", {}],
  ["non-hex key", { pubkey_hex: "zz".repeat(32) }],
  ["npub-only key", { npub: npubEncode(OTHER_HEX) }],
  // A checksum-valid npub stored in the hex field itself is not a hex
  // key: it must fail closed like any other unusable spelling, never
  // display as the account's authoritative key, and never read as a
  // usable binding when the local comparison is skipped.
  ["npub stored in the hex field", { pubkey_hex: npubEncode(OTHER_HEX) }],
];

for (const [label, payload] of UNUSABLE_BOUND_KEY_PAYLOADS) {
  test(`identity: ${label} is recovery, never a connected account or actions`, async ({
    page,
  }) => {
    await openHostedCommunitiesSettings(page, payload);

    const settingsView = page.getByTestId("settings-view");
    // No connected claim anywhere on the surface, despite the identity
    // object being present.
    await expect(page.getByText("Buzz identity connected")).toHaveCount(0);
    // The mismatch recovery block owns the identity panel instead.
    await expect(
      page.getByText("This account is connected to a different Buzz identity"),
    ).toBeVisible();
    await expect(
      page
        .getByText("Account uses", { exact: true })
        .locator("xpath=following-sibling::dd[1]"),
    ).toHaveText("Unavailable");
    // The unverified server-sent npub spelling never renders.
    await expect(settingsView.getByText(npubEncode(OTHER_HEX))).toHaveCount(0);
    // Connect stays unavailable for every owned community.
    await expect(
      page.getByRole("button", { name: "Connect", exact: true }),
    ).toHaveCount(0);
    // Creation stays unavailable: no key the new community would bind to.
    await expect(page.getByLabel("Community address")).toBeDisabled();
    await expect(
      page.getByRole("button", { name: "Create and connect", exact: true }),
    ).toBeDisabled();
  });
}

test("capture: community icon picker sits beside its hosted community", async ({
  page,
}) => {
  const activeRow = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "E2E Test" });
  const otherRow = page
    .getByTestId("hosted-community-row")
    .filter({ hasText: "Design studio" });

  await expect(activeRow.getByTestId("community-icon-settings")).toBeVisible();
  await expect(otherRow.getByTestId("community-icon-settings")).toHaveCount(0);

  const iconDataUrl = `data:image/svg+xml,${encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128"><rect width="128" height="128" rx="28" fill="#ff56c3"/><text x="64" y="80" text-anchor="middle" font-size="48">😅</text></svg>',
  )}`;
  await activeRow.getByLabel("Add community icon").click();

  const picker = page.getByRole("group", { name: "Community icon picker" });
  await expect(picker).toBeVisible();
  await expect(page.getByRole("tab", { name: "Image" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "Emoji" })).toBeVisible();
  await page.getByPlaceholder("Paste a URL").fill(iconDataUrl);
  await page.getByRole("button", { name: "Apply" }).click();

  const icon = activeRow.getByRole("img", { name: /community icon$/i });
  await expect(icon).toBeVisible();
  const maskImage = await activeRow
    .getByTestId("community-icon-mask")
    .evaluate((element) => getComputedStyle(element).webkitMaskImage);
  expect(maskImage).toContain("radial-gradient");
  await expect(page.getByTestId("community-icon-save")).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(() =>
        window.__BUZZ_E2E_SIGNED_EVENTS__?.some(
          (event) =>
            event.kind === 9033 &&
            event.tags.some(
              (tag) => tag[0] === "icon" && tag[1]?.startsWith("data:image/"),
            ),
        ),
      ),
    )
    .toBe(true);

  const iconBox = await activeRow
    .getByTestId("community-icon-settings")
    .boundingBox();
  const nameBox = await activeRow
    .getByText("E2E Test", { exact: true })
    .boundingBox();
  expect(iconBox).not.toBeNull();
  expect(nameBox).not.toBeNull();
  expect(iconBox?.x ?? Number.POSITIVE_INFINITY).toBeLessThan(
    nameBox?.x ?? Number.NEGATIVE_INFINITY,
  );

  await waitForAnimations(page);
  await activeRow.screenshot({
    path: `${OUTDIR}/01-community-icon-row.png`,
  });
});
