import { invoke } from "@tauri-apps/api/core";

import { safeNpub } from "@/shared/lib/nostrUtils";

export const HOSTED_COMMUNITY_SUFFIX = "communities.buzz.xyz";
export const HOSTED_COMMUNITY_LIMIT = 5;
export const VALID_HOSTED_COMMUNITY_NAME = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

export type BuilderlabAuth = {
  email?: string;
  name?: string;
  expiresAt: string;
  canDeleteBuzzCommunities?: boolean;
};

export type HostedCommunityApiError = {
  code?: string;
  message?: string;
  setup_needed?: boolean;
};

export type HostedNostrIdentity = {
  npub?: string;
  pubkey_hex?: string;
};

export type HostedIdentityResponse = {
  identity?: HostedNostrIdentity;
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunity = {
  id?: string;
  name?: string;
  slug?: string;
  normalized_host?: string;
  owner_pubkey?: string;
  archived_at?: string | null;
};

export type HostedCommunitiesResponse = {
  communities?: HostedCommunity[];
  quota_used?: number;
  quota_limit?: number;
  can_create?: boolean;
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunityDeletionResponse = {
  request_id?: string;
  community_id?: string;
  host?: string;
  acknowledgement_version?: number;
  status?: string;
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunityAvailabilityResponse = {
  available?: boolean;
  normalized_host?: string;
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunityMutationResponse = {
  community?: HostedCommunity;
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunityAccount = {
  communities: HostedCommunity[];
  identity: HostedNostrIdentity | null;
  quotaUsed: number | null;
  quotaLimit: number | null;
  canCreate: boolean;
};

export function hostedCommunityErrorMessage(
  error: HostedCommunityApiError | undefined,
  correlationId: string | undefined,
  fallback: string,
) {
  const messages: Record<string, string> = {
    missing_mapping: "Connect your Buzz identity before creating a community.",
    invalid_name: "Use lowercase letters, numbers, and hyphens.",
    taken: "That Buzz address is already taken.",
    limit_reached: `You've reached the limit of ${HOSTED_COMMUNITY_LIMIT} hosted communities.`,
    relay_unavailable: "Community provisioning is temporarily unavailable.",
    identity_already_bound:
      "This Builderlab account is connected to another Buzz identity.",
    pubkey_already_bound:
      "This Buzz identity is connected to another Builderlab account.",
    not_owner: "Only the community owner can do that.",
    transferee_not_registered:
      "That person needs a connected Buzz identity before you can transfer ownership to them.",
    confirmation_mismatch: "The host acknowledgement did not match exactly.",
    must_archive: "Archive this community before deleting it.",
    protected_target: "This community cannot be deleted.",
    deletion_conflict:
      "This community already has a conflicting deletion request.",
    unsupported_acknowledgement_version:
      "This app version cannot confirm the current deletion terms.",
    deletion_aborted: "The deletion request was stopped by an operator.",
    acceptance_unknown:
      "Deletion acceptance is uncertain. Check deletion status; do not start a new request.",
    unknown: "The deletion service returned an invalid response.",
  };
  const message = messages[error?.code ?? ""] ?? error?.message ?? fallback;
  return correlationId
    ? `${message} Correlation ID: ${correlationId}`
    : message;
}

export function hostedCommunityRelayUrl(community: HostedCommunity) {
  const host = community.normalized_host?.trim();
  return host ? `wss://${host.replace(/^wss?:\/\//, "")}` : null;
}

const BOUND_KEY_HEX_REGEX = /^[0-9a-f]{64}$/;

/**
 * The ONE operational form of a hosted identity's bound key: the trimmed,
 * lowercased `pubkey_hex` when that field carries a 64-character hex key,
 * null otherwise.
 *
 * `pubkey_hex` is the authoritative field the mismatch gate and every
 * hosted-community operation act on, so its accepted shape is defined here
 * once and is hex-only. An `npub` spelling belongs to the server's separate
 * display field: one stored in the hex field is not a hex key and fails
 * closed here instead of widening the authorization domain beyond what
 * comparisons see. Whitespace-padded or mixed-case hex normalizes to the
 * identical value on both sides of every comparison, so a key that is the
 * same after normalization never reads as a mismatch demanding a
 * delete/rebind of an identity the device already holds. Consumers
 * normalize the local key through this same function before comparing.
 */
export function normalizedBoundKeyHex(
  value: string | null | undefined,
): string | null {
  if (!value) return null;
  const normalized = value.trim().toLowerCase();
  return BOUND_KEY_HEX_REGEX.test(normalized) ? normalized : null;
}

/**
 * The canonical npub of the hosted identity's authoritative bound key, or
 * null when the payload carries no key the app can act on.
 *
 * `pubkey_hex` is the field the mismatch gate and every hosted-community
 * operation act on, and it is only usable when it normalizes to a hex key
 * (`normalizedBoundKeyHex`) — the server-sent `npub` is an independent,
 * unverified spelling and is never accepted in its place. The returned npub
 * is derived from that same normalized key, so the displayed account
 * identity can never disagree with the binding the decisions act on. Hosted
 * surfaces derive their connected/readiness/create/connect gates from the
 * normalized key instead of identity-object presence, so an identity payload
 * without a usable authoritative key (missing, empty, invalid/non-hex,
 * wrong-length, npub-only, or an npub stored in the hex field) fails closed
 * into recovery rather than presenting the account as connected.
 */
export function usableBoundIdentityNpub(
  identity: HostedNostrIdentity | null | undefined,
): string | null {
  const hex = normalizedBoundKeyHex(identity?.pubkey_hex);
  return hex === null ? null : safeNpub(hex);
}

export function getBuilderlabAuth() {
  return invoke<BuilderlabAuth | null>("get_builderlab_auth");
}

export function cancelBuilderlabLogin() {
  return invoke<void>("cancel_builderlab_login");
}

export function clearBuilderlabAuth() {
  return invoke<void>("clear_builderlab_auth");
}

export function startBuilderlabLogin() {
  return invoke<BuilderlabAuth>("start_builderlab_login");
}

export async function loadHostedCommunityAccount(): Promise<HostedCommunityAccount> {
  const [identityResponse, communitiesResponse] = await Promise.all([
    invoke<HostedIdentityResponse>("get_builderlab_nostr_identity"),
    invoke<HostedCommunitiesResponse>("list_builderlab_communities"),
  ]);
  if (
    identityResponse.error &&
    identityResponse.error.code !== "unauthorized" &&
    !identityResponse.error.setup_needed
  ) {
    throw new Error(
      hostedCommunityErrorMessage(
        identityResponse.error,
        identityResponse.correlation_id,
        "Could not load the connected Buzz identity.",
      ),
    );
  }
  if (communitiesResponse.error && !communitiesResponse.error.setup_needed) {
    throw new Error(
      hostedCommunityErrorMessage(
        communitiesResponse.error,
        communitiesResponse.correlation_id,
        "Could not load communities.",
      ),
    );
  }
  return {
    identity: identityResponse.identity ?? null,
    communities: communitiesResponse.communities ?? [],
    quotaUsed: Number.isInteger(communitiesResponse.quota_used)
      ? (communitiesResponse.quota_used as number)
      : null,
    quotaLimit: Number.isInteger(communitiesResponse.quota_limit)
      ? (communitiesResponse.quota_limit as number)
      : null,
    canCreate:
      Number.isInteger(communitiesResponse.quota_used) &&
      Number.isInteger(communitiesResponse.quota_limit) &&
      communitiesResponse.can_create === true,
  };
}

export function bindBuilderlabIdentity() {
  return invoke<HostedIdentityResponse>("bind_builderlab_nostr_identity");
}

export function deleteBuilderlabIdentity() {
  return invoke<HostedIdentityResponse>("delete_builderlab_nostr_identity");
}

export function checkHostedCommunityName(name: string) {
  return invoke<HostedCommunityAvailabilityResponse>(
    "check_builderlab_community_name",
    { name },
  );
}

export function createHostedCommunity(name: string) {
  return invoke<HostedCommunityMutationResponse>(
    "create_builderlab_community",
    {
      name,
    },
  );
}
