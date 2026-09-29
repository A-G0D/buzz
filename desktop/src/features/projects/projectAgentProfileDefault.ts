import { normalizeRelayUrl } from "@/shared/lib/normalizeRelayUrl";
import { setLocalStorageItemWithRecovery } from "@/shared/lib/localStorageQuota";
import type { CreateChannelManagedAgentInput } from "@/features/agents/channelAgents";

const STORAGE_KEY_PREFIX = "buzz-project-agent-profile.v1";
const RESOURCE_DEFAULTS_KEY_PREFIX = "buzz-project-agent-resource-defaults.v1";
const ROUTE_PROFILE_DEFAULT_KEY_PREFIX = "buzz-project-agent-route-profile.v1";
const PROFILE_ID_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const CHANNEL_ID_PATTERN = /^[0-9a-f-]{36}$/i;
const PUBKEY_PATTERN = /^[0-9a-f]{64}$/i;
export const BUZZ_AGENT_ROUTE_PROFILE_ID_ENV = "BUZZ_AGENT_ROUTE_PROFILE_ID";

function storageKey(relayUrl: string, ownerPubkey: string, channelId: string) {
  return `${STORAGE_KEY_PREFIX}:${normalizeRelayUrl(relayUrl)}:${ownerPubkey.toLowerCase()}:${channelId.toLowerCase()}`;
}

function resourceDefaultsStorageKey(
  relayUrl: string,
  ownerPubkey: string,
  channelId: string,
) {
  return `${RESOURCE_DEFAULTS_KEY_PREFIX}:${normalizeRelayUrl(relayUrl)}:${ownerPubkey.toLowerCase()}:${channelId.toLowerCase()}`;
}

function routeProfileDefaultStorageKey(
  relayUrl: string,
  ownerPubkey: string,
  channelId: string,
) {
  return `${ROUTE_PROFILE_DEFAULT_KEY_PREFIX}:${normalizeRelayUrl(relayUrl)}:${ownerPubkey.toLowerCase()}:${channelId.toLowerCase()}`;
}

function validScope(relayUrl: string, ownerPubkey: string, channelId: string) {
  return Boolean(
    relayUrl &&
      PUBKEY_PATTERN.test(ownerPubkey) &&
      CHANNEL_ID_PATTERN.test(channelId),
  );
}

export function readProjectAgentProfileDefault(
  relayUrl: string,
  ownerPubkey: string,
  channelId: string,
): string | null {
  if (!validScope(relayUrl, ownerPubkey, channelId)) return null;
  try {
    const raw = window.localStorage.getItem(
      storageKey(relayUrl, ownerPubkey, channelId),
    );
    if (!raw) return null;
    const record = JSON.parse(raw) as {
      version?: unknown;
      profileId?: unknown;
    };
    return record.version === 1 &&
      typeof record.profileId === "string" &&
      PROFILE_ID_PATTERN.test(record.profileId)
      ? record.profileId
      : null;
  } catch {
    return null;
  }
}

export function writeProjectAgentProfileDefault(
  relayUrl: string,
  ownerPubkey: string,
  channelId: string,
  profileId: string | null,
): boolean {
  if (!validScope(relayUrl, ownerPubkey, channelId)) return false;
  try {
    const key = storageKey(relayUrl, ownerPubkey, channelId);
    if (profileId === null) {
      window.localStorage.removeItem(key);
      return true;
    }
    if (!PROFILE_ID_PATTERN.test(profileId)) return false;
    return setLocalStorageItemWithRecovery(
      key,
      JSON.stringify({ version: 1, profileId }),
    );
  } catch {
    return false;
  }
}

export function readProjectAgentRouteProfileDefault(
  relayUrl: string,
  ownerPubkey: string,
  channelId: string,
): string | null {
  if (!validScope(relayUrl, ownerPubkey, channelId)) return null;
  try {
    const raw = window.localStorage.getItem(
      routeProfileDefaultStorageKey(relayUrl, ownerPubkey, channelId),
    );
    if (!raw) return null;
    const record = JSON.parse(raw) as {
      version?: unknown;
      profileId?: unknown;
    };
    return record.version === 1 &&
      typeof record.profileId === "string" &&
      PROFILE_ID_PATTERN.test(record.profileId)
      ? record.profileId
      : null;
  } catch {
    return null;
  }
}

export function writeProjectAgentRouteProfileDefault(
  relayUrl: string,
  ownerPubkey: string,
  channelId: string,
  profileId: string | null,
): boolean {
  if (!validScope(relayUrl, ownerPubkey, channelId)) return false;
  try {
    const key = routeProfileDefaultStorageKey(relayUrl, ownerPubkey, channelId);
    if (profileId === null) {
      window.localStorage.removeItem(key);
      return true;
    }
    if (!PROFILE_ID_PATTERN.test(profileId)) return false;
    return setLocalStorageItemWithRecovery(
      key,
      JSON.stringify({ version: 1, profileId }),
    );
  } catch {
    return false;
  }
}

export type ProjectAgentResourceDefaults = {
  parallelism?: number;
  idleTimeoutSeconds?: number;
  maxTurnDurationSeconds?: number;
};

export type ProjectAgentResourceDefaultsDraft = Record<
  keyof ProjectAgentResourceDefaults,
  string
>;

export function toProjectAgentResourceDefaultsDraft(
  defaults: ProjectAgentResourceDefaults | null,
): ProjectAgentResourceDefaultsDraft {
  return {
    parallelism: defaults?.parallelism?.toString() ?? "",
    idleTimeoutSeconds: defaults?.idleTimeoutSeconds?.toString() ?? "",
    maxTurnDurationSeconds: defaults?.maxTurnDurationSeconds?.toString() ?? "",
  };
}

export function parseProjectAgentResourceDefaultsDraft(
  draft: ProjectAgentResourceDefaultsDraft,
): ProjectAgentResourceDefaults | null {
  const values = Object.entries(draft).flatMap(([key, raw]) => {
    if (!raw.trim()) return [];
    const value = Number(raw);
    if (!Number.isSafeInteger(value) || value < 1) return [[key, Number.NaN]];
    return [[key, value]];
  });
  const result = Object.fromEntries(values) as ProjectAgentResourceDefaults;
  if (Object.values(result).some((value) => Number.isNaN(value))) return null;
  if (result.parallelism !== undefined && result.parallelism > 32) return null;
  return result;
}

function validResourceDefaults(
  value: unknown,
): value is ProjectAgentResourceDefaults {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const defaults = value as Record<string, unknown>;
  const keys = ["parallelism", "idleTimeoutSeconds", "maxTurnDurationSeconds"];
  if (Object.keys(defaults).some((key) => !keys.includes(key))) return false;
  if (Object.keys(defaults).length === 0) return false;
  return keys.every((key) => {
    const number = defaults[key];
    if (number === undefined) return true;
    if (typeof number !== "number" || !Number.isSafeInteger(number))
      return false;
    return key === "parallelism" ? number >= 1 && number <= 32 : number >= 1;
  });
}

export function readProjectAgentResourceDefaults(
  relayUrl: string,
  ownerPubkey: string,
  channelId: string,
): ProjectAgentResourceDefaults | null {
  if (!validScope(relayUrl, ownerPubkey, channelId)) return null;
  try {
    const raw = window.localStorage.getItem(
      resourceDefaultsStorageKey(relayUrl, ownerPubkey, channelId),
    );
    if (!raw) return null;
    const record = JSON.parse(raw) as { version?: unknown; defaults?: unknown };
    return record.version === 1 && validResourceDefaults(record.defaults)
      ? record.defaults
      : null;
  } catch {
    return null;
  }
}

export function writeProjectAgentResourceDefaults(
  relayUrl: string,
  ownerPubkey: string,
  channelId: string,
  defaults: ProjectAgentResourceDefaults | null,
): boolean {
  if (!validScope(relayUrl, ownerPubkey, channelId)) return false;
  try {
    const key = resourceDefaultsStorageKey(relayUrl, ownerPubkey, channelId);
    if (defaults === null) {
      window.localStorage.removeItem(key);
      return true;
    }
    if (!validResourceDefaults(defaults)) return false;
    return setLocalStorageItemWithRecovery(
      key,
      JSON.stringify({ version: 1, defaults }),
    );
  } catch {
    return false;
  }
}

export function applyProjectAgentProfileDefault(
  inputs: readonly CreateChannelManagedAgentInput[],
  defaultProfileId: string | null,
): CreateChannelManagedAgentInput[] {
  return inputs.map((input) => {
    const executionProfileId = input.executionProfileId || defaultProfileId;
    return executionProfileId
      ? { ...input, executionProfileId, forceNewInstance: true }
      : input;
  });
}

export function applyProjectAgentResourceDefaults(
  inputs: readonly CreateChannelManagedAgentInput[],
  defaults: ProjectAgentResourceDefaults | null,
): CreateChannelManagedAgentInput[] {
  if (!defaults) return [...inputs];
  return inputs.map((input) => {
    const next = { ...input };
    let applied = false;
    if (next.parallelism === undefined && defaults.parallelism !== undefined) {
      next.parallelism = defaults.parallelism;
      applied = true;
    }
    if (
      next.idleTimeoutSeconds === undefined &&
      defaults.idleTimeoutSeconds !== undefined
    ) {
      next.idleTimeoutSeconds = defaults.idleTimeoutSeconds;
      applied = true;
    }
    if (
      next.maxTurnDurationSeconds === undefined &&
      defaults.maxTurnDurationSeconds !== undefined
    ) {
      next.maxTurnDurationSeconds = defaults.maxTurnDurationSeconds;
      applied = true;
    }
    if (applied) next.forceNewInstance = true;
    return next;
  });
}

function supportsLocalBuzzAgentRouteProfile(
  input: Pick<CreateChannelManagedAgentInput, "runtime" | "backend">,
) {
  return (
    input.runtime.id === "buzz-agent" && input.backend?.type !== "provider"
  );
}

export function applyProjectAgentRouteProfileDefault(
  inputs: readonly CreateChannelManagedAgentInput[],
  defaultProfileId: string | null,
): CreateChannelManagedAgentInput[] {
  if (!defaultProfileId || !PROFILE_ID_PATTERN.test(defaultProfileId)) {
    return [...inputs];
  }
  return inputs.map((input) => {
    if (!supportsLocalBuzzAgentRouteProfile(input)) {
      return input;
    }
    if (input.routeProfileId) {
      return input.forceNewInstance
        ? input
        : { ...input, forceNewInstance: true };
    }
    return {
      ...input,
      routeProfileId: defaultProfileId,
      forceNewInstance: true,
    };
  });
}

export function projectAgentRouteProfileEnvVars(
  input: Pick<
    CreateChannelManagedAgentInput,
    "runtime" | "backend" | "routeProfileId"
  >,
): Record<string, string> | undefined {
  const profileId = input.routeProfileId;
  if (
    !supportsLocalBuzzAgentRouteProfile(input) ||
    !profileId ||
    !PROFILE_ID_PATTERN.test(profileId)
  ) {
    return undefined;
  }
  return { [BUZZ_AGENT_ROUTE_PROFILE_ID_ENV]: profileId };
}

export async function applyStoredProjectAgentRouteProfileDefault(
  scope: { relayUrl: string; ownerPubkey: string; channelId: string },
  inputs: readonly CreateChannelManagedAgentInput[],
  listProfiles: () => Promise<readonly { id: string }[]>,
): Promise<CreateChannelManagedAgentInput[]> {
  const defaultProfileId = readProjectAgentRouteProfileDefault(
    scope.relayUrl,
    scope.ownerPubkey,
    scope.channelId,
  );
  if (
    !defaultProfileId ||
    !inputs.some(
      (input) =>
        supportsLocalBuzzAgentRouteProfile(input) && !input.routeProfileId,
    )
  ) {
    return [...inputs];
  }

  const catalog = await listProfiles();
  if (!catalog.some((profile) => profile.id === defaultProfileId)) {
    writeProjectAgentRouteProfileDefault(
      scope.relayUrl,
      scope.ownerPubkey,
      scope.channelId,
      null,
    );
    return [...inputs];
  }
  return applyProjectAgentRouteProfileDefault(inputs, defaultProfileId);
}

export async function applyStoredProjectAgentProfileDefault(
  scope: { relayUrl: string; ownerPubkey: string; channelId: string },
  inputs: readonly CreateChannelManagedAgentInput[],
  listProfiles: () => Promise<readonly { id: string }[]>,
): Promise<CreateChannelManagedAgentInput[]> {
  const resourceDefaults = readProjectAgentResourceDefaults(
    scope.relayUrl,
    scope.ownerPubkey,
    scope.channelId,
  );
  const defaultProfileId = readProjectAgentProfileDefault(
    scope.relayUrl,
    scope.ownerPubkey,
    scope.channelId,
  );
  if (!defaultProfileId) {
    return applyProjectAgentResourceDefaults(
      applyProjectAgentProfileDefault(inputs, null),
      resourceDefaults,
    );
  }

  const catalog = await listProfiles();
  if (!catalog.some((profile) => profile.id === defaultProfileId)) {
    writeProjectAgentProfileDefault(
      scope.relayUrl,
      scope.ownerPubkey,
      scope.channelId,
      null,
    );
    return applyProjectAgentResourceDefaults(
      applyProjectAgentProfileDefault(inputs, null),
      resourceDefaults,
    );
  }
  return applyProjectAgentResourceDefaults(
    applyProjectAgentProfileDefault(inputs, defaultProfileId),
    resourceDefaults,
  );
}
