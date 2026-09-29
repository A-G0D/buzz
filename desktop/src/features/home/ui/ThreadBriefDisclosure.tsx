import {
  AlertCircle,
  Check,
  ChevronDown,
  Copy,
  FileText,
  LoaderCircle,
  RefreshCw,
} from "lucide-react";
import * as React from "react";

import { formatTime } from "@/features/messages/lib/dateFormatters";
import { getThreadBrief } from "@/shared/api/tauri";
import type { ThreadBriefResponse } from "@/shared/api/types";
import { cn } from "@/shared/lib/cn";
import { Button } from "@/shared/ui/button";
import { useCopyFeedback } from "@/shared/ui/HoverCopyIndicator";

type ThreadBriefDisclosureProps = {
  channelId: string;
  compact?: boolean;
  rootEventId: string;
};

function effectiveControlsSummary(
  events: ThreadBriefResponse["status"]["managed_turns"][number]["recent_events"],
) {
  const details = events.find(
    (event) => event.kind === "turn_started",
  )?.details;
  const snapshot = details?.effective_controls;
  if (typeof snapshot !== "object" || snapshot === null) return null;

  const controls = snapshot as Record<string, unknown>;
  const slots = controls.configured_worker_pool_slots;
  const idleTimeout = controls.idle_timeout_secs;
  const maxDuration = controls.max_turn_duration_secs;
  if (
    typeof slots !== "number" ||
    typeof idleTimeout !== "number" ||
    typeof maxDuration !== "number"
  ) {
    return null;
  }

  const policy = details?.resource_policy_v1;
  const metrics =
    typeof policy === "object" && policy !== null
      ? (policy as Record<string, unknown>).metrics
      : null;
  const unknownLabels = Array.isArray(metrics)
    ? metrics.flatMap((metric) => {
        if (typeof metric !== "object" || metric === null) return [];
        const entry = metric as Record<string, unknown>;
        return entry.state === "unknown" && typeof entry.label === "string"
          ? [entry.label]
          : [];
      })
    : null;
  const availability = unknownLabels
    ? ` Unknown at dispatch: ${unknownLabels.join(", ")}.`
    : " Token, spend, and memory caps are unavailable.";

  return `At dispatch: ACP pool ${slots} slots · idle limit ${idleTimeout}s · turn limit ${maxDuration}s.${availability}`;
}

function agentProfileSummary(
  events: ThreadBriefResponse["status"]["managed_turns"][number]["recent_events"],
) {
  const candidate = events.find((event) => event.kind === "turn_started")
    ?.details?.agent_profile_v1;
  if (typeof candidate !== "object" || candidate === null) {
    return null;
  }

  const configured = candidate as Record<string, unknown>;
  if (configured.schema_version !== 1) return null;
  const harness =
    typeof configured.harness_id === "string"
      ? configured.harness_id
      : "unknown";
  const provider =
    typeof configured.provider_id === "string"
      ? configured.provider_id
      : "not set";
  const model =
    typeof configured.model_id === "string" ? configured.model_id : "not set";
  const promptFingerprint =
    typeof configured.agent_prompt_sha256 === "string"
      ? ` · prompt fingerprint ${configured.agent_prompt_sha256.slice(0, 12)}…`
      : " · no configured agent prompt";
  return `Profile at dispatch: ${harness} · ${provider} · ${model}${promptFingerprint}. Prompt text is not stored; this snapshot does not prove the adapter applied it.`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function shortSafeLabel(value: unknown, maxLength = 120) {
  if (
    typeof value !== "string" ||
    value.trim().length === 0 ||
    Array.from(value).some((character) => {
      const codePoint = character.codePointAt(0);
      return (
        codePoint !== undefined && (codePoint < 0x20 || codePoint === 0x7f)
      );
    })
  ) {
    return null;
  }
  const normalized = value.trim();
  return normalized.length > maxLength
    ? `${normalized.slice(0, maxLength - 1)}…`
    : normalized;
}

const routeReasonLabels: Record<string, string> = {
  safety_refusal: "Safety refusal",
  invalid_preference_configuration: "Invalid preference settings",
  unknown_preference: "Preferred candidate is unknown",
  ineligible_preference: "Preferred candidate is ineligible",
  manual_override_model_not_listed:
    "Manual model is not listed in the active route profile",
  manual_override_model_ambiguous:
    "Manual model matches multiple active route candidates",
  no_eligible_candidate: "No candidate met the route requirements",
  context_capacity_insufficient:
    "No candidate fits the conservative UTF-8 upper-bound estimate",
  context_fit_unavailable:
    "Could not estimate strict context fit; request was skipped",
  multiple_eligible_without_preference:
    "Several candidates qualified without a preference",
  route_abstained: "Reason unavailable",
  route_configuration_invalid: "Route configuration is invalid",
  provider_client_initialization_failed:
    "Provider connection could not initialize",
  provider_connection_unconfigured: "Provider connection is not configured",
  explicit_session_model_override: "Manual model choice passed profile gates",
};

function routeDecisionSummary(
  events: ThreadBriefResponse["status"]["managed_turns"][number]["recent_events"],
) {
  const event = events.find(
    (candidate) => candidate.kind === "route_decision_v1",
  );
  if (!event) return null;
  if (!isRecord(event.details)) return "Route decision unavailable.";

  const details = event.details;
  const profileId = shortSafeLabel(details.profileId, 64);
  const profileVersion = details.profileVersion;
  const profile =
    profileId &&
    typeof profileVersion === "number" &&
    Number.isInteger(profileVersion) &&
    profileVersion > 0
      ? ` · profile ${profileId} v${profileVersion}`
      : "";

  if (details.outcome === "selected") {
    const provider = shortSafeLabel(details.providerId, 48);
    const model = shortSafeLabel(details.modelId);
    const target =
      provider && model
        ? `${provider} · ${model}`
        : "provider or model unavailable";
    const contextFit = isRecord(details.contextFit) ? details.contextFit : null;
    const inputUpperBound = contextFit?.inputTokensUpperBound;
    const capacity = contextFit?.capacityTokens;
    const contextSummary =
      contextFit?.capacitySource === "operator_declared" &&
      contextFit?.estimateMethod ===
        "utf8_bytes_plus_framing_and_output_reserve_v1" &&
      typeof inputUpperBound === "number" &&
      Number.isSafeInteger(inputUpperBound) &&
      inputUpperBound > 0 &&
      typeof capacity === "number" &&
      Number.isSafeInteger(capacity) &&
      capacity >= inputUpperBound
        ? ` · conservative UTF-8 upper-bound estimate ${inputUpperBound} tokens; operator-declared capacity ${capacity} tokens`
        : "";
    return `Selected before request · ${target}${contextSummary}${profile}`;
  }

  const reason =
    typeof details.reasonCode === "string" &&
    Object.hasOwn(routeReasonLabels, details.reasonCode)
      ? routeReasonLabels[details.reasonCode]
      : undefined;
  if (details.outcome === "overridden") {
    const candidateId = shortSafeLabel(details.candidateId, 64);
    const provider = shortSafeLabel(details.providerId, 48);
    const model = shortSafeLabel(details.modelId);
    if (
      details.reasonCode === "explicit_session_model_override" &&
      candidateId &&
      provider &&
      model
    ) {
      return `Pinned before request · reported candidate ${candidateId} · ${provider} · ${model} · ${reason}; provider request outcome unknown${profile}`;
    }
    return `Manual model override recorded · profile gate result unknown; provider request outcome unknown${profile}`;
  }

  const outcome =
    details.outcome === "abstained"
      ? "Abstained"
      : details.outcome === "refused"
        ? "Refused"
        : null;
  if (!outcome) return "Route decision unavailable.";
  return `${outcome} · ${reason ?? "Reason unavailable"}${profile}`;
}

function truncateCopiedText(value: string, maxLength: number) {
  if (value.length <= maxLength) return value;
  return `${value.slice(0, maxLength)}… [text truncated]`;
}

function formatObservationTime(observedAtMs?: number) {
  if (
    typeof observedAtMs !== "number" ||
    !Number.isFinite(observedAtMs) ||
    observedAtMs <= 0
  ) {
    return "Observation time unavailable";
  }
  return new Date(observedAtMs).toLocaleString();
}

function threadStatusMarkdown(brief: ThreadBriefResponse) {
  const lines = [
    "# Thread status snapshot",
    `- Evidence observed at: ${formatObservationTime(brief.status.observed_at_ms)}`,
    `- Original intent source: ${brief.original_intent.id}`,
    "- Task completion: unknown",
    `- Replies loaded: ${brief.status.reply_event_count}`,
    `- Thread evidence may be truncated: ${brief.status.possibly_truncated ? "yes" : "not reported"}`,
    "",
    "## Original intent",
    truncateCopiedText(
      brief.original_intent.content || "(No text content)",
      3000,
    ),
    "",
    "## Recent progress",
  ];
  const recentEvents = brief.progress_events.slice(-3).reverse();
  if (recentEvents.length === 0) {
    lines.push("No reply events were returned.");
  } else {
    for (const event of recentEvents) {
      lines.push(
        `- ${truncateCopiedText(event.content || "(No text content)", 2000)}`,
        `  Source event: ${event.id}`,
      );
    }
  }
  lines.push("", "## Local run evidence");
  if (brief.status.coordinator_runs.length === 0) {
    lines.push("No source-linked coordinator runs are recorded.");
  } else {
    for (const run of brief.status.coordinator_runs) {
      lines.push(
        `- Run ${run.run_id}: ${run.attempt_turns.length} recorded attempt(s); task state unknown.`,
      );
    }
  }
  if (brief.status.managed_turns.length > 0) {
    lines.push("", "Managed attempts:");
    for (const { turn } of brief.status.managed_turns) {
      lines.push(
        `- ${turn.turn_id}: last recorded ${turn.status}; worker liveness unknown.`,
      );
    }
  }
  lines.push(
    "",
    "## Evidence limits",
    brief.status.next_cursor
      ? "More reply pages are available; this snapshot contains only the currently loaded page(s)."
      : "No additional reply page was reported.",
    brief.status.depth_limit_may_truncate
      ? `Nested replies beyond depth ${brief.status.applied_depth_limit} may be omitted.`
      : "No depth truncation was reported.",
    `Local attempt capture is best-effort; ${brief.status.coordinator_run_lookup.capture_gap_count} journal write failure(s) are recorded for this local identity journal.`,
    "Task completion and worker liveness remain unknown unless separate evidence proves them.",
  );
  return lines.join("\n");
}

/** On-demand, source-backed thread context. This view does not infer completion. */
export function ThreadBriefDisclosure({
  channelId,
  compact = false,
  rootEventId,
}: ThreadBriefDisclosureProps) {
  const [open, setOpen] = React.useState(false);
  const [brief, setBrief] = React.useState<ThreadBriefResponse | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [loadingMore, setLoadingMore] = React.useState(false);
  const requestId = React.useRef(0);
  const panelId = React.useId();

  const loadBrief = React.useCallback(async () => {
    const request = ++requestId.current;
    setLoading(true);
    setLoadingMore(false);
    setError(null);
    try {
      const result = await getThreadBrief(rootEventId, channelId, {
        limit: 30,
      });
      if (request === requestId.current) setBrief(result);
    } catch {
      if (request === requestId.current) {
        setError("Couldn’t load this thread brief.");
      }
    } finally {
      if (request === requestId.current) setLoading(false);
    }
  }, [channelId, rootEventId]);

  React.useEffect(() => {
    if (!open) return;
    void loadBrief();
    return () => {
      requestId.current += 1;
    };
  }, [loadBrief, open]);

  const currentBrief = brief?.thread_root_id === rootEventId ? brief : null;
  const snapshotText = currentBrief ? threadStatusMarkdown(currentBrief) : "";
  const { copied, copy } = useCopyFeedback({
    label: "thread status snapshot",
    value: snapshotText,
  });
  const loadMore = React.useCallback(async () => {
    const cursor = currentBrief?.status.next_cursor;
    if (!cursor || !currentBrief) return;

    const request = ++requestId.current;
    setLoadingMore(true);
    setError(null);
    try {
      const page = await getThreadBrief(rootEventId, channelId, {
        limit: 30,
        cursor: {
          createdAt: cursor.created_at,
          eventId: cursor.event_id,
        },
      });
      if (request === requestId.current) {
        setBrief((previous) => {
          if (!previous || previous.thread_root_id !== rootEventId) return page;
          const progressById = new Map(
            [...previous.progress_events, ...page.progress_events].map(
              (event) => [event.id, event],
            ),
          );
          const progressEvents = [...progressById.values()].sort(
            (left, right) =>
              left.created_at - right.created_at ||
              left.id.localeCompare(right.id),
          );
          const auxiliaryById = new Map(
            [...previous.auxiliary_events, ...page.auxiliary_events].map(
              (event) => [event.id, event],
            ),
          );
          const turnsById = new Map(
            [
              ...previous.status.managed_turns,
              ...page.status.managed_turns,
            ].map((entry) => [entry.turn.turn_id, entry]),
          );
          const managedTurns = [...turnsById.values()].sort(
            (left, right) => right.turn.updated_at_ms - left.turn.updated_at_ms,
          );
          const runsById = new Map(
            [
              ...previous.status.coordinator_runs,
              ...page.status.coordinator_runs,
            ].map((run) => [run.run_id, run]),
          );
          const coordinatorRuns = [...runsById.values()].sort(
            (left, right) => right.updated_at_ms - left.updated_at_ms,
          );
          return {
            ...page,
            progress_events: progressEvents,
            auxiliary_events: [...auxiliaryById.values()],
            source_event_ids: [
              ...new Set([
                ...previous.source_event_ids,
                ...page.source_event_ids,
              ]),
            ],
            status: {
              ...page.status,
              reply_event_count: progressEvents.length,
              managed_turns: managedTurns.slice(0, 20),
              managed_turn_lookup: {
                ...page.status.managed_turn_lookup,
                has_more_turns:
                  previous.status.managed_turn_lookup.has_more_turns ||
                  page.status.managed_turn_lookup.has_more_turns ||
                  managedTurns.length > 20,
              },
              coordinator_runs: coordinatorRuns.slice(0, 20),
              coordinator_run_lookup: {
                ...page.status.coordinator_run_lookup,
                has_more_runs:
                  previous.status.coordinator_run_lookup.has_more_runs ||
                  page.status.coordinator_run_lookup.has_more_runs ||
                  coordinatorRuns.length > 20,
              },
            },
          };
        });
      }
    } catch {
      if (request === requestId.current) {
        setError("Couldn’t load the next page of thread replies.");
      }
    } finally {
      if (request === requestId.current) setLoadingMore(false);
    }
  }, [channelId, currentBrief, rootEventId]);

  const recentProgress =
    currentBrief?.progress_events.slice(-3).reverse() ?? [];

  return (
    <>
      <Button
        aria-controls={panelId}
        aria-expanded={open}
        className="h-8 gap-1.5 rounded-full px-2.5 text-xs text-muted-foreground"
        data-testid="thread-brief-toggle"
        onClick={() => setOpen((value) => !value)}
        type="button"
        variant="ghost"
      >
        <FileText aria-hidden="true" className="h-3.5 w-3.5" />
        <span>Brief</span>
        <ChevronDown
          aria-hidden="true"
          className={cn(
            "h-3.5 w-3.5 transition-transform",
            open && "rotate-180",
          )}
        />
      </Button>

      {open ? (
        <section
          aria-label="Thread brief"
          className={cn(
            "border-t border-border/60 bg-muted/15 px-5 pb-3 pt-2",
            compact && "rounded-md border border-border/60 px-2.5",
          )}
          data-testid="thread-brief-panel"
          id={panelId}
        >
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <p className="text-xs font-semibold text-foreground">
                Thread context
              </p>
              <p
                className="text-2xs text-muted-foreground"
                data-testid="thread-brief-observed-at"
              >
                Evidence observed at{" "}
                {formatObservationTime(currentBrief?.status.observed_at_ms)}
                {" · Source events · completion status unknown"}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-1">
              {currentBrief ? (
                <Button
                  aria-label="Copy thread status snapshot"
                  className="h-7 w-7 rounded-full text-muted-foreground"
                  data-testid="thread-brief-copy"
                  onClick={() => void copy()}
                  size="icon"
                  title="Copy source-linked status"
                  type="button"
                  variant="ghost"
                >
                  {copied ? (
                    <Check aria-hidden="true" />
                  ) : (
                    <Copy aria-hidden="true" />
                  )}
                </Button>
              ) : null}
              <Button
                aria-label="Refresh thread brief"
                className="h-7 w-7 rounded-full text-muted-foreground"
                data-testid="thread-brief-refresh"
                disabled={loading || loadingMore}
                onClick={() => void loadBrief()}
                size="icon"
                type="button"
                variant="ghost"
              >
                {loading ? (
                  <LoaderCircle aria-hidden="true" className="animate-spin" />
                ) : (
                  <RefreshCw aria-hidden="true" />
                )}
              </Button>
            </div>
          </div>

          {error ? (
            <p
              className="mt-2 flex items-center gap-1.5 text-xs text-destructive"
              data-testid="thread-brief-error"
              role="status"
            >
              <AlertCircle
                aria-hidden="true"
                className="h-3.5 w-3.5 shrink-0"
              />
              {error}
            </p>
          ) : null}

          {!currentBrief && loading ? (
            <p
              className="mt-2 flex items-center gap-2 text-xs text-muted-foreground"
              data-testid="thread-brief-loading"
              role="status"
            >
              <LoaderCircle
                aria-hidden="true"
                className="h-3.5 w-3.5 animate-spin"
              />
              Loading source events…
            </p>
          ) : null}

          {currentBrief ? (
            <>
              <div className="mt-2 rounded-md border border-border/60 bg-background/70 px-3 py-2">
                <p className="text-2xs font-medium text-muted-foreground">
                  Deterministic snapshot
                </p>
                <p
                  className="mt-1 text-xs text-foreground"
                  data-testid="thread-brief-summary"
                >
                  {currentBrief.summary.text}
                </p>
              </div>
              <div
                className={cn(
                  "mt-2 grid gap-2",
                  !compact && "sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]",
                )}
              >
                <div className="min-w-0 rounded-md border border-border/60 bg-background/70 px-3 py-2">
                  <p className="text-2xs font-medium text-muted-foreground">
                    Original intent
                  </p>
                  <p className="mt-1 max-h-20 overflow-auto whitespace-pre-wrap break-words text-xs text-foreground">
                    {currentBrief.original_intent.content ||
                      "(No text content)"}
                  </p>
                  <p
                    className="mt-1 truncate font-mono text-2xs text-muted-foreground"
                    title={currentBrief.original_intent.id}
                  >
                    source {currentBrief.original_intent.id}
                  </p>
                </div>
                <div className="min-w-0 rounded-md border border-border/60 bg-background/70 px-3 py-2">
                  <p className="text-2xs font-medium text-muted-foreground">
                    Recent progress · {currentBrief.status.reply_event_count}{" "}
                    replies loaded
                  </p>
                  {recentProgress.length ? (
                    <ul className="mt-1 space-y-1.5">
                      {recentProgress.map((event) => (
                        <li className="min-w-0" key={event.id}>
                          <p className="line-clamp-2 whitespace-pre-wrap break-words text-xs text-foreground">
                            {event.content || "(No text content)"}
                          </p>
                          <p
                            className="truncate font-mono text-2xs text-muted-foreground"
                            title={event.id}
                          >
                            {formatTime(event.created_at)} · {event.id}
                          </p>
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p className="mt-1 text-xs text-muted-foreground">
                      No reply events were returned.
                    </p>
                  )}
                  {currentBrief.status.next_cursor ? (
                    <Button
                      className="mt-2 h-7 rounded-full px-2.5 text-xs"
                      data-testid="thread-brief-load-more"
                      disabled={loadingMore || loading}
                      onClick={() => void loadMore()}
                      type="button"
                      variant="outline"
                    >
                      {loadingMore ? (
                        <LoaderCircle
                          aria-hidden="true"
                          className="mr-1.5 h-3.5 w-3.5 animate-spin"
                        />
                      ) : null}
                      Continue to newer replies
                    </Button>
                  ) : null}
                  {currentBrief.status.depth_limit_may_truncate ? (
                    <p className="mt-1 text-2xs text-muted-foreground">
                      Nested replies beyond depth{" "}
                      {currentBrief.status.applied_depth_limit} may be omitted.
                    </p>
                  ) : null}
                </div>
              </div>
              <div className="mt-2 rounded-md border border-border/60 bg-background/70 px-3 py-2">
                <p className="text-2xs font-medium text-muted-foreground">
                  Queryable local run IDs
                </p>
                <p className="text-2xs text-muted-foreground">
                  IDs bind a source request to recorded ACP attempts. Task
                  completion remains unknown.
                </p>
                {currentBrief.status.coordinator_run_lookup
                  .capture_reliability === "best_effort" ? (
                  <p className="mt-1 text-2xs text-muted-foreground">
                    {currentBrief.status.coordinator_run_lookup
                      .capture_gap_count > 0
                      ? `${currentBrief.status.coordinator_run_lookup.capture_gap_count} local journal write failure(s) were recorded for this relay and identity; gaps may include other threads.`
                      : "Local capture is best-effort; uncommitted events may be missing after process exit or storage failure."}
                  </p>
                ) : null}
                {currentBrief.status.coordinator_runs.length ? (
                  <ul className="mt-1.5 grid gap-2 sm:grid-cols-2">
                    {currentBrief.status.coordinator_runs.map((run) => (
                      <li
                        className="min-w-0 rounded border border-border/50 px-2.5 py-2"
                        key={run.run_id}
                      >
                        <p className="text-xs text-foreground">
                          {run.attempt_turns.length} recorded attempt
                          {run.attempt_turns.length === 1 ? "" : "s"} · task
                          state unknown
                        </p>
                        <p
                          className="mt-0.5 truncate font-mono text-2xs text-muted-foreground"
                          title={run.run_id}
                        >
                          run {run.run_id}
                        </p>
                        <p
                          className="mt-0.5 truncate font-mono text-2xs text-muted-foreground"
                          title={run.original_intent_event_id}
                        >
                          intent source {run.original_intent_event_id}
                        </p>
                        {run.project_coordinate ? (
                          <p
                            className="mt-0.5 truncate font-mono text-2xs text-muted-foreground"
                            title={run.project_coordinate}
                          >
                            project {run.project_coordinate}
                          </p>
                        ) : null}
                        {run.project_link_conflict ? (
                          <p className="mt-1 text-2xs text-destructive">
                            Later attempts resolved a different project; the
                            original project link is retained.
                          </p>
                        ) : null}
                        {run.event_history_may_be_truncated ? (
                          <p className="mt-1 text-2xs text-muted-foreground">
                            Older run events are omitted.
                          </p>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="mt-1 text-xs text-muted-foreground">
                    No source-linked local coordinator runs are recorded for
                    this thread.
                  </p>
                )}
                {currentBrief.status.coordinator_run_lookup.has_more_runs ? (
                  <p className="mt-1.5 text-2xs text-muted-foreground">
                    More than 20 runs match; showing the latest 20.
                  </p>
                ) : null}
              </div>
              <div className="mt-2 rounded-md border border-border/60 bg-background/70 px-3 py-2">
                <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
                  <p className="text-2xs font-medium text-muted-foreground">
                    Local managed-agent attempts
                  </p>
                  <p className="text-2xs text-muted-foreground">
                    Transport evidence · task completion unknown
                  </p>
                </div>
                <p className="mt-0.5 text-2xs text-muted-foreground">
                  An adapter acknowledgement means accepted for delivery; it
                  does not prove the model saw the steer or finished the task.
                </p>
                {currentBrief.status.steering_controls?.length ? (
                  <div className="mt-2 rounded border border-border/50 px-2.5 py-2">
                    <p className="text-2xs font-medium text-muted-foreground">
                      Guidance delivery evidence
                    </p>
                    <ul className="mt-1 space-y-1">
                      {currentBrief.status.steering_controls.map((control) => (
                        <li
                          className="break-words text-2xs text-muted-foreground"
                          key={`${control.turn_id}:${control.source_event_id}`}
                        >
                          Agent slot {control.agent_index + 1} ·{" "}
                          {control.state.replaceAll("_", " ")} · model
                          observation unknown
                          <span
                            className="ml-1 font-mono"
                            title={control.source_event_id}
                          >
                            · {control.source_event_id.slice(0, 12)}…
                          </span>
                          {control.event_history_may_be_truncated ? (
                            <span> · older control events may be missing</span>
                          ) : null}
                        </li>
                      ))}
                    </ul>
                  </div>
                ) : null}
                {currentBrief.status.managed_turns.length ? (
                  <ul className="mt-1.5 grid gap-2 sm:grid-cols-2">
                    {currentBrief.status.managed_turns.map(
                      ({
                        turn,
                        recent_events,
                        event_history_may_be_truncated,
                      }) => {
                        const controlsSummary =
                          effectiveControlsSummary(recent_events);
                        const profileSummary =
                          agentProfileSummary(recent_events);
                        const routeSummary =
                          routeDecisionSummary(recent_events);
                        const lifecycleEvents = recent_events.filter(
                          (event) =>
                            event.kind !== "steer_submitted" &&
                            event.kind !== "steer_outcome" &&
                            event.kind !== "route_decision_v1",
                        );
                        return (
                          <li
                            className="min-w-0 rounded border border-border/50 px-2.5 py-2"
                            key={turn.turn_id}
                          >
                            <p className="text-xs text-foreground">
                              Agent slot {turn.agent_index + 1} · last recorded:{" "}
                              {turn.status}
                            </p>
                            <p
                              className="mt-0.5 truncate font-mono text-2xs text-muted-foreground"
                              title={turn.turn_id}
                            >
                              attempt {turn.turn_id}
                            </p>
                            <p className="mt-0.5 text-2xs text-muted-foreground">
                              Updated{" "}
                              {formatTime(
                                Math.floor(turn.updated_at_ms / 1000),
                              )}{" "}
                              · worker liveness unknown
                            </p>
                            {controlsSummary ? (
                              <p className="mt-1 text-2xs text-muted-foreground">
                                {controlsSummary}
                              </p>
                            ) : null}
                            {profileSummary ? (
                              <p className="mt-1 break-words text-2xs text-muted-foreground">
                                {profileSummary}
                              </p>
                            ) : null}
                            {routeSummary ? (
                              <p
                                className="mt-1 break-words text-2xs text-muted-foreground"
                                data-testid="thread-brief-route-decision"
                              >
                                Route: {routeSummary}
                              </p>
                            ) : null}
                            {lifecycleEvents.length ? (
                              <ul className="mt-1 space-y-0.5">
                                {lifecycleEvents
                                  .slice(-2)
                                  .reverse()
                                  .map((event) => {
                                    const outcome = event.details.outcome;
                                    const sourceId =
                                      event.details.source_event_id;
                                    return (
                                      <li
                                        className="break-words text-2xs text-muted-foreground"
                                        key={event.sequence}
                                      >
                                        {event.kind.replaceAll("_", " ")}
                                        {typeof outcome === "string"
                                          ? ` · ${outcome.replaceAll("_", " ")}`
                                          : ""}
                                        {typeof sourceId === "string" ? (
                                          <span
                                            className="ml-1 font-mono"
                                            title={sourceId}
                                          >
                                            · {sourceId.slice(0, 12)}…
                                          </span>
                                        ) : null}
                                      </li>
                                    );
                                  })}
                              </ul>
                            ) : null}
                            {event_history_may_be_truncated ? (
                              <p className="mt-1 text-2xs text-muted-foreground">
                                Older lifecycle events are omitted.
                              </p>
                            ) : null}
                          </li>
                        );
                      },
                    )}
                  </ul>
                ) : (
                  <p className="mt-1 text-xs text-muted-foreground">
                    No ACP attempts are recorded for this thread under the
                    current local identity. Other agents and runtimes may not
                    appear here.
                  </p>
                )}
                {currentBrief.status.managed_turn_lookup.has_more_turns ? (
                  <p className="mt-1.5 text-2xs text-muted-foreground">
                    More than 20 attempts match; showing the latest 20.
                  </p>
                ) : null}
              </div>
            </>
          ) : null}
        </section>
      ) : null}
    </>
  );
}
