import * as React from "react";
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { AlertCircle, LoaderCircle, Play, Square } from "lucide-react";

import {
  cancelCriticRound,
  listCriticRouteProfiles,
  previewCriticCoordinatorGuide,
  previewCriticRouteProfile,
  runCriticRound,
} from "@/shared/api/tauriCriticRuns";
import type {
  CriticRole,
  CriticRunParams,
  CriticRunResult,
  CriticRouteProfileRef,
} from "@/shared/api/types";
import {
  allocateEstimatedCriticBudget,
  criticFailureLabel,
  formatEstimatedCriticBudgetUsd,
  parseEstimatedCriticBudgetUsd,
} from "./criticCostBudget";
import { Button } from "@/shared/ui/button";
import { Checkbox } from "@/shared/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Textarea } from "@/shared/ui/textarea";

const HISTORY_KEY = ["critic-round-history"] as const;
const ROLES: Array<{ id: CriticRole; title: string; description: string }> = [
  {
    id: "correctness",
    title: "Correctness",
    description: "Bugs, edge cases, and failure handling",
  },
  {
    id: "security",
    title: "Security",
    description: "Trust boundaries and unsafe defaults",
  },
  {
    id: "architecture",
    title: "Architecture",
    description: "Ownership, boundaries, and duplication",
  },
  {
    id: "ui_accessibility",
    title: "UI and accessibility",
    description: "Hierarchy, interaction, and keyboard use",
  },
  {
    id: "performance",
    title: "Performance",
    description: "Latency and unbounded work",
  },
  {
    id: "product",
    title: "Product fit",
    description: "User need and discoverability",
  },
];

function criticRunDefinitionKey(
  params: CriticRunParams,
  estimatedRoundBudgetUsd: string,
  routeProfileIds: Partial<Record<CriticRole, string>>,
): string {
  return JSON.stringify({
    ...params,
    estimatedRoundBudgetUsd,
    roles: [...params.roles].sort(),
    routeProfileIds: Object.fromEntries(
      params.roles
        .map((role) => [role, routeProfileIds[role] ?? ""] as const)
        .sort(([left], [right]) => left.localeCompare(right)),
    ),
    routeProfiles: Object.fromEntries(
      Object.entries(params.routeProfiles).sort(([left], [right]) =>
        left.localeCompare(right),
      ),
    ),
  });
}

export function AgentCriticRunDialog({
  threadContext = null,
  onOpenChange,
  open,
}: {
  onOpenChange: (open: boolean) => void;
  open: boolean;
  threadContext?: {
    disclosure: string;
    snapshot: string;
    sourceIds: readonly string[];
  } | null;
}) {
  const queryClient = useQueryClient();
  const [objective, setObjective] = React.useState("");
  const [scope, setScope] = React.useState("");
  const [snapshot, setSnapshot] = React.useState("");
  const [roles, setRoles] = React.useState<CriticRole[]>([
    "correctness",
    "security",
  ]);
  const [maxOutputTokens, setMaxOutputTokens] = React.useState(1024);
  const [timeLimitSeconds, setTimeLimitSeconds] = React.useState(60);
  const [estimatedRoundBudgetUsd, setEstimatedRoundBudgetUsd] =
    React.useState("");
  const [thinkingEffort, setThinkingEffort] =
    React.useState<CriticRunParams["thinkingEffort"]>(null);
  const [routeProfileIds, setRouteProfileIds] = React.useState<
    Partial<Record<CriticRole, string>>
  >({});
  const [requestId, setRequestId] = React.useState<string | null>(null);
  const [canceling, setCanceling] = React.useState(false);
  const [cancelError, setCancelError] = React.useState<string | null>(null);
  const [result, setResult] = React.useState<CriticRunResult | null>(null);
  const [resultInputKey, setResultInputKey] = React.useState<string | null>(
    null,
  );
  const [error, setError] = React.useState<string | null>(null);

  const guideQuery = useQuery({
    queryKey: ["critic-coordinator-guide"],
    queryFn: previewCriticCoordinatorGuide,
    enabled: open,
    retry: false,
  });
  const guidePreview = guideQuery.data;
  const guideReady =
    guidePreview?.path === "AGENT_GUIDES/CRITICS.md" &&
    /^[0-9a-f]{64}$/.test(guidePreview.sha256);

  const routeProfilesQuery = useQuery({
    queryKey: ["critic-route-profiles"],
    queryFn: listCriticRouteProfiles,
    enabled: open,
  });
  const routeProfiles = routeProfilesQuery.data ?? [];
  const hasLocalRouteProfile = routeProfiles.some(
    (profile) => profile.dataPolicy === "local-only",
  );
  const routeProfileIdsInUse = [
    ...new Set(
      roles
        .map((role) => routeProfileIds[role])
        .filter((profileId): profileId is string => Boolean(profileId)),
    ),
  ];
  const routePreviewQueries = useQueries({
    queries: routeProfileIdsInUse.map((profileId) => ({
      queryKey: ["critic-route-profile-preview", profileId],
      queryFn: () => previewCriticRouteProfile(profileId),
      enabled: open && profileId.length > 0,
      retry: false,
    })),
  });
  const routePreviewFor = (role: CriticRole) => {
    const profileId = routeProfileIds[role];
    if (!profileId) return undefined;
    const index = routeProfileIdsInUse.indexOf(profileId);
    return index < 0 ? undefined : routePreviewQueries[index];
  };
  const parsedRoundBudgetMicrousd = parseEstimatedCriticBudgetUsd(
    estimatedRoundBudgetUsd,
  );
  const roundBudgetIsValid = parsedRoundBudgetMicrousd !== undefined;
  const estimatedRoundBudgetMicrousd = parsedRoundBudgetMicrousd ?? null;
  const roleBudgetShares =
    estimatedRoundBudgetMicrousd === null
      ? {}
      : allocateEstimatedCriticBudget(estimatedRoundBudgetMicrousd, roles);
  const rolesMissingBudgetPricing =
    estimatedRoundBudgetMicrousd === null
      ? []
      : roles.filter((role) => {
          const preview = routePreviewFor(role)?.data;
          return (
            preview !== undefined &&
            preview.profile.id === routeProfileIds[role] &&
            !preview.candidates.some(
              (candidate) =>
                candidate.configured && candidate.costPricingAvailable,
            )
          );
        });
  const routeReady = roles.every((role) => {
    const profileId = routeProfileIds[role];
    const preview = routePreviewFor(role)?.data;
    return (
      Boolean(profileId && preview) &&
      preview?.profile.id === profileId &&
      preview?.candidates.some(
        (candidate) =>
          candidate.configured &&
          (estimatedRoundBudgetMicrousd === null ||
            candidate.costPricingAvailable),
      )
    );
  });
  const selectedRouteProfiles = roles.reduce<
    Partial<Record<CriticRole, CriticRouteProfileRef>>
  >((selected, role) => {
    const profile = routePreviewFor(role)?.data?.profile;
    if (profile?.id === routeProfileIds[role]) selected[role] = profile;
    return selected;
  }, {});
  const currentRunParams: CriticRunParams = {
    objective,
    scope,
    snapshot,
    roles,
    maxOutputTokens,
    timeLimitSeconds,
    thinkingEffort,
    routeProfiles: selectedRouteProfiles,
    estimatedRoundCostBudgetMicrousd: estimatedRoundBudgetMicrousd,
    coordinatorGuideSha256: guideReady ? (guidePreview?.sha256 ?? "") : "",
  };
  const currentRunDefinitionKey = criticRunDefinitionKey(
    currentRunParams,
    estimatedRoundBudgetUsd,
    routeProfileIds,
  );

  const mutation = useMutation({
    mutationFn: ({ id, params }: { id: string; params: CriticRunParams }) =>
      runCriticRound(id, params),
    onSuccess: (value) => {
      setResult(value);
      setError(null);
      void queryClient.invalidateQueries({ queryKey: HISTORY_KEY });
    },
    onError: (reason) => {
      setError(errorText(reason));
    },
    onSettled: (_value, _reason, variables) => {
      setRequestId((current) => (current === variables.id ? null : current));
      setCanceling(false);
    },
  });

  const snapshotBytes = new TextEncoder().encode(snapshot).length;
  const objectiveBytes = new TextEncoder().encode(objective).length;
  const scopeBytes = new TextEncoder().encode(scope).length;
  const inputValid =
    objective.trim().length > 0 &&
    objectiveBytes <= 4 * 1024 &&
    scope.trim().length > 0 &&
    scopeBytes <= 2 * 1024 &&
    snapshot.trim().length > 0 &&
    snapshotBytes <= 64 * 1024 &&
    roles.length > 0 &&
    guideReady &&
    roundBudgetIsValid &&
    routeReady &&
    !mutation.isPending;

  React.useEffect(() => {
    if (open || mutation.isPending) return;
    setObjective("");
    setScope("");
    setSnapshot("");
    setRoles(["correctness", "security"]);
    setMaxOutputTokens(1024);
    setTimeLimitSeconds(60);
    setEstimatedRoundBudgetUsd("");
    setThinkingEffort(null);
    setRouteProfileIds({});
    setRequestId(null);
    setCanceling(false);
    setCancelError(null);
    setResult(null);
    setResultInputKey(null);
    setError(null);
  }, [open, mutation.isPending]);

  React.useEffect(() => {
    if (!open || !threadContext) return;
    setObjective(
      "Summarize this discussion against the user's original intent.",
    );
    setScope(
      "Identify the original request, decisions and progress, unresolved blockers, and useful next steps. Cite message IDs. Use only the frozen text below, and distinguish explicit facts from inference.",
    );
    setSnapshot(threadContext.snapshot);
    setResult(null);
    setResultInputKey(null);
    setError(null);
  }, [open, threadContext]);

  const startRun = () => {
    if (!inputValid || !routeReady) return;
    const params = currentRunParams;
    const routeProfiles = params.routeProfiles;
    if (roles.some((role) => !routeProfiles[role])) return;
    const id = crypto.randomUUID();
    setRequestId(id);
    setCanceling(false);
    setCancelError(null);
    setResult(null);
    setResultInputKey(
      criticRunDefinitionKey(params, estimatedRoundBudgetUsd, routeProfileIds),
    );
    setError(null);
    mutation.mutate({ id, params });
  };

  const cancelRun = async () => {
    if (!requestId) return;
    setCanceling(true);
    setCancelError(null);
    try {
      const accepted = await cancelCriticRound(requestId);
      if (!accepted) {
        setCanceling(false);
        setCancelError("This review already finished or is no longer active.");
      }
    } catch (reason) {
      setCanceling(false);
      setCancelError(errorText(reason));
    }
  };

  return (
    <Dialog
      onOpenChange={(nextOpen) => {
        if (!nextOpen && mutation.isPending) return;
        onOpenChange(nextOpen);
      }}
      open={open}
    >
      <DialogContent
        className="max-h-[92vh] grid-rows-[auto_minmax(0,1fr)] overflow-hidden sm:max-w-5xl"
        data-testid="critic-run-dialog"
      >
        <DialogHeader className="pr-8">
          <DialogTitle>Run critics</DialogTitle>
          <DialogDescription>
            Ask up to three focused reviewers to inspect the same frozen text.
            They cannot edit files or use tools.
          </DialogDescription>
        </DialogHeader>

        <div
          className="grid min-h-0 min-w-0 gap-5 overflow-x-hidden overflow-y-auto lg:grid-cols-[minmax(0,1.1fr)_minmax(18rem,0.9fr)]"
          data-testid="critic-run-scroll-region"
        >
          <form
            className="min-w-0 space-y-4"
            onChange={() => setError(null)}
            onSubmit={(event) => {
              event.preventDefault();
              startRun();
            }}
          >
            <section
              aria-labelledby="critic-coordinator-guide-heading"
              className="space-y-2 rounded-xl border border-border/70 bg-card/40 p-3"
              data-testid="critic-coordinator-guide"
            >
              <div>
                <h3
                  className="text-sm font-medium"
                  id="critic-coordinator-guide-heading"
                >
                  Coordinator guide for you
                </h3>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  This local guide is shown to you as the human coordinator.
                  Buzz does not send it to the reviewer workers or interpret it
                  with an in-app AI agent.
                </p>
              </div>
              {guideQuery.isPending ? (
                <p className="text-xs text-muted-foreground" role="status">
                  Loading the guide from the active Buzz nest…
                </p>
              ) : guideQuery.isError ? (
                <p className="text-xs text-destructive" role="alert">
                  {errorText(guideQuery.error)}
                </p>
              ) : guidePreview && guideReady ? (
                <div className="space-y-2 text-xs">
                  <p className="font-mono">{guidePreview.path}</p>
                  <p className="break-all font-mono text-muted-foreground">
                    SHA-256: {guidePreview.sha256}
                  </p>
                  <p className="text-muted-foreground">
                    {guidePreview.byteLength.toLocaleString()} bytes · exact
                    text, with no truncation
                  </p>
                  <details className="rounded-lg border border-border/70 p-2">
                    <summary className="cursor-pointer font-medium">
                      Preview guide text
                    </summary>
                    <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap break-words rounded-md bg-background/70 p-2 font-sans text-xs leading-relaxed">
                      {guidePreview.text}
                    </pre>
                  </details>
                  <Button
                    disabled={guideQuery.isFetching || mutation.isPending}
                    onClick={() => void guideQuery.refetch()}
                    size="sm"
                    type="button"
                    variant="outline"
                  >
                    {guideQuery.isFetching ? "Refreshing…" : "Refresh preview"}
                  </Button>
                </div>
              ) : (
                <p className="text-xs text-destructive" role="alert">
                  The active nest did not return the canonical coordinator guide
                  preview.
                </p>
              )}
            </section>

            <Field label="Original goal" htmlFor="critic-objective">
              <input
                className="flex h-10 w-full rounded-lg border border-input/40 bg-background px-3 py-2 text-sm focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-50"
                data-testid="critic-objective"
                id="critic-objective"
                maxLength={4096}
                onChange={(event) => setObjective(event.target.value)}
                placeholder="What should this work accomplish?"
                value={objective}
              />
              <ByteCount current={objectiveBytes} maximum={4096} />
            </Field>

            <Field label="Review focus" htmlFor="critic-scope">
              <Textarea
                className="min-h-16"
                data-testid="critic-scope"
                id="critic-scope"
                maxLength={2048}
                onChange={(event) => setScope(event.target.value)}
                placeholder="What should reviewers check?"
                value={scope}
              />
              <ByteCount current={scopeBytes} maximum={2048} />
            </Field>

            <Field
              label="Frozen review text"
              htmlFor="critic-snapshot"
              hint={
                threadContext
                  ? "Review and edit this frozen thread snapshot before any request is sent. Stable event IDs appear inline with their messages."
                  : "Paste the patch, design notes, or selected text to review. Buzz will not read other files."
              }
            >
              {threadContext ? (
                <div
                  className="space-y-1 rounded-lg border border-amber-500/25 bg-amber-500/5 p-3 text-xs leading-relaxed text-muted-foreground"
                  data-testid="critic-thread-disclosure"
                  role="note"
                >
                  <p>{threadContext.disclosure}</p>
                  <p>
                    The captured snapshot includes{" "}
                    {threadContext.sourceIds.length} stable source event ID
                    {threadContext.sourceIds.length === 1 ? "" : "s"} inline.
                    This count describes the original capture; editing the text
                    below can change or remove references. Messages without
                    stable IDs are labeled as unavailable.
                  </p>
                </div>
              ) : null}
              <Textarea
                className="min-h-36 font-mono text-xs"
                data-testid="critic-snapshot"
                id="critic-snapshot"
                maxLength={64 * 1024}
                onChange={(event) => setSnapshot(event.target.value)}
                placeholder="Paste the exact text for review…"
                value={snapshot}
              />
              <ByteCount current={snapshotBytes} maximum={64 * 1024} />
            </Field>

            <fieldset className="space-y-2 rounded-xl border border-border/70 p-3">
              <legend className="px-1 text-sm font-medium">
                Critic roles (up to three)
              </legend>
              <div className="grid gap-2 sm:grid-cols-2">
                {ROLES.map((role) => {
                  const checked = roles.includes(role.id);
                  const profileId = routeProfileIds[role.id] ?? "";
                  const routeQuery = routePreviewFor(role.id);
                  const routePreview = routeQuery?.data;
                  const disabled =
                    !checked && roles.length >= 3 && !mutation.isPending;
                  return (
                    <div
                      className="min-w-0 rounded-lg bg-muted/30 p-2.5"
                      key={role.id}
                    >
                      <div className="flex items-start gap-2">
                        <Checkbox
                          aria-label={role.title}
                          checked={checked}
                          disabled={disabled || mutation.isPending}
                          id={`critic-role-${role.id}`}
                          onCheckedChange={(next) => {
                            if (next === true) {
                              setRoles((current) => [...current, role.id]);
                            } else {
                              setRoles((current) =>
                                current.filter((item) => item !== role.id),
                              );
                            }
                            setResult(null);
                            setError(null);
                          }}
                        />
                        <div>
                          <div className="text-sm font-medium">
                            {role.title}
                          </div>
                          <div className="text-xs text-muted-foreground">
                            {role.description}
                          </div>
                        </div>
                      </div>
                      {checked ? (
                        <div className="mt-3 space-y-2 pl-7">
                          <label
                            className="block space-y-1 text-xs font-medium"
                            htmlFor={`critic-route-${role.id}`}
                          >
                            Local route for {role.title}
                            <select
                              className="mt-1 h-10 w-full min-w-0 rounded-lg border border-input/40 bg-background px-3 text-sm font-normal"
                              data-testid={`critic-route-profile-${role.id}`}
                              disabled={
                                mutation.isPending ||
                                routeProfilesQuery.isLoading
                              }
                              id={`critic-route-${role.id}`}
                              onChange={(event) => {
                                setRouteProfileIds((current) => ({
                                  ...current,
                                  [role.id]: event.target.value,
                                }));
                                setResult(null);
                                setError(null);
                              }}
                              value={profileId}
                            >
                              <option value="">
                                Choose a Local only route
                              </option>
                              {routeProfiles.map((profile) => (
                                <option
                                  disabled={profile.dataPolicy !== "local-only"}
                                  key={profile.id}
                                  value={profile.id}
                                >
                                  {profile.name} ·{" "}
                                  {profile.dataPolicy === "local-only"
                                    ? "Local only"
                                    : "Hosted"}
                                </option>
                              ))}
                            </select>
                          </label>
                          {profileId && routeQuery?.isPending ? (
                            <p
                              className="text-xs text-muted-foreground"
                              role="status"
                            >
                              Checking local endpoint…
                            </p>
                          ) : null}
                          {routeQuery?.isError ? (
                            <p
                              className="text-xs text-destructive"
                              role="alert"
                            >
                              {errorText(routeQuery.error)}
                            </p>
                          ) : null}
                          {routePreview?.profile.id === profileId ? (
                            <div className="space-y-1 rounded-lg border border-border/70 bg-background/50 p-2.5 text-xs">
                              <p
                                className="truncate font-mono"
                                title={`${routePreview.profile.id} · v${routePreview.profile.version} · ${routePreview.profile.hash}`}
                              >
                                {routePreview.profile.id} · v
                                {routePreview.profile.version} ·{" "}
                                {routePreview.profile.hash.slice(0, 12)}…
                              </p>
                              <ul className="space-y-1 text-muted-foreground">
                                {routePreview.candidates.map((candidate) => (
                                  <li key={candidate.id}>
                                    {candidate.provider} / {candidate.model}:{" "}
                                    {candidate.configured
                                      ? "ready"
                                      : "not configured"}
                                    {estimatedRoundBudgetMicrousd !== null
                                      ? candidate.costPricingAvailable
                                        ? " · prices set"
                                        : " · input/output prices missing"
                                      : ""}
                                    {candidate.promptProfile
                                      ? ` · prompt pack ${candidate.promptProfile.id} v${candidate.promptProfile.version} (${candidate.promptProfile.promptHash.slice(0, 12)}…)`
                                      : ""}
                                  </li>
                                ))}
                              </ul>
                              <p className="text-muted-foreground">
                                {routePreview.estimatedCostLimitMicrousd !==
                                null
                                  ? `Estimated ceiling per reviewer turn: $${(routePreview.estimatedCostLimitMicrousd / 1_000_000).toFixed(4)}; the provider invoice may differ.`
                                  : "No estimated cost ceiling is set for this profile."}
                              </p>
                            </div>
                          ) : null}
                        </div>
                      ) : null}
                    </div>
                  );
                })}
              </div>
              {routeProfilesQuery.isError ? (
                <p className="text-xs text-destructive" role="alert">
                  {errorText(routeProfilesQuery.error)}
                </p>
              ) : routeProfilesQuery.isSuccess && !hasLocalRouteProfile ? (
                <p className="text-xs text-muted-foreground">
                  No Local only route is available. Create one in Agents →
                  Routing, then return here.
                </p>
              ) : null}
            </fieldset>

            <div className="space-y-2 rounded-xl border border-border/70 bg-card/40 p-3">
              <label className="block space-y-1" htmlFor="critic-round-budget">
                <span className="text-sm font-medium">
                  Requested round estimate ceiling (USD)
                </span>
                <span className="block text-xs text-muted-foreground">
                  Optional total estimate split evenly across selected
                  reviewers. Each saved route ceiling can lower that reviewer's
                  share. Up to $1,000,000 with six decimal places.
                </span>
              </label>
              <input
                aria-describedby="critic-round-budget-help"
                aria-label="Requested round estimate ceiling in USD"
                className="h-10 w-full rounded-lg border border-input/40 bg-background px-3 text-sm"
                disabled={mutation.isPending}
                id="critic-round-budget"
                inputMode="decimal"
                onChange={(event) =>
                  setEstimatedRoundBudgetUsd(event.target.value)
                }
                placeholder="No round ceiling"
                type="text"
                value={estimatedRoundBudgetUsd}
              />
              <p
                className="text-xs leading-relaxed text-muted-foreground"
                id="critic-round-budget-help"
              >
                This requested total uses operator-entered route rates and
                conservative request estimates, including all retry attempts
                Buzz may issue. Each saved route ceiling can lower its
                reviewer's share. Estimates may differ from provider billing.
                Local inference services may forward requests elsewhere.
              </p>
              {!roundBudgetIsValid ? (
                <p className="text-xs text-destructive" role="alert">
                  Enter USD from $0 to $1,000,000 with no more than six decimal
                  places.
                </p>
              ) : null}
              {estimatedRoundBudgetMicrousd !== null ? (
                <div aria-live="polite" className="space-y-1 text-xs">
                  <p className="font-medium">
                    {"$"}
                    {formatEstimatedCriticBudgetUsd(
                      estimatedRoundBudgetMicrousd,
                    )}{" "}
                    requested round estimate ceiling
                  </p>
                  <ul className="space-y-0.5 text-muted-foreground">
                    {roles.map((role) => {
                      const share = roleBudgetShares[role] ?? 0;
                      const profileLimit =
                        routePreviewFor(role)?.data?.estimatedCostLimitMicrousd;
                      const effective =
                        profileLimit == null
                          ? share
                          : Math.min(profileLimit, share);
                      return (
                        <li key={role}>
                          {role.replaceAll("_", " ")}: {"$"}
                          {formatEstimatedCriticBudgetUsd(share)} requested
                          share; effective estimate ceiling {"$"}
                          {formatEstimatedCriticBudgetUsd(effective)}
                        </li>
                      );
                    })}
                  </ul>
                </div>
              ) : null}
              {rolesMissingBudgetPricing.length > 0 ? (
                <p className="text-xs text-destructive" role="alert">
                  Add input and output prices to a configured Local candidate
                  for each selected route before using a round ceiling.
                </p>
              ) : null}
            </div>

            <div className="grid gap-3 sm:grid-cols-3">
              <label className="space-y-1 text-sm">
                <span className="font-medium">Output cap</span>
                <select
                  aria-label="Output cap per critic"
                  className="h-10 w-full rounded-lg border border-input/40 bg-background px-3"
                  disabled={mutation.isPending}
                  onChange={(event) =>
                    setMaxOutputTokens(Number(event.target.value))
                  }
                  value={maxOutputTokens}
                >
                  {[256, 512, 1024, 1536, 2048].map((value) => (
                    <option key={value} value={value}>
                      {value} tokens
                    </option>
                  ))}
                </select>
              </label>
              <label className="space-y-1 text-sm">
                <span className="font-medium">Time cap</span>
                <select
                  aria-label="Time cap per critic"
                  className="h-10 w-full rounded-lg border border-input/40 bg-background px-3"
                  disabled={mutation.isPending}
                  onChange={(event) =>
                    setTimeLimitSeconds(Number(event.target.value))
                  }
                  value={timeLimitSeconds}
                >
                  {[15, 30, 60, 90, 120].map((value) => (
                    <option key={value} value={value}>
                      {value} seconds
                    </option>
                  ))}
                </select>
              </label>
              <label className="space-y-1 text-sm">
                <span className="font-medium">Reasoning effort</span>
                <select
                  aria-label="Requested reasoning effort"
                  className="h-10 w-full rounded-lg border border-input/40 bg-background px-3"
                  disabled={mutation.isPending}
                  onChange={(event) =>
                    setThinkingEffort(
                      (event.target.value ||
                        null) as CriticRunParams["thinkingEffort"],
                    )
                  }
                  value={thinkingEffort ?? ""}
                >
                  <option value="">Provider default</option>
                  {[
                    "none",
                    "minimal",
                    "low",
                    "medium",
                    "high",
                    "xhigh",
                    "max",
                  ].map((value) => (
                    <option key={value} value={value}>
                      {value}
                    </option>
                  ))}
                </select>
              </label>
            </div>

            <div className="rounded-lg border border-amber-500/25 bg-amber-500/5 p-3 text-xs leading-relaxed text-muted-foreground">
              No request is sent until you choose Run review. Buzz sends this
              frozen text to the selected route's loopback endpoint. The local
              inference service may forward data elsewhere; Buzz cannot inspect
              its egress. Buzz saves reviewer output and fingerprints locally,
              but not the submitted text.
            </div>

            <div className="flex flex-wrap items-center gap-2">
              <Button
                data-testid="critic-run-submit"
                disabled={!inputValid}
                type="submit"
              >
                {mutation.isPending ? (
                  <>
                    <LoaderCircle aria-hidden="true" className="animate-spin" />
                    Reviewing…
                  </>
                ) : (
                  <>
                    <Play aria-hidden="true" />
                    Run review
                  </>
                )}
              </Button>
              {mutation.isPending ? (
                <Button
                  disabled={canceling}
                  onClick={() => void cancelRun()}
                  type="button"
                  variant="outline"
                >
                  <Square aria-hidden="true" />
                  {canceling ? "Canceling…" : "Cancel review"}
                </Button>
              ) : null}
            </div>
            {cancelError ? (
              <p className="text-sm text-destructive" role="alert">
                {cancelError}
              </p>
            ) : null}
          </form>

          <section
            aria-label="Critic results"
            className="min-h-64 space-y-3 rounded-xl border border-border/70 bg-card/50 p-4 lg:sticky lg:top-0 lg:self-start"
            data-testid="critic-run-results"
          >
            {mutation.isPending ? (
              <p
                className="flex items-center gap-2 text-sm text-muted-foreground"
                role="status"
              >
                <LoaderCircle
                  aria-hidden="true"
                  className="size-4 animate-spin"
                />
                Separate review passes are running on the same snapshot…
              </p>
            ) : result ? (
              <>
                {resultInputKey !== currentRunDefinitionKey ? (
                  <p
                    className="rounded-lg border border-amber-500/25 bg-amber-500/5 p-3 text-xs leading-relaxed text-muted-foreground"
                    role="status"
                  >
                    The review setup changed after this run. These results use
                    the previous settings.
                  </p>
                ) : null}
                <RunResults result={result} />
              </>
            ) : error ? (
              <p
                className="flex items-start gap-2 text-sm text-destructive"
                role="alert"
              >
                <AlertCircle
                  aria-hidden="true"
                  className="mt-0.5 size-4 shrink-0"
                />
                {error}
              </p>
            ) : (
              <p className="flex min-h-56 items-center justify-center text-center text-sm text-muted-foreground">
                Results will appear here after you run a review.
              </p>
            )}
          </section>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function Field({
  children,
  hint,
  htmlFor,
  label,
}: {
  children: React.ReactNode;
  hint?: string;
  htmlFor: string;
  label: string;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium" htmlFor={htmlFor}>
        {label}
      </label>
      {hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}
      {children}
    </div>
  );
}

function ByteCount({ current, maximum }: { current: number; maximum: number }) {
  return (
    <p
      className={`text-right text-[11px] ${current > maximum ? "text-destructive" : "text-muted-foreground"}`}
    >
      {current.toLocaleString()} / {maximum.toLocaleString()} bytes
    </p>
  );
}

function RunResults({ result }: { result: CriticRunResult }) {
  return (
    <div className="space-y-4">
      <div>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 className="text-sm font-semibold">Review results</h3>
          <span
            className={
              result.ledgerStatus === "saved"
                ? "text-xs text-emerald-600 dark:text-emerald-400"
                : "text-xs text-amber-700 dark:text-amber-300"
            }
          >
            {result.ledgerStatus === "saved"
              ? "Saved to local history"
              : "Not saved to history"}
          </span>
        </div>
        <p className="mt-1 text-xs text-muted-foreground">
          {result.reviewers.length} reviewer
          {result.reviewers.length === 1 ? "" : "s"} · up to{" "}
          {result.limits.outputTokensPerReviewer} output tokens each ·{" "}
          {result.limits.timeLimitSecondsPerReviewer}s per reviewer
        </p>
      </div>

      {result.ledgerStatus === "not_saved" ? (
        <p className="rounded-lg border border-amber-500/25 bg-amber-500/5 p-3 text-xs text-muted-foreground">
          Results are available here, but Buzz could not save this round to the
          local identity history.
        </p>
      ) : null}

      {result.reviewers.map((reviewer, index) => (
        <article
          className="space-y-2 rounded-lg border border-border/70 p-3"
          // biome-ignore lint/suspicious/noArrayIndexKey: reviewers have a stable order within this immutable round result
          key={`${reviewer.role}-${index}`}
        >
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <h4 className="text-sm font-semibold capitalize">
              {reviewer.role.replaceAll("_", " ")}
            </h4>
            <span
              className={
                reviewer.status === "completed"
                  ? "text-xs text-emerald-600 dark:text-emerald-400"
                  : "text-xs text-destructive"
              }
            >
              {reviewer.status}
            </span>
          </div>
          <p className="text-xs text-muted-foreground">
            {[reviewer.providerId, reviewer.modelId]
              .filter(Boolean)
              .join(" · ") || "Model metadata unavailable"}
            {reviewer.elapsedMs !== null
              ? ` · ${(reviewer.elapsedMs / 1000).toFixed(1)}s`
              : ""}
            {reviewer.outputTruncated ? " · output truncated" : ""}
          </p>
          {reviewer.routeProfile ? (
            <p className="break-all font-mono text-[11px] text-muted-foreground">
              Route {reviewer.routeProfile.id} v{reviewer.routeProfile.version}{" "}
              · {reviewer.routeProfile.hash.slice(0, 12)}…
            </p>
          ) : null}
          {reviewer.estimatedCostLimitMicrousd != null ? (
            <p className="text-xs text-muted-foreground">
              Estimated per-reviewer ceiling: {"$"}
              {formatEstimatedCriticBudgetUsd(
                reviewer.estimatedCostLimitMicrousd,
              )}
              . Estimate only; provider usage or billing may differ.
            </p>
          ) : null}
          {reviewer.output ? (
            <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-background/70 p-3 font-sans text-sm leading-relaxed">
              {reviewer.output}
            </pre>
          ) : reviewer.errorCode ? (
            <p className="text-sm text-muted-foreground">
              Review unavailable ({criticFailureLabel(reviewer.errorCode)}).
            </p>
          ) : null}
        </article>
      ))}

      <details className="rounded-lg border border-border/70 p-3 text-xs">
        <summary className="cursor-pointer font-medium">Run details</summary>
        <div className="mt-3 space-y-2 text-muted-foreground">
          <p>{result.execution}.</p>
          <p>{result.dataBoundary}</p>
          <p>{result.independence}</p>
          {result.limits.estimatedRoundCostBudgetMicrousd !== null ? (
            <p>
              Requested round estimate ceiling: {"$"}
              {formatEstimatedCriticBudgetUsd(
                result.limits.estimatedRoundCostBudgetMicrousd,
              )}
              . Reviewer estimate ceilings are applied independently; actual
              provider usage or billing may differ.
            </p>
          ) : null}
          <p className="break-all font-mono">
            Snapshot SHA-256: {result.snapshotSha256}
          </p>
          <ul className="space-y-1">
            {Object.entries(result.routeProfiles).map(([role, profile]) => (
              <li className="break-all font-mono" key={role}>
                {role}: {profile.id} v{profile.version} · {profile.hash}
              </li>
            ))}
          </ul>
          {result.roundId ? (
            <p className="break-all font-mono">Local round: {result.roundId}</p>
          ) : null}
          {result.limits.thinkingEffortRequested ? (
            <p>
              Requested reasoning: {result.limits.thinkingEffortRequested}.{" "}
              {result.limits.thinkingEffortNote}
            </p>
          ) : null}
        </div>
      </details>
    </div>
  );
}

function errorText(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  if (typeof reason === "string" && reason.trim()) return reason;
  return "The critic round could not be completed.";
}
