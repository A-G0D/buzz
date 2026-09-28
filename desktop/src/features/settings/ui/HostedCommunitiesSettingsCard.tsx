import * as React from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertCircle,
  CheckCircle2,
  ExternalLink,
  LoaderCircle,
  LogOut,
  RefreshCw,
  Unlink,
} from "lucide-react";

import { useIdentityQuery } from "@/shared/api/hooks";
import {
  HOSTED_COMMUNITY_SUFFIX as HOST_SUFFIX,
  hostedCommunityErrorMessage as errorMessage,
  hostedCommunityRelayUrl as relayUrl,
  normalizedBoundKeyHex,
  usableBoundIdentityNpub,
  type BuilderlabAuth,
  type HostedCommunityAvailabilityResponse as AvailabilityResponse,
  type HostedCommunitiesResponse as CommunitiesResponse,
  type HostedCommunity,
  type HostedCommunityDeletionResponse,
  type HostedCommunityMutationResponse as CommunityMutationResponse,
  type HostedIdentityResponse as IdentityResponse,
  type HostedNostrIdentity as NostrIdentity,
  VALID_HOSTED_COMMUNITY_NAME as VALID_NAME,
} from "@/features/communities/hostedCommunityApi";
import {
  BUILDERLAB_BACKEND_ORIGIN,
  clearPendingCommunityDeletion,
  deletionResponseDisposition,
  loadPendingCommunityDeletion,
  pendingCommunityDeletionMatchesAccount,
  persistPendingCommunityDeletion,
  publicDeletionRequest,
  type CommunityDeletionAttempt,
  type PendingCommunityDeletion,
} from "@/features/communities/communityDeletionPending";
import { useCommunities } from "@/features/communities/useCommunities";
import { useCommunityOnboarding } from "@/features/onboarding/communityOnboarding";
import { safeNpub } from "@/shared/lib/nostrUtils";
import { UNAVAILABLE_KEY_LABEL } from "@/shared/lib/pubkey";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/shared/ui/alert-dialog";
import { Button, buttonVariants } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { SettingsSectionHeader } from "./SettingsSectionHeader";
import { HostedCommunityRow } from "./HostedCommunityRow";

function relayHost(url: string | null | undefined) {
  if (!url) return null;
  try {
    return new URL(url).host.toLowerCase();
  } catch {
    return null;
  }
}

type LoadedHostedAccount = {
  communities: HostedCommunity[];
  owner: string | null;
};

export function HostedCommunitiesSettingsCard() {
  const onboarding = useCommunityOnboarding();
  const { activeCommunity } = useCommunities();
  const localPubkey = useIdentityQuery().data?.pubkey ?? null;
  const [auth, setAuth] = React.useState<BuilderlabAuth | null>(null);
  const [communities, setCommunities] = React.useState<HostedCommunity[]>([]);
  const [identity, setIdentity] = React.useState<NostrIdentity | null>(null);
  const [name, setName] = React.useState("");
  const [availability, setAvailability] = React.useState<boolean | null>(null);
  const [checkingName, setCheckingName] = React.useState(false);
  const [loading, setLoading] = React.useState(true);
  const [action, setAction] = React.useState<string | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [statusMessage, setStatusMessage] = React.useState<string | null>(null);
  const [quota, setQuota] = React.useState<{
    used: number;
    limit: number;
    canCreate: boolean;
  } | null>(null);
  const [pendingDeletion, setPendingDeletion] =
    React.useState<PendingCommunityDeletion | null>(null);
  const hiddenCommunityIds = React.useRef(new Set<string>());
  const recoveryAccount = React.useRef<string | null>(null);
  const deleteInFlight = React.useRef<symbol | null>(null);
  const accountOwner = React.useRef<string | null>(null);
  const accountGeneration = React.useRef(0);

  const adoptAccountOwner = React.useCallback((nextOwner: string | null) => {
    if (accountOwner.current === nextOwner) return;
    accountOwner.current = nextOwner;
    accountGeneration.current += 1;
    deleteInFlight.current = null;
    setAction(null);
    setError(null);
    setStatusMessage(null);
  }, []);

  const loadAccount =
    React.useCallback(async (): Promise<LoadedHostedAccount> => {
      setError(null);
      const [identityResponse, communitiesResponse] = await Promise.all([
        invoke<IdentityResponse>("get_builderlab_nostr_identity"),
        invoke<CommunitiesResponse>("list_builderlab_communities"),
      ]);
      if (
        identityResponse.error &&
        identityResponse.error.code !== "unauthorized" &&
        // `missing_mapping` (setup_needed) just means this account hasn't linked a
        // Buzz identity yet — that's the connect-card empty state, not an error to
        // surface at the top of the page.
        !identityResponse.error.setup_needed
      ) {
        throw new Error(
          errorMessage(
            identityResponse.error,
            identityResponse.correlation_id,
            "Could not load the connected Buzz identity.",
          ),
        );
      }
      if (
        communitiesResponse.error &&
        !communitiesResponse.error.setup_needed
      ) {
        throw new Error(
          errorMessage(
            communitiesResponse.error,
            communitiesResponse.correlation_id,
            "Could not load communities.",
          ),
        );
      }
      const nextOwner = normalizedBoundKeyHex(
        identityResponse.identity?.pubkey_hex,
      );
      adoptAccountOwner(nextOwner);
      const storedDeletion = loadPendingCommunityDeletion();
      if (
        storedDeletion &&
        (!nextOwner ||
          !pendingCommunityDeletionMatchesAccount(
            storedDeletion,
            nextOwner,
            BUILDERLAB_BACKEND_ORIGIN,
          ))
      ) {
        clearPendingCommunityDeletion();
        setPendingDeletion(null);
      }
      setIdentity(identityResponse.identity ?? null);
      const nextCommunities = (communitiesResponse.communities ?? []).filter(
        (community) =>
          !community.id || !hiddenCommunityIds.current.has(community.id),
      );
      setCommunities(nextCommunities);
      const hasQuota =
        Number.isInteger(communitiesResponse.quota_used) &&
        Number.isInteger(communitiesResponse.quota_limit) &&
        typeof communitiesResponse.can_create === "boolean";
      setQuota(
        hasQuota
          ? {
              used: communitiesResponse.quota_used as number,
              limit: communitiesResponse.quota_limit as number,
              canCreate: communitiesResponse.can_create === true,
            }
          : null,
      );
      return { communities: nextCommunities, owner: nextOwner };
    }, [adoptAccountOwner]);

  React.useEffect(() => {
    let active = true;
    void invoke<BuilderlabAuth | null>("get_builderlab_auth")
      .then(async (nextAuth) => {
        if (!active) return;
        setAuth(nextAuth);
        if (nextAuth) await loadAccount();
      })
      .catch((cause) => {
        if (active) setError(String(cause));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [loadAccount]);

  // Returns whether the operation completed without throwing so callers (e.g.
  // dialogs) can close themselves only on success.
  const run = async (label: string, operation: () => Promise<void>) => {
    setAction(label);
    setError(null);
    try {
      await operation();
      return true;
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      return false;
    } finally {
      setAction(null);
    }
  };

  const signIn = () =>
    run("Signing in…", async () => {
      const nextAuth = await invoke<BuilderlabAuth>("start_builderlab_login");
      setAuth(nextAuth);
      await loadAccount();
    });

  const signOut = () =>
    run("Signing out…", async () => {
      await invoke("clear_builderlab_auth");
      adoptAccountOwner(null);
      setAuth(null);
      setIdentity(null);
      setCommunities([]);
      setQuota(null);
      setPendingDeletion(null);
      setStatusMessage(null);
      clearPendingCommunityDeletion();
      setName("");
      setAvailability(null);
    });

  const connectIdentity = () =>
    run("Connecting Buzz identity…", async () => {
      const response = await invoke<IdentityResponse>(
        "bind_builderlab_nostr_identity",
      );
      if (response.error) {
        throw new Error(
          errorMessage(
            response.error,
            response.correlation_id,
            "Could not connect the Buzz identity.",
          ),
        );
      }
      setIdentity(response.identity ?? null);
      await loadAccount();
    });

  const unpairIdentity = () =>
    run("Unpairing identity…", async () => {
      const response = await invoke<IdentityResponse>(
        "delete_builderlab_nostr_identity",
      );
      if (response.error) {
        throw new Error(
          errorMessage(
            response.error,
            response.correlation_id,
            "Could not unpair the Buzz identity.",
          ),
        );
      }
      adoptAccountOwner(null);
      setIdentity(null);
      clearPendingCommunityDeletion();
      setPendingDeletion(null);
      setStatusMessage(null);
      await loadAccount();
    });

  // The Builderlab account can be bound to an npub that differs from the key
  // this Desktop is currently signing with (e.g. you signed into an email tied
  // to a different test identity). When that happens the community list and
  // Connect buttons operate on the *bound* npub's communities, so "Connect"
  // would drop you into a relay your local key isn't a member of. Detect it and
  // block Connect + Create until the identities match. Both keys are compared
  // in the one normalized hex form, so a padded or mixed-case spelling of
  // the same key never reads as a mismatch, and an npub stored in the hex
  // field is not a key at all.
  // Identity rows display npubs; an unencodable or non-identity-length key
  // renders the neutral label instead of leaking raw hex. The account's
  // `pubkey_hex` is the authoritative binding key — the mismatch gate and
  // every hosted-community operation act on it — so the displayed account
  // npub is derived from it, not from the server-provided `npub` spelling.
  // Nothing on this path proves the two fields encode the same key, and two
  // individually valid but contradictory values must never make the screen
  // show one identity while binding decisions act on another.
  const boundHex = normalizedBoundKeyHex(identity?.pubkey_hex);
  const localHex = normalizedBoundKeyHex(localPubkey);
  const localNpub = localHex === null ? null : safeNpub(localHex);
  const boundNpub = usableBoundIdentityNpub(identity);
  // The account is only usable here when its authoritative `pubkey_hex`
  // normalizes to a hex key: without that key this card cannot establish
  // which identity the connected claim, Connect, and Create actions affect.
  // An identity payload without a usable authoritative key therefore counts
  // as a mismatch that requires recovery — never as a connected account.
  const usableBoundIdentity = boundHex !== null;
  const identityMismatch = Boolean(
    identity &&
      (!usableBoundIdentity ||
        (boundHex !== null && localHex !== null && boundHex !== localHex)),
  );
  const switchToDeviceIdentity = () =>
    run("Switching identity…", async () => {
      // The account is bound to a different npub, so re-binding directly returns
      // identity_already_bound. Release the current binding first, then bind
      // this device's key. If the local key is reserved by another Builderlab
      // account, the bind fails with pubkey_already_bound — surface that instead
      // of leaving the swap half-finished silently.
      const released = await invoke<IdentityResponse>(
        "delete_builderlab_nostr_identity",
      );
      if (released.error) {
        throw new Error(
          errorMessage(
            released.error,
            released.correlation_id,
            "Could not release the previously connected Buzz identity.",
          ),
        );
      }
      adoptAccountOwner(null);
      clearPendingCommunityDeletion();
      setPendingDeletion(null);
      setStatusMessage(null);
      const bound = await invoke<IdentityResponse>(
        "bind_builderlab_nostr_identity",
      );
      if (bound.error) {
        // Refresh so the UI reflects the now-unbound account before surfacing
        // the reason the swap could not complete.
        await loadAccount();
        throw new Error(
          bound.error.code === "pubkey_already_bound"
            ? "This device's Buzz identity is already reserved by another Builderlab account, so it can't be connected here. Sign in with that account, or transfer the identity there first."
            : errorMessage(
                bound.error,
                bound.correlation_id,
                "Could not connect this device's Buzz identity.",
              ),
        );
      }
      setIdentity(bound.identity ?? null);
      await loadAccount();
    });

  const archiveCommunity = (community: HostedCommunity) => {
    if (!community.id) return Promise.resolve(false);
    return run("Archiving community…", async () => {
      const response = await invoke<CommunityMutationResponse>(
        "archive_builderlab_community",
        { communityId: community.id },
      );
      // Treat a returned archived timestamp as success even if the payload also
      // carries a soft error (existing connections may take time to close).
      if (response.error && !response.community?.archived_at) {
        throw new Error(
          errorMessage(
            response.error,
            response.correlation_id,
            "Could not archive the community.",
          ),
        );
      }
      await loadAccount();
    });
  };

  const unarchiveCommunity = (community: HostedCommunity) => {
    if (!community.id) return Promise.resolve(false);
    return run("Unarchiving community…", async () => {
      const response = await invoke<CommunityMutationResponse>(
        "unarchive_builderlab_community",
        { communityId: community.id },
      );
      if (response.error && response.community?.archived_at !== null) {
        throw new Error(
          errorMessage(
            response.error,
            response.correlation_id,
            "Could not unarchive the community.",
          ),
        );
      }
      await loadAccount();
    });
  };

  const transferCommunity = (community: HostedCommunity, npub: string) =>
    run("Transferring ownership…", async () => {
      const response = await invoke<CommunityMutationResponse>(
        "transfer_builderlab_community",
        { communityId: community.id, transfereeNpub: npub },
      );
      if (response.error) {
        throw new Error(
          errorMessage(
            response.error,
            response.correlation_id,
            "Could not transfer ownership.",
          ),
        );
      }
      await loadAccount();
    });

  const applyDeletionResponse = async (
    response: HostedCommunityDeletionResponse,
    envelope: PendingCommunityDeletion,
    attempt: CommunityDeletionAttempt,
    generation: number,
  ) => {
    if (
      accountGeneration.current !== generation ||
      !pendingCommunityDeletionMatchesAccount(
        envelope,
        accountOwner.current ?? "",
        BUILDERLAB_BACKEND_ORIGIN,
      )
    ) {
      return;
    }
    const disposition = deletionResponseDisposition(
      response,
      envelope,
      attempt,
    );
    if (response.error) {
      if (disposition === "abort" || disposition === "clear") {
        clearPendingCommunityDeletion();
        setPendingDeletion(null);
      }
      throw new Error(
        errorMessage(
          response.error,
          response.correlation_id,
          attempt === "receipt"
            ? "Could not confirm deletion status. The existing request remains pending."
            : "Could not start community deletion.",
        ),
      );
    }
    if (disposition !== "accept") {
      throw new Error(
        "Deletion acceptance is uncertain. Check deletion status; do not start a new request.",
      );
    }
    clearPendingCommunityDeletion();
    setPendingDeletion(null);
    hiddenCommunityIds.current.add(envelope.community_id);
    setCommunities((current) =>
      current.filter((community) => community.id !== envelope.community_id),
    );
    setStatusMessage("Deletion started");
    await loadAccount();
  };

  const deletionContextMatches = (
    envelope: PendingCommunityDeletion,
    generation: number,
  ) =>
    accountGeneration.current === generation &&
    pendingCommunityDeletionMatchesAccount(
      envelope,
      accountOwner.current ?? "",
      BUILDERLAB_BACKEND_ORIGIN,
    );

  const runDeletion = async (
    label: string,
    envelope: PendingCommunityDeletion,
    operation: (generation: number) => Promise<void>,
  ) => {
    const generation = accountGeneration.current;
    setAction(label);
    setError(null);
    try {
      await operation(generation);
      return true;
    } catch (cause) {
      if (deletionContextMatches(envelope, generation)) {
        setError(cause instanceof Error ? cause.message : String(cause));
      }
      return false;
    } finally {
      if (deletionContextMatches(envelope, generation)) setAction(null);
    }
  };

  const invokeDeletion = async (
    command:
      | "delete_builderlab_community"
      | "get_builderlab_community_deletion_receipt",
    envelope: PendingCommunityDeletion,
    attempt: CommunityDeletionAttempt,
    generation: number,
  ) => {
    const request = publicDeletionRequest(envelope);
    let response: HostedCommunityDeletionResponse;
    try {
      response = await invoke<HostedCommunityDeletionResponse>(command, {
        communityId: request.community_id,
        host: request.host,
        requestId: request.request_id,
        acknowledgementVersion: request.acknowledgement_version,
      });
    } catch (cause) {
      if (!deletionContextMatches(envelope, generation)) return;
      throw cause;
    }
    if (!deletionContextMatches(envelope, generation)) return;
    await applyDeletionResponse(response, envelope, attempt, generation);
  };

  const startCommunityDeletion = (community: HostedCommunity) => {
    if (
      deleteInFlight.current !== null ||
      auth?.canDeleteBuzzCommunities !== true ||
      identityMismatch ||
      !community.archived_at ||
      !community.id ||
      !community.normalized_host ||
      !boundHex
    )
      return;
    const inFlight = Symbol("community-deletion");
    deleteInFlight.current = inFlight;
    const envelope: PendingCommunityDeletion = {
      community_id: community.id,
      host: community.normalized_host,
      request_id: crypto.randomUUID().toLowerCase(),
      acknowledgement_version: 1,
      bound_owner_pubkey: boundHex,
      backend_origin: BUILDERLAB_BACKEND_ORIGIN,
    };
    if (!persistPendingCommunityDeletion(envelope)) {
      if (deleteInFlight.current === inFlight) deleteInFlight.current = null;
      setError(
        "Could not safely save the pending deletion request. Nothing was sent.",
      );
      return;
    }
    setPendingDeletion(envelope);
    void runDeletion("Starting deletion…", envelope, async (generation) => {
      try {
        await invokeDeletion(
          "delete_builderlab_community",
          envelope,
          "initial",
          generation,
        );
      } finally {
        if (deleteInFlight.current === inFlight) deleteInFlight.current = null;
      }
    });
  };

  const checkDeletionStatus = (envelope: PendingCommunityDeletion) =>
    runDeletion("Checking deletion status…", envelope, async (generation) => {
      await invokeDeletion(
        "get_builderlab_community_deletion_receipt",
        envelope,
        "receipt",
        generation,
      );
    });

  const resubmitPendingDeletion = (envelope: PendingCommunityDeletion) => {
    if (
      identityMismatch ||
      accountOwner.current !== envelope.bound_owner_pubkey ||
      deleteInFlight.current !== null
    )
      return;
    const inFlight = Symbol("community-deletion-resubmit");
    deleteInFlight.current = inFlight;
    void runDeletion("Resubmitting deletion…", envelope, async (generation) => {
      try {
        const refreshedAuth = await invoke<BuilderlabAuth | null>(
          "get_builderlab_auth",
        );
        if (!deletionContextMatches(envelope, generation)) return;
        if (!refreshedAuth) {
          adoptAccountOwner(null);
          setAuth(null);
          setIdentity(null);
          setCommunities([]);
          setQuota(null);
          clearPendingCommunityDeletion();
          setPendingDeletion(null);
          return;
        }
        setAuth(refreshedAuth);
        if (refreshedAuth.canDeleteBuzzCommunities !== true) {
          throw new Error(
            "Community deletion is no longer enabled for this account. The existing request remains pending.",
          );
        }
        const refreshedAccount = await loadAccount();
        if (!deletionContextMatches(envelope, generation)) return;
        const confirmedAuth = await invoke<BuilderlabAuth | null>(
          "get_builderlab_auth",
        );
        if (!deletionContextMatches(envelope, generation)) return;
        if (!confirmedAuth) {
          adoptAccountOwner(null);
          setAuth(null);
          setIdentity(null);
          setCommunities([]);
          setQuota(null);
          clearPendingCommunityDeletion();
          setPendingDeletion(null);
          return;
        }
        setAuth(confirmedAuth);
        if (confirmedAuth.canDeleteBuzzCommunities !== true) {
          throw new Error(
            "Community deletion is no longer enabled for this account. The existing request remains pending.",
          );
        }
        const current = refreshedAccount.communities.find(
          (community) =>
            community.id === envelope.community_id &&
            community.normalized_host === envelope.host &&
            Boolean(community.archived_at),
        );
        if (!current) {
          throw new Error(
            "The exact archived community is not present in the fresh owner list. The existing request remains pending; check its deletion status.",
          );
        }
        await invokeDeletion(
          "delete_builderlab_community",
          envelope,
          "resubmit",
          generation,
        );
      } finally {
        if (deleteInFlight.current === inFlight) deleteInFlight.current = null;
      }
    });
  };

  // biome-ignore lint/correctness/useExhaustiveDependencies: account key bounds the one recovery lookup
  React.useEffect(() => {
    if (!auth || loading || !boundHex) return;
    const accountKey = `${BUILDERLAB_BACKEND_ORIGIN}:${boundHex}`;
    if (recoveryAccount.current === accountKey) return;
    recoveryAccount.current = accountKey;
    const envelope = loadPendingCommunityDeletion();
    if (!envelope) return;
    if (
      !pendingCommunityDeletionMatchesAccount(
        envelope,
        boundHex,
        BUILDERLAB_BACKEND_ORIGIN,
      )
    ) {
      clearPendingCommunityDeletion();
      setPendingDeletion(null);
      return;
    }
    setPendingDeletion(envelope);
    void checkDeletionStatus(envelope);
    // One lookup per authoritative account/origin on reopen; later checks are manual.
  }, [auth, boundHex, loading]);

  const normalizedName = name.trim().toLowerCase();
  const validName =
    normalizedName.length <= 63 && VALID_NAME.test(normalizedName);

  // Debounced typeahead availability check: once the user pauses on a valid
  // address, check it ~500ms later so the result is ready before they click
  // Create (no separate "check" click). onChange clears the previous result, so
  // the indicator reflects the current input while typing.
  React.useEffect(() => {
    if (
      !usableBoundIdentity ||
      identityMismatch ||
      !normalizedName ||
      !validName
    ) {
      setCheckingName(false);
      return;
    }
    let cancelled = false;
    setCheckingName(true);
    const handle = window.setTimeout(() => {
      void (async () => {
        try {
          const response = await invoke<AvailabilityResponse>(
            "check_builderlab_community_name",
            { name: normalizedName },
          );
          if (cancelled) return;
          setAvailability(
            response.error ? null : (response.available ?? false),
          );
        } catch {
          if (!cancelled) setAvailability(null);
        } finally {
          if (!cancelled) setCheckingName(false);
        }
      })();
    }, 500);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [normalizedName, validName, usableBoundIdentity, identityMismatch]);

  const createCommunity = (event: React.FormEvent) => {
    event.preventDefault();
    if (
      !validName ||
      !usableBoundIdentity ||
      identityMismatch ||
      quota?.canCreate !== true
    )
      return;
    void run("Creating community…", async () => {
      const availabilityResponse = await invoke<AvailabilityResponse>(
        "check_builderlab_community_name",
        { name: normalizedName },
      );
      if (availabilityResponse.error || !availabilityResponse.available) {
        setAvailability(false);
        throw new Error(
          errorMessage(
            availabilityResponse.error,
            availabilityResponse.correlation_id,
            "That Buzz address is already taken.",
          ),
        );
      }
      const response = await invoke<CommunityMutationResponse>(
        "create_builderlab_community",
        { name: normalizedName },
      );
      if (response.error || !response.community) {
        throw new Error(
          errorMessage(
            response.error,
            response.correlation_id,
            "Could not create the community.",
          ),
        );
      }
      const url = relayUrl(response.community);
      if (!url)
        throw new Error("The new community did not return a relay address.");
      setName("");
      setAvailability(null);
      await loadAccount();
      if (
        !onboarding.start({
          source: "add-community",
          relayUrl: url,
          communityName: response.community.name ?? normalizedName,
        })
      ) {
        throw new Error(
          "Another community is already being connected. Finish it before connecting this one.",
        );
      }
    });
  };

  const busy = action != null;
  const atCommunityLimit = quota?.canCreate !== true;
  const deletionCapability = auth?.canDeleteBuzzCommunities === true;

  return (
    <section className="space-y-6" data-testid="hosted-communities-settings">
      <SettingsSectionHeader
        title="Hosted communities"
        description="Buzz works with any relay. This page is only for relay hosting provided by Block — sign in with a Builderlab account to create and manage Block-hosted communities. Builderlab sign-in is used on this page alone."
      />

      {error ? (
        <div className="flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/5 p-3 text-sm text-destructive">
          <AlertCircle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{error}</span>
        </div>
      ) : null}

      {statusMessage ? (
        <div
          className="rounded-lg border border-emerald-500/40 bg-emerald-500/5 p-3 text-sm"
          aria-live="polite"
        >
          {statusMessage}
        </div>
      ) : null}

      {pendingDeletion ? (
        <div className="space-y-2 rounded-lg border border-amber-500/40 bg-amber-500/5 p-3 text-sm">
          <p>
            Deletion acceptance for {pendingDeletion.host} is uncertain. Keep
            request <code>{pendingDeletion.request_id}</code> pending until its
            existing receipt is confirmed. Status checks may remain unresolved;
            do not start a new request.
          </p>
          <div className="flex flex-wrap gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => void checkDeletionStatus(pendingDeletion)}
            >
              Check deletion status
            </Button>
            {deletionCapability ? (
              <Button
                variant="outline"
                size="sm"
                disabled={busy || identityMismatch}
                onClick={() => resubmitPendingDeletion(pendingDeletion)}
              >
                Resubmit same request
              </Button>
            ) : null}
          </div>
        </div>
      ) : null}

      {loading ? (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <LoaderCircle className="h-4 w-4 animate-spin" /> Checking sign-in…
        </div>
      ) : !auth ? (
        <div className="rounded-xl border border-border/70 p-5">
          <h3 className="font-medium">Sign in to manage hosted communities</h3>
          <p
            className="mt-2 max-w-2xl text-sm text-muted-foreground/70"
            data-settings-subcopy
          >
            Authentication opens in your browser and returns securely to Buzz.
            You can use every other part of the app without signing in.
          </p>
          <Button
            className="mt-4"
            disabled={busy}
            onClick={() => void signIn()}
          >
            {action ? (
              <LoaderCircle className="h-4 w-4 animate-spin" />
            ) : (
              <ExternalLink className="h-4 w-4" />
            )}
            {action ?? "Sign in with Builderlab"}
          </Button>
        </div>
      ) : (
        <>
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border/70 p-4">
            <div>
              <p className="text-sm font-medium">
                {auth.name || auth.email || "Builderlab account"}
              </p>
              {auth.name && auth.email ? (
                <p className="text-xs text-muted-foreground">{auth.email}</p>
              ) : null}
            </div>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => void signOut()}
            >
              <LogOut className="h-4 w-4" /> Sign out
            </Button>
          </div>

          {!identity ? (
            <div className="rounded-xl border border-amber-500/40 bg-amber-500/5 p-5">
              <h3 className="font-medium">
                Link this account to your Buzz identity
              </h3>
              <p
                className="mt-2 text-sm text-muted-foreground/70"
                data-settings-subcopy
              >
                This Builderlab account isn&apos;t linked to a Buzz identity
                yet. Connect this device&apos;s key to create and own
                communities under it — Buzz signs a one-time challenge locally,
                so your private key never leaves Desktop.
              </p>
              <Button
                className="mt-4"
                disabled={busy}
                onClick={() => void connectIdentity()}
              >
                {action ? (
                  <LoaderCircle className="h-4 w-4 animate-spin" />
                ) : null}
                {action ?? "Connect Buzz identity"}
              </Button>
            </div>
          ) : identityMismatch ? (
            <div className="rounded-xl border border-amber-500/50 bg-amber-500/5 p-5">
              <div className="flex items-start gap-2">
                <AlertCircle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600" />
                <div>
                  <h3 className="font-medium">
                    This account is connected to a different Buzz identity
                  </h3>
                  <p
                    className="mt-2 text-sm text-muted-foreground/70"
                    data-settings-subcopy
                  >
                    Your Builderlab account owns communities under another Buzz
                    key, so connecting them here would join a relay this device
                    isn&apos;t a member of. Creating and connecting are paused
                    until the identities match.
                  </p>
                  <dl className="mt-3 space-y-1 text-xs">
                    <div className="flex flex-wrap gap-x-2">
                      <dt className="text-muted-foreground">Account uses</dt>
                      <dd className="font-mono">
                        {boundNpub ?? UNAVAILABLE_KEY_LABEL}
                      </dd>
                    </div>
                    <div className="flex flex-wrap gap-x-2">
                      <dt className="text-muted-foreground">This device</dt>
                      <dd className="font-mono">
                        {localNpub ?? UNAVAILABLE_KEY_LABEL}
                      </dd>
                    </div>
                  </dl>
                </div>
              </div>
              <Button
                className="mt-4"
                disabled={busy}
                onClick={() => void switchToDeviceIdentity()}
              >
                {action ? (
                  <LoaderCircle className="h-4 w-4 animate-spin" />
                ) : null}
                {action ?? "Switch to this device's identity"}
              </Button>
            </div>
          ) : (
            <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border/70 p-4">
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <CheckCircle2 className="h-4 w-4 text-emerald-500" /> Buzz
                identity connected
                {boundNpub ? (
                  <span className="font-mono text-xs">{boundNpub}</span>
                ) : null}
              </div>
              <UnpairIdentityButton
                busy={busy}
                onConfirm={() => void unpairIdentity()}
              />
            </div>
          )}

          <div className="space-y-3">
            <div className="flex items-center justify-between gap-3">
              <h3 className="font-medium">
                Your communities
                <span className="ml-2 text-xs font-normal text-muted-foreground">
                  {quota
                    ? `${quota.used} of ${quota.limit} used`
                    : "Quota unavailable"}
                </span>
              </h3>
              <Button
                variant="ghost"
                size="sm"
                disabled={busy}
                onClick={() =>
                  void run("Refreshing…", async () => {
                    await loadAccount();
                  })
                }
              >
                <RefreshCw className="h-4 w-4" /> Refresh
              </Button>
            </div>
            {communities.length === 0 ? (
              <p className="rounded-xl border border-dashed p-5 text-sm text-muted-foreground">
                No hosted communities yet.
              </p>
            ) : (
              <ul className="space-y-2">
                {[...communities]
                  .sort(
                    (a, b) =>
                      Number(Boolean(a.archived_at)) -
                      Number(Boolean(b.archived_at)),
                  )
                  .map((community, index) => (
                    <HostedCommunityRow
                      key={community.id ?? community.normalized_host ?? index}
                      community={community}
                      busy={
                        busy || pendingDeletion?.community_id === community.id
                      }
                      deletionPending={
                        pendingDeletion?.community_id === community.id
                      }
                      canDelete={
                        deletionCapability &&
                        usableBoundIdentity &&
                        !identityMismatch
                      }
                      canConnect={usableBoundIdentity && !identityMismatch}
                      showIconPicker={
                        relayHost(relayUrl(community)) ===
                        relayHost(activeCommunity?.relayUrl)
                      }
                      onConnect={() => {
                        // Invocation guard: the Connect affordance only
                        // renders for a usable, matching binding, but this
                        // callback is the last gate — a click that lands after
                        // a refresh returned an absent or unusable identity
                        // must not start onboarding either.
                        if (!usableBoundIdentity || identityMismatch) {
                          return;
                        }
                        const url = relayUrl(community);
                        if (url)
                          onboarding.start({
                            source: "add-community",
                            relayUrl: url,
                            communityName: community.name,
                          });
                      }}
                      onArchive={() => void archiveCommunity(community)}
                      onUnarchive={() => void unarchiveCommunity(community)}
                      onTransfer={(npub) => transferCommunity(community, npub)}
                      onDelete={() => startCommunityDeletion(community)}
                    />
                  ))}
              </ul>
            )}
          </div>

          <form
            className="space-y-4 rounded-xl border border-border/70 p-5"
            onSubmit={createCommunity}
          >
            <div>
              <h3 className="font-medium">Create a community</h3>
              <p
                className="mt-1 text-sm text-muted-foreground/70"
                data-settings-subcopy
              >
                Choose the address your team will use to connect.
              </p>
            </div>
            {atCommunityLimit ? (
              <p className="text-sm text-muted-foreground">
                {quota
                  ? `You've reached the limit of ${quota.limit} hosted communities. A deletion frees its slot only after logical cleanup completes.`
                  : "Community quota is unavailable. Creation stays disabled until the server returns an authoritative quota."}
              </p>
            ) : null}
            <div className="flex max-w-xl items-center gap-2">
              <Input
                aria-label="Community address"
                autoComplete="off"
                disabled={
                  !usableBoundIdentity ||
                  identityMismatch ||
                  busy ||
                  atCommunityLimit
                }
                maxLength={63}
                onChange={(event) => {
                  setName(event.target.value.toLowerCase());
                  setAvailability(null);
                }}
                placeholder="north-star"
                spellCheck={false}
                value={name}
              />
              <span className="shrink-0 text-sm text-muted-foreground">
                .{HOST_SUFFIX}
              </span>
            </div>
            {name && !validName ? (
              <p className="text-sm text-destructive">
                Use lowercase letters, numbers, and single hyphens.
              </p>
            ) : validName && checkingName ? (
              <p className="text-sm text-muted-foreground">
                Checking availability…
              </p>
            ) : availability === false ? (
              <p className="text-sm text-destructive">
                That address is already taken.
              </p>
            ) : availability === true ? (
              <p className="text-sm text-emerald-600">
                That address is available.
              </p>
            ) : null}
            <Button
              disabled={
                !usableBoundIdentity ||
                identityMismatch ||
                !validName ||
                availability === false ||
                checkingName ||
                busy ||
                atCommunityLimit
              }
              type="submit"
            >
              {action ? (
                <LoaderCircle className="h-4 w-4 animate-spin" />
              ) : null}
              {action ?? "Create and connect"}
            </Button>
          </form>
        </>
      )}
    </section>
  );
}

function UnpairIdentityButton({
  busy,
  onConfirm,
}: {
  busy: boolean;
  onConfirm: () => void;
}) {
  const [open, setOpen] = React.useState(false);
  return (
    <AlertDialog open={open} onOpenChange={setOpen}>
      <Button
        variant="ghost"
        size="sm"
        className="text-destructive hover:text-destructive"
        disabled={busy}
        onClick={() => setOpen(true)}
      >
        <Unlink className="h-4 w-4" /> Unpair identity
      </Button>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Unpair this Buzz identity?</AlertDialogTitle>
          <AlertDialogDescription>
            Your Builderlab account will no longer be connected to this Buzz
            key. You can reconnect any key later, but community actions stay
            unavailable until you do.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            className={buttonVariants({ variant: "destructive" })}
            onClick={onConfirm}
          >
            Unpair identity
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
