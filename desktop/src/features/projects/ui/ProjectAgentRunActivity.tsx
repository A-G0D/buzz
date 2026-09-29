import { Bot, ChevronDown, LoaderCircle, RefreshCw } from "lucide-react";
import * as React from "react";

import { ThreadBriefDisclosure } from "@/features/home/ui/ThreadBriefDisclosure";
import { getProjectCoordinatorRuns } from "@/shared/api/tauri";
import { sendThreadGuidance } from "@/shared/api/tauriMessages";
import type {
  ProjectCoordinatorRunCursor,
  ProjectCoordinatorRunEvidence,
} from "@/shared/api/types";
import { cn } from "@/shared/lib/cn";
import { Button } from "@/shared/ui/button";
import { Textarea } from "@/shared/ui/textarea";

const PAGE_SIZE = 20;

export function ProjectAgentRunActivity({
  homeChannelId,
  identityPubkey,
  onOpenThread,
  projectCoordinate,
  relayUrl,
}: {
  homeChannelId: string;
  identityPubkey?: string;
  onOpenThread: (channelId: string, rootEventId: string) => void;
  projectCoordinate: string;
  relayUrl: string;
}) {
  const [open, setOpen] = React.useState(false);
  const [runs, setRuns] = React.useState<ProjectCoordinatorRunEvidence[]>([]);
  const [cursor, setCursor] =
    React.useState<ProjectCoordinatorRunCursor | null>(null);
  const [hasMoreCandidates, setHasMoreCandidates] = React.useState(false);
  const [attemptEvidenceMayBeTruncated, setAttemptEvidenceMayBeTruncated] =
    React.useState(false);
  const [firstPageCheckedAt, setFirstPageCheckedAt] = React.useState<
    number | null
  >(null);
  const [loading, setLoading] = React.useState(false);
  const [loaded, setLoaded] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [guidanceRunId, setGuidanceRunId] = React.useState<string | null>(null);
  const [guidanceDraft, setGuidanceDraft] = React.useState("");
  const [guidanceSending, setGuidanceSending] = React.useState(false);
  const [guidanceError, setGuidanceError] = React.useState<string | null>(null);
  const [guidancePosted, setGuidancePosted] = React.useState(false);
  const scopeKey = JSON.stringify([
    projectCoordinate,
    homeChannelId,
    relayUrl,
    identityPubkey ?? null,
  ]);
  const requestId = React.useRef(0);
  const activeScope = React.useRef(scopeKey);
  const guidanceSendingRef = React.useRef(false);

  React.useLayoutEffect(() => {
    if (activeScope.current === scopeKey) return;
    activeScope.current = scopeKey;
    requestId.current += 1;
    setRuns([]);
    setCursor(null);
    setHasMoreCandidates(false);
    setAttemptEvidenceMayBeTruncated(false);
    setFirstPageCheckedAt(null);
    setLoading(false);
    setLoaded(false);
    setError(null);
    setGuidanceRunId(null);
    setGuidanceDraft("");
    setGuidanceSending(false);
    setGuidanceError(null);
    setGuidancePosted(false);
    guidanceSendingRef.current = false;
  }, [scopeKey]);

  const loadPage = React.useCallback(
    async (pageCursor: ProjectCoordinatorRunCursor | null) => {
      const id = ++requestId.current;
      const requestedScope = scopeKey;
      setLoading(true);
      setError(null);
      try {
        const result = await getProjectCoordinatorRuns(
          projectCoordinate,
          homeChannelId,
          { limit: PAGE_SIZE, cursor: pageCursor },
        );
        if (
          requestId.current !== id ||
          activeScope.current !== requestedScope
        ) {
          return;
        }
        setRuns((current) => {
          if (!pageCursor) return result.runs;
          const byId = new Map(current.map((run) => [run.run_id, run]));
          for (const run of result.runs) byId.set(run.run_id, run);
          return [...byId.values()].sort(
            (left, right) =>
              right.updated_at_ms - left.updated_at_ms ||
              right.run_id.localeCompare(left.run_id),
          );
        });
        setCursor(result.next_cursor);
        setHasMoreCandidates(result.has_more_candidates);
        setAttemptEvidenceMayBeTruncated((current) =>
          pageCursor
            ? current || result.attempt_evidence_may_be_truncated
            : result.attempt_evidence_may_be_truncated,
        );
        if (!pageCursor) setFirstPageCheckedAt(Date.now());
        setLoaded(true);
      } catch (cause) {
        if (
          requestId.current !== id ||
          activeScope.current !== requestedScope
        ) {
          return;
        }
        setError(
          cause instanceof Error ? cause.message : "Could not load agent runs.",
        );
        setLoaded(true);
      } finally {
        if (
          requestId.current === id &&
          activeScope.current === requestedScope
        ) {
          setLoading(false);
        }
      }
    },
    [homeChannelId, projectCoordinate, scopeKey],
  );

  React.useEffect(() => {
    if (open && !loaded && !loading) void loadPage(null);
  }, [loadPage, loaded, loading, open]);

  React.useEffect(
    () => () => {
      requestId.current += 1;
    },
    [],
  );

  const toggleOpen = () => {
    setOpen((current) => !current);
  };

  const submitGuidance = async (
    event: React.FormEvent<HTMLFormElement>,
    run: Pick<
      ProjectCoordinatorRunEvidence,
      | "channel_id"
      | "original_intent_event_id"
      | "run_id"
      | "thread_root_event_id"
    >,
  ) => {
    event.preventDefault();
    if (!identityPubkey || guidanceSendingRef.current) return;
    const content = guidanceDraft.trim();
    if (!content) return;
    const requestedScope = scopeKey;
    guidanceSendingRef.current = true;
    setGuidanceSending(true);
    setGuidanceError(null);
    setGuidancePosted(false);
    try {
      await sendThreadGuidance({
        channelId: run.channel_id,
        content,
        expectedRelayUrl: relayUrl,
        expectedSignerPubkey: identityPubkey,
        rootEventId: run.thread_root_event_id ?? run.original_intent_event_id,
      });
      if (activeScope.current !== requestedScope) return;
      setGuidanceDraft("");
      setGuidancePosted(true);
    } catch (cause) {
      if (activeScope.current !== requestedScope) return;
      setGuidanceError(
        cause instanceof Error ? cause.message : "Could not post guidance.",
      );
    } finally {
      if (activeScope.current === requestedScope) {
        guidanceSendingRef.current = false;
        setGuidanceSending(false);
      }
    }
  };

  return (
    <section
      className="group/sidebar-section space-y-1"
      data-testid="project-agent-run-activity"
    >
      <div className="flex h-8 min-w-0 items-center justify-between gap-2 px-2">
        <button
          aria-expanded={open}
          className="group/section-label flex min-w-0 items-center gap-1 text-left text-xs font-medium text-sidebar-foreground/70"
          onClick={toggleOpen}
          type="button"
        >
          <Bot aria-hidden="true" className="size-3.5" />
          <span className="truncate">Agent activity</span>
          <ChevronDown
            aria-hidden="true"
            className={cn(
              "size-3 shrink-0 opacity-0 transition-[opacity,transform] group-hover/sidebar-section:opacity-100 group-focus-within/sidebar-section:opacity-100",
              open ? "rotate-0" : "-rotate-90",
            )}
          />
          {runs.length > 0 ? (
            <span className="tabular-nums text-sidebar-foreground/50">
              {runs.length}
            </span>
          ) : null}
        </button>
        {open && loaded && error ? (
          <Button
            aria-label="Retry loading agent activity"
            className="h-6 px-2 text-xs"
            disabled={loading}
            onClick={() => void loadPage(null)}
            size="sm"
            type="button"
            variant="ghost"
          >
            Retry
          </Button>
        ) : null}
        {open && loaded && !error ? (
          <Button
            aria-label="Refresh agent activity"
            className="size-6 p-0"
            disabled={loading}
            onClick={() => void loadPage(null)}
            size="sm"
            title="Refresh agent activity"
            type="button"
            variant="ghost"
          >
            <RefreshCw
              aria-hidden="true"
              className={cn("size-3.5", loading && "animate-spin")}
            />
          </Button>
        ) : null}
      </div>
      {open ? (
        <div className="space-y-1 px-2">
          {firstPageCheckedAt !== null ? (
            <p
              className="px-2 text-2xs text-sidebar-foreground/50"
              data-testid="project-agent-activity-first-page-checked"
            >
              First page last checked at{" "}
              {new Date(firstPageCheckedAt).toLocaleTimeString([], {
                hour: "numeric",
                minute: "2-digit",
              })}
            </p>
          ) : null}
          {attemptEvidenceMayBeTruncated ? (
            <p className="px-2 py-1 text-xs text-amber-600 dark:text-amber-400">
              Some attempt evidence was omitted by page safety limits.
            </p>
          ) : null}
          {loading && runs.length === 0 ? (
            <p
              className="flex items-center gap-2 px-2 py-2 text-xs text-sidebar-foreground/60"
              role="status"
            >
              <LoaderCircle
                aria-hidden="true"
                className="size-3 animate-spin"
              />
              Loading agent activity…
            </p>
          ) : null}
          {error ? (
            <p className="px-2 py-2 text-xs text-destructive" role="alert">
              {error}
            </p>
          ) : null}
          {loaded && !error && runs.length === 0 ? (
            <p className="px-2 py-2 text-xs text-sidebar-foreground/60">
              {hasMoreCandidates
                ? "No readable runs in this batch. Another batch is available."
                : "No readable agent runs were found."}
            </p>
          ) : null}
          {runs.map((run) => (
            <div
              className="rounded-md px-2 py-2 text-xs text-sidebar-foreground/80 hover:bg-sidebar-accent/60"
              key={run.run_id}
            >
              <div className="flex items-center justify-between gap-3">
                <span className="min-w-0 truncate font-medium">
                  Run {run.run_id.slice(0, 8)}
                </span>
                <span className="shrink-0 text-sidebar-foreground/50">
                  {run.attempt_turns.length} readable{" "}
                  {run.attempt_turns.length === 1 ? "attempt" : "attempts"}
                </span>
              </div>
              <div className="mt-1 flex items-center justify-between gap-3 text-sidebar-foreground/55">
                <span className="min-w-0 truncate">
                  Intent {run.original_intent_event_id.slice(0, 12)}
                </span>
                <span title="Worker liveness and task completion are unknown.">
                  Completion unverified
                </span>
              </div>
              <p className="mt-1 text-sidebar-foreground/55">
                Attempt history is best effort; completeness is unknown.
              </p>
              <p className="mt-1 text-sidebar-foreground/55">
                Worker liveness and task completion are unknown.
              </p>
              {run.attempt_history_may_be_truncated ||
              run.event_history_may_be_truncated ? (
                <p className="mt-1 text-amber-600 dark:text-amber-400">
                  Some retained attempt or event history is known to be
                  truncated.
                </p>
              ) : null}
              {run.project_link_conflict ? (
                <p className="mt-1 text-amber-600 dark:text-amber-400">
                  Project link changed during this run.
                </p>
              ) : null}
              <div className="mt-2 flex flex-wrap items-center gap-2">
                <ThreadBriefDisclosure
                  channelId={run.channel_id}
                  compact
                  rootEventId={
                    run.thread_root_event_id ?? run.original_intent_event_id
                  }
                />
                <Button
                  className="h-7 shrink-0 px-2 text-xs"
                  onClick={() =>
                    onOpenThread(
                      run.channel_id,
                      run.thread_root_event_id ?? run.original_intent_event_id,
                    )
                  }
                  size="sm"
                  type="button"
                  variant="outline"
                >
                  Open conversation
                </Button>
                {identityPubkey ? (
                  <Button
                    aria-expanded={guidanceRunId === run.run_id}
                    className="h-7 shrink-0 px-2 text-xs"
                    disabled={guidanceSending}
                    onClick={() => {
                      setGuidanceRunId((current) =>
                        current === run.run_id ? null : run.run_id,
                      );
                      setGuidanceDraft("");
                      setGuidanceError(null);
                      setGuidancePosted(false);
                    }}
                    size="sm"
                    type="button"
                    variant="outline"
                  >
                    Guide run
                  </Button>
                ) : null}
              </div>
              {guidanceRunId === run.run_id ? (
                <form
                  className="mt-2 space-y-2"
                  id={`project-run-guidance-form-${run.run_id}`}
                  onSubmit={(event) => void submitGuidance(event, run)}
                >
                  <label
                    className="sr-only"
                    htmlFor={`project-run-guidance-${run.run_id}`}
                  >
                    Guidance for run {run.run_id.slice(0, 8)}
                  </label>
                  <Textarea
                    autoComplete="off"
                    disabled={guidanceSending}
                    id={`project-run-guidance-${run.run_id}`}
                    maxLength={10_000}
                    onChange={(event) => setGuidanceDraft(event.target.value)}
                    placeholder="Add direction for this run…"
                    rows={3}
                    value={guidanceDraft}
                  />
                  <div className="flex items-center gap-2">
                    <Button
                      className="h-7 px-2 text-xs"
                      disabled={guidanceSending || !guidanceDraft.trim()}
                      size="sm"
                      type="submit"
                    >
                      {guidanceSending ? "Posting…" : "Send guidance"}
                    </Button>
                    <Button
                      className="h-7 px-2 text-xs"
                      disabled={guidanceSending}
                      onClick={() => setGuidanceRunId(null)}
                      size="sm"
                      type="button"
                      variant="ghost"
                    >
                      Close
                    </Button>
                  </div>
                  {guidancePosted ? (
                    <p className="text-sidebar-foreground/60" role="status">
                      Guidance posted to the thread. Agent delivery is not
                      confirmed.
                    </p>
                  ) : null}
                  {guidanceError ? (
                    <p className="text-destructive" role="alert">
                      {guidanceError}
                    </p>
                  ) : null}
                </form>
              ) : null}
            </div>
          ))}
          {hasMoreCandidates ? (
            <Button
              className="h-7 w-full justify-start px-2 text-xs"
              disabled={loading || !cursor}
              onClick={() => void loadPage(cursor)}
              size="sm"
              type="button"
              variant="ghost"
            >
              {loading ? "Loading…" : "Load more agent activity"}
            </Button>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
