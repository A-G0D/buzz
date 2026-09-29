import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import React from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ThemeProvider } from "@/shared/theme/ThemeProvider";

const handlers = new Map();
const tauriMock = {
  invoke(command, args) {
    const handler = handlers.get(command);
    if (handler) return handler(args);
    return Promise.reject(new Error(`Unexpected Tauri command: ${command}`));
  },
  transformCallback() {
    return 1;
  },
};
globalThis.__TAURI_INTERNALS__ = tauriMock;
globalThis.window.__TAURI_INTERNALS__ = tauriMock;
globalThis.window.matchMedia ??= () => ({
  matches: false,
  addEventListener() {},
  removeEventListener() {},
});

const { AgentSkillLibrary } = await import("./AgentSkillLibrary.tsx");

afterEach(() => {
  cleanup();
  handlers.clear();
});

function mountLibrary(onBack = () => {}) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  render(
    React.createElement(
      ThemeProvider,
      null,
      React.createElement(
        QueryClientProvider,
        { client: queryClient },
        React.createElement(AgentSkillLibrary, { onBack }),
      ),
    ),
  );
  return { queryClient };
}

test("Agent Skill Library reviews local text and saves a new skill through the guarded API", async () => {
  const original = {
    name: "plan-review",
    description: "Review project plans for missing assumptions.",
    contentHash: "old-hash",
    validationError: null,
    runtimeCompatibility: [
      {
        runtimeId: "claude",
        runtimeLabel: "Claude Code",
        skillDirectory: ".claude/skills",
        status: "linked",
      },
      {
        runtimeId: "codex",
        runtimeLabel: "Codex",
        skillDirectory: ".codex/skills",
        status: "linked",
      },
    ],
    content:
      "---\nname: plan-review\ndescription: Review project plans for missing assumptions.\n---\n\n# Plan review\n\nCheck evidence and risks.\n",
  };
  const second = {
    name: "decision-review",
    description: "Review project decisions.",
    contentHash: "second-hash",
    validationError: null,
    runtimeCompatibility: [],
    content:
      "---\nname: decision-review\ndescription: Review project decisions.\n---\n\n# Decision review\n\nList tradeoffs.\n",
  };
  let savedInput;
  let exportedInput;
  handlers.set("list_agent_skills", () => [original, second]);
  handlers.set("read_agent_skill", ({ name }) =>
    name === original.name ? original : second,
  );
  handlers.set("export_agent_skill_pack", (args) => {
    exportedInput = args;
    return true;
  });
  handlers.set("save_agent_skill", (args) => {
    savedInput = args;
    return {
      name: args.name,
      description: "Review project notes for decisions.",
      contentHash: "new-hash",
      validationError: null,
      runtimeCompatibility: [original.runtimeCompatibility[0]],
      content: args.content,
    };
  });

  mountLibrary();

  await waitFor(() =>
    assert.match(
      screen.getByLabelText("SKILL.md").value,
      /Check evidence and risks\./,
    ),
  );
  assert.ok(screen.getByText("Runtime compatibility"));
  assert.ok(screen.getByText("Claude Code"));
  fireEvent.click(screen.getByRole("button", { name: "Build skill pack" }));
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: "Include decision-review in skill pack",
    }),
  );
  await waitFor(() =>
    assert.ok(
      screen.getByTestId("agent-skill-pack-export-review-decision-review"),
    ),
  );
  const exportConfirm = screen.getByRole("button", {
    name: "Export reviewed pack",
  });
  assert.equal(exportConfirm.disabled, true);
  fireEvent.click(
    screen.getByTestId("agent-skill-pack-export-review-decision-review"),
  );
  assert.match(
    screen.getByText(/List tradeoffs\./).textContent,
    /List tradeoffs\./,
  );
  assert.equal(exportConfirm.disabled, false);
  fireEvent.click(exportConfirm);
  await waitFor(() => assert.equal(exportedInput?.skills?.length, 2));
  assert.deepEqual(
    exportedInput.skills,
    [original, second].map(({ name, contentHash }) => ({
      name,
      expectedContentHash: contentHash,
    })),
  );
  assert.match(screen.getByRole("status").textContent, /chosen location/);

  fireEvent.click(screen.getByRole("button", { name: "Create skill" }));
  const nameField = screen.getByLabelText("Skill name");
  fireEvent.change(nameField, { target: { value: "decision-review" } });
  const contentField = screen.getByLabelText("SKILL.md");
  assert.match(contentField.value, /# decision-review/);
  fireEvent.change(contentField, {
    target: {
      value:
        "---\nname: decision-review\ndescription: Review project notes for decisions.\n---\n\n# Decision review\n\nList decisions and open questions.\n",
    },
  });
  fireEvent.click(screen.getByRole("button", { name: "Save skill" }));

  await waitFor(() => assert.equal(savedInput?.name, "decision-review"));
  assert.equal(savedInput.expectedContentHash, null);
  assert.match(savedInput.content, /name: decision-review/);
  await waitFor(() =>
    assert.match(
      screen.getByLabelText("SKILL.md").value,
      /List decisions and open questions\./,
    ),
  );
});

test("Agent Skill Library asks before discarding edits when switching or leaving", async () => {
  const firstSkill = {
    name: "plan-review",
    description: "Review project plans.",
    contentHash: "first-hash",
    validationError: null,
    runtimeCompatibility: [],
    content:
      "---\nname: plan-review\ndescription: Review project plans.\n---\n\n# Plan review\n\nKeep the original text.\n",
  };
  const secondSkill = {
    name: "decision-review",
    description: "Review project decisions.",
    contentHash: "second-hash",
    validationError: null,
    runtimeCompatibility: [],
    content:
      "---\nname: decision-review\ndescription: Review project decisions.\n---\n\n# Decision review\n\nList open questions.\n",
  };
  let didBack = false;
  let refreshedFirstSkill = firstSkill;
  handlers.set("list_agent_skills", () => [firstSkill, secondSkill]);
  handlers.set("read_agent_skill", ({ name }) =>
    name === firstSkill.name ? refreshedFirstSkill : secondSkill,
  );

  const { queryClient } = mountLibrary(() => {
    didBack = true;
  });

  await waitFor(() =>
    assert.match(
      screen.getByLabelText("SKILL.md").value,
      /Keep the original text\./,
    ),
  );
  fireEvent.change(screen.getByLabelText("SKILL.md"), {
    target: {
      value: firstSkill.content.replace(
        "Keep the original text.",
        "My unsaved edit.",
      ),
    },
  });
  refreshedFirstSkill = {
    ...firstSkill,
    contentHash: "refreshed-hash",
    content: firstSkill.content.replace(
      "Keep the original text.",
      "Changed elsewhere.",
    ),
  };
  await queryClient.invalidateQueries({
    queryKey: ["agent-skills", firstSkill.name],
  });
  assert.match(screen.getByLabelText("SKILL.md").value, /My unsaved edit\./);
  fireEvent.click(screen.getByRole("button", { name: /decision-review/ }));

  assert.equal(
    screen
      .getByRole("alertdialog")
      .textContent.includes("Discard unsaved changes?"),
    true,
  );
  fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
  assert.match(screen.getByLabelText("SKILL.md").value, /My unsaved edit\./);

  fireEvent.click(screen.getByRole("button", { name: /decision-review/ }));
  fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
  await waitFor(() =>
    assert.match(
      screen.getByLabelText("SKILL.md").value,
      /List open questions\./,
    ),
  );

  fireEvent.change(screen.getByLabelText("SKILL.md"), {
    target: {
      value: secondSkill.content.replace(
        "List open questions.",
        "More unsaved edits.",
      ),
    },
  });
  fireEvent.click(screen.getByRole("button", { name: "Back to agents" }));
  assert.equal(didBack, false);
  fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
  assert.match(screen.getByLabelText("SKILL.md").value, /More unsaved edits\./);

  fireEvent.click(screen.getByRole("button", { name: "Back to agents" }));
  fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
  assert.equal(didBack, true);
});

test("Agent Skill Library previews pack contents and preserves an existing draft on install", async () => {
  const currentSkill = {
    name: "plan-review",
    description: "Review project plans.",
    contentHash: "current-hash",
    validationError: null,
    runtimeCompatibility: [],
    content:
      "---\nname: plan-review\ndescription: Review project plans.\n---\n\n# Plan review\n\nKeep this draft.\n",
  };
  const importedSkill = {
    name: "decision-review",
    description: "Review project decisions.",
    contentHash: "pack-hash",
    validationError: null,
    runtimeCompatibility: [],
    content:
      "---\nname: decision-review\ndescription: Review project decisions.\n---\n\n# Decision review\n\nList assumptions.\n",
  };
  const anotherImportedSkill = {
    name: "risk-review",
    description: "Identify project risks.",
    contentHash: "risk-hash",
    validationError: null,
    runtimeCompatibility: [],
    content:
      "---\nname: risk-review\ndescription: Identify project risks.\n---\n\n# Risk review\n\nName failure modes.\n",
  };
  let previewInput;
  let installInput;
  handlers.set("list_agent_skills", () => [currentSkill]);
  handlers.set("read_agent_skill", () => currentSkill);
  handlers.set("preview_agent_skill_pack", (args) => {
    previewInput = args;
    return {
      version: 2,
      exportedFrom: "Buzz Skill Library",
      skills: [
        { ...importedSkill, license: "Apache-2.0", alreadyInstalled: false },
        { ...anotherImportedSkill, license: null, alreadyInstalled: false },
      ],
    };
  });
  handlers.set("install_agent_skill_pack", (args) => {
    installInput = args;
    return [importedSkill, anotherImportedSkill];
  });

  mountLibrary();
  await waitFor(() =>
    assert.match(screen.getByLabelText("SKILL.md").value, /Keep this draft\./),
  );
  fireEvent.change(screen.getByLabelText("SKILL.md"), {
    target: {
      value: currentSkill.content.replace("Keep this draft.", "Unsaved work."),
    },
  });

  const input = screen.getByTestId("agent-skill-pack-input");
  const file = new window.File(["pack bytes"], "decision-review.agent.zip", {
    type: "application/zip",
  });
  fireEvent.change(input, { target: { files: [file] } });
  await waitFor(() => assert.ok(previewInput));
  assert.ok(Array.isArray(previewInput.fileBytes));
  assert.equal(
    screen.getByRole("dialog").textContent.includes(importedSkill.content),
    true,
  );
  assert.ok(screen.getByText("Apache-2.0"));
  const installButton = screen.getByRole("button", {
    name: "Install 2 skills",
  });
  assert.equal(installButton.disabled, true);
  fireEvent.click(screen.getByRole("button", { name: /risk-review/ }));
  assert.equal(
    screen
      .getByRole("dialog")
      .textContent.includes(anotherImportedSkill.content),
    true,
  );
  assert.equal(installButton.disabled, true);
  fireEvent.click(screen.getByRole("button", { name: /decision-review/ }));
  assert.equal(installButton.disabled, false);

  fireEvent.click(installButton);
  await waitFor(() => assert.equal(installInput?.expectedSkills?.length, 2));
  assert.deepEqual(installInput.expectedSkills, [
    { name: importedSkill.name, contentHash: importedSkill.contentHash },
    {
      name: anotherImportedSkill.name,
      contentHash: anotherImportedSkill.contentHash,
    },
  ]);
  await waitFor(() =>
    assert.match(screen.getByRole("status").textContent, /Installed 2 skills/),
  );
  assert.match(screen.getByLabelText("SKILL.md").value, /Unsaved work\./);
});

test("Agent Skill Library rejects oversized packs before sending them to Rust", async () => {
  let previewCalled = false;
  handlers.set("list_agent_skills", () => []);
  handlers.set("preview_agent_skill_pack", () => {
    previewCalled = true;
    return Promise.reject(new Error("Should not preview an oversized pack"));
  });

  mountLibrary();
  const file = new window.File(
    [new Uint8Array(1024 * 1024 + 1)],
    "too-large.agent.zip",
    { type: "application/zip" },
  );
  const input = screen.getByTestId("agent-skill-pack-input");
  fireEvent.change(input, { target: { files: [file] } });

  assert.match(screen.getByRole("alert").textContent, /smaller than 1 MiB/);
  assert.equal(previewCalled, false);
  assert.equal(input.value, "");
});

test("Agent Skill Library shows exact overlap reasons and hides stale results while editing", async () => {
  const selected = {
    name: "api-review",
    description: "Review API migration plans",
    contentHash: "selected-hash",
    validationError: null,
    runtimeCompatibility: [],
    content:
      "---\nname: api-review\ndescription: Review API migration plans\n---\n\n# API migration checklist\n\nCheck release readiness.\n",
  };
  const candidate = {
    name: "migration-check",
    description: "API migration review",
    contentHash: "candidate-hash",
    validationError: null,
  };
  const identical = {
    name: "renamed-copy",
    description: "Unrelated title",
    contentHash: "selected-hash",
    validationError: null,
  };
  const commands = [];
  handlers.set("list_agent_skills", () => [selected, candidate, identical]);
  handlers.set("read_agent_skill", () => selected);
  handlers.set("save_agent_skill", (args) => {
    commands.push(["save_agent_skill", args]);
    return selected;
  });
  handlers.set("install_agent_skill_pack", (args) => {
    commands.push(["install_agent_skill_pack", args]);
    return [];
  });

  mountLibrary();
  const preview = await screen.findByTestId("agent-skill-overlap-preview");
  assert.match(preview.textContent, /migration-check/);
  assert.match(
    preview.textContent,
    /Shared normalized terms: api, migration, review/,
  );
  assert.match(preview.textContent, /renamed-copy/);
  assert.match(preview.textContent, /Matching SHA-256 content hash/);
  assert.match(preview.textContent, /not a quality grade/);
  assert.equal(screen.queryByRole("button", { name: /merge|delete/i }), null);

  fireEvent.change(screen.getByLabelText("SKILL.md"), {
    target: { value: `${selected.content}\nA new unsaved edit.\n` },
  });
  await waitFor(() =>
    assert.equal(screen.queryByTestId("agent-skill-overlap-preview"), null),
  );
  assert.deepEqual(commands, []);
});
