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

export type CommunityDeletionAttempt = "initial" | "receipt" | "resubmit";

export type CommunityDeletionResponseLike = {
  request_id?: string;
  community_id?: string;
  host?: string;
  acknowledgement_version?: number;
  status?: string;
  error?: { code?: string };
  correlation_id?: string;
};

export type CommunityDeletionTransport = {
  http_status?: number;
  body?: CommunityDeletionResponseLike;
};

export type CommunityDeletionDisposition =
  | "accept"
  | "abort"
  | "clear"
  | "retain";

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
    // This key is the one-envelope admission boundary. Never replace an
    // existing valid envelope, even if another mounted view has stale React
    // state. loadPendingCommunityDeletion removes malformed values first; a
    // value that remains (or cannot be removed) fails closed.
    if (loadPendingCommunityDeletion(storage) !== null) return false;
    if (storage.getItem(PENDING_COMMUNITY_DELETION_KEY) !== null) return false;
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

function responseMatchesDeletionTuple(
  response: CommunityDeletionResponseLike,
  envelope: PendingCommunityDeletion,
): boolean {
  return (
    response.request_id === envelope.request_id &&
    response.community_id === envelope.community_id &&
    response.host === envelope.host &&
    response.acknowledgement_version === envelope.acknowledgement_version
  );
}

/**
 * Decide whether one server result can terminate a persisted deletion intent.
 * Once dispatch is ambiguous, ordinary receipt/resubmit errors retain the same
 * UUID; only a tuple-bound acceptance or abort is terminal.
 */
export function deletionResponseDisposition(
  transport: CommunityDeletionTransport,
  envelope: PendingCommunityDeletion,
  attempt: CommunityDeletionAttempt,
): CommunityDeletionDisposition {
  const response = transport.body ?? {};
  const httpStatus = transport.http_status;
  const tupleMatches = responseMatchesDeletionTuple(response, envelope);
  if (httpStatus === 202 && response.status === "accepted" && tupleMatches) {
    return "accept";
  }
  if (
    httpStatus === 409 &&
    response.error?.code === "deletion_aborted" &&
    tupleMatches
  ) {
    return "abort";
  }
  if (attempt !== "initial" || !response.error?.code) return "retain";

  // This exact status/code map is the established cross-client contract. It
  // narrows terminal fresh-admission failures but does not prove an intermediary
  // could not synthesize a matching pair; the remaining trust is the native
  // authenticated Builderlab boundary, never a status claimed by the body.
  const terminalFreshAdmissionErrors: Readonly<Record<string, number>> = {
    missing_mapping: 400,
    invalid_request: 400,
    confirmation_mismatch: 400,
    unsupported_acknowledgement_version: 400,
    not_owner: 404,
    must_archive: 409,
    protected_target: 409,
    deletion_conflict: 409,
  };
  return terminalFreshAdmissionErrors[response.error.code] === httpStatus
    ? "clear"
    : "retain";
}
