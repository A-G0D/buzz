import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { History, RefreshCw } from "lucide-react";

import {
  getCriticRound,
  getRecentCriticRounds,
} from "@/shared/api/tauriCriticRuns";
import type { CriticRoundRecord, CriticRoundSummary } from "@/shared/api/types";
import {
  criticFailureLabel,
  formatEstimatedCriticBudgetUsd,
} from "./criticCostBudget";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";

const HISTORY_KEY = ["critic-round-history"] as const;
const HISTORY_LIMIT = 20;

export function AgentCriticHistoryDialog({
  onOpenChange,
  open,
}: {
  onOpenChange: (open: boolean) => void;
  open: boolean;
}) {
  const [selectedId, setSelectedId] = React.useState<string | null>(null);
  const historyQuery = useQuery({
    queryKey: [...HISTORY_KEY, HISTORY_LIMIT],
    queryFn: () => getRecentCriticRounds(HISTORY_LIMIT),
    enabled: open,
  });
  const selectedQuery = useQuery({
    queryKey: [...HISTORY_KEY, selectedId],
    queryFn: () => getCriticRound(selectedId ?? ""),
    enabled: open && selectedId !== null,
  });

  React.useEffect(() => {
    const rounds = historyQuery.data;
    if (!rounds) return;
    if (selectedId && rounds.some((round) => round.roundId === selectedId)) {
      return;
    }
    setSelectedId(rounds[0]?.roundId ?? null);
  }, [historyQuery.data, selectedId]);

  const refresh = () => {
    void historyQuery.refetch();
    if (selectedId) void selectedQuery.refetch();
  };

  return (
    <Dialog onOpenChange={onOpenChange} open={open}>
      <DialogContent
        className="max-h-[90vh] overflow-hidden sm:max-w-4xl"
        data-testid="critic-history-dialog"
      >
        <DialogHeader className="pr-8">
          <DialogTitle className="flex items-center gap-2">
            <History aria-hidden="true" className="size-5 text-primary" />
            Critic history
          </DialogTitle>
          <DialogDescription>
            Review results saved locally for this Buzz identity. Source text is
            not included in the history.
          </DialogDescription>
        </DialogHeader>

        <div
          className="grid min-h-0 gap-4 overflow-y-auto md:grid-cols-[minmax(13rem,0.8fr)_minmax(0,1.6fr)]"
          data-testid="critic-history-scroll-region"
        >
          <section aria-label="Recent critic rounds" className="space-y-2">
            <div className="flex items-center justify-between gap-2">
              <h3 className="text-sm font-medium">Recent rounds</h3>
              <Button
                aria-label="Refresh critic history"
                disabled={historyQuery.isFetching}
                onClick={refresh}
                size="icon"
                type="button"
                variant="ghost"
              >
                <RefreshCw
                  aria-hidden="true"
                  className={historyQuery.isFetching ? "animate-spin" : ""}
                />
              </Button>
            </div>

            {historyQuery.isPending ? (
              <p className="py-6 text-sm text-muted-foreground" role="status">
                Loading critic history…
              </p>
            ) : historyQuery.isError ? (
              <div className="space-y-2 rounded-lg border border-destructive/30 p-3">
                <p className="text-sm text-destructive">
                  {errorText(
                    historyQuery.error,
                    "Could not load critic history.",
                  )}
                </p>
                <Button
                  onClick={() => void historyQuery.refetch()}
                  size="sm"
                  type="button"
                  variant="outline"
                >
                  Retry
                </Button>
              </div>
            ) : historyQuery.data.length === 0 ? (
              <p className="rounded-lg border border-border/70 px-3 py-5 text-sm text-muted-foreground">
                No critic rounds have been saved yet.
              </p>
            ) : (
              <div
                className="max-h-none space-y-1 overflow-visible pr-1 md:max-h-[55vh] md:overflow-y-auto"
                data-testid="critic-history-round-list"
              >
                {historyQuery.data.map((round) => (
                  <RoundButton
                    key={round.roundId}
                    onClick={() => setSelectedId(round.roundId)}
                    round={round}
                    selected={selectedId === round.roundId}
                  />
                ))}
              </div>
            )}
          </section>

          <section
            aria-label="Critic round details"
            className="min-h-64 rounded-xl border border-border/70 bg-card/50 p-4"
          >
            {!selectedId ? (
              <div className="flex min-h-56 items-center justify-center text-center text-sm text-muted-foreground">
                Select a saved round to inspect its findings.
              </div>
            ) : selectedQuery.isPending ? (
              <p className="py-8 text-sm text-muted-foreground" role="status">
                Loading review details…
              </p>
            ) : selectedQuery.isError ? (
              <p className="py-8 text-sm text-destructive" role="alert">
                {errorText(
                  selectedQuery.error,
                  "Could not load review details.",
                )}
              </p>
            ) : selectedQuery.data ? (
              <RoundDetails round={selectedQuery.data} />
            ) : (
              <p className="py-8 text-sm text-muted-foreground" role="status">
                This round is not available in the current identity history.
              </p>
            )}
          </section>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function RoundButton({
  onClick,
  round,
  selected,
}: {
  onClick: () => void;
  round: CriticRoundSummary;
  selected: boolean;
}) {
  const completed = round.reviewers.filter(
    (reviewer) => reviewer.status === "completed",
  ).length;
  const failed = round.reviewers.length - completed;
  return (
    <button
      aria-pressed={selected}
      className={`w-full rounded-lg border px-3 py-2.5 text-left transition-colors ${
        selected
          ? "border-primary/50 bg-primary/5"
          : "border-transparent hover:border-border/70 hover:bg-muted/50"
      }`}
      onClick={onClick}
      type="button"
    >
      <span className="block text-sm font-medium">
        {formatDate(round.createdAtMs)}
      </span>
      <span className="mt-1 block text-xs text-muted-foreground">
        {round.reviewers.map((reviewer) => reviewer.role).join(" · ")} ·{" "}
        {completed} completed{failed > 0 ? ` · ${failed} failed` : ""}
      </span>
    </button>
  );
}

function RoundDetails({ round }: { round: CriticRoundRecord }) {
  return (
    <div className="space-y-4">
      <div>
        <p className="text-xs text-muted-foreground">
          {formatDate(round.createdAtMs)} · {round.reviewers.length} reviewer
          {round.reviewers.length === 1 ? "" : "s"}
        </p>
        <h3 className="mt-1 break-all font-mono text-xs text-foreground/80">
          {round.roundId}
        </h3>
      </div>

      <dl className="grid grid-cols-2 gap-x-4 gap-y-2 rounded-lg bg-muted/40 p-3 text-xs sm:grid-cols-3">
        <Meta
          label="Output cap"
          value={`${round.settings.maxOutputTokens} tokens`}
        />
        <Meta label="Time cap" value={`${round.settings.timeLimitSeconds}s`} />
        <Meta
          label="Requested effort"
          value={round.settings.thinkingEffortRequested ?? "Provider default"}
        />
        <Meta
          label="Requested round estimate ceiling"
          value={
            round.settings.estimatedRoundCostBudgetMicrousd == null
              ? "Not set"
              : `$${formatEstimatedCriticBudgetUsd(round.settings.estimatedRoundCostBudgetMicrousd)}`
          }
        />
      </dl>

      <div className="space-y-3">
        {round.reviewers.map((reviewer, index) => (
          <article
            className="space-y-2 rounded-lg border border-border/70 p-3"
            // biome-ignore lint/suspicious/noArrayIndexKey: persisted reviewer rows keep an immutable order within each round
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
                Route {reviewer.routeProfile.id} v
                {reviewer.routeProfile.version} · {reviewer.routeProfile.hash}
              </p>
            ) : round.settings.routeProfile ? (
              <p className="break-all font-mono text-[11px] text-muted-foreground">
                Shared route {round.settings.routeProfile.id} v
                {round.settings.routeProfile.version} ·{" "}
                {round.settings.routeProfile.hash}
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
              <pre className="max-h-none overflow-visible whitespace-pre-wrap break-words rounded-md bg-background/70 p-3 font-sans text-sm leading-relaxed md:max-h-64 md:overflow-auto">
                {reviewer.output}
              </pre>
            ) : reviewer.errorCode ? (
              <p className="text-sm text-muted-foreground">
                Review unavailable ({criticFailureLabel(reviewer.errorCode)}).
              </p>
            ) : (
              <p className="text-sm text-muted-foreground">
                This reviewer returned no text.
              </p>
            )}
          </article>
        ))}
      </div>

      <details className="rounded-lg border border-border/60 px-3 py-2">
        <summary className="cursor-pointer text-xs font-medium">
          Source fingerprints
        </summary>
        <dl className="mt-3 space-y-2 text-xs">
          <HashMeta label="Snapshot" value={round.snapshotSha256} />
          <HashMeta label="Objective" value={round.objectiveSha256} />
          <HashMeta label="Scope" value={round.scopeSha256} />
          <HashMeta
            label="Coordinator guide reference"
            value={
              round.settings.coordinatorGuide
                ? `${round.settings.coordinatorGuide.path} · SHA-256 ${round.settings.coordinatorGuide.sha256}`
                : "Not recorded"
            }
          />
        </dl>
        <p className="mt-3 text-[11px] text-muted-foreground">
          This stored reference does not indicate that an AI coordinator used
          the guide.
        </p>
      </details>
    </div>
  );
}

function Meta({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="mt-0.5 font-medium">{value}</dd>
    </div>
  );
}

function HashMeta({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="break-all font-mono text-[10px]">{value}</dd>
    </div>
  );
}

function formatDate(timestamp: number) {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(timestamp);
}

function errorText(error: unknown, fallback: string) {
  return error instanceof Error ? error.message : fallback;
}
