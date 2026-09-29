export const PROJECT_ISSUE_PLAN_SCHEMA_VERSION = 4;
export const PROJECT_ISSUE_PLAN_DRAFT_SCHEMA_VERSION = 1;
export const PROJECT_ISSUE_PLAN_MAX_CHARS = 12_000;

export type ProjectIssuePlanSourceTask = {
  content?: string | null;
  id: string;
  status: string;
  title: string;
};

export type ProjectIssuePlanSnapshot = {
  schemaVersion: 4;
  projectAddress: string;
  issue: ProjectIssuePlanSourceTask;
  tasks: ProjectIssuePlanSourceTask[];
  planText: string;
  routeProfile: ProjectIssuePlanRouteProfile | null;
  routeCandidate: ProjectIssuePlanRouteCandidate | null;
  budget: ProjectIssuePlanBudget;
  dataPath: { kind: "local-only-draft" };
};

/** Data attached to a chat draft only after the saved local approval verifies. */
export type ProjectIssuePlanChatHandoff = {
  approvedHash: string;
  snapshot: ProjectIssuePlanSnapshot;
};

export type ProjectIssuePlanRouteProfile = {
  id: string;
  version: number;
  documentHash: string;
  dataPolicy: "local-only" | "allow-hosted";
};

export type ProjectIssuePlanRouteCandidate = {
  id: string;
  provider: string;
  model: string;
  dataLocation: "local" | "hosted";
};

export type ProjectIssuePlanBudget = {
  maxOutputTokensPerCall: number | null;
  maxTurnDurationSeconds: number | null;
};

export type SavedProjectIssuePlanDraft = {
  schemaVersion: 1;
  planText: string;
  approvedSnapshot: string | null;
  approvedHash: string | null;
  routeProfileId: string | null;
  routeProfileCandidateId: string | null;
  maxOutputTokensPerCall: string;
  maxTurnDurationSeconds: string;
  savedAt: number;
};

function sourceTask(task: ProjectIssuePlanSourceTask) {
  return {
    id: task.id,
    title: task.title,
    status: task.status,
    content: task.content ?? "",
  };
}

export function buildProjectIssuePlanSnapshot({
  projectAddress,
  issue,
  subtasks,
  planText,
  routeProfile = null,
  routeCandidate = null,
  budget = { maxOutputTokensPerCall: null, maxTurnDurationSeconds: null },
}: {
  projectAddress: string;
  issue: ProjectIssuePlanSourceTask;
  subtasks: readonly ProjectIssuePlanSourceTask[];
  planText: string;
  routeProfile?: ProjectIssuePlanRouteProfile | null;
  routeCandidate?: ProjectIssuePlanRouteCandidate | null;
  budget?: ProjectIssuePlanBudget;
}): ProjectIssuePlanSnapshot {
  return {
    schemaVersion: PROJECT_ISSUE_PLAN_SCHEMA_VERSION,
    projectAddress,
    issue: sourceTask(issue),
    tasks: (subtasks.length > 0 ? subtasks : [issue]).map(sourceTask),
    planText,
    routeProfile,
    routeCandidate,
    budget,
    dataPath: { kind: "local-only-draft" },
  };
}

/** Stable field ordering makes the exact source + editable plan hashable. */
export function serializeProjectIssuePlanSnapshot(
  snapshot: ProjectIssuePlanSnapshot,
): string {
  return JSON.stringify({
    schemaVersion: snapshot.schemaVersion,
    projectAddress: snapshot.projectAddress,
    issue: sourceTask(snapshot.issue),
    tasks: snapshot.tasks.map(sourceTask),
    planText: snapshot.planText,
    routeProfile: snapshot.routeProfile,
    routeCandidate: snapshot.routeCandidate,
    budget: snapshot.budget,
    dataPath: { kind: snapshot.dataPath.kind },
  });
}

export async function hashProjectIssuePlanSnapshot(
  snapshot: ProjectIssuePlanSnapshot,
): Promise<string> {
  if (!globalThis.crypto?.subtle) {
    throw new Error("This device does not support local plan hashing.");
  }
  const digest = await globalThis.crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(serializeProjectIssuePlanSnapshot(snapshot)),
  );
  return Array.from(new Uint8Array(digest), (value) =>
    value.toString(16).padStart(2, "0"),
  ).join("");
}

/** Local display check only; dispatch must still enforce the approved snapshot. */
export async function verifyProjectIssuePlanApproval(
  draft: SavedProjectIssuePlanDraft | null,
  snapshot: ProjectIssuePlanSnapshot,
): Promise<boolean> {
  if (
    !draft?.approvedHash ||
    draft.approvedSnapshot !== serializeProjectIssuePlanSnapshot(snapshot)
  ) {
    return false;
  }
  try {
    return (
      (await hashProjectIssuePlanSnapshot(snapshot)) === draft.approvedHash
    );
  } catch {
    return false;
  }
}

export function projectIssuePlanStorageKey(
  projectAddress: string,
  issueId: string,
): string {
  return `buzz:project-issue-plan:v1:${encodeURIComponent(projectAddress)}:${issueId}`;
}

export function readProjectIssuePlanDraft(
  storage: Pick<Storage, "getItem">,
  key: string,
): SavedProjectIssuePlanDraft | null {
  try {
    const raw = storage.getItem(key);
    if (!raw || raw.length > PROJECT_ISSUE_PLAN_MAX_CHARS * 8) return null;
    const parsed = JSON.parse(raw) as Partial<SavedProjectIssuePlanDraft>;
    if (
      parsed.schemaVersion !== PROJECT_ISSUE_PLAN_DRAFT_SCHEMA_VERSION ||
      typeof parsed.planText !== "string" ||
      parsed.planText.length > PROJECT_ISSUE_PLAN_MAX_CHARS ||
      typeof parsed.savedAt !== "number" ||
      !Number.isFinite(parsed.savedAt) ||
      (parsed.approvedSnapshot !== null &&
        typeof parsed.approvedSnapshot !== "string") ||
      (typeof parsed.approvedSnapshot === "string" &&
        parsed.approvedSnapshot.length > PROJECT_ISSUE_PLAN_MAX_CHARS * 8) ||
      (parsed.approvedHash !== null &&
        (typeof parsed.approvedHash !== "string" ||
          !/^[a-f0-9]{64}$/.test(parsed.approvedHash))) ||
      (parsed.routeProfileId !== undefined &&
        parsed.routeProfileId !== null &&
        (typeof parsed.routeProfileId !== "string" ||
          parsed.routeProfileId.length > 64)) ||
      (parsed.routeProfileCandidateId !== undefined &&
        parsed.routeProfileCandidateId !== null &&
        (typeof parsed.routeProfileCandidateId !== "string" ||
          parsed.routeProfileCandidateId.length > 64)) ||
      (parsed.maxOutputTokensPerCall !== undefined &&
        (typeof parsed.maxOutputTokensPerCall !== "string" ||
          parsed.maxOutputTokensPerCall.length > 32)) ||
      (parsed.maxTurnDurationSeconds !== undefined &&
        (typeof parsed.maxTurnDurationSeconds !== "string" ||
          parsed.maxTurnDurationSeconds.length > 32))
    ) {
      return null;
    }
    return {
      schemaVersion: PROJECT_ISSUE_PLAN_DRAFT_SCHEMA_VERSION,
      planText: parsed.planText,
      approvedSnapshot: parsed.approvedSnapshot ?? null,
      approvedHash: parsed.approvedHash ?? null,
      routeProfileId: parsed.routeProfileId ?? null,
      routeProfileCandidateId: parsed.routeProfileCandidateId ?? null,
      maxOutputTokensPerCall: parsed.maxOutputTokensPerCall ?? "",
      maxTurnDurationSeconds: parsed.maxTurnDurationSeconds ?? "",
      savedAt: parsed.savedAt,
    };
  } catch {
    return null;
  }
}

export function writeProjectIssuePlanDraft(
  storage: Pick<Storage, "setItem">,
  key: string,
  draft: SavedProjectIssuePlanDraft,
): boolean {
  if (
    draft.schemaVersion !== PROJECT_ISSUE_PLAN_DRAFT_SCHEMA_VERSION ||
    draft.planText.length > PROJECT_ISSUE_PLAN_MAX_CHARS ||
    (draft.routeProfileId !== null &&
      (typeof draft.routeProfileId !== "string" ||
        draft.routeProfileId.length > 64)) ||
    (draft.routeProfileCandidateId !== null &&
      (typeof draft.routeProfileCandidateId !== "string" ||
        draft.routeProfileCandidateId.length > 64)) ||
    typeof draft.maxOutputTokensPerCall !== "string" ||
    draft.maxOutputTokensPerCall.length > 32 ||
    typeof draft.maxTurnDurationSeconds !== "string" ||
    draft.maxTurnDurationSeconds.length > 32
  ) {
    return false;
  }
  try {
    storage.setItem(key, JSON.stringify(draft));
    return true;
  } catch {
    return false;
  }
}

/** Empty means unset; undefined means present but not a positive integer. */
export function parsePositiveIntegerLimitDraft(
  raw: string,
): number | null | undefined {
  if (!raw.trim()) return null;
  if (!/^\d+$/.test(raw.trim())) return undefined;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

export function defaultProjectIssuePlan(
  issue: ProjectIssuePlanSourceTask,
  subtasks: readonly ProjectIssuePlanSourceTask[],
): string {
  const tasks = subtasks.length > 0 ? subtasks : [issue];
  const lines = tasks.map((task) => {
    const criterion = projectIssueAcceptanceCriteria(task.content ?? "");
    return [
      `- [ ] ${JSON.stringify(task.title)} (event ${task.id})`,
      criterion
        ? `  Acceptance criteria: ${JSON.stringify(criterion)}`
        : "  Add a clear acceptance criterion before execution.",
    ].join("\n");
  });
  return [`Goal: ${JSON.stringify(issue.title)}`, "", "Tasks:", ...lines].join(
    "\n",
  );
}

export function projectIssueAcceptanceCriteria(content: string): string {
  const match = content.match(/(?:^|\n)Acceptance criteria:\s*\n([\s\S]*)$/i);
  return match?.[1]?.trim() ?? "";
}
