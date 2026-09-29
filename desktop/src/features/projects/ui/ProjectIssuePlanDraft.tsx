import * as React from "react";
import { useQuery } from "@tanstack/react-query";

import { PersonaDropdownField } from "@/features/agents/ui/PersonaDropdownField";
import type { PersonaDropdownOption } from "@/features/agents/ui/agentConfigOptions";
import {
  buildProjectIssuePlanSnapshot,
  defaultProjectIssuePlan,
  hashProjectIssuePlanSnapshot,
  parsePositiveIntegerLimitDraft,
  projectIssuePlanStorageKey,
  readProjectIssuePlanDraft,
  serializeProjectIssuePlanSnapshot,
  verifyProjectIssuePlanApproval,
  writeProjectIssuePlanDraft,
  type ProjectIssuePlanChatHandoff,
  type SavedProjectIssuePlanDraft,
  type ProjectIssuePlanSourceTask,
} from "@/features/projects/lib/projectIssuePlan";
import type { ProjectIssue } from "@/features/projects/hooks";
import {
  listAgentRouteProfiles,
  readAgentRouteProfile,
} from "@/shared/api/tauriAgentRouteProfiles";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Textarea } from "@/shared/ui/textarea";
import { ProjectDetailSection } from "./ProjectDetailSection";

export function ProjectIssuePlanDraft({
  issue,
  onUseApprovedPlan,
  projectAddress,
  subtasks,
}: {
  issue: ProjectIssue;
  onUseApprovedPlan?: (handoff: ProjectIssuePlanChatHandoff) => boolean;
  projectAddress: string;
  subtasks: readonly ProjectIssue[];
}) {
  const sourceIssue = React.useMemo(() => planSourceTask(issue), [issue]);
  const sourceSubtasks = React.useMemo(
    () => subtasks.map(planSourceTask),
    [subtasks],
  );
  const storageKey = projectIssuePlanStorageKey(projectAddress, issue.id);
  const defaultText = defaultProjectIssuePlan(sourceIssue, sourceSubtasks);
  const [initialDraft] = React.useState(() => readLocalDraft(storageKey));
  const [planText, setPlanText] = React.useState(
    initialDraft?.planText ?? defaultText,
  );
  const [savedDraft, setSavedDraft] = React.useState(initialDraft);
  const [routeProfileId, setRouteProfileId] = React.useState(
    initialDraft?.routeProfileId ?? "",
  );
  const [routeCandidateId, setRouteCandidateId] = React.useState(
    initialDraft?.routeProfileCandidateId ?? "",
  );
  const [maxOutputTokensPerCall, setMaxOutputTokensPerCall] = React.useState(
    initialDraft?.maxOutputTokensPerCall ?? "",
  );
  const [maxTurnDurationSeconds, setMaxTurnDurationSeconds] = React.useState(
    initialDraft?.maxTurnDurationSeconds ?? "",
  );
  const routeProfilesQuery = useQuery({
    queryKey: ["agent-route-profiles"],
    queryFn: listAgentRouteProfiles,
  });
  const selectedRouteProfileSummary = routeProfilesQuery.data?.find(
    (profile) => profile.id === routeProfileId,
  );
  const routeProfileIsUnavailable = Boolean(
    routeProfileId && routeProfilesQuery.data && !selectedRouteProfileSummary,
  );
  const routeProfileDetailQuery = useQuery({
    queryKey: [
      "agent-route-profile-detail",
      routeProfileId,
      selectedRouteProfileSummary?.version,
      selectedRouteProfileSummary?.documentHash,
    ],
    queryFn: () => readAgentRouteProfile(routeProfileId),
    enabled: Boolean(selectedRouteProfileSummary),
  });
  const selectedRouteProfile =
    selectedRouteProfileSummary &&
    routeProfileDetailQuery.data?.id === selectedRouteProfileSummary.id &&
    routeProfileDetailQuery.data.version ===
      selectedRouteProfileSummary.version &&
    routeProfileDetailQuery.data.documentHash ===
      selectedRouteProfileSummary.documentHash
      ? routeProfileDetailQuery.data
      : undefined;
  const routeProfileIsUnverified = Boolean(
    selectedRouteProfileSummary &&
      !selectedRouteProfile &&
      !routeProfileDetailQuery.isLoading,
  );
  const selectedRouteCandidate = selectedRouteProfile?.document.candidates.find(
    (candidate) => candidate.id === routeCandidateId,
  );
  const routeCandidateIsUnavailable = Boolean(
    routeCandidateId && !selectedRouteCandidate,
  );
  const routeProfileOptions: PersonaDropdownOption[] = [
    { label: "No route profile pinned", value: "" },
    ...(routeProfilesQuery.data ?? []).map((profile) => ({
      label: `${profile.name} · ${profile.dataPolicy} · v${profile.version}`,
      value: profile.id,
    })),
    ...(routeProfileIsUnavailable
      ? [{ label: `${routeProfileId} · unavailable`, value: routeProfileId }]
      : []),
  ];
  const outputTokenLimit = parsePositiveIntegerLimitDraft(
    maxOutputTokensPerCall,
  );
  const turnDurationLimit = parsePositiveIntegerLimitDraft(
    maxTurnDurationSeconds,
  );
  const budgetIsValid =
    typeof outputTokenLimit === "number" &&
    typeof turnDurationLimit === "number";
  const budget = React.useMemo(
    () => ({
      maxOutputTokensPerCall:
        typeof outputTokenLimit === "number" ? outputTokenLimit : null,
      maxTurnDurationSeconds:
        typeof turnDurationLimit === "number" ? turnDurationLimit : null,
    }),
    [outputTokenLimit, turnDurationLimit],
  );
  const [verifiedApproval, setVerifiedApproval] = React.useState<{
    hash: string;
    snapshot: string;
  } | null>(null);
  const [isApproving, setIsApproving] = React.useState(false);
  const [message, setMessage] = React.useState("");

  const snapshot = React.useMemo(
    () =>
      buildProjectIssuePlanSnapshot({
        projectAddress,
        issue: sourceIssue,
        subtasks: sourceSubtasks,
        planText,
        routeProfile: selectedRouteProfileSummary
          ? {
              id: selectedRouteProfileSummary.id,
              version: selectedRouteProfileSummary.version,
              documentHash: selectedRouteProfileSummary.documentHash,
              dataPolicy: selectedRouteProfileSummary.dataPolicy,
            }
          : null,
        routeCandidate: selectedRouteCandidate
          ? {
              id: selectedRouteCandidate.id,
              provider: selectedRouteCandidate.provider,
              model: selectedRouteCandidate.model,
              dataLocation: selectedRouteCandidate.data_location,
            }
          : null,
        budget,
      }),
    [
      planText,
      projectAddress,
      selectedRouteProfileSummary,
      selectedRouteCandidate,
      sourceIssue,
      sourceSubtasks,
      budget,
    ],
  );
  const snapshotJson = serializeProjectIssuePlanSnapshot(snapshot);
  React.useEffect(() => {
    let cancelled = false;
    const draft = savedDraft;
    if (!draft?.approvedHash) {
      setVerifiedApproval(null);
      return () => {
        cancelled = true;
      };
    }
    void verifyProjectIssuePlanApproval(draft, snapshot).then((valid) => {
      if (!cancelled && valid) {
        setVerifiedApproval({
          hash: draft.approvedHash as string,
          snapshot: snapshotJson,
        });
      } else if (!cancelled) {
        setVerifiedApproval(null);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [savedDraft, snapshot, snapshotJson]);
  const approvalIsCurrent =
    savedDraft?.approvedHash !== null &&
    verifiedApproval?.hash === savedDraft?.approvedHash &&
    verifiedApproval?.snapshot === snapshotJson;
  const taskCount = sourceSubtasks.length || 1;

  function saveDraft(next: SavedProjectIssuePlanDraft) {
    if (typeof window === "undefined") {
      setMessage("Could not save this draft on the device.");
      return false;
    }
    try {
      if (!writeProjectIssuePlanDraft(window.localStorage, storageKey, next)) {
        setMessage("Could not save this draft on the device.");
        return false;
      }
    } catch {
      setMessage("Could not save this draft on the device.");
      return false;
    }
    setSavedDraft(next);
    return true;
  }

  function handleSave() {
    saveDraft({
      schemaVersion: 1,
      planText,
      approvedSnapshot: savedDraft?.approvedSnapshot ?? null,
      approvedHash: savedDraft?.approvedHash ?? null,
      routeProfileId: routeProfileId || null,
      routeProfileCandidateId: routeCandidateId || null,
      maxOutputTokensPerCall,
      maxTurnDurationSeconds,
      savedAt: Date.now(),
    });
  }

  async function handleApprove() {
    if (
      !selectedRouteProfile ||
      !selectedRouteProfileSummary ||
      !selectedRouteCandidate ||
      routeProfileIsUnavailable ||
      routeCandidateIsUnavailable ||
      !budgetIsValid
    ) {
      return;
    }
    setIsApproving(true);
    setMessage("");
    try {
      const approvedHash = await hashProjectIssuePlanSnapshot(snapshot);
      const didSave = saveDraft({
        schemaVersion: 1,
        planText,
        approvedSnapshot: snapshotJson,
        approvedHash,
        routeProfileId: routeProfileId || null,
        routeProfileCandidateId: routeCandidateId || null,
        maxOutputTokensPerCall,
        maxTurnDurationSeconds,
        savedAt: Date.now(),
      });
      if (didSave) {
        setMessage("Plan snapshot approved locally. No agent was started.");
      }
    } catch (error) {
      setMessage(
        error instanceof Error
          ? error.message
          : "Could not approve this plan snapshot.",
      );
    } finally {
      setIsApproving(false);
    }
  }

  return (
    <ProjectDetailSection
      count={taskCount}
      defaultOpen={false}
      testId="project-issue-plan-section"
      title="Agent plan"
    >
      <div className="space-y-3" data-testid="project-issue-plan">
        <p className="text-xs leading-relaxed text-muted-foreground">
          This draft is saved on this device. Its approval hash covers the
          current task, direct subtasks, exact plan text, selected route-profile
          revision, exact provider candidate, and per-call output and per-turn
          duration ceilings. Draft stays local. The selected profile’s data
          policy applies only to a future run; this draft records limits but
          does not enforce them, send a message, or start an agent.
        </p>
        <div
          className="space-y-2"
          data-testid="project-issue-plan-route-profile-field"
        >
          <label
            className="text-sm font-medium"
            htmlFor="project-issue-plan-route-profile"
          >
            Planned Buzz Agent route profile
          </label>
          <PersonaDropdownField
            disabled={routeProfilesQuery.isLoading}
            id="project-issue-plan-route-profile"
            onValueChange={(value) => {
              setRouteProfileId(value);
              setRouteCandidateId("");
              setMessage("");
            }}
            options={routeProfileOptions}
            placeholder={
              routeProfilesQuery.isLoading
                ? "Loading route profiles…"
                : "Choose a route profile"
            }
            value={routeProfileId}
          />
          {routeProfilesQuery.isError ? (
            <p className="text-xs text-destructive" role="status">
              Could not load route profiles. Existing selections cannot be
              approved until Buzz can verify the saved revision.
            </p>
          ) : routeProfileIsUnavailable ? (
            <p
              className="text-xs text-amber-700 dark:text-amber-300"
              role="status"
            >
              This saved profile is unavailable. Choose another profile or clear
              the selection before approving.
            </p>
          ) : selectedRouteProfileSummary ? (
            <p className="break-all text-xs text-muted-foreground">
              v{selectedRouteProfileSummary.version} ·{" "}
              {selectedRouteProfileSummary.dataPolicy}
              {" · "}
              SHA-256 <code>{selectedRouteProfileSummary.documentHash}</code>
            </p>
          ) : (
            <p className="text-xs text-muted-foreground">
              No route is pinned yet. Select a profile before treating this plan
              as ready for execution.
            </p>
          )}
        </div>
        {selectedRouteProfileSummary ? (
          <div
            className="space-y-2"
            data-testid="project-issue-plan-route-candidate-field"
          >
            <label
              className="text-sm font-medium"
              htmlFor="project-issue-plan-route-candidate"
            >
              Planned provider candidate
            </label>
            <PersonaDropdownField
              disabled={!selectedRouteProfile}
              id="project-issue-plan-route-candidate"
              onValueChange={(value) => {
                setRouteCandidateId(value);
                setMessage("");
              }}
              options={[
                { label: "Choose an exact candidate", value: "" },
                ...(selectedRouteProfile?.document.candidates ?? []).map(
                  (candidate) => ({
                    label: `${candidate.id} · ${candidate.provider}/${candidate.model} · ${candidate.data_location}`,
                    value: candidate.id,
                  }),
                ),
                ...(routeCandidateIsUnavailable
                  ? [
                      {
                        label: `${routeCandidateId} · unavailable`,
                        value: routeCandidateId,
                      },
                    ]
                  : []),
              ]}
              placeholder={
                routeProfileDetailQuery.isLoading
                  ? "Loading saved candidates…"
                  : "Choose a candidate"
              }
              value={routeCandidateId}
            />
            {routeProfileDetailQuery.isLoading ? (
              <p className="text-xs text-muted-foreground" role="status">
                Loading the saved candidate list…
              </p>
            ) : routeProfileIsUnverified ? (
              <p className="text-xs text-destructive" role="status">
                Could not verify the saved profile document and candidate list.
                Reload the profile before approving.
              </p>
            ) : routeCandidateIsUnavailable ? (
              <p
                className="text-xs text-amber-700 dark:text-amber-300"
                role="status"
              >
                This saved candidate is unavailable. Choose an available
                candidate before approving.
              </p>
            ) : selectedRouteCandidate ? (
              <p className="text-xs text-muted-foreground">
                Candidate pinned in the local approval snapshot. The runtime
                must still verify it before dispatch.
              </p>
            ) : selectedRouteProfile?.document.candidates.length === 0 ? (
              <p
                className="text-xs text-amber-700 dark:text-amber-300"
                role="status"
              >
                This saved profile has no candidates. Add one before approving.
              </p>
            ) : (
              <p className="text-xs text-muted-foreground" role="status">
                Choose one exact candidate from this saved profile.
              </p>
            )}
          </div>
        ) : null}
        <div className="grid gap-3 sm:grid-cols-2">
          <label
            className="space-y-1 text-sm font-medium"
            htmlFor="project-issue-plan-max-output-tokens"
          >
            <span>Maximum output tokens per provider call</span>
            <Input
              aria-invalid={
                maxOutputTokensPerCall.trim() !== "" &&
                outputTokenLimit === undefined
              }
              aria-label="Maximum output tokens per provider call"
              id="project-issue-plan-max-output-tokens"
              inputMode="numeric"
              min={1}
              onChange={(event) => {
                setMaxOutputTokensPerCall(event.target.value);
                setMessage("");
              }}
              placeholder="Required"
              step={1}
              type="number"
              value={maxOutputTokensPerCall}
            />
          </label>
          <label
            className="space-y-1 text-sm font-medium"
            htmlFor="project-issue-plan-max-turn-duration"
          >
            <span>Maximum agent turn duration (seconds)</span>
            <Input
              aria-invalid={
                maxTurnDurationSeconds.trim() !== "" &&
                turnDurationLimit === undefined
              }
              aria-label="Maximum agent turn duration (seconds)"
              id="project-issue-plan-max-turn-duration"
              inputMode="numeric"
              min={1}
              onChange={(event) => {
                setMaxTurnDurationSeconds(event.target.value);
                setMessage("");
              }}
              placeholder="Required"
              step={1}
              type="number"
              value={maxTurnDurationSeconds}
            />
          </label>
        </div>
        {!budgetIsValid ? (
          <p
            className="text-xs text-amber-700 dark:text-amber-300"
            role="status"
          >
            Enter positive integer limits to approve this plan.
          </p>
        ) : null}
        {!selectedRouteProfileSummary ? (
          <p
            className="text-xs text-amber-700 dark:text-amber-300"
            role="status"
          >
            Pin an available route profile before approving this plan.
          </p>
        ) : null}
        {selectedRouteProfileSummary && !selectedRouteCandidate ? (
          <p
            className="text-xs text-amber-700 dark:text-amber-300"
            role="status"
          >
            Pin an exact saved provider candidate before approving this plan.
          </p>
        ) : null}
        <Textarea
          aria-label="Agent plan draft"
          className="min-h-48 resize-y font-mono text-xs leading-relaxed"
          maxLength={12_000}
          onChange={(event) => {
            setPlanText(event.target.value);
            setMessage("");
          }}
          value={planText}
        />
        <div className="flex flex-wrap items-center justify-end gap-2">
          {onUseApprovedPlan ? (
            <Button
              data-testid="project-issue-plan-use-in-chat"
              disabled={
                !approvalIsCurrent || !savedDraft?.approvedHash || isApproving
              }
              onClick={() => {
                if (!approvalIsCurrent || !savedDraft?.approvedHash) return;
                const prepared = onUseApprovedPlan({
                  approvedHash: savedDraft.approvedHash,
                  snapshot,
                });
                setMessage(
                  prepared === false
                    ? "Could not attach this plan to the current task chat. Reopen the task and try again."
                    : "Approved plan added to the chat preview. No message was sent.",
                );
              }}
              size="sm"
              variant="outline"
            >
              Use approved plan in chat
            </Button>
          ) : null}
          <Button onClick={handleSave} size="sm" variant="outline">
            Save draft
          </Button>
          <Button
            disabled={
              !planText.trim() ||
              isApproving ||
              routeProfileIsUnavailable ||
              !selectedRouteProfile ||
              !selectedRouteCandidate ||
              routeCandidateIsUnavailable ||
              !budgetIsValid
            }
            onClick={() => void handleApprove()}
            size="sm"
          >
            {isApproving ? "Approving…" : "Approve plan snapshot"}
          </Button>
        </div>
        {onUseApprovedPlan ? (
          <p className="text-xs text-muted-foreground">
            This only prepares the pre-send chat context. The selected chat
            agent may differ from the planned provider, and these limits are not
            enforced here. Nothing is sent until you send a message.
          </p>
        ) : null}
        <div aria-live="polite" className="space-y-1 text-xs" role="status">
          {approvalIsCurrent && savedDraft?.approvedHash ? (
            <p className="text-green-600 dark:text-green-400">
              Approved locally · SHA-256{" "}
              <code
                className="select-all font-mono"
                data-testid="project-issue-plan-hash"
              >
                {savedDraft.approvedHash}
              </code>
            </p>
          ) : savedDraft?.approvedHash &&
            savedDraft.approvedSnapshot === snapshotJson ? (
            <p className="text-amber-600 dark:text-amber-400">
              The saved approval hash does not match this plan. Approve the
              current snapshot again.
            </p>
          ) : savedDraft?.approvedHash ? (
            <p className="text-amber-600 dark:text-amber-400">
              The task or plan changed after approval. Approve the current
              snapshot again.
            </p>
          ) : savedDraft ? (
            <p className="text-muted-foreground">Draft saved on this device.</p>
          ) : null}
          {message ? <p>{message}</p> : null}
        </div>
      </div>
    </ProjectDetailSection>
  );
}

function readLocalDraft(key: string): SavedProjectIssuePlanDraft | null {
  if (typeof window === "undefined") return null;
  try {
    return readProjectIssuePlanDraft(window.localStorage, key);
  } catch {
    return null;
  }
}

function planSourceTask(issue: ProjectIssue): ProjectIssuePlanSourceTask {
  return {
    content: issue.content,
    id: issue.id,
    status: issue.status,
    title: issue.title,
  };
}
