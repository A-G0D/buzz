import assert from "node:assert/strict";
import test from "node:test";

import {
  applyLegacyCommunityStorage,
  migrateLegacyCommunityStorageBeforeRender,
} from "./legacyCommunityStorage.ts";
import { migrateLegacyCommunityStorage } from "./communityStorage.ts";

function createMemoryStorage(initial = {}) {
  const values = new Map(Object.entries(initial));
  return {
    getItem(key) {
      return values.has(key) ? values.get(key) : null;
    },
    setItem(key, value) {
      values.set(key, String(value));
    },
    removeItem(key) {
      values.delete(key);
    },
    clear() {
      values.clear();
    },
    key(index) {
      return Array.from(values.keys())[index] ?? null;
    },
    get length() {
      return values.size;
    },
  };
}

const legacyCommunities = JSON.stringify([
  {
    id: "legacy-community",
    name: "Existing relay",
    relayUrl: "wss://relay.example.com",
    addedAt: "2026-06-12T00:00:00.000Z",
  },
]);

const currentCommunities = JSON.stringify([
  {
    id: "current-community",
    name: "Current relay",
    relayUrl: "wss://current.example.com",
    addedAt: "2026-06-12T00:00:00.000Z",
  },
]);

const localhostCommunities = JSON.stringify([
  {
    id: "local-community",
    name: "Local Dev",
    relayUrl: "ws://localhost:3000",
    addedAt: "2026-06-12T00:00:00.000Z",
  },
]);

test("applyLegacyCommunityStorage seeds missing communities and active community", () => {
  const storage = createMemoryStorage();

  applyLegacyCommunityStorage(
    {
      workspaces: legacyCommunities,
      activeWorkspaceId: "legacy-community",
      onboardingCompletions: [],
    },
    storage,
  );

  assert.equal(storage.getItem("buzz-communities"), legacyCommunities);
  assert.equal(storage.getItem("buzz-active-community-id"), "legacy-community");
});

test("applyLegacyCommunityStorage preserves existing non-local Buzz communities", () => {
  const storage = createMemoryStorage({
    "buzz-communities": currentCommunities,
    "buzz-active-community-id": "current-community",
  });

  applyLegacyCommunityStorage(
    {
      workspaces: legacyCommunities,
      activeWorkspaceId: "legacy-community",
      onboardingCompletions: [],
    },
    storage,
  );

  assert.equal(storage.getItem("buzz-communities"), currentCommunities);
  assert.equal(
    storage.getItem("buzz-active-community-id"),
    "current-community",
  );
});

test("applyLegacyCommunityStorage replaces broken localhost first-run community", () => {
  const storage = createMemoryStorage({
    "buzz-communities": localhostCommunities,
    "buzz-active-community-id": "local-community",
  });

  applyLegacyCommunityStorage(
    {
      workspaces: legacyCommunities,
      activeWorkspaceId: "legacy-community",
      onboardingCompletions: [],
    },
    storage,
  );

  assert.equal(storage.getItem("buzz-communities"), legacyCommunities);
  assert.equal(storage.getItem("buzz-active-community-id"), "legacy-community");
});

test("applyLegacyCommunityStorage treats trailing-slash localhost as broken", () => {
  const storage = createMemoryStorage({
    "buzz-communities": JSON.stringify([
      {
        id: "local-community",
        name: "Local Dev",
        relayUrl: "ws://localhost:3000/",
        addedAt: "2026-06-12T00:00:00.000Z",
      },
    ]),
    "buzz-active-community-id": "local-community",
  });

  applyLegacyCommunityStorage(
    {
      workspaces: legacyCommunities,
      activeWorkspaceId: "legacy-community",
      onboardingCompletions: [],
    },
    storage,
  );

  assert.equal(storage.getItem("buzz-communities"), legacyCommunities);
  assert.equal(storage.getItem("buzz-active-community-id"), "legacy-community");
});

test("applyLegacyCommunityStorage migrates onboarding completion keys", () => {
  const storage = createMemoryStorage();

  applyLegacyCommunityStorage(
    {
      workspaces: null,
      activeWorkspaceId: null,
      onboardingCompletions: [{ pubkey: "abc123", value: "true" }],
    },
    storage,
  );

  assert.equal(storage.getItem("buzz-onboarding-complete.v1:abc123"), "true");
});

test("migrateLegacyCommunityStorage tolerates a denied localStorage getter", () => {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: Object.defineProperty({}, "localStorage", {
      get() {
        throw new DOMException("Storage access denied", "SecurityError");
      },
    }),
  });

  try {
    assert.doesNotThrow(() => migrateLegacyCommunityStorage());
  } finally {
    if (originalWindow) {
      Object.defineProperty(globalThis, "window", originalWindow);
    } else {
      Reflect.deleteProperty(globalThis, "window");
    }
  }
});

test("pre-render community migration resolves when localStorage is denied", async () => {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const originalWarn = console.warn;
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: Object.defineProperty({}, "localStorage", {
      get() {
        throw new DOMException("Storage access denied", "SecurityError");
      },
    }),
  });
  console.warn = () => {};

  try {
    await assert.doesNotReject(migrateLegacyCommunityStorageBeforeRender());
  } finally {
    console.warn = originalWarn;
    if (originalWindow) {
      Object.defineProperty(globalThis, "window", originalWindow);
    } else {
      Reflect.deleteProperty(globalThis, "window");
    }
  }
});

test("pre-render community migration continues when the legacy IPC read stalls", async () => {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const originalWarn = console.warn;
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { localStorage: createMemoryStorage() },
  });
  console.warn = () => {};

  try {
    await assert.doesNotReject(
      migrateLegacyCommunityStorageBeforeRender(() => new Promise(() => {}), 1),
    );
  } finally {
    console.warn = originalWarn;
    if (originalWindow) {
      Object.defineProperty(globalThis, "window", originalWindow);
    } else {
      Reflect.deleteProperty(globalThis, "window");
    }
  }
});

test("pre-render community migration applies a legacy IPC result before its deadline", async () => {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const storage = createMemoryStorage();
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { localStorage: storage },
  });

  try {
    await migrateLegacyCommunityStorageBeforeRender(
      async () => ({
        workspaces: legacyCommunities,
        activeWorkspaceId: "legacy-community",
        onboardingCompletions: [],
      }),
      1_000,
    );
    assert.equal(storage.getItem("buzz-communities"), legacyCommunities);
    assert.equal(
      storage.getItem("buzz-active-community-id"),
      "legacy-community",
    );
  } finally {
    if (originalWindow) {
      Object.defineProperty(globalThis, "window", originalWindow);
    } else {
      Reflect.deleteProperty(globalThis, "window");
    }
  }
});
