import * as React from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FileText, Plus, Save, X } from "lucide-react";

import {
  listAgentPromptProfiles,
  readAgentPromptProfile,
  saveAgentPromptProfile,
  type AgentPromptProfile,
  type AgentPromptProfileTargetKind,
} from "@/shared/api/tauriAgentPromptProfiles";
import { managedAgentsQueryKey } from "../hooks";
import { PersonaDropdownField } from "./PersonaDropdownField";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Textarea } from "@/shared/ui/textarea";

const PROFILES_KEY = ["agent-prompt-profiles"] as const;

type ProfileDraft = {
  id: string;
  name: string;
  targetKind: AgentPromptProfileTargetKind;
  targetId: string;
  modelId: string;
  prompt: string;
};

function toDraft(profile: AgentPromptProfile): ProfileDraft {
  return {
    id: profile.id,
    name: profile.name,
    targetKind: profile.target.kind,
    targetId: profile.target.targetId,
    modelId: profile.target.modelId ?? "",
    prompt: profile.prompt,
  };
}

function targetLabel(kind: AgentPromptProfileTargetKind) {
  switch (kind) {
    case "buzz_agent_api":
      return "Buzz Agent API";
    case "acp_harness":
      return "ACP harness";
    case "cli_harness":
      return "CLI harness";
    case "consumer_app":
      return "Consumer app";
  }
}

function newDraft(): ProfileDraft {
  return {
    id: `new-profile-${Date.now().toString(36)}`,
    name: "New prompt profile",
    targetKind: "buzz_agent_api",
    targetId: "provider-id",
    modelId: "",
    prompt: "",
  };
}

export function AgentPromptProfileLibrary({ onBack }: { onBack: () => void }) {
  const queryClient = useQueryClient();
  const profilesQuery = useQuery({
    queryKey: PROFILES_KEY,
    queryFn: listAgentPromptProfiles,
  });
  const [selectedId, setSelectedId] = React.useState<string | null>(null);
  const [isCreating, setIsCreating] = React.useState(false);
  const [draft, setDraft] = React.useState<ProfileDraft | null>(null);
  const [savedProfile, setSavedProfile] =
    React.useState<AgentPromptProfile | null>(null);
  const [saveError, setSaveError] = React.useState<string | null>(null);

  const selectedQuery = useQuery({
    queryKey: [...PROFILES_KEY, selectedId],
    queryFn: () => readAgentPromptProfile(selectedId ?? ""),
    enabled: selectedId !== null && !isCreating,
  });

  React.useEffect(() => {
    const profile = selectedQuery.data;
    if (!profile || profile.id !== selectedId) return;
    setSavedProfile(profile);
    setDraft(toDraft(profile));
    setSaveError(null);
  }, [selectedId, selectedQuery.data]);

  React.useEffect(() => {
    if (selectedId !== null || isCreating) return;
    const firstProfile = profilesQuery.data?.[0];
    if (firstProfile) setSelectedId(firstProfile.id);
  }, [isCreating, profilesQuery.data, selectedId]);

  const isDirty = isCreating
    ? draft !== null
    : draft !== null && savedProfile !== null
      ? JSON.stringify(draft) !== JSON.stringify(toDraft(savedProfile))
      : false;

  const saveMutation = useMutation({
    mutationFn: saveAgentPromptProfile,
    onSuccess: async (profile) => {
      setSelectedId(profile.id);
      setIsCreating(false);
      setSavedProfile(profile);
      setDraft(toDraft(profile));
      setSaveError(null);
      queryClient.setQueryData([...PROFILES_KEY, profile.id], profile);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: PROFILES_KEY }),
        queryClient.invalidateQueries({ queryKey: managedAgentsQueryKey }),
      ]);
    },
    onError: (error) => {
      setSaveError(error instanceof Error ? error.message : String(error));
      void queryClient.invalidateQueries({ queryKey: PROFILES_KEY });
    },
  });

  function startNew() {
    setSelectedId(null);
    setSavedProfile(null);
    setDraft(newDraft());
    setIsCreating(true);
    setSaveError(null);
  }

  function cancelEdits() {
    if (isCreating) {
      setIsCreating(false);
      setDraft(null);
      setSelectedId(null);
      return;
    }
    if (savedProfile) setDraft(toDraft(savedProfile));
    setSaveError(null);
  }

  function save() {
    if (!draft || !canSave(draft)) return;
    saveMutation.mutate({
      id: draft.id.trim(),
      name: draft.name.trim(),
      target: {
        kind: draft.targetKind,
        targetId: draft.targetId.trim(),
        modelId:
          draft.targetKind === "buzz_agent_api" && draft.modelId.trim()
            ? draft.modelId.trim()
            : null,
      },
      prompt: draft.prompt,
      expectedVersion: isCreating ? null : (savedProfile?.version ?? null),
    });
  }

  const profiles = profilesQuery.data ?? [];
  const isLoading = profilesQuery.isLoading || selectedQuery.isLoading;

  return (
    <section className="space-y-5" data-testid="agent-prompt-profile-library">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">Prompt profiles</h2>
          <p className="text-sm text-muted-foreground">
            Matching API profiles are appended to local Buzz Agent instructions
            at launch. Exact model matches take precedence over provider-wide
            profiles. DSH ACP profiles match the configured launch model, then
            fall back to a harness-wide profile. A model changed inside a live
            DSH session takes effect for its prompt profile after restart.
          </p>
        </div>
        <Button onClick={onBack} size="sm" variant="outline">
          <X />
          Back to agents
        </Button>
      </div>

      <div className="grid gap-4 lg:grid-cols-[15rem_minmax(0,1fr)]">
        <aside className="space-y-2 rounded-xl border border-border/70 p-3">
          <Button
            className="w-full justify-start"
            disabled={isDirty || saveMutation.isPending}
            onClick={startNew}
            size="sm"
            variant="outline"
          >
            <Plus />
            New profile
          </Button>
          {profilesQuery.isError ? (
            <p className="text-sm text-destructive">
              {profilesQuery.error instanceof Error
                ? profilesQuery.error.message
                : String(profilesQuery.error)}
            </p>
          ) : null}
          {profiles.length === 0 && !profilesQuery.isLoading ? (
            <p className="px-2 py-4 text-sm text-muted-foreground">
              No prompt profiles saved.
            </p>
          ) : null}
          <div className="max-h-[60vh] space-y-1 overflow-y-auto">
            {profiles.map((profile) => (
              <button
                className={`w-full rounded-lg px-3 py-2 text-left transition-colors ${
                  profile.id === selectedId
                    ? "bg-primary/10 text-foreground"
                    : "text-muted-foreground hover:bg-muted/60 hover:text-foreground"
                }`}
                disabled={isDirty || saveMutation.isPending}
                key={profile.id}
                onClick={() => setSelectedId(profile.id)}
                type="button"
              >
                <span className="block truncate text-sm font-medium">
                  {profile.name}
                </span>
                <span className="mt-0.5 block truncate text-xs">
                  {targetLabel(profile.target.kind)} · v{profile.version}
                </span>
              </button>
            ))}
          </div>
        </aside>

        <div className="min-w-0 rounded-xl border border-border/70 p-4 sm:p-5">
          {isLoading && !draft ? (
            <p className="text-sm text-muted-foreground">
              Loading prompt profile…
            </p>
          ) : draft ? (
            <div className="space-y-4">
              <div className="grid gap-4 sm:grid-cols-2">
                <label
                  className="space-y-1.5 text-sm font-medium"
                  htmlFor="prompt-profile-name"
                >
                  Profile name
                  <Input
                    id="prompt-profile-name"
                    onChange={(event) =>
                      updateDraft(setDraft, "name", event.target.value)
                    }
                    value={draft.name}
                  />
                </label>
                <label
                  className="space-y-1.5 text-sm font-medium"
                  htmlFor="prompt-profile-id"
                >
                  Stable ID
                  <Input
                    disabled={!isCreating}
                    id="prompt-profile-id"
                    onChange={(event) =>
                      updateDraft(setDraft, "id", event.target.value)
                    }
                    value={draft.id}
                  />
                </label>
              </div>
              {draft.targetKind === "acp_harness" &&
              draft.targetId.trim() === "dsh" ? (
                <div className="rounded-md border border-border/70 p-3 text-sm text-muted-foreground">
                  Buzz composes this profile after the agent’s saved
                  instructions and writes a versioned patch inside the Buzz
                  workspace. For this Buzz-launched DSH process, the patch
                  replaces the effective <code>system-prompt</code>
                  row config; DSH’s saved profile files remain untouched. It
                  sets
                  <code> includeHarnessIdentity</code>,{" "}
                  <code>includeRuntimeContext</code>, and{" "}
                  <code>personaPrefix</code>. The prior{" "}
                  <code>personaSuffix</code>,<code>toolOrder</code>, and unknown
                  custom fields within that row are not preserved or verified.
                  Other rows are left unchanged. DSH’s generic ACP systemPrompt
                  field is not used.
                </div>
              ) : null}

              <div className="space-y-1.5 text-sm font-medium">
                <label htmlFor="prompt-profile-target-kind">Target type</label>
                <PersonaDropdownField
                  disabled={saveMutation.isPending}
                  id="prompt-profile-target-kind"
                  onValueChange={(value) =>
                    setDraft((current) =>
                      current
                        ? {
                            ...current,
                            targetKind: value as AgentPromptProfileTargetKind,
                            modelId: "",
                          }
                        : current,
                    )
                  }
                  options={[
                    { label: "Buzz Agent API", value: "buzz_agent_api" },
                    { label: "ACP harness", value: "acp_harness" },
                    { label: "CLI harness", value: "cli_harness" },
                    { label: "Consumer app", value: "consumer_app" },
                  ]}
                  placeholder="Choose a target type"
                  value={draft.targetKind}
                />
              </div>

              <div className="grid gap-4 sm:grid-cols-2">
                <label
                  className="space-y-1.5 text-sm font-medium"
                  htmlFor="prompt-profile-target-id"
                >
                  Provider, harness, or app ID
                  <Input
                    disabled={saveMutation.isPending}
                    id="prompt-profile-target-id"
                    onChange={(event) =>
                      updateDraft(setDraft, "targetId", event.target.value)
                    }
                    placeholder={targetPlaceholder(draft.targetKind)}
                    value={draft.targetId}
                  />
                </label>
                {draft.targetKind === "buzz_agent_api" ||
                (draft.targetKind === "acp_harness" &&
                  draft.targetId.trim() === "dsh") ? (
                  <label
                    className="space-y-1.5 text-sm font-medium"
                    htmlFor="prompt-profile-model-id"
                  >
                    Exact model ID (optional)
                    <Input
                      disabled={saveMutation.isPending}
                      id="prompt-profile-model-id"
                      onChange={(event) =>
                        updateDraft(setDraft, "modelId", event.target.value)
                      }
                      placeholder={
                        draft.targetKind === "buzz_agent_api"
                          ? "Leave blank for provider-wide"
                          : "Leave blank for all DSH models"
                      }
                      value={draft.modelId}
                    />
                  </label>
                ) : null}
              </div>

              <label
                className="block space-y-1.5 text-sm font-medium"
                htmlFor="prompt-profile-prompt"
              >
                Prompt text
                <Textarea
                  className="min-h-64 resize-y font-mono text-xs leading-relaxed"
                  disabled={saveMutation.isPending}
                  id="prompt-profile-prompt"
                  onChange={(event) =>
                    updateDraft(setDraft, "prompt", event.target.value)
                  }
                  placeholder="Write the exact target-specific prompt to use."
                  value={draft.prompt}
                />
              </label>

              <details className="rounded-lg bg-muted/35 px-3 py-2 text-sm">
                <summary className="cursor-pointer font-medium">
                  Preview literal prompt
                </summary>
                <pre className="mt-3 max-h-64 overflow-auto whitespace-pre-wrap font-sans text-xs leading-relaxed text-muted-foreground">
                  {draft.prompt || "Prompt text is empty."}
                </pre>
              </details>

              {savedProfile ? (
                <>
                  <p className="text-xs text-muted-foreground">
                    Saved as version {savedProfile.version} · SHA-256{" "}
                    <code>{savedProfile.promptHash}</code>
                  </p>
                  {savedProfile.target.kind === "acp_harness" &&
                  savedProfile.target.targetId === "dsh" ? (
                    <p className="text-xs text-muted-foreground">
                      Buzz overlay path:{" "}
                      <code>
                        .agents/dsh-prompt-overlays/{savedProfile.id}-v
                        {savedProfile.version}.patch.yml
                      </code>
                    </p>
                  ) : null}
                </>
              ) : null}
              {saveError ? (
                <p className="text-sm text-destructive">{saveError}</p>
              ) : null}
              <div className="flex flex-wrap justify-end gap-2">
                {isDirty ? (
                  <Button
                    disabled={saveMutation.isPending}
                    onClick={cancelEdits}
                    size="sm"
                    variant="ghost"
                  >
                    Cancel edits
                  </Button>
                ) : null}
                <Button
                  disabled={
                    !canSave(draft) || !isDirty || saveMutation.isPending
                  }
                  onClick={save}
                  size="sm"
                >
                  <Save />
                  {saveMutation.isPending ? "Saving…" : "Save profile"}
                </Button>
              </div>
            </div>
          ) : (
            <div className="flex min-h-64 flex-col items-center justify-center gap-3 text-center text-sm text-muted-foreground">
              <FileText className="size-8" />
              {selectedQuery.isError ? (
                <>
                  <p className="text-destructive">
                    {selectedQuery.error instanceof Error
                      ? selectedQuery.error.message
                      : String(selectedQuery.error)}
                  </p>
                  <Button
                    onClick={() => void selectedQuery.refetch()}
                    size="sm"
                    variant="outline"
                  >
                    Retry loading profile
                  </Button>
                </>
              ) : (
                <p>Select a profile or create one.</p>
              )}
            </div>
          )}
        </div>
      </div>
    </section>
  );
}

function updateDraft<K extends keyof ProfileDraft>(
  setDraft: React.Dispatch<React.SetStateAction<ProfileDraft | null>>,
  key: K,
  value: ProfileDraft[K],
) {
  setDraft((current) => (current ? { ...current, [key]: value } : current));
}

function canSave(draft: ProfileDraft) {
  return (
    /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(draft.id) &&
    draft.id.length <= 64 &&
    draft.name.trim().length > 0 &&
    draft.targetId.trim().length > 0 &&
    draft.prompt.trim().length > 0 &&
    (draft.targetKind === "buzz_agent_api" ||
      (draft.targetKind === "acp_harness" && draft.targetId.trim() === "dsh") ||
      draft.modelId.trim().length === 0)
  );
}

function targetPlaceholder(kind: AgentPromptProfileTargetKind) {
  switch (kind) {
    case "buzz_agent_api":
      return "provider ID, for example openai";
    case "acp_harness":
      return "harness ID, for example codex";
    case "cli_harness":
      return "harness ID, for example claude-code";
    case "consumer_app":
      return "app ID, for example chatgpt-app";
  }
}
