export const BUILDERLAB_BACKEND_ORIGIN = "https://app.builderlab.xyz";
export const PENDING_COMMUNITY_DELETION_KEY =
  "buzz:hosted-community-delete-pending:v1";

const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const PUBKEY = /^[0-9a-f]{64}$/;
const KEYS = [
  "acknowledgement_version",
  "backend_origin",
  "bound_owner_pubkey",
  "community_id",
  "host",
  "request_id",
] as const;

export type CommunityDeletionRequest = {
  community_id: string;
  host: string;
  request_id: string;
  acknowledgement_version: 1;
};

export type PendingCommunityDeletion = CommunityDeletionRequest & {
  bound_owner_pubkey: string;
  backend_origin: string;
};

type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function isPendingCommunityDeletion(
  value: unknown,
): value is PendingCommunityDeletion {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  if (
    Object.keys(record).length !== KEYS.length ||
    !KEYS.every((key) => Object.hasOwn(record, key))
  )
    return false;
  return (
    typeof record.community_id === "string" &&
    UUID.test(record.community_id) &&
    typeof record.host === "string" &&
    record.host.length > 0 &&
    record.host === record.host.trim() &&
    typeof record.request_id === "string" &&
    UUID.test(record.request_id) &&
    record.acknowledgement_version === 1 &&
    typeof record.bound_owner_pubkey === "string" &&
    PUBKEY.test(record.bound_owner_pubkey) &&
    typeof record.backend_origin === "string" &&
    record.backend_origin === BUILDERLAB_BACKEND_ORIGIN
  );
}

function defaultStorage(): StorageLike {
  return window.localStorage;
}

export function loadPendingCommunityDeletion(
  storage: StorageLike = defaultStorage(),
): PendingCommunityDeletion | null {
  try {
    const raw = storage.getItem(PENDING_COMMUNITY_DELETION_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (isPendingCommunityDeletion(parsed)) return parsed;
    storage.removeItem(PENDING_COMMUNITY_DELETION_KEY);
  } catch {
    try {
      storage.removeItem(PENDING_COMMUNITY_DELETION_KEY);
    } catch {
      // The caller still fails closed when storage itself is unavailable.
    }
  }
  return null;
}

export function persistPendingCommunityDeletion(
  envelope: PendingCommunityDeletion,
  storage: StorageLike = defaultStorage(),
): boolean {
  if (!isPendingCommunityDeletion(envelope)) return false;
  try {
    storage.setItem(PENDING_COMMUNITY_DELETION_KEY, JSON.stringify(envelope));
    return (
      storage.getItem(PENDING_COMMUNITY_DELETION_KEY) ===
      JSON.stringify(envelope)
    );
  } catch {
    return false;
  }
}

export function clearPendingCommunityDeletion(
  storage: StorageLike = defaultStorage(),
): void {
  try {
    storage.removeItem(PENDING_COMMUNITY_DELETION_KEY);
  } catch {
    // Clearing is best effort after a terminal server result.
  }
}

export function pendingCommunityDeletionMatchesAccount(
  envelope: PendingCommunityDeletion,
  ownerPubkey: string,
  backendOrigin: string,
): boolean {
  return (
    envelope.bound_owner_pubkey === ownerPubkey &&
    envelope.backend_origin === backendOrigin
  );
}

export function publicDeletionRequest(
  envelope: PendingCommunityDeletion,
): CommunityDeletionRequest {
  return {
    community_id: envelope.community_id,
    host: envelope.host,
    request_id: envelope.request_id,
    acknowledgement_version: envelope.acknowledgement_version,
  };
}
