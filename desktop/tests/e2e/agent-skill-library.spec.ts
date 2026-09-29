import { expect, test } from "@playwright/test";
import { waitForAnimations } from "../helpers/animations";

import { installMockBridge } from "../helpers/bridge";

const SHOTS = "test-results/agent-skill-library";
const skillContent =
  "---\nname: review-notes\ndescription: Keep code reviews focused\n---\nCheck the changed behavior and report evidence.\n";
const seededSkills = [
  {
    name: "review-notes",
    description: "Keep code reviews focused",
    content: skillContent,
    contentHash: "a".repeat(64),
    runtimeCompatibility: [
      {
        runtimeId: "goose",
        runtimeLabel: "Goose",
        skillDirectory: ".agents/skills",
        status: "linked" as const,
      },
    ],
  },
  {
    name: "test-planning",
    description: "Turn requirements into a small test plan",
    content:
      "---\nname: test-planning\ndescription: Turn requirements into a small test plan\n---\nList behavior and edge cases.\n",
    contentHash: "b".repeat(64),
    runtimeCompatibility: [],
  },
];

test.describe("Agent Skill Library mock-bridge smoke", () => {
  test.beforeEach(async ({ page }) => {
    // All app HTTP(S) traffic must stay on loopback; all WebSockets are closed
    // before connecting so the browser cannot contact a relay or provider.
    await page.route("**/*", async (route) => {
      const url = new URL(route.request().url());
      if (url.protocol === "http:" || url.protocol === "https:") {
        if (["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)) {
          await route.continue();
        } else {
          await route.abort("blockedbyclient");
        }
        return;
      }
      await route.continue();
    });
    await page.routeWebSocket("**/*", (socket) => {
      void socket.close({ code: 1008, reason: "E2E network isolation" });
    });

    const mock = { agentSkills: seededSkills } as unknown as NonNullable<
      Parameters<typeof installMockBridge>[1]
    >;
    await installMockBridge(page, mock);
  });

  test("opens, reads, creates, protects edits, and reviews a colliding pack", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/", { waitUntil: "domcontentloaded" });
    await page.getByTestId("open-agents-view").click();
    await page.getByTestId("agent-skill-library-open").click();

    const library = page.getByTestId("agent-skill-library");
    await expect(library).toBeVisible();
    await expect(
      library.getByRole("heading", { name: "review-notes" }),
    ).toBeVisible();
    await expect(page.getByTestId("agent-skill-content")).toHaveValue(
      skillContent,
    );
    await expect(library.getByText("Goose")).toBeVisible();
    await page.screenshot({
      path: `${SHOTS}/mocked-ipc-1440x900-desktop.png`,
      fullPage: false,
    });

    await page.getByTestId("agent-skill-create").click();
    await page.getByLabel("Skill name").fill("daily-review");
    const createdContent =
      "---\nname: daily-review\ndescription: Summarize review findings\n---\nGroup findings by impact.\n";
    await page.getByTestId("agent-skill-content").fill(createdContent);
    await page.getByTestId("agent-skill-save").click();
    await expect(
      library.getByRole("button", { name: /daily-review/ }),
    ).toBeVisible();

    await page.getByRole("button", { name: /review-notes/ }).click();
    await page
      .getByTestId("agent-skill-content")
      .fill(`${skillContent}\nUpdated.`);
    await page.getByRole("button", { name: /test-planning/ }).click();
    const discardDialog = page.getByRole("alertdialog", {
      name: "Discard unsaved changes?",
    });
    await expect(discardDialog).toBeVisible();
    await discardDialog.getByRole("button", { name: "Keep editing" }).click();
    await expect(discardDialog).toBeHidden();
    await page.getByRole("button", { name: /test-planning/ }).click();
    await page
      .getByRole("alertdialog", { name: "Discard unsaved changes?" })
      .getByRole("button", { name: "Discard changes" })
      .click();
    await expect(page.getByTestId("agent-skill-content")).toHaveValue(
      seededSkills[1].content,
    );

    await page.getByTestId("agent-skill-export-pack").click();
    const exportDialog = page.getByRole("dialog", {
      name: "Build a skill pack",
    });
    await expect(exportDialog).toBeVisible();
    const includeReviewNotes = exportDialog.getByRole("checkbox", {
      name: "Include review-notes in skill pack",
    });
    await includeReviewNotes.check();
    await expect(includeReviewNotes).toBeChecked();
    await exportDialog
      .getByTestId("agent-skill-pack-export-review-review-notes")
      .click();
    const exportConfirm = exportDialog.getByTestId(
      "agent-skill-pack-export-confirm",
    );
    await expect(exportConfirm).toBeEnabled();
    await exportConfirm.click();
    await expect(exportDialog).toBeHidden();
    await expect(
      page.getByText("Skill pack saved to your chosen location.", {
        exact: true,
      }),
    ).toBeVisible();

    await page.getByTestId("agent-skill-import-pack").click();
    await page.getByTestId("agent-skill-pack-input").setInputFiles({
      name: "duplicate.agent.zip",
      mimeType: "application/zip",
      buffer: Buffer.from("mock pack bytes"),
    });
    const reviewDialog = page.getByRole("dialog", {
      name: "Review 2-skill pack",
    });
    await expect(reviewDialog).toBeVisible();
    await expect(
      reviewDialog.getByText("Reviewed 0 of 2 skills."),
    ).toBeVisible();
    await expect(
      reviewDialog.getByText(/will install none of the skills/),
    ).toBeVisible();
    await reviewDialog.getByRole("button", { name: /new-from-pack/ }).click();
    await expect(
      reviewDialog.getByText("Reviewed 1 of 2 skills."),
    ).toBeVisible();
    await reviewDialog.getByRole("button", { name: /review-notes/ }).click();
    await expect(
      reviewDialog.getByText("Reviewed 2 of 2 skills."),
    ).toBeVisible();
    await expect(
      reviewDialog.getByRole("button", { name: "Install 2 skills" }),
    ).toBeDisabled();

    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({
      path: `${SHOTS}/mocked-ipc-390x844-pack-review.png`,
      fullPage: false,
    });
    await reviewDialog.getByRole("button", { name: "Cancel" }).click();
    await expect(reviewDialog).toBeHidden();
    await page.getByTestId("dialog-overlay").waitFor({ state: "detached" });
    await expect(library).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      )
      .toBe(true);
    await page.screenshot({
      path: `${SHOTS}/mocked-ipc-390x844-layout.png`,
      fullPage: false,
    });
  });

  test("opens the critic run flow cleanly on desktop and mobile", async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/", { waitUntil: "domcontentloaded" });
    await page.getByTestId("open-agents-view").click();
    await page.getByTestId("agent-critic-run-open").click();

    const dialog = page.getByTestId("critic-run-dialog");
    await expect(dialog).toBeVisible();
    await waitForAnimations(page);
    await expect(
      dialog.getByText(
        /The local inference service may forward data elsewhere/,
      ),
    ).toBeVisible();
    await expect(dialog.getByTestId("critic-run-submit")).toBeDisabled();
    const scrollRegion = dialog.getByTestId("critic-run-scroll-region");
    const correctnessRoute = dialog.getByLabel("Local route for Correctness");
    const securityRoute = dialog.getByLabel("Local route for Security");
    await correctnessRoute.selectOption("e2e-local-critic");
    await securityRoute.selectOption("e2e-local-critic");
    await expect(
      dialog.getByText(/openai \/ e2e-local-model: ready/).first(),
    ).toBeVisible();
    await expect(
      dialog.getByText(/e2e-local-critic · v1/).first(),
    ).toBeVisible();
    await expect(dialog.getByTestId("critic-run-submit")).toBeDisabled();
    await dialog.getByLabel("Original goal").fill("Check the patch");
    await dialog.getByLabel("Review focus").fill("Check edge cases");
    await dialog
      .getByLabel("Frozen review text")
      .fill("diff --git a/app.ts b/app.ts\n+safe change");
    await dialog
      .getByLabel("Requested round estimate ceiling in USD")
      .fill("0.12");
    await expect(
      dialog.getByText(/\$0\.12 requested round estimate ceiling/),
    ).toBeVisible();
    await expect(
      dialog.getByText(
        /correctness: \$0\.06 requested share; effective estimate ceiling \$0\.06/,
      ),
    ).toBeVisible();
    await expect(dialog.getByTestId("critic-run-submit")).toBeEnabled();
    const roundBudget = dialog.getByLabel(
      "Requested round estimate ceiling in USD",
    );
    await roundBudget.scrollIntoViewIfNeeded();
    await scrollRegion.evaluate((element) => {
      element.scrollTop += 180;
    });
    await page.screenshot({
      path: testInfo.outputPath("critic-budget-desktop.png"),
      fullPage: false,
    });
    await correctnessRoute.scrollIntoViewIfNeeded();
    await expect(
      dialog.getByText("Results will appear here after you run a review."),
    ).toBeInViewport();
    await page.screenshot({
      path: testInfo.outputPath("critic-route-profile-desktop.png"),
      fullPage: false,
    });
    await expect
      .poll(() =>
        scrollRegion.evaluate(
          (element) => element.scrollHeight > element.clientHeight,
        ),
      )
      .toBe(true);
    const submit = dialog.getByTestId("critic-run-submit");
    await submit.scrollIntoViewIfNeeded();
    await expect(submit).toBeInViewport();
    await scrollRegion.evaluate((element) => {
      element.scrollTop = 0;
    });
    await page.screenshot({
      path: testInfo.outputPath("critic-run-desktop.png"),
      fullPage: false,
    });

    await page.setViewportSize({ width: 390, height: 844 });
    await expect(dialog).toBeVisible();
    await waitForAnimations(page);
    await roundBudget.scrollIntoViewIfNeeded();
    await page.screenshot({
      path: testInfo.outputPath("critic-budget-mobile.png"),
      fullPage: false,
    });
    await expect
      .poll(() =>
        page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      )
      .toBe(true);
    await expect
      .poll(() =>
        scrollRegion.evaluate(
          (element) => element.scrollHeight > element.clientHeight,
        ),
      )
      .toBe(true);
    await correctnessRoute.scrollIntoViewIfNeeded();
    await expect
      .poll(() => scrollRegion.evaluate((element) => element.scrollLeft))
      .toBe(0);
    await page.screenshot({
      path: testInfo.outputPath("critic-route-profile-mobile.png"),
      fullPage: false,
    });
    await submit.scrollIntoViewIfNeeded();
    await expect(submit).toBeInViewport();
    await scrollRegion.evaluate((element) => {
      element.scrollTop = 0;
    });
    await page.screenshot({
      path: testInfo.outputPath("critic-run-mobile.png"),
      fullPage: false,
    });
  });
});
