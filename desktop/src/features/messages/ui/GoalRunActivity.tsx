import * as React from "react";

import { useCommunities } from "@/features/communities/useCommunities";
import { useIdentityQuery } from "@/shared/api/hooks";
import { invokeTauri, sendChannelMessage } from "@/shared/api/tauri";
import { startManagedAgent } from "@/shared/api/tauriManagedAgents";
import { normalizePubkey } from "@/shared/lib/pubkey";
import { Button } from "@/shared/ui/button";
import type { TimelineMessage } from "@/features/messages/types";

type GoalTask = {
  attemptTurnIds: string[];
  generation: number;
  assignedAgentPubkey: string | null;
  acceptanceCriteria: string;
  instructions: string;
  taskId: string;
  title: string;
  state: string;
  evidence: Array<unknown>;
};

type GoalRun = {
  goalRunId: string;
  goal: string;
  state: string;
  sourceEventId: string;
  threadRootEventId: string | null;
  maxParallel: number;
  scopeVersion: number;
  tasks: GoalTask[];
};

type GoalPlanTask = {
  title: string;
  instructions: string;
  acceptanceCriteria: string;
  dependsOn: number[];
};

function stateLabel(state: string) {
  return state.replaceAll("_", " ");
}

function nextTaskPrompt(run: GoalRun, task: GoalTask) {
  return [
    `Goal task assignment · run ${run.goalRunId}`,
    `Task: ${task.title}`,
    `Scope: ${task.instructions}`,
    `Acceptance: ${task.acceptanceCriteria}`,
    "Post [BUZZ_GOAL_REPORT] with this goal/task/generation, evidence, and needs_evidence or blocked state when finished.",
    `goal_run_id: ${run.goalRunId}`,
    `task_id: ${task.taskId}`,
    `generation: ${task.generation}`,
  ].join("\n");
}

function completedGoalReceipt(run: GoalRun) {
  const evidence = run.tasks.reduce(
    (total, task) => total + task.evidence.length,
    0,
  );
  return [
    `Goal run completed · ${run.goalRunId}`,
    `Goal: ${run.goal}`,
    `Accepted tasks: ${run.tasks.length}`,
    `Recorded evidence: ${evidence}`,
    "Open Goal Runs to inspect worker-turn receipts and evidence references.",
  ].join("\n");
}

function parseGoalReport(message: TimelineMessage) {
  if (!message.body.includes("[BUZZ_GOAL_REPORT]")) return null;
  const fields = new Map<string, string>();
  for (const line of message.body.split("\n")) {
    const separator = line.indexOf(":");
    if (separator <= 0) continue;
    fields.set(
      line.slice(0, separator).trim().toLowerCase(),
      line.slice(separator + 1).trim(),
    );
  }
  const goalRunId = fields.get("goal_run_id");
  const taskId = fields.get("task_id");
  const generation = Number(fields.get("generation"));
  const state = fields.get("state");
  const evidence = fields
    .get("evidence")
    ?.split(";")
    .map((value) => value.trim())
    .filter(Boolean);
  const reporterPubkey = message.signerPubkey ?? message.pubkey;
  if (
    !goalRunId ||
    !taskId ||
    !Number.isInteger(generation) ||
    generation < 1 ||
    (state !== "needs_evidence" && state !== "blocked") ||
    !reporterPubkey ||
    (state === "needs_evidence" && !evidence?.length)
  ) {
    return null;
  }
  return {
    evidence: evidence ?? [],
    generation,
    goalRunId,
    reporterPubkey,
    state,
    taskId,
  };
}

function parseGoalPlan(message: TimelineMessage) {
  const marker = "[BUZZ_GOAL_PLAN]";
  const markerAt = message.body.indexOf(marker);
  if (markerAt < 0) return null;
  const reporterPubkey = message.signerPubkey ?? message.pubkey;
  if (!reporterPubkey) return null;
  try {
    const parsed: unknown = JSON.parse(
      message.body.slice(markerAt + marker.length).trim(),
    );
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return null;
    }
    const value = parsed as Record<string, unknown>;
    if (
      typeof value.goal_run_id !== "string" ||
      typeof value.planner_task_id !== "string" ||
      !Number.isInteger(value.generation) ||
      !Array.isArray(value.tasks)
    ) {
      return null;
    }
    const tasks = value.tasks.map((task): GoalPlanTask | null => {
      if (!task || typeof task !== "object" || Array.isArray(task)) return null;
      const candidate = task as Record<string, unknown>;
      if (
        typeof candidate.title !== "string" ||
        typeof candidate.instructions !== "string" ||
        typeof candidate.acceptanceCriteria !== "string" ||
        !Array.isArray(candidate.dependsOn) ||
        !candidate.dependsOn.every(Number.isInteger)
      ) {
        return null;
      }
      return {
        title: candidate.title,
        instructions: candidate.instructions,
        acceptanceCriteria: candidate.acceptanceCriteria,
        dependsOn: candidate.dependsOn as number[],
      };
    });
    if (tasks.some((task) => task === null)) return null;
    return {
      generation: value.generation as number,
      goalRunId: value.goal_run_id,
      plannerTaskId: value.planner_task_id,
      reporterPubkey,
      tasks: tasks as GoalPlanTask[],
    };
  } catch {
    return null;
  }
}

/** Shows durable local goal state. Worker execution remains separately evidenced. */
export function GoalRunActivity({
  channelId,
  messages,
}: {
  channelId: string | null;
  messages: TimelineMessage[];
}) {
  const { activeCommunity } = useCommunities();
  const identityQuery = useIdentityQuery();
  const [runs, setRuns] = React.useState<GoalRun[]>([]);
  const [error, setError] = React.useState<string | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [acceptingTaskId, setAcceptingTaskId] = React.useState<string | null>(
    null,
  );
  const ingestedReportIds = React.useRef(new Set<string>());

  const refresh = React.useCallback(async () => {
    if (!channelId) {
      setRuns([]);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      setRuns(
        await invokeTauri<GoalRun[]>("list_goal_runs_for_channel", {
          channelId,
          limit: 3,
        }),
      );
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : "Could not read goal runs.",
      );
    } finally {
      setLoading(false);
    }
  }, [channelId]);

  React.useEffect(() => {
    void refresh();
  }, [refresh]);

  const acceptTask = React.useCallback(
    async (run: GoalRun, task: GoalTask) => {
      // Capture tenant scope before the first await. Both the publication and
      // runtime wake fail closed if a community switch lands mid-operation.
      const expectedRelayUrl = activeCommunity?.relayUrl?.trim()
        ? activeCommunity.relayUrl
        : undefined;
      const expectedSignerPubkey =
        normalizePubkey(identityQuery.data?.pubkey ?? "") || undefined;
      let acceptedLocally = false;
      setAcceptingTaskId(task.taskId);
      try {
        const updated = await invokeTauri<GoalRun>("accept_goal_task", {
          goalRunId: run.goalRunId,
          taskId: task.taskId,
          generation: task.generation,
        });
        acceptedLocally = true;
        const runningCount = updated.tasks.filter(
          (candidate) => candidate.state === "running",
        ).length;
        const readyTasks = updated.tasks
          .filter((candidate) => candidate.state === "ready")
          .slice(0, Math.max(0, updated.maxParallel - runningCount));
        if (channelId && updated.state === "completed") {
          if (!expectedRelayUrl || !expectedSignerPubkey) {
            throw new Error(
              "Evidence was accepted locally. Reconnect to the active community to publish the completion receipt.",
            );
          }
          await sendChannelMessage(
            channelId,
            completedGoalReceipt(updated),
            updated.sourceEventId,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            expectedRelayUrl,
            expectedSignerPubkey,
            updated.threadRootEventId ?? updated.sourceEventId,
            "goal_result",
          );
        } else if (channelId && readyTasks.length > 0) {
          if (!expectedRelayUrl || !expectedSignerPubkey) {
            throw new Error(
              "Evidence was accepted locally. Reconnect to the active community to dispatch the next task.",
            );
          }
          const coordinators = await invokeTauri<string[]>(
            "select_goal_coordinators",
            {
              candidatePubkeys: [],
              channelId,
              limit: readyTasks.length,
            },
          );
          if (coordinators.length === 0) {
            throw new Error(
              "Evidence was accepted locally, but this channel has no active managed agent to run the next task.",
            );
          }
          for (const [index, nextTask] of readyTasks.entries()) {
            const coordinator = coordinators[index % coordinators.length];
            if (!coordinator) break;
            const assignment = await sendChannelMessage(
              channelId,
              nextTaskPrompt(updated, nextTask),
              updated.sourceEventId,
              undefined,
              [coordinator],
              undefined,
              undefined,
              undefined,
              undefined,
              undefined,
              expectedRelayUrl,
              expectedSignerPubkey,
              updated.threadRootEventId ?? updated.sourceEventId,
              "goal_task",
            );
            await invokeTauri("bind_goal_task_assignment", {
              goalRunId: updated.goalRunId,
              taskId: nextTask.taskId,
              generation: nextTask.generation,
              sourceEventId: assignment.eventId,
              assignedAgentPubkey: coordinator,
            });
            await invokeTauri("start_goal_task", {
              goalRunId: updated.goalRunId,
              taskId: nextTask.taskId,
              generation: nextTask.generation,
            });
          }
          await Promise.all(
            [...new Set(coordinators)].map((coordinator) =>
              startManagedAgent(coordinator, {
                expectedRelayUrl,
                expectedSignerPubkey,
                replayFloorUnix: Math.floor(Date.now() / 1_000),
              }),
            ),
          );
        }
        await refresh();
      } catch (cause) {
        setError(
          cause instanceof Error
            ? cause.message
            : "Could not accept task evidence.",
        );
        if (acceptedLocally) await refresh();
      } finally {
        setAcceptingTaskId(null);
      }
    },
    [activeCommunity, channelId, identityQuery.data?.pubkey, refresh],
  );

  React.useEffect(() => {
    let cancelled = false;
    const ingest = async () => {
      for (const message of messages) {
        if (ingestedReportIds.current.has(message.id)) continue;
        const plan = parseGoalPlan(message);
        if (plan) {
          ingestedReportIds.current.add(message.id);
          try {
            const updated = await invokeTauri<GoalRun>(
              "append_goal_plan",
              plan,
            );
            const planner = updated.tasks.find(
              (task) => task.taskId === plan.plannerTaskId,
            );
            // The evidence report may have reached the timeline before this
            // plan message. Complete the same automatic planning handoff in
            // either arrival order.
            if (planner?.state === "needs_evidence") {
              await acceptTask(updated, planner);
            }
            if (!cancelled) await refresh();
          } catch {
            // A planner proposal is untrusted until native validation confirms
            // the assigned sender, generation, and bounded DAG.
          }
          continue;
        }
        const report = parseGoalReport(message);
        if (!report) continue;
        ingestedReportIds.current.add(message.id);
        try {
          const updated = await invokeTauri<GoalRun>(
            "ingest_goal_task_report",
            {
              ...report,
              reportEventId: message.id,
            },
          );
          const updatedTask = updated.tasks.find(
            (task) => task.taskId === report.taskId,
          );
          // A plan is the controller's own bounded graph artifact. Once the
          // assigned planner has supplied that graph and its signed evidence
          // report, advance without waiting for a human click so the goal can
          // actually begin its ready work.
          if (
            updated.scopeVersion > 1 &&
            updatedTask?.title === "Plan the work" &&
            updatedTask.state === "needs_evidence"
          ) {
            await acceptTask(updated, updatedTask);
          }
          if (!cancelled) await refresh();
        } catch {
          // Reports are untrusted chat text. The native command rejects
          // unassigned or stale reporters without surfacing a noisy toast.
        }
      }
    };
    void ingest();
    return () => {
      cancelled = true;
    };
  }, [acceptTask, messages, refresh]);

  React.useEffect(() => {
    const onGoalRunCreated = () => void refresh();
    window.addEventListener("buzz:goal-run-created", onGoalRunCreated);
    return () =>
      window.removeEventListener("buzz:goal-run-created", onGoalRunCreated);
  }, [refresh]);

  if (!channelId || (runs.length === 0 && !error && !loading)) return null;

  return (
    <section
      aria-label="Goal runs"
      className="mx-5 mb-2 rounded-xl border border-border/70 bg-background/80 px-3 py-2 text-xs shadow-sm"
    >
      <div className="flex items-center justify-between gap-3">
        <span className="font-medium">Goal runs</span>
        <Button
          className="h-6 px-2 text-[11px]"
          disabled={loading}
          onClick={() => void refresh()}
          size="sm"
          type="button"
          variant="ghost"
        >
          {loading ? "Loading…" : "Refresh"}
        </Button>
      </div>
      {error ? <p className="mt-1 text-destructive">{error}</p> : null}
      <div className="mt-1 space-y-1.5">
        {runs.map((run) => (
          <div
            className="rounded-lg bg-muted/45 px-2 py-1.5"
            key={run.goalRunId}
          >
            <div className="flex items-center justify-between gap-2">
              <span className="min-w-0 truncate font-medium">{run.goal}</span>
              <span className="shrink-0 capitalize text-muted-foreground">
                {stateLabel(run.state)}
              </span>
            </div>
            <ol className="mt-1 space-y-0.5 text-muted-foreground">
              {run.tasks.map((task) => (
                <li
                  className="flex items-center justify-between gap-2"
                  key={task.taskId}
                >
                  <span className="min-w-0 truncate">{task.title}</span>
                  <span className="shrink-0">
                    {stateLabel(task.state)} · {task.attemptTurnIds.length}{" "}
                    worker {task.attemptTurnIds.length === 1 ? "turn" : "turns"}{" "}
                    · {task.evidence.length} evidence
                  </span>
                  {task.state === "needs_evidence" ? (
                    <Button
                      className="h-6 px-2 text-[11px]"
                      disabled={acceptingTaskId === task.taskId}
                      onClick={() => void acceptTask(run, task)}
                      size="sm"
                      type="button"
                      variant="outline"
                    >
                      {acceptingTaskId === task.taskId
                        ? "Accepting…"
                        : "Accept evidence"}
                    </Button>
                  ) : null}
                </li>
              ))}
            </ol>
          </div>
        ))}
      </div>
    </section>
  );
}
