import assert from "node:assert/strict";
import test from "node:test";

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
} from "./projectIssuePlan.ts";

const issue = {
  id: "a".repeat(64),
  title: "Ship the project runner",
  status: "Backlog",
  content: "Acceptance criteria:\nThe result links to the task.",
};
const child = {
  id: "b".repeat(64),
  title: "Bind one worker",
  status: "In Progress",
  content: "Acceptance criteria:\nThe run records this event ID.",
};
const secondChild = {
  id: "c".repeat(64),
  title: "Record the result",
  status: "Backlog",
  content: "Acceptance criteria:\nThe outcome links to the run.",
};
const routeProfile = {
  id: "local-route",
  version: 2,
  documentHash: "a".repeat(64),
  dataPolicy: "local-only",
};
const budget = {
  maxOutputTokensPerCall: 4096,
  maxTurnDurationSeconds: 900,
};
const routeCandidate = {
  id: "loopback",
  provider: "openai",
  model: "qwen-local",
  dataLocation: "local",
};

test("versioned plan snapshot pins issue, direct child IDs, and local-only data path", () => {
  const snapshot = buildProjectIssuePlanSnapshot({
    projectAddress: "30617:owner:repo",
    issue,
    subtasks: [child],
    planText: "Run the child once.",
  });
  assert.equal(snapshot.schemaVersion, 4);
  assert.equal(snapshot.issue.id, issue.id);
  assert.deepEqual(
    snapshot.tasks.map((task) => task.id),
    [child.id],
  );
  assert.deepEqual(snapshot.dataPath, { kind: "local-only-draft" });
  assert.deepEqual(snapshot.routeProfile, null);
  assert.deepEqual(snapshot.routeCandidate, null);
  assert.deepEqual(snapshot.budget, {
    maxOutputTokensPerCall: null,
    maxTurnDurationSeconds: null,
  });
  assert.equal(
    JSON.parse(serializeProjectIssuePlanSnapshot(snapshot)).planText,
    "Run the child once.",
  );
});

test("plan hash changes for every pinned source field and task list edit", async () => {
  const build = ({
    projectAddress = "30617:owner:repo",
    selectedIssue = issue,
    selectedChildren = [child, secondChild],
    selectedRouteProfile = routeProfile,
    selectedRouteCandidate = routeCandidate,
    selectedBudget = budget,
    planText = "Run the child once.",
  } = {}) =>
    buildProjectIssuePlanSnapshot({
      projectAddress,
      issue: selectedIssue,
      subtasks: selectedChildren,
      routeProfile: selectedRouteProfile,
      routeCandidate: selectedRouteCandidate,
      budget: selectedBudget,
      planText,
    });
  const baseline = await hashProjectIssuePlanSnapshot(build());
  assert.equal(baseline, await hashProjectIssuePlanSnapshot(build()));
  const changedSnapshots = [
    build({ projectAddress: "30617:other:repo" }),
    build({ selectedIssue: { ...issue, id: "e".repeat(64) } }),
    build({ selectedIssue: { ...issue, title: "Different epic" } }),
    build({ selectedIssue: { ...issue, status: "In Progress" } }),
    build({ selectedIssue: { ...issue, content: "Changed criterion" } }),
    build({
      selectedChildren: [{ ...child, id: "d".repeat(64) }, secondChild],
    }),
    build({
      selectedChildren: [{ ...child, title: "Different child" }, secondChild],
    }),
    build({ selectedChildren: [{ ...child, status: "Done" }, secondChild] }),
    build({
      selectedChildren: [
        { ...child, content: "Changed criterion" },
        secondChild,
      ],
    }),
    build({
      selectedChildren: [
        child,
        secondChild,
        { ...secondChild, id: "d".repeat(64) },
      ],
    }),
    build({ selectedChildren: [] }),
    build({ selectedChildren: [secondChild, child] }),
    build({ selectedRouteProfile: { ...routeProfile, version: 3 } }),
    build({
      selectedRouteProfile: { ...routeProfile, documentHash: "b".repeat(64) },
    }),
    build({
      selectedRouteProfile: { ...routeProfile, dataPolicy: "allow-hosted" },
    }),
    build({ selectedRouteProfile: null }),
    build({
      selectedRouteCandidate: { ...routeCandidate, id: "other" },
    }),
    build({
      selectedRouteCandidate: { ...routeCandidate, provider: "anthropic" },
    }),
    build({
      selectedRouteCandidate: { ...routeCandidate, model: "other-model" },
    }),
    build({
      selectedRouteCandidate: { ...routeCandidate, dataLocation: "hosted" },
    }),
    build({ selectedRouteCandidate: null }),
    build({ selectedBudget: { ...budget, maxOutputTokensPerCall: 2048 } }),
    build({ selectedBudget: { ...budget, maxTurnDurationSeconds: 600 } }),
    build({ planText: "Run the child twice." }),
  ];
  for (const changed of changedSnapshots) {
    assert.notEqual(await hashProjectIssuePlanSnapshot(changed), baseline);
  }
});

test("saved approval display rejects changed snapshots and mismatched local hashes", async () => {
  const snapshot = buildProjectIssuePlanSnapshot({
    projectAddress: "30617:owner:repo",
    issue,
    subtasks: [child],
    routeProfile,
    routeCandidate,
    planText: "Run the child once.",
  });
  const approved = {
    schemaVersion: 1,
    planText: snapshot.planText,
    approvedSnapshot: serializeProjectIssuePlanSnapshot(snapshot),
    approvedHash: await hashProjectIssuePlanSnapshot(snapshot),
    routeProfileId: routeProfile.id,
    routeProfileCandidateId: routeCandidate.id,
    savedAt: 123,
  };
  assert.equal(await verifyProjectIssuePlanApproval(approved, snapshot), true);
  assert.equal(
    await verifyProjectIssuePlanApproval(
      { ...approved, approvedHash: "c".repeat(64) },
      snapshot,
    ),
    false,
  );
  assert.equal(
    await verifyProjectIssuePlanApproval(approved, {
      ...snapshot,
      planText: "Changed after approval",
    }),
    false,
  );
});

test("default plan includes task IDs and criteria as quoted source data", () => {
  const plan = defaultProjectIssuePlan(issue, [child]);
  assert.match(plan, /Goal: "Ship the project runner"/);
  assert.match(plan, new RegExp(`event ${child.id}`));
  assert.match(plan, /Acceptance criteria: "The run records this event ID\."/);
});

test("planned limit inputs accept only positive safe integers", () => {
  assert.equal(parsePositiveIntegerLimitDraft(""), null);
  assert.equal(parsePositiveIntegerLimitDraft("  "), null);
  assert.equal(parsePositiveIntegerLimitDraft("4096"), 4096);
  for (const invalid of [
    "0",
    "-1",
    "+1",
    "1.5",
    "1e2",
    "Infinity",
    "1e100",
    "tokens",
  ]) {
    assert.equal(parsePositiveIntegerLimitDraft(invalid), undefined);
  }
});

test("local draft persistence is versioned, scoped, and rejects malformed data", () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const key = projectIssuePlanStorageKey("30617:owner:repo", issue.id);
  const draft = {
    schemaVersion: 1,
    planText: "Run the child once.",
    approvedSnapshot: '{"schemaVersion":1}',
    approvedHash: "c".repeat(64),
    routeProfileId: null,
    routeProfileCandidateId: "loopback",
    maxOutputTokensPerCall: "",
    maxTurnDurationSeconds: "",
    savedAt: 123,
  };
  assert.match(key, /30617%3Aowner%3Arepo/);
  assert.equal(writeProjectIssuePlanDraft(storage, key, draft), true);
  assert.deepEqual(readProjectIssuePlanDraft(storage, key), draft);
  assert.equal(
    writeProjectIssuePlanDraft(storage, key, {
      ...draft,
      maxOutputTokensPerCall: 4096,
    }),
    false,
  );
  values.set(
    key,
    JSON.stringify({
      schemaVersion: 1,
      planText: "Legacy plan text.",
      approvedSnapshot: null,
      approvedHash: null,
      savedAt: 124,
    }),
  );
  assert.equal(
    readProjectIssuePlanDraft(storage, key)?.planText,
    "Legacy plan text.",
  );
  assert.equal(readProjectIssuePlanDraft(storage, key)?.routeProfileId, null);
  assert.equal(
    readProjectIssuePlanDraft(storage, key)?.routeProfileCandidateId,
    null,
  );
  assert.equal(
    readProjectIssuePlanDraft(storage, key)?.maxOutputTokensPerCall,
    "",
  );
  assert.equal(
    readProjectIssuePlanDraft(storage, key)?.maxTurnDurationSeconds,
    "",
  );
  values.set(key, "not json");
  assert.equal(readProjectIssuePlanDraft(storage, key), null);
});
