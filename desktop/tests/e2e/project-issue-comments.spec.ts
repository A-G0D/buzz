import { expect, test } from "@playwright/test";

import { installMockBridge } from "../helpers/bridge";

const ISSUE_COMMENTS = [
  "First issue comment",
  "Second issue comment",
  "Third issue comment",
  "Fourth issue comment",
];
const DEFAULT_MOCK_PUBKEY = "deadbeef".repeat(8);
const PROJECT_HOME_CHANNEL_ID = "cf63feec-21bb-5bf0-a2f8-0e4c3de8ec73";

async function openBuzzProject(page: import("@playwright/test").Page) {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByTestId("open-projects-view").click();
  await page.getByTestId("projects-section-projects").click();
  const projectEntry = page
    .locator(
      '[data-testid="project-card-buzz"], [data-testid="project-row-buzz"]',
    )
    .first();
  await expect(projectEntry).toBeVisible({ timeout: 10_000 });
  await projectEntry.click();
  await page.getByTestId("project-home-context-repo-buzz").click();
}

test("issue detail can open agent chat or seed a channel question", async ({
  page,
}) => {
  await installMockBridge(page);
  await openBuzzProject(page);

  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  const issueRow = page.getByTestId("project-issue-row").first();
  await expect(issueRow).toBeVisible({ timeout: 10_000 });
  await issueRow.getByRole("button", { name: /^#/ }).click();

  const communication = page.getByTestId(
    "project-context-communication-actions",
  );
  await expect(communication).toBeVisible();
  const contextPanel = page.getByTestId("project-repository-actions-panel");
  await expect(
    contextPanel.getByRole("heading", { name: "Actions", exact: true }),
  ).toHaveCount(0);
  await expect(
    contextPanel.getByRole("heading", { name: "Details", exact: true }),
  ).toBeVisible();
  await expect(
    contextPanel.getByRole("heading", { name: "Assignment", exact: true }),
  ).toHaveCount(0);
  await expect(
    contextPanel.getByRole("heading", { name: "Discussion", exact: true }),
  ).toHaveCount(0);
  await expect(
    contextPanel.getByTestId("project-repository-people"),
  ).toHaveCount(0);
  await page.getByTestId("project-context-chat-agent").click();
  await expect(page.getByTestId("project-agent-chat-panel")).toBeVisible();
  await expect(page.getByTestId("projects-agent-selection-item")).toHaveCount(
    1,
  );
  await page.getByRole("button", { name: "Close agent chat" }).click();
  await expect(
    page.getByTestId("project-right-panel-repository-tab"),
  ).toHaveAttribute("aria-pressed", "false");

  await page.getByTestId("project-right-panel-repository-tab").click();
  await page.getByTestId("project-context-discuss").click();
  await expect(
    page.getByTestId("project-context-channel-choices"),
  ).toBeVisible();
  await page.getByTestId("project-context-related-channel").first().click();
  await expect(page.getByTestId("message-input")).toContainText(
    "Let's talk about this task:",
  );
});

test("issue agent chat sends the selected task snapshot to its project home", async ({
  page,
}) => {
  const issueId = "c".repeat(64);
  const issueDescription =
    "Summary: include task context.\n\nAcceptance criteria:\nVerify the exact criterion reaches the agent.";
  const owner = DEFAULT_MOCK_PUBKEY;
  await page.addInitScript(
    ({ issueDescription, issueId, owner }) => {
      window.__BUZZ_E2E_EXTRA_PROJECT_EVENTS__ = [
        {
          id: issueId,
          kind: 1621,
          pubkey: owner,
          created_at: Math.floor(Date.now() / 1_000) + 60,
          content: issueDescription,
          tags: [
            ["a", `30617:${owner}:buzz`],
            ["subject", "Agent context payload acceptance"],
          ],
        },
      ];
    },
    { issueDescription, issueId, owner },
  );
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  const issueRow = page
    .getByTestId("project-issue-row")
    .filter({ hasText: "Agent context payload acceptance" });
  await expect(issueRow).toBeVisible({ timeout: 10_000 });
  await expect(issueRow).toHaveAttribute("data-project-event-id", issueId);
  await issueRow.getByRole("button", { name: /^#/ }).click();
  await page.getByTestId("project-context-chat-agent").click();

  const chat = page.getByTestId("project-agent-chat-panel");
  await expect(chat).toBeVisible();
  await chat.getByTestId("message-input").fill("Start the selected task");
  await chat.getByTestId("message-input").press("Enter");

  const readSentMessage = () =>
    page.evaluate(() => {
      const entries =
        (
          window as Window & {
            __BUZZ_E2E_COMMAND_PAYLOADS__?: Array<{
              command: string;
              payload: { channelId?: string; content?: string };
            }>;
          }
        ).__BUZZ_E2E_COMMAND_PAYLOADS__ ?? [];
      return entries
        .filter((entry) => entry.command === "send_channel_message")
        .at(-1)?.payload;
    });
  await expect.poll(readSentMessage).toMatchObject({
    channelId: PROJECT_HOME_CHANNEL_ID,
  });
  await expect.poll(readSentMessage).toHaveProperty("content");
  await expect
    .poll(async () => (await readSentMessage())?.content ?? "")
    .toContain(`(id: "${issueId}")`);
  await expect
    .poll(async () => (await readSentMessage())?.content ?? "")
    .toContain(
      'Description: "Summary: include task context. Acceptance criteria: Verify the exact criterion reaches the agent."',
    );
});

test("Epic agent context includes its validated direct subtasks", async ({
  page,
}) => {
  const epicId = "e".repeat(64);
  const childId = "f".repeat(64);
  const owner = DEFAULT_MOCK_PUBKEY;
  await page.addInitScript(
    ({ childId, epicId, owner }) => {
      const projectAddress = `30617:${owner}:buzz`;
      window.__BUZZ_E2E_EXTRA_PROJECT_EVENTS__ = [
        {
          id: epicId,
          kind: 1621,
          pubkey: owner,
          created_at: Math.floor(Date.now() / 1_000) + 60,
          content: "Plan one routed project task.",
          tags: [
            ["a", projectAddress],
            ["subject", "Epic plan context"],
            ["t", "epic"],
          ],
        },
        {
          id: childId,
          kind: 1621,
          pubkey: owner,
          created_at: Math.floor(Date.now() / 1_000) + 61,
          content:
            "Acceptance criteria: the selected child is visible to the agent.",
          tags: [
            ["a", projectAddress],
            ["subject", "Inspect the selected child"],
            ["parent", epicId],
          ],
        },
      ];
    },
    { childId, epicId, owner },
  );
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();

  const epicRow = page
    .getByTestId("project-issue-row")
    .filter({ hasText: "Epic plan context" });
  await expect(epicRow).toBeVisible({ timeout: 10_000 });
  await epicRow.getByRole("button", { name: /^#/ }).click();
  await page.getByTestId("project-context-chat-agent").click();

  const chat = page.getByTestId("project-agent-chat-panel");
  await chat.getByTestId("message-input").fill("Work through the plan");
  await chat.getByTestId("message-input").press("Enter");
  const readSentMessage = () =>
    page.evaluate(() => {
      const entries =
        (
          window as Window & {
            __BUZZ_E2E_COMMAND_PAYLOADS__?: Array<{
              command: string;
              payload: { channelId?: string; content?: string };
            }>;
          }
        ).__BUZZ_E2E_COMMAND_PAYLOADS__ ?? [];
      return entries
        .filter((entry) => entry.command === "send_channel_message")
        .at(-1)?.payload;
    });

  await expect
    .poll(async () => (await readSentMessage())?.content ?? "")
    .toContain(`Planned subtasks: 1`);
  await expect
    .poll(async () => (await readSentMessage())?.content ?? "")
    .toContain(`id: "${childId}"`);
  await expect
    .poll(async () => (await readSentMessage())?.content ?? "")
    .toContain(
      "Acceptance criteria: the selected child is visible to the agent.",
    );
});

test("an Epic plan draft persists and approval becomes stale after edits", async ({
  page,
}) => {
  const epicId = "d".repeat(64);
  const childId = "f".repeat(64);
  const owner = DEFAULT_MOCK_PUBKEY;
  await page.addInitScript(
    ({ childId, epicId, owner }) => {
      const projectAddress = `30617:${owner}:buzz`;
      window.__BUZZ_E2E_EXTRA_PROJECT_EVENTS__ = [
        {
          id: epicId,
          kind: 1621,
          pubkey: owner,
          created_at: Math.floor(Date.now() / 1_000) + 60,
          content: "Plan work for this project.",
          tags: [
            ["a", projectAddress],
            ["subject", "Persistent plan acceptance"],
            ["t", "epic"],
          ],
        },
        {
          id: childId,
          kind: 1621,
          pubkey: owner,
          created_at: Math.floor(Date.now() / 1_000) + 61,
          content: "Acceptance criteria:\nThe approved hash changes on edits.",
          tags: [
            ["a", projectAddress],
            ["subject", "Verify plan hash invalidation"],
            ["parent", epicId],
          ],
        },
      ];
    },
    { childId, epicId, owner },
  );
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();

  const openEpic = async () => {
    const expandWorkspace = page.getByTestId(
      "project-home-workspace-sheet-expand",
    );
    if (await expandWorkspace.isVisible().catch(() => false)) {
      await expandWorkspace.click();
    }
    await page.getByRole("tab", { name: "Tasks", exact: true }).click();
    const epicRow = page
      .getByTestId("project-issue-row")
      .filter({ hasText: "Persistent plan acceptance" });
    await expect(epicRow).toBeVisible({ timeout: 10_000 });
    await epicRow.getByRole("button", { name: /^#/ }).click();
    await page
      .getByTestId("project-issue-plan-section")
      .getByRole("button", { name: /Agent plan/ })
      .click();
  };
  await openEpic();

  const approvePlan = page.getByRole("button", {
    name: "Approve plan snapshot",
  });
  await expect(approvePlan).toBeDisabled();
  const plan = page.getByRole("textbox", { name: "Agent plan draft" });
  await expect(plan).toHaveValue(new RegExp(`event ${childId}`));
  await expect(plan).toHaveValue(/The approved hash changes on edits\./);
  const routeField = page.getByTestId("project-issue-plan-route-profile-field");
  await routeField.getByRole("button").click();
  await page
    .getByRole("menuitemradio", { name: /E2E local critic · local-only · v1/ })
    .click();
  const candidateField = page.getByTestId(
    "project-issue-plan-route-candidate-field",
  );
  await candidateField.getByRole("button").click();
  await page
    .getByRole("menuitemradio", {
      name: /loopback · openai\/e2e-local-model · local/,
    })
    .click();
  await expect(approvePlan).toBeDisabled();
  await page
    .getByRole("spinbutton", {
      name: "Maximum output tokens per provider call",
    })
    .fill("4096");
  await page
    .getByRole("spinbutton", {
      name: "Maximum agent turn duration (seconds)",
    })
    .fill("900");
  await expect(approvePlan).toBeEnabled();
  await plan.fill(`Complete the task ${childId} and verify its criterion.`);
  await page.getByRole("button", { name: "Save draft" }).click();
  await expect(page.getByText("Draft saved on this device.")).toBeVisible();

  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  await openEpic();
  const reopenedPlan = page.getByRole("textbox", {
    name: "Agent plan draft",
  });
  await expect(reopenedPlan).toHaveValue(
    `Complete the task ${childId} and verify its criterion.`,
  );
  await expect(
    page.getByRole("spinbutton", {
      name: "Maximum output tokens per provider call",
    }),
  ).toHaveValue("4096");
  await expect(
    page.getByRole("spinbutton", {
      name: "Maximum agent turn duration (seconds)",
    }),
  ).toHaveValue("900");
  await expect(routeField.getByRole("button")).toContainText(
    "E2E local critic",
  );
  await expect(candidateField.getByRole("button")).toContainText("loopback");
  await approvePlan.click();
  const approvedHash = page.getByTestId("project-issue-plan-hash");
  await expect(approvedHash).toHaveText(/^[a-f0-9]{64}$/);
  await candidateField.getByRole("button").click();
  await page
    .getByRole("menuitemradio", {
      name: /local-backup · openai\/e2e-local-backup · local/,
    })
    .click();
  await expect(
    page.getByText("The task or plan changed after approval.", {
      exact: false,
    }),
  ).toBeVisible();
  await approvePlan.click();
  await expect(approvedHash).toHaveText(/^[a-f0-9]{64}$/);
  const useApprovedPlan = page.getByTestId("project-issue-plan-use-in-chat");
  await expect(useApprovedPlan).toBeEnabled();
  await useApprovedPlan.click();
  const agentChat = page.getByTestId("project-agent-chat-panel");
  await expect(agentChat).toBeVisible();
  await agentChat
    .getByRole("button", { name: "Preview message context" })
    .click();
  const contextPreview = page.getByTestId("agent-context-preview-payload");
  await expect(contextPreview).toContainText("User-approved local issue plan:");
  await expect(contextPreview).toContainText(
    `Approval SHA-256: ${await approvedHash.textContent()}`,
  );
  await expect(contextPreview).toContainText("local-backup");
  await expect(contextPreview).toContainText(
    `Complete the task ${childId} and verify its criterion.`,
  );
  await expect(contextPreview).toContainText(
    "This chat may use a different agent/provider",
  );
  const sentBeforeExplicitSend = await page.evaluate(
    () =>
      (
        window as Window & {
          __BUZZ_E2E_COMMAND_PAYLOADS__?: Array<{ command: string }>;
        }
      ).__BUZZ_E2E_COMMAND_PAYLOADS__?.filter(
        (entry) => entry.command === "send_channel_message",
      ).length ?? 0,
  );
  expect(sentBeforeExplicitSend).toBe(0);
  await page.getByTestId("agent-context-preview-trigger").click();
  await agentChat
    .getByTestId("message-input")
    .fill("Work through the approved plan.");
  await agentChat.getByTestId("message-input").press("Enter");
  const readLastSentPlanMessage = () =>
    page.evaluate(() => {
      const entries =
        (
          window as Window & {
            __BUZZ_E2E_COMMAND_PAYLOADS__?: Array<{
              command: string;
              payload: { channelId?: string; content?: string };
            }>;
          }
        ).__BUZZ_E2E_COMMAND_PAYLOADS__ ?? [];
      return entries
        .filter((entry) => entry.command === "send_channel_message")
        .at(-1)?.payload;
    });
  await expect.poll(readLastSentPlanMessage).toMatchObject({
    channelId: PROJECT_HOME_CHANNEL_ID,
  });
  await expect
    .poll(async () => (await readLastSentPlanMessage())?.content ?? "")
    .toContain(`Approval SHA-256: ${await approvedHash.textContent()}`);
  await expect
    .poll(async () => (await readLastSentPlanMessage())?.content ?? "")
    .toContain(`Complete the task ${childId} and verify its criterion.`);
  await expect
    .poll(async () => (await readLastSentPlanMessage())?.content ?? "")
    .toContain(`Issue ID: "${epicId}"`);
  await expect
    .poll(async () => (await readLastSentPlanMessage())?.content ?? "")
    .toContain(`id: "${childId}"`);
  await expect
    .poll(async () => (await readLastSentPlanMessage())?.content ?? "")
    .toContain("Acceptance criteria: The approved hash changes on edits.");
  await expect
    .poll(async () => (await readLastSentPlanMessage())?.content ?? "")
    .toContain('"openai" / "e2e-local-backup"');

  await page
    .getByRole("spinbutton", {
      name: "Maximum agent turn duration (seconds)",
    })
    .fill("901");
  await expect(
    page.getByText("The task or plan changed after approval.", {
      exact: false,
    }),
  ).toBeVisible();
  await approvePlan.click();
  await expect(approvedHash).toHaveText(/^[a-f0-9]{64}$/);

  await page.evaluate((issueId) => {
    for (const key of Object.keys(window.localStorage)) {
      if (
        !key.startsWith("buzz:project-issue-plan:v1:") ||
        !key.endsWith(issueId)
      ) {
        continue;
      }
      const saved = JSON.parse(window.localStorage.getItem(key) ?? "null");
      saved.approvedHash = "c".repeat(64);
      window.localStorage.setItem(key, JSON.stringify(saved));
      return;
    }
    throw new Error("Saved issue plan draft was not found.");
  }, epicId);

  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  await openEpic();
  await expect(
    page.getByText("The saved approval hash does not match this plan.", {
      exact: false,
    }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Approve plan snapshot" }).click();
  await expect(page.getByTestId("project-issue-plan-hash")).toHaveText(
    /^[a-f0-9]{64}$/,
  );

  await reopenedPlan.fill(`Changed plan for ${childId}.`);
  await expect(
    page.getByText("The task or plan changed after approval.", {
      exact: false,
    }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Save draft" }).click();
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  await openEpic();
  await expect(
    page.getByText("The task or plan changed after approval.", {
      exact: false,
    }),
  ).toBeVisible();
});

test("an Epic creates a repository-scoped subtask with a parent link", async ({
  page,
}) => {
  const epicId = "e".repeat(64);
  const owner = DEFAULT_MOCK_PUBKEY;
  await page.addInitScript(
    ({ epicId, owner }) => {
      window.__BUZZ_E2E_EXTRA_PROJECT_EVENTS__ = [
        {
          id: epicId,
          kind: 1621,
          pubkey: owner,
          created_at: Math.floor(Date.now() / 1_000) + 60,
          content: "Ship the project task tree.",
          tags: [
            ["a", `30617:${owner}:buzz`],
            ["subject", "Project task tree"],
            ["t", "epic"],
          ],
        },
      ];
    },
    { epicId, owner },
  );
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();

  const epicRow = page
    .getByTestId("project-issue-row")
    .filter({ hasText: "Project task tree" });
  await expect(epicRow).toBeVisible();
  await epicRow.getByRole("button", { name: /^#/ }).click();
  await expect(
    page
      .getByTestId("project-issue-detail")
      .getByRole("heading", { name: /Project task tree/ }),
  ).toBeVisible();
  await expect(page.getByText("Subtasks (0)")).toBeVisible();

  await page
    .getByRole("textbox", { name: "Subtask title" })
    .fill("Build the coordinator spine");
  await page
    .getByRole("textbox", { name: "Subtask description" })
    .fill("Persist project intent and link each child run.");
  await page
    .getByRole("textbox", { name: "Subtask acceptance criteria" })
    .fill("A run stays linked to exactly one project issue.");
  await page.getByRole("button", { name: "Add subtask" }).click();

  await expect
    .poll(() =>
      page.evaluate(
        (parentId) =>
          window.__BUZZ_E2E_SIGNED_EVENTS__?.some(
            (event) =>
              event.kind === 1621 &&
              event.tags.some(
                (tag) => tag[0] === "parent" && tag[1] === parentId,
              ),
          ),
        epicId,
      ),
    )
    .toBe(true);
  const createdSubtask = await page.evaluate((parentId) => {
    const event = window.__BUZZ_E2E_SIGNED_EVENTS__?.find(
      (candidate) =>
        candidate.kind === 1621 &&
        candidate.tags.some(
          (tag) => tag[0] === "parent" && tag[1] === parentId,
        ),
    );
    return event ? { content: event.content, id: event.id } : null;
  }, epicId);
  expect(createdSubtask?.content).toContain(
    "Acceptance criteria:\nA run stays linked to exactly one project issue.",
  );
  expect(createdSubtask?.id).toBeTruthy();

  await expect(page.getByText("Subtasks (1)")).toBeVisible();
  await page
    .getByRole("button", { name: "Open subtask Build the coordinator spine" })
    .click();
  const childDetail = page.getByTestId("project-issue-detail");
  await expect(
    childDetail.getByRole("heading", {
      name: /Build the coordinator spine/,
    }),
  ).toBeVisible();
  await childDetail.getByRole("button", { name: /Project task tree/ }).click();
  await expect(
    page
      .getByTestId("project-issue-detail")
      .getByRole("heading", { name: /Project task tree/ }),
  ).toBeVisible();
  await page.getByTestId("project-context-chat-agent").click();
  const chat = page.getByTestId("project-agent-chat-panel");
  await chat.getByTestId("message-input").fill("Work through the project plan");
  await chat.getByTestId("message-input").press("Enter");
  const readSentMessage = () =>
    page.evaluate(() => {
      const entries =
        (
          window as Window & {
            __BUZZ_E2E_COMMAND_PAYLOADS__?: Array<{
              command: string;
              payload: { channelId?: string; content?: string };
            }>;
          }
        ).__BUZZ_E2E_COMMAND_PAYLOADS__ ?? [];
      return entries
        .filter((entry) => entry.command === "send_channel_message")
        .at(-1)?.payload;
    });
  await expect
    .poll(async () => (await readSentMessage())?.content ?? "")
    .toContain(`Planned subtasks: 1`);
  await expect
    .poll(async () => (await readSentMessage())?.content ?? "")
    .toContain(`id: "${createdSubtask?.id}"`);
  await expect
    .poll(async () => (await readSentMessage())?.content ?? "")
    .toContain(
      "Acceptance criteria: A run stays linked to exactly one project issue.",
    );
});

test("issue discussion ignores an author-claimed origin channel", async ({
  page,
}) => {
  const forgedIssueId = "f".repeat(64);
  await page.addInitScript(
    ({ issueId, owner }) => {
      window.__BUZZ_E2E_EXTRA_PROJECT_EVENTS__ = [
        {
          id: issueId,
          kind: 1621,
          pubkey: owner,
          created_at: Math.floor(Date.now() / 1000) + 10,
          content: "This task claims an unrelated visible channel.",
          tags: [
            ["a", `30617:${owner}:buzz`],
            ["subject", "Forged origin task"],
            ["h", "9dae0116-799b-5071-a0a8-fdd30a91a35d"],
          ],
        },
      ];
    },
    { issueId: forgedIssueId, owner: DEFAULT_MOCK_PUBKEY },
  );
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();

  const issueRow = page
    .getByTestId("project-issue-row")
    .filter({ hasText: "Forged origin task" });
  await expect(issueRow).toBeVisible();
  await issueRow.getByRole("button", { name: /^#/ }).click();

  await page.getByTestId("project-context-discuss").click();
  const channelChoices = page.getByTestId("project-context-channel-choices");
  const relatedChannel = channelChoices.getByTestId(
    "project-context-related-channel",
  );
  await expect(relatedChannel).toHaveCount(1);
  await expect(relatedChannel).toContainText("#buzz");
  await expect(channelChoices).not.toContainText("#random");
  await relatedChannel.click();

  await expect(page.getByTestId("chat-title")).toHaveText("buzz");
  const issueDraftChip = page
    .getByTestId("message-input")
    .locator('[data-composer-buzz-link=""]', {
      hasText: "buzz",
    });
  await expect(issueDraftChip).toHaveAttribute(
    "data-href",
    new RegExp(`id=${forgedIssueId}`),
  );
  await page.getByTestId("channel-random").click();
  await expect(
    page.getByTestId("message-input").locator('[data-composer-buzz-link=""]'),
  ).toHaveCount(0);
});

test("issue comments use the project activity timeline", async ({ page }) => {
  await installMockBridge(page);
  await openBuzzProject(page);

  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  const issueRow = page.getByTestId("project-issue-row").first();
  await expect(issueRow).toBeVisible({ timeout: 10_000 });
  await issueRow.getByRole("button", { name: /^#/ }).click();

  const composer = page.getByTestId("project-issue-comment-composer");
  await expect(composer).toBeVisible();

  for (const comment of ISSUE_COMMENTS) {
    await composer.locator('[contenteditable="true"]').fill(comment);
    await composer.getByRole("button", { name: "Send message" }).click();
    await expect(page.getByText(comment, { exact: true })).toBeVisible({
      timeout: 10_000,
    });
  }

  const timelineRows = page.getByTestId("project-issue-comment-timeline-row");
  const earlierComments = page.getByTestId("project-issue-earlier-comments");
  const historyToggle = page.getByTestId(
    "project-issue-comment-history-toggle",
  );

  await expect(timelineRows).toHaveCount(3);
  await expect(earlierComments).toContainText("Show 1 earlier comment");
  await expect(
    timelineRows.filter({ hasText: "First issue comment" }),
  ).toHaveCount(0);
  await expect(
    timelineRows.filter({ hasText: "Fourth issue comment" }),
  ).toHaveCount(1);

  await earlierComments.click();
  await expect(timelineRows).toHaveCount(4);
  for (const comment of ISSUE_COMMENTS) {
    await expect(timelineRows.filter({ hasText: comment })).toHaveCount(1);
  }

  await historyToggle.click();
  await expect(timelineRows).toHaveCount(0);
  await expect(historyToggle).toContainText("Show 4 earlier comments");

  await historyToggle.click();
  await expect(timelineRows).toHaveCount(4);
});

test("issue assignees can be assigned and unassigned", async ({ page }) => {
  await installMockBridge(page);
  await openBuzzProject(page);

  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  const issueRow = page.getByTestId("project-issue-row").first();
  await expect(issueRow).toBeVisible({ timeout: 10_000 });
  await issueRow.getByRole("button", { name: /^#/ }).click();

  const issueHeader = page
    .getByTestId("project-issue-detail")
    .locator("header")
    .first();
  await expect(issueHeader).toContainText("Task created");
  await expect(issueHeader).not.toContainText("alice");
  await expect(issueHeader.locator("img")).toHaveCount(0);
  const contextAssignment = page.getByTestId("project-context-task-assignment");
  await expect(contextAssignment).toBeVisible();
  await expect(contextAssignment.locator("img")).toHaveCount(0);
  const selfAssign = page.getByTestId("project-context-issue-self-assign");
  await expect(selfAssign).toBeVisible();
  await expect(page.getByTestId("project-context-issue-assign")).toBeVisible();
  const createTask = page
    .getByTestId("project-repository-actions-panel")
    .getByRole("button", { name: "Create task", exact: true });
  await expect(createTask).toBeVisible();
  await expect(selfAssign.locator("svg")).toHaveCount(1);
  const actionGeometry = await Promise.all(
    [selfAssign, createTask].map((action) =>
      action.evaluate((element) => {
        const bounds = element.getBoundingClientRect();
        return {
          height: bounds.height,
          left: bounds.left,
          width: bounds.width,
        };
      }),
    ),
  );
  expect(actionGeometry[0]).toEqual(actionGeometry[1]);
  await selfAssign.click();
  await expect(contextAssignment).toContainText("Assigned to me");
  await expect(page.getByTestId("project-detail-section").first()).toHaveCSS(
    "border-top-width",
    "0px",
  );

  await page.getByTestId("project-issue-assign").click();
  const candidate = page
    .locator('[data-testid^="project-assignee-result-"]')
    .first();
  await expect(candidate).toBeVisible();
  const candidateTestId = await candidate.getAttribute("data-testid");
  const assignee = candidateTestId?.replace("project-assignee-result-", "");
  if (!assignee) throw new Error("Assignee result is missing its pubkey.");
  expect(assignee).toMatch(/^[0-9a-f]{64}$/);
  await candidate.click();

  const unassign = page.getByTestId(`project-issue-unassign-${assignee}`);
  await expect(unassign).toBeVisible({ timeout: 10_000 });
  const assigneeAvatar = unassign.locator("[data-avatar-shape]");
  const expectedShape = await assigneeAvatar.getAttribute("data-avatar-shape");
  await unassign.focus();
  await expect(unassign).toBeFocused();
  await expect(unassign).toHaveCSS("clip-path", "none");
  await expect(unassign).not.toHaveClass(/rounded-squircle/);
  await expect(assigneeAvatar).toHaveCSS(
    "clip-path",
    expectedShape === "squircle"
      ? /url\(["']?#rounded-squircle-clip["']?\)/
      : "none",
  );
  await unassign.click();
  await expect(page.getByText("Task unassigned.")).toBeVisible();
  await expect(unassign).toHaveCount(0, { timeout: 10_000 });
});
