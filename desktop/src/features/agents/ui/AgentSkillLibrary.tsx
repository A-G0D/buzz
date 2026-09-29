import * as React from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft,
  BookOpen,
  Download,
  FilePlus2,
  Save,
  Search,
  Upload,
} from "lucide-react";

import {
  listAgentSkills,
  readAgentSkill,
  exportAgentSkillPack,
  installAgentSkillPack,
  previewAgentSkillPack,
  saveAgentSkill,
  type AgentSkillDetails,
  type AgentSkillPackSelection,
  type AgentSkillPackPreview,
} from "@/shared/api/tauriAgentSkills";
import { possibleSkillOverlaps } from "./skillOverlap";
import { Button } from "@/shared/ui/button";
import { Checkbox } from "@/shared/ui/checkbox";
import { Input } from "@/shared/ui/input";
import { Textarea } from "@/shared/ui/textarea";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/shared/ui/alert-dialog";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";

const SKILLS_KEY = ["agent-skills"] as const;

type SkillPackReview = {
  preview: AgentSkillPackPreview;
  fileBytes: number[];
};

type SkillPackExportReview = {
  selectedNames: string[];
  reviewedNames: string[];
  skills: Record<string, AgentSkillDetails>;
  activeName: string | null;
  loadingNames: string[];
  error: string | null;
};

type PendingNavigation =
  | { kind: "create" }
  | { kind: "select"; name: string }
  | { kind: "back" };

function starterSkill(name: string) {
  return `---\nname: ${name}\ndescription: Describe what this skill helps an agent do.\n---\n\n# ${name}\n\n## Instructions\n\nWrite the steps an agent should follow. Keep this skill focused and reviewable.\n`;
}

export function AgentSkillLibrary({ onBack }: { onBack: () => void }) {
  const queryClient = useQueryClient();
  const skillsQuery = useQuery({
    queryKey: SKILLS_KEY,
    queryFn: listAgentSkills,
  });
  const [selectedName, setSelectedName] = React.useState<string | null>(null);
  const [isCreating, setIsCreating] = React.useState(false);
  const [search, setSearch] = React.useState("");
  const [nameDraft, setNameDraft] = React.useState("");
  const [contentDraft, setContentDraft] = React.useState("");
  const [savedSkill, setSavedSkill] = React.useState<AgentSkillDetails | null>(
    null,
  );
  const [saveError, setSaveError] = React.useState<string | null>(null);
  const [exportNotice, setExportNotice] = React.useState<string | null>(null);
  const [exportReviewOpen, setExportReviewOpen] = React.useState(false);
  const [exportReview, setExportReview] = React.useState<SkillPackExportReview>(
    {
      selectedNames: [],
      reviewedNames: [],
      skills: {},
      activeName: null,
      loadingNames: [],
      error: null,
    },
  );
  const [packReview, setPackReview] = React.useState<SkillPackReview | null>(
    null,
  );
  const [activePackSkillName, setActivePackSkillName] = React.useState<
    string | null
  >(null);
  const [reviewedPackSkillNames, setReviewedPackSkillNames] = React.useState<
    string[]
  >([]);
  const [packImportError, setPackImportError] = React.useState<string | null>(
    null,
  );
  const [packImportNotice, setPackImportNotice] = React.useState<string | null>(
    null,
  );
  const packInputRef = React.useRef<HTMLInputElement>(null);
  const [pendingNavigation, setPendingNavigation] =
    React.useState<PendingNavigation | null>(null);
  const isDirty = isCreating
    ? nameDraft !== "new-skill" || contentDraft !== starterSkill("new-skill")
    : savedSkill !== null && contentDraft !== savedSkill.content;

  const selectedQuery = useQuery({
    queryKey: [...SKILLS_KEY, selectedName],
    queryFn: () => readAgentSkill(selectedName ?? ""),
    enabled: selectedName !== null && !isCreating,
  });

  React.useEffect(() => {
    if (isCreating) return;
    const skill = selectedQuery.data;
    if (!skill || skill.name !== selectedName) return;
    if (isDirty) return;
    setNameDraft(skill.name);
    setContentDraft(skill.content);
    setSavedSkill(skill);
    setSaveError(null);
  }, [isCreating, isDirty, selectedName, selectedQuery.data]);

  React.useEffect(() => {
    if (selectedName !== null || isCreating) return;
    const firstSkill = skillsQuery.data?.[0];
    if (firstSkill) setSelectedName(firstSkill.name);
  }, [isCreating, selectedName, skillsQuery.data]);

  const saveMutation = useMutation({
    mutationFn: saveAgentSkill,
    onSuccess: async (skill) => {
      setSelectedName(skill.name);
      setIsCreating(false);
      setNameDraft(skill.name);
      setContentDraft(skill.content);
      setSavedSkill(skill);
      setSaveError(null);
      setExportNotice(null);
      queryClient.setQueryData([...SKILLS_KEY, skill.name], skill);
      await queryClient.invalidateQueries({ queryKey: SKILLS_KEY });
    },
    onError: (error) => {
      setSaveError(error instanceof Error ? error.message : String(error));
    },
  });

  const exportMutation = useMutation({
    mutationFn: exportAgentSkillPack,
    onSuccess: (saved) => {
      setExportNotice(
        saved ? "Skill pack saved to your chosen location." : null,
      );
      if (saved) setExportReviewOpen(false);
    },
    onError: (error, input) => {
      const message = error instanceof Error ? error.message : String(error);
      setExportReview((current) => ({ ...current, error: message }));
      void queryClient.invalidateQueries({ queryKey: SKILLS_KEY });
      void Promise.all(
        input.skills.map(({ name }) =>
          queryClient.invalidateQueries({
            queryKey: [...SKILLS_KEY, name],
          }),
        ),
      );
    },
  });

  const previewPackMutation = useMutation({
    mutationFn: previewAgentSkillPack,
    onSuccess: (preview, fileBytes) => {
      setPackImportError(null);
      setPackImportNotice(null);
      setPackReview({ preview, fileBytes });
      setActivePackSkillName(preview.skills[0]?.name ?? null);
      setReviewedPackSkillNames([]);
    },
    onError: (error) => {
      setPackImportError(
        error instanceof Error ? error.message : String(error),
      );
    },
  });

  const installPackMutation = useMutation({
    mutationFn: installAgentSkillPack,
    onSuccess: async (installedSkills) => {
      setPackReview(null);
      setPackImportError(null);
      setPackImportNotice(
        `Installed ${installedSkills.length} skill${installedSkills.length === 1 ? "" : "s"} in this Buzz workspace.`,
      );
      for (const skill of installedSkills) {
        queryClient.setQueryData([...SKILLS_KEY, skill.name], skill);
      }
      await queryClient.invalidateQueries({ queryKey: SKILLS_KEY });
    },
    onError: (error) => {
      setPackImportError(
        error instanceof Error ? error.message : String(error),
      );
    },
  });

  const skills = skillsQuery.data ?? [];
  const overlapPreviewReady =
    skillsQuery.isSuccess &&
    !isCreating &&
    !isDirty &&
    savedSkill !== null &&
    savedSkill.validationError === null;
  const possibleOverlaps = overlapPreviewReady
    ? possibleSkillOverlaps(savedSkill, skills)
    : [];
  const filteredSkills = skills.filter((skill) =>
    `${skill.name} ${skill.description}`
      .toLowerCase()
      .includes(search.toLowerCase()),
  );
  const canSave = isDirty && !saveMutation.isPending;

  function applyNavigation(navigation: PendingNavigation) {
    if (navigation.kind === "back") {
      onBack();
      return;
    }
    if (navigation.kind === "select") {
      setIsCreating(false);
      setSelectedName(navigation.name);
      setSavedSkill(null);
      setSaveError(null);
      setExportNotice(null);
      return;
    }

    const base = "new-skill";
    setSelectedName(null);
    setIsCreating(true);
    setNameDraft(base);
    setContentDraft(starterSkill(base));
    setSavedSkill(null);
    setSaveError(null);
    setExportNotice(null);
  }

  function requestNavigation(navigation: PendingNavigation) {
    if (
      navigation.kind === "select" &&
      !isCreating &&
      selectedName === navigation.name
    ) {
      return;
    }
    if (isDirty) {
      setPendingNavigation(navigation);
      return;
    }
    applyNavigation(navigation);
  }

  function save() {
    saveMutation.mutate({
      name: nameDraft.trim(),
      content: contentDraft,
      expectedContentHash: isCreating
        ? null
        : (savedSkill?.contentHash ?? null),
    });
  }

  function openExportReview() {
    if (!savedSkill || isDirty || savedSkill.validationError) return;
    setExportNotice(null);
    setExportReview({
      selectedNames: [savedSkill.name],
      reviewedNames: [savedSkill.name],
      skills: { [savedSkill.name]: savedSkill },
      activeName: savedSkill.name,
      loadingNames: [],
      error: null,
    });
    setExportReviewOpen(true);
  }

  async function toggleExportSkill(name: string, checked: boolean) {
    const selectedNames = exportReview.selectedNames;
    if (!checked) {
      const remaining = selectedNames.filter((item) => item !== name);
      setExportReview((current) => ({
        ...current,
        selectedNames: remaining,
        reviewedNames: current.reviewedNames.filter((item) => item !== name),
        activeName:
          current.activeName === name
            ? (remaining[0] ?? null)
            : current.activeName,
        error: null,
      }));
      return;
    }

    if (selectedNames.includes(name)) return;
    if (selectedNames.length >= 32) {
      setExportReview((current) => ({
        ...current,
        error: "A skill pack can include at most 32 skills.",
      }));
      return;
    }

    setExportReview((current) => ({
      ...current,
      selectedNames: [...current.selectedNames, name],
      activeName: current.activeName ?? name,
      loadingNames: [...current.loadingNames, name],
      error: null,
    }));
    try {
      const skill = await readAgentSkill(name);
      const summary = skills.find((item) => item.name === name);
      if (!summary || skill.contentHash !== summary.contentHash) {
        throw new Error(
          `${name} changed while the pack was being reviewed. Refresh the library and review it again.`,
        );
      }
      if (skill.validationError) {
        throw new Error(
          `${name} needs review before it can be exported: ${skill.validationError}`,
        );
      }
      setExportReview((current) =>
        current.selectedNames.includes(name)
          ? {
              ...current,
              skills: { ...current.skills, [name]: skill },
              reviewedNames:
                current.activeName === name &&
                !current.reviewedNames.includes(name)
                  ? [...current.reviewedNames, name]
                  : current.reviewedNames,
            }
          : current,
      );
    } catch (error) {
      setExportReview((current) => ({
        ...current,
        error: error instanceof Error ? error.message : String(error),
      }));
      void queryClient.invalidateQueries({ queryKey: SKILLS_KEY });
    } finally {
      setExportReview((current) => ({
        ...current,
        loadingNames: current.loadingNames.filter((item) => item !== name),
      }));
    }
  }

  function exportReviewedPack() {
    const reviewedSkills = exportReview.selectedNames.map(
      (name) => exportReview.skills[name],
    );
    if (
      reviewedSkills.length === 0 ||
      reviewedSkills.some((skill) => !skill || skill.validationError) ||
      exportReview.loadingNames.length > 0
    ) {
      return;
    }
    setExportReview((current) => ({ ...current, error: null }));
    const selection: AgentSkillPackSelection[] = reviewedSkills.map(
      (skill) => ({ name: skill.name, expectedContentHash: skill.contentHash }),
    );
    exportMutation.mutate({ skills: selection });
  }

  const activeExportSkill = exportReview.activeName
    ? exportReview.skills[exportReview.activeName]
    : undefined;
  const reviewedSelectionReady =
    exportReview.selectedNames.length > 0 &&
    exportReview.loadingNames.length === 0 &&
    exportReview.selectedNames.every((name) => {
      const detail = exportReview.skills[name];
      const summary = skills.find((item) => item.name === name);
      return (
        detail !== undefined &&
        detail.validationError === null &&
        summary?.contentHash === detail.contentHash
      );
    }) &&
    exportReview.selectedNames.every((name) =>
      exportReview.reviewedNames.includes(name),
    );
  const activeImportedSkill = packReview?.preview.skills.find(
    (skill) => skill.name === activePackSkillName,
  );

  return (
    <div className="space-y-3">
      <Button
        disabled={exportMutation.isPending}
        onClick={() => requestNavigation({ kind: "back" })}
        size="sm"
        type="button"
        variant="outline"
      >
        <ArrowLeft />
        Back to agents
      </Button>
      <section
        aria-label="Agent skill library"
        className="grid min-h-[32rem] grid-cols-1 overflow-hidden rounded-lg border border-border bg-card md:grid-cols-[18rem_minmax(0,1fr)]"
        data-testid="agent-skill-library"
      >
        <aside className="flex min-h-64 flex-col border-b border-border md:border-b-0 md:border-r">
          <div className="space-y-3 p-4">
            <div className="flex items-center justify-between gap-2">
              <div>
                <h2 className="text-sm font-semibold">Skills</h2>
                <p className="text-xs text-muted-foreground">
                  {skills.length} in this workspace
                </p>
              </div>
              <div className="flex items-center gap-1">
                <Button
                  aria-label="Import skill pack"
                  data-testid="agent-skill-import-pack"
                  disabled={
                    previewPackMutation.isPending ||
                    installPackMutation.isPending
                  }
                  onClick={() => packInputRef.current?.click()}
                  size="icon"
                  type="button"
                  variant="outline"
                >
                  <Upload />
                </Button>
                <Button
                  aria-label="Create skill"
                  data-testid="agent-skill-create"
                  onClick={() => requestNavigation({ kind: "create" })}
                  disabled={exportMutation.isPending}
                  size="icon"
                  type="button"
                  variant="outline"
                >
                  <FilePlus2 />
                </Button>
              </div>
            </div>
            <div className="relative block">
              <label className="sr-only" htmlFor="agent-skill-search">
                Search skills
              </label>
              <Search className="pointer-events-none absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
              <Input
                className="pl-8"
                id="agent-skill-search"
                onChange={(event) => setSearch(event.target.value)}
                placeholder="Search skills"
                value={search}
              />
            </div>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
            {skillsQuery.isLoading ? (
              <p className="px-3 py-6 text-sm text-muted-foreground">
                Loading skills…
              </p>
            ) : skillsQuery.error ? (
              <p className="px-3 py-6 text-sm text-destructive">
                {skillsQuery.error instanceof Error
                  ? skillsQuery.error.message
                  : "Skills could not be loaded."}
              </p>
            ) : filteredSkills.length === 0 ? (
              <p className="px-3 py-6 text-sm text-muted-foreground">
                {skills.length === 0
                  ? "Create a skill to get started."
                  : "No matching skills."}
              </p>
            ) : (
              <ul className="space-y-1">
                {filteredSkills.map((skill) => (
                  <li key={skill.name}>
                    <button
                      aria-current={
                        selectedName === skill.name && !isCreating
                          ? "page"
                          : undefined
                      }
                      className={`w-full rounded-md px-3 py-2 text-left transition-colors hover:bg-accent ${selectedName === skill.name && !isCreating ? "bg-accent" : ""}`}
                      disabled={exportMutation.isPending}
                      onClick={() =>
                        requestNavigation({ kind: "select", name: skill.name })
                      }
                      type="button"
                    >
                      <span className="flex items-center gap-2 text-sm font-medium">
                        <BookOpen className="h-4 w-4 shrink-0 text-muted-foreground" />
                        <span className="truncate">{skill.name}</span>
                      </span>
                      <span className="mt-1 block line-clamp-2 pl-6 text-xs text-muted-foreground">
                        {skill.validationError ?? skill.description}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </aside>

        <div className="flex min-w-0 flex-col p-4 sm:p-6">
          {isCreating || selectedName ? (
            <>
              <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
                <div className="space-y-1">
                  <h2 className="text-base font-semibold">
                    {isCreating ? "Create a skill" : (selectedName ?? "Skill")}
                  </h2>
                  <p className="max-w-2xl text-sm text-muted-foreground">
                    Skills are plain-text instructions shared by agents in this
                    local Buzz workspace. Review them before assigning them to
                    work.
                  </p>
                </div>
                <Button
                  data-testid="agent-skill-save"
                  disabled={!canSave}
                  onClick={save}
                  size="sm"
                  type="button"
                >
                  <Save />
                  {saveMutation.isPending ? "Saving…" : "Save skill"}
                </Button>
                {!isCreating && savedSkill ? (
                  <div className="space-y-2">
                    <Button
                      data-testid="agent-skill-export-pack"
                      disabled={
                        exportMutation.isPending ||
                        isDirty ||
                        savedSkill.validationError !== null
                      }
                      onClick={openExportReview}
                      size="sm"
                      type="button"
                      variant="outline"
                    >
                      <Download />
                      {exportMutation.isPending
                        ? "Exporting…"
                        : "Build skill pack"}
                    </Button>
                    <p className="max-w-md text-xs text-muted-foreground">
                      Select and review up to 32 saved, text-only skills before
                      creating a checksummed pack.
                    </p>
                  </div>
                ) : null}
              </div>

              <div className="mb-3 block max-w-sm space-y-1.5">
                <label
                  className="text-sm font-medium"
                  htmlFor="agent-skill-name"
                >
                  Skill name
                </label>
                <Input
                  autoComplete="off"
                  disabled={!isCreating}
                  id="agent-skill-name"
                  onChange={(event) => {
                    const nextName = event.target.value;
                    setNameDraft(nextName);
                    if (isCreating) {
                      setContentDraft((current) => {
                        const lines = current.split("\n");
                        const nameLine = lines.findIndex((line) =>
                          line.startsWith("name:"),
                        );
                        if (nameLine === -1) return current;
                        const previousName = lines[nameLine]
                          .slice("name:".length)
                          .trim();
                        lines[nameLine] = `name: ${nextName}`;
                        const titleLine = lines.indexOf(`# ${previousName}`);
                        if (titleLine !== -1)
                          lines[titleLine] = `# ${nextName}`;
                        return lines.join("\n");
                      });
                    }
                  }}
                  value={nameDraft}
                />
                <span className="block text-xs text-muted-foreground">
                  1–64 lowercase letters or numbers, including Unicode, and
                  single hyphens; match the frontmatter name.
                </span>
              </div>

              {selectedQuery.isLoading && !isCreating ? (
                <div className="flex-1 rounded-md border border-border bg-muted/20 p-4 text-sm text-muted-foreground">
                  Loading skill…
                </div>
              ) : selectedQuery.error && !isCreating ? (
                <p
                  className="flex-1 rounded-md border border-destructive/30 bg-destructive/5 p-4 text-sm text-destructive"
                  role="alert"
                >
                  {selectedQuery.error instanceof Error
                    ? selectedQuery.error.message
                    : "This skill could not be read."}
                </p>
              ) : (
                <div className="flex min-h-64 flex-1 flex-col gap-1.5">
                  <label
                    className="text-sm font-medium"
                    htmlFor="agent-skill-content"
                  >
                    SKILL.md
                  </label>
                  <Textarea
                    className="min-h-64 flex-1 resize-y font-mono text-xs leading-relaxed"
                    data-testid="agent-skill-content"
                    disabled={exportMutation.isPending}
                    id="agent-skill-content"
                    onChange={(event) => setContentDraft(event.target.value)}
                    spellCheck={false}
                    value={contentDraft}
                  />
                </div>
              )}

              {savedSkill?.validationError ? (
                <p className="mt-3 rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-amber-700 dark:text-amber-400">
                  This file needs review: {savedSkill.validationError}
                </p>
              ) : null}
              {savedSkill && !savedSkill.validationError ? (
                <div className="mt-4 space-y-2">
                  <h3 className="text-sm font-medium">Runtime compatibility</h3>
                  {savedSkill.runtimeCompatibility.length > 0 ? (
                    <ul className="divide-y divide-border rounded-md border border-border">
                      {savedSkill.runtimeCompatibility.map((runtime) => {
                        const statusLabel = {
                          linked: "Available",
                          missing: "Link missing",
                          conflict: "Path conflict",
                          blocked: "Path blocked",
                        }[runtime.status];
                        const statusClass =
                          runtime.status === "linked"
                            ? "text-emerald-700 dark:text-emerald-400"
                            : runtime.status === "missing"
                              ? "text-muted-foreground"
                              : "text-destructive";
                        return (
                          <li
                            className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 px-3 py-2 text-xs"
                            key={runtime.runtimeId}
                          >
                            <span className="min-w-0">
                              <span className="font-medium">
                                {runtime.runtimeLabel}
                              </span>
                              <code className="ml-2 text-muted-foreground">
                                {runtime.skillDirectory}
                              </code>
                            </span>
                            <span className={statusClass}>{statusLabel}</span>
                          </li>
                        );
                      })}
                    </ul>
                  ) : (
                    <p className="text-xs text-muted-foreground">
                      No harness-specific skill paths are declared in Buzz’s
                      runtime catalog.
                    </p>
                  )}
                  <p className="text-xs text-muted-foreground">
                    Buzz reports only paths declared by its built-in runtime
                    catalog.
                  </p>
                </div>
              ) : null}
              {overlapPreviewReady ? (
                <section
                  aria-label="Possible skill overlaps"
                  className="mt-4 space-y-2 rounded-md border border-border bg-muted/20 p-3"
                  data-testid="agent-skill-overlap-preview"
                >
                  <div>
                    <h3 className="text-sm font-medium">Possible overlap</h3>
                    <p className="text-xs text-muted-foreground">
                      Local exact-text signals only. Terms use Unicode NFKC and
                      lowercase; words under three characters and a short
                      built-in stop-word list are ignored. Buzz compares this
                      skill’s name, description, and headings with installed
                      skills’ names and available descriptions. Two shared terms
                      or a matching SHA-256 content hash lists a possible
                      overlap.
                    </p>
                  </div>
                  {possibleOverlaps.length > 0 ? (
                    <ul className="divide-y divide-border rounded-md border border-border">
                      {possibleOverlaps.map((candidate) => (
                        <li
                          className="space-y-1 px-3 py-2 text-xs"
                          key={candidate.name}
                        >
                          <p className="font-medium">{candidate.name}</p>
                          {candidate.sameContentHash ? (
                            <p className="text-muted-foreground">
                              Matching SHA-256 content hash (identical-content
                              evidence).
                            </p>
                          ) : null}
                          {candidate.sharedTerms.length > 0 ? (
                            <p className="text-muted-foreground">
                              Shared normalized terms:{" "}
                              {candidate.sharedTerms.join(", ")}
                            </p>
                          ) : null}
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p className="text-xs text-muted-foreground">
                      No possible overlaps found using these exact signals.
                    </p>
                  )}
                  <p className="text-xs text-muted-foreground">
                    This advisory is not a quality grade and may miss overlap
                    written with different words. It does not change skills.
                  </p>
                </section>
              ) : null}
              {saveError ? (
                <p className="mt-3 text-sm text-destructive" role="alert">
                  {saveError}
                </p>
              ) : null}
              {exportNotice ? (
                <p className="mt-3 text-sm text-muted-foreground" role="status">
                  {exportNotice}
                </p>
              ) : null}
            </>
          ) : (
            <div className="grid flex-1 place-items-center rounded-md border border-dashed border-border p-8 text-center">
              <div className="max-w-sm space-y-2">
                <BookOpen className="mx-auto h-6 w-6 text-muted-foreground" />
                <p className="font-medium">Choose a skill to review</p>
                <p className="text-sm text-muted-foreground">
                  Skills stay on this device until you explicitly share or
                  export them.
                </p>
              </div>
            </div>
          )}
        </div>
      </section>
      <input
        accept=".agent.zip,.zip,application/zip"
        className="hidden"
        data-testid="agent-skill-pack-input"
        ref={packInputRef}
        type="file"
        onChange={(event) => {
          const file = event.target.files?.[0];
          if (!file) return;
          setPackReview(null);
          setPackImportError(null);
          setPackImportNotice(null);
          if (file.size > 1024 * 1024) {
            setPackImportError("Agent skill packs must be smaller than 1 MiB.");
            event.target.value = "";
            return;
          }
          const reader = new FileReader();
          reader.onload = () => {
            if (!(reader.result instanceof ArrayBuffer)) {
              setPackImportError("The selected pack could not be read.");
              return;
            }
            const bytes = Array.from(new Uint8Array(reader.result));
            previewPackMutation.mutate(bytes);
          };
          reader.onerror = () => {
            setPackImportError("The selected pack could not be read.");
          };
          reader.readAsArrayBuffer(file);
          event.target.value = "";
        }}
      />
      {previewPackMutation.isPending ? (
        <p className="text-sm text-muted-foreground" role="status">
          Inspecting skill pack…
        </p>
      ) : null}
      {packImportError ? (
        <p className="text-sm text-destructive" role="alert">
          {packImportError}
        </p>
      ) : null}
      {packImportNotice ? (
        <p className="text-sm text-muted-foreground" role="status">
          {packImportNotice}
        </p>
      ) : null}
      <AlertDialog
        onOpenChange={(open) => {
          if (!open) setPendingNavigation(null);
        }}
        open={pendingNavigation !== null}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Discard unsaved changes?</AlertDialogTitle>
            <AlertDialogDescription>
              Your edits to this skill have not been saved. You can keep editing
              or discard them and continue.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel asChild>
              <Button type="button" variant="outline">
                Keep editing
              </Button>
            </AlertDialogCancel>
            <AlertDialogAction asChild>
              <Button
                onClick={() => {
                  if (pendingNavigation) applyNavigation(pendingNavigation);
                  setPendingNavigation(null);
                }}
                type="button"
                variant="destructive"
              >
                Discard changes
              </Button>
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <Dialog
        onOpenChange={(open) => {
          if (!open && !exportMutation.isPending) setExportReviewOpen(false);
        }}
        open={exportReviewOpen}
      >
        <DialogContent
          className="max-h-[90vh] overflow-y-auto sm:max-w-3xl"
          data-testid="agent-skill-pack-export-dialog"
        >
          <DialogHeader>
            <DialogTitle>Build a skill pack</DialogTitle>
            <DialogDescription>
              Choose up to 32 valid skills. Review each exact SKILL.md and
              checksum before exporting; Buzz includes text only and runs
              nothing from the pack.
            </DialogDescription>
          </DialogHeader>
          <div className="grid min-h-0 gap-4 sm:grid-cols-[minmax(14rem,0.8fr)_minmax(0,1.2fr)]">
            <section aria-label="Choose skills" className="min-w-0 space-y-2">
              <div className="flex items-center justify-between">
                <h3 className="text-sm font-medium">Workspace skills</h3>
                <span className="text-xs text-muted-foreground">
                  {exportReview.selectedNames.length}/32 selected
                </span>
              </div>
              <ul className="max-h-64 space-y-1 overflow-y-auto rounded-md border border-border p-2">
                {skills.length === 0 ? (
                  <li className="p-2 text-sm text-muted-foreground">
                    No saved skills are available.
                  </li>
                ) : (
                  skills.map((skill) => {
                    const checked = exportReview.selectedNames.includes(
                      skill.name,
                    );
                    const loading = exportReview.loadingNames.includes(
                      skill.name,
                    );
                    const invalid = skill.validationError !== null;
                    return (
                      <li key={skill.name}>
                        <div className="flex items-start gap-2 rounded-md p-2 hover:bg-accent">
                          <Checkbox
                            aria-label={`Include ${skill.name} in skill pack`}
                            checked={checked}
                            disabled={
                              invalid ||
                              loading ||
                              exportMutation.isPending ||
                              (!checked &&
                                exportReview.selectedNames.length >= 32)
                            }
                            onCheckedChange={(value) =>
                              void toggleExportSkill(skill.name, value === true)
                            }
                          />
                          <span
                            className="min-w-0 flex-1"
                            id={`agent-skill-pack-option-${skill.name}`}
                          >
                            <span className="flex items-center gap-2 text-sm font-medium">
                              <span className="truncate">{skill.name}</span>
                              {loading ? (
                                <span className="text-xs text-muted-foreground">
                                  Loading…
                                </span>
                              ) : null}
                            </span>
                            <span className="mt-0.5 block text-xs text-muted-foreground">
                              {skill.validationError ?? skill.description}
                            </span>
                          </span>
                        </div>
                      </li>
                    );
                  })
                )}
              </ul>
              <p className="text-xs text-muted-foreground">
                A pack contains a manifest and one SKILL.md per selected skill.
                Supporting files are not included.
              </p>
            </section>
            <section
              aria-label="Review selected skill"
              className="min-w-0 space-y-2"
            >
              <h3 className="text-sm font-medium">Exact content review</h3>
              {exportReview.selectedNames.length > 0 ? (
                <fieldset className="flex flex-wrap gap-1">
                  <legend className="sr-only">
                    Selected skills to inspect
                  </legend>
                  {exportReview.selectedNames.map((name) => (
                    <button
                      aria-pressed={exportReview.activeName === name}
                      className={`rounded-md px-2 py-1 text-xs ${exportReview.activeName === name ? "bg-accent font-medium" : "text-muted-foreground hover:bg-accent"}`}
                      data-testid={`agent-skill-pack-export-review-${name}`}
                      key={name}
                      onClick={() =>
                        setExportReview((current) => ({
                          ...current,
                          activeName: name,
                          reviewedNames: current.reviewedNames.includes(name)
                            ? current.reviewedNames
                            : [...current.reviewedNames, name],
                        }))
                      }
                      type="button"
                    >
                      {name}
                    </button>
                  ))}
                </fieldset>
              ) : null}
              {activeExportSkill ? (
                <>
                  <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 rounded-md border border-border p-3 text-xs">
                    <dt className="font-medium">Description</dt>
                    <dd>{activeExportSkill.description}</dd>
                    <dt className="font-medium">SHA-256</dt>
                    <dd className="break-all font-mono">
                      {activeExportSkill.contentHash}
                    </dd>
                  </dl>
                  <pre className="max-h-64 overflow-auto rounded-md border border-border bg-muted/30 p-3 font-mono text-xs leading-relaxed whitespace-pre-wrap">
                    {activeExportSkill.content}
                  </pre>
                </>
              ) : (
                <p className="rounded-md border border-dashed border-border p-4 text-sm text-muted-foreground">
                  Select a valid skill to inspect its exact saved instructions.
                </p>
              )}
            </section>
          </div>
          {exportReview.error ? (
            <p className="text-sm text-destructive" role="alert">
              {exportReview.error}
            </p>
          ) : null}
          <DialogFooter>
            <DialogClose asChild>
              <Button
                disabled={exportMutation.isPending}
                type="button"
                variant="outline"
              >
                Cancel
              </Button>
            </DialogClose>
            <Button
              data-testid="agent-skill-pack-export-confirm"
              disabled={!reviewedSelectionReady || exportMutation.isPending}
              onClick={exportReviewedPack}
              type="button"
            >
              <Download />
              {exportMutation.isPending ? "Exporting…" : "Export reviewed pack"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={packReview !== null}
        onOpenChange={(open) => {
          if (!open && !installPackMutation.isPending) setPackReview(null);
        }}
      >
        <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>
              Review {packReview?.preview.skills.length ?? 0}-skill pack
            </DialogTitle>
            <DialogDescription>
              Open every skill to inspect its exact text and checksum before
              installing. Provenance is self-reported; Buzz does not execute
              pack contents.
            </DialogDescription>
          </DialogHeader>
          {packReview ? (
            <div className="space-y-4 text-sm">
              <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 rounded-md border border-border p-3 text-xs">
                <dt className="font-medium">Format</dt>
                <dd>Version {packReview.preview.version}</dd>
                <dt className="font-medium">Self-reported source</dt>
                <dd className="break-words">
                  {packReview.preview.exportedFrom} (not verified)
                </dd>
              </dl>
              <div className="grid gap-3 sm:grid-cols-[minmax(12rem,0.7fr)_minmax(0,1.3fr)]">
                <ul
                  aria-label="Skills in pack"
                  className="max-h-64 space-y-1 overflow-y-auto rounded-md border border-border p-2"
                >
                  {packReview.preview.skills.map((skill) => (
                    <li key={skill.name}>
                      <button
                        aria-pressed={activePackSkillName === skill.name}
                        className={`w-full rounded-md px-2 py-2 text-left hover:bg-accent ${activePackSkillName === skill.name ? "bg-accent" : ""}`}
                        onClick={() => {
                          setActivePackSkillName(skill.name);
                          setReviewedPackSkillNames((current) =>
                            current.includes(skill.name)
                              ? current
                              : [...current, skill.name],
                          );
                        }}
                        type="button"
                      >
                        <span className="block truncate text-sm font-medium">
                          {skill.name}
                        </span>
                        <span className="mt-0.5 block line-clamp-2 text-xs text-muted-foreground">
                          {skill.description}
                        </span>
                        {skill.alreadyInstalled ? (
                          <span className="mt-1 block text-xs text-amber-700 dark:text-amber-400">
                            Name conflict
                          </span>
                        ) : null}
                      </button>
                    </li>
                  ))}
                </ul>
                {activeImportedSkill ? (
                  <section
                    aria-label={`Review ${activeImportedSkill.name}`}
                    className="min-w-0 space-y-3"
                  >
                    <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 rounded-md border border-border p-3 text-xs">
                      <dt className="font-medium">Description</dt>
                      <dd>{activeImportedSkill.description}</dd>
                      {activeImportedSkill.license ? (
                        <>
                          <dt className="font-medium">Declared license</dt>
                          <dd>{activeImportedSkill.license}</dd>
                        </>
                      ) : null}
                      <dt className="font-medium">SHA-256</dt>
                      <dd className="break-all font-mono">
                        {activeImportedSkill.contentHash}
                      </dd>
                    </dl>
                    <pre className="max-h-72 overflow-auto rounded-md border border-border bg-muted/30 p-3 font-mono text-xs leading-relaxed whitespace-pre-wrap">
                      {activeImportedSkill.content}
                    </pre>
                  </section>
                ) : null}
              </div>
              <p className="text-xs text-muted-foreground" role="status">
                Reviewed {reviewedPackSkillNames.length} of{" "}
                {packReview.preview.skills.length} skills.
              </p>
              {packReview.preview.skills.some(
                (skill) => skill.alreadyInstalled,
              ) ? (
                <p className="text-amber-700 dark:text-amber-400" role="alert">
                  This pack contains a skill name that already exists. The
                  importer will install none of the skills and will not
                  overwrite existing files.
                </p>
              ) : null}
              {packImportError ? (
                <p className="text-destructive" role="alert">
                  {packImportError}
                </p>
              ) : null}
            </div>
          ) : null}
          <DialogFooter>
            <DialogClose asChild>
              <Button
                disabled={installPackMutation.isPending}
                type="button"
                variant="outline"
              >
                Cancel
              </Button>
            </DialogClose>
            <Button
              disabled={
                !packReview ||
                packReview.preview.skills.some(
                  (skill) => !reviewedPackSkillNames.includes(skill.name),
                ) ||
                packReview.preview.skills.some(
                  (skill) => skill.alreadyInstalled,
                ) ||
                installPackMutation.isPending
              }
              onClick={() => {
                if (!packReview) return;
                setPackImportError(null);
                installPackMutation.mutate({
                  fileBytes: packReview.fileBytes,
                  expectedSkills: packReview.preview.skills.map(
                    ({ name, contentHash }) => ({ name, contentHash }),
                  ),
                });
              }}
              type="button"
            >
              {installPackMutation.isPending
                ? "Installing…"
                : `Install ${packReview?.preview.skills.length ?? 0} skills`}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
