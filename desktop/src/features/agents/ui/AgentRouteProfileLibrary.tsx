import * as React from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Check,
  FileText,
  Plus,
  RefreshCw,
  Save,
  Trash2,
  X,
} from "lucide-react";

import {
  listAgentRouteProfiles,
  listAgentRouteThroughputSummaries,
  readAgentRouteProfile,
  saveAgentRouteProfile,
  testAgentRouteCandidate,
  type AgentRouteCandidateTestReceipt,
  type AgentRouteProfile,
  type AgentRouteProfileCandidate,
  type AgentRouteThroughputGroupSummary,
} from "@/shared/api/tauriAgentRouteProfiles";
import { managedAgentsQueryKey } from "../hooks";
import { AgentTaskFitEvidenceReview } from "./AgentTaskFitEvidenceReview";
import { PERSONA_LLM_PROVIDER_OPTIONS } from "./agentConfigOptions";
import { PersonaDropdownField } from "./PersonaDropdownField";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Textarea } from "@/shared/ui/textarea";
import { Checkbox } from "@/shared/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";

const PROFILES_KEY = ["agent-route-profiles"] as const;
const ROUTE_PROVIDERS = PERSONA_LLM_PROVIDER_OPTIONS.filter((option) =>
  [
    "anthropic",
    "openai",
    "openai-compat",
    "deepseek",
    "openrouter",
    "databricks",
    "databricks_v2",
  ].includes(option.id),
);

type RouteProfileDraft = {
  id: string;
  name: string;
  dataPolicy: "local-only" | "allow-hosted";
  strictContextFit: boolean;
  maxCostUsd: string;
  minEffectiveOutputTokensPerSecondMilli: number | null;
  preferFastestMeasured: boolean;
  allowPreferenceOrderWarmup: boolean;
  taskFitEnabled: boolean;
  taskFitClass: string;
  taskFitMinimumDistinctTasks: string;
  taskFitMinimumWilsonLowerBound95: string;
  taskFitMaximumAgeDays: string;
  taskFitRequireObservedModelIdentity: boolean;
  candidates: RouteCandidateDraft[];
  preferenceText: string;
};

type RouteCandidateDraft = Omit<
  AgentRouteProfileCandidate,
  | "input_cost_microusd_per_million_tokens"
  | "output_cost_microusd_per_million_tokens"
> & {
  inputCostUsdPerMillion: string;
  outputCostUsdPerMillion: string;
};

type RoutePreset = {
  id: string;
  label: string;
  name: string;
  description: string;
  dataPolicy: "local-only" | "allow-hosted";
  candidateId: string;
  provider: string;
  model: string;
  dataLocation: "local" | "hosted";
  contextCapacityTokens?: number;
};

const ROUTE_PRESETS: RoutePreset[] = [
  {
    id: "fast",
    label: "Fast · DeepSeek Flash",
    name: "Fast hosted · DeepSeek Flash",
    description:
      "DeepSeek’s current model catalog identifies deepseek-flash as V4.1 Flash. Hosted prompts leave this device.",
    dataPolicy: "allow-hosted",
    candidateId: "deepseek-flash",
    provider: "deepseek",
    model: "deepseek-flash",
    dataLocation: "hosted",
    contextCapacityTokens: 1_000_000,
  },
  {
    id: "balanced",
    label: "Balanced · GPT-6 Sol",
    name: "Balanced · GPT-6 Sol",
    description:
      "OpenAI positions Sol for complex coding and agentic work. Hosted prompts leave this device.",
    dataPolicy: "allow-hosted",
    candidateId: "gpt-6-sol",
    provider: "openai",
    model: "gpt-6-sol",
    dataLocation: "hosted",
    contextCapacityTokens: 1_050_000,
  },
  {
    id: "deep",
    label: "Deep · GPT-6 Astra",
    name: "Deep reasoning · GPT-6 Astra",
    description:
      "OpenAI positions Astra for its hardest end-to-end work. Hosted prompts leave this device.",
    dataPolicy: "allow-hosted",
    candidateId: "gpt-6-astra",
    provider: "openai",
    model: "gpt-6-astra",
    dataLocation: "hosted",
    contextCapacityTokens: 1_050_000,
  },
  {
    id: "local",
    label: "Local · OpenAI-compatible",
    name: "Local · OpenAI-compatible",
    description:
      "Requires a running loopback-compatible server. Enter the exact model ID reported by its /v1/models endpoint before saving.",
    dataPolicy: "local-only",
    candidateId: "local",
    provider: "openai",
    model: "",
    dataLocation: "local",
  },
];

function candidateDraft(
  candidate: AgentRouteProfileCandidate,
): RouteCandidateDraft {
  return {
    id: candidate.id,
    provider: candidate.provider,
    model: candidate.model,
    data_location: candidate.data_location,
    context_capacity_tokens: candidate.context_capacity_tokens,
    prompt_profile: candidate.prompt_profile,
    prompt_addendum: candidate.prompt_addendum,
    inputCostUsdPerMillion:
      candidate.input_cost_microusd_per_million_tokens === undefined
        ? ""
        : formatMicrousd(candidate.input_cost_microusd_per_million_tokens),
    outputCostUsdPerMillion:
      candidate.output_cost_microusd_per_million_tokens === undefined
        ? ""
        : formatMicrousd(candidate.output_cost_microusd_per_million_tokens),
  };
}

function candidateDocument(candidate: RouteCandidateDraft) {
  const { inputCostUsdPerMillion, outputCostUsdPerMillion, ...document } =
    candidate;
  const inputRate = inputCostUsdPerMillion
    ? (parseUsdMicrousd(inputCostUsdPerMillion) ?? undefined)
    : undefined;
  const outputRate = outputCostUsdPerMillion
    ? (parseUsdMicrousd(outputCostUsdPerMillion) ?? undefined)
    : undefined;
  return {
    ...document,
    ...(inputRate === undefined
      ? {}
      : { input_cost_microusd_per_million_tokens: inputRate }),
    ...(outputRate === undefined
      ? {}
      : { output_cost_microusd_per_million_tokens: outputRate }),
  };
}

function toDraft(profile: AgentRouteProfile): RouteProfileDraft {
  return {
    id: profile.id,
    name: profile.name,
    dataPolicy: profile.document.data_policy,
    strictContextFit: profile.document.strict_context_fit ?? false,
    maxCostUsd:
      profile.document.max_turn_cost_microusd === undefined ||
      profile.document.max_turn_cost_microusd === null
        ? ""
        : formatMicrousd(profile.document.max_turn_cost_microusd),
    minEffectiveOutputTokensPerSecondMilli:
      profile.document.min_effective_output_tokens_per_second_milli ?? null,
    preferFastestMeasured: profile.document.prefer_fastest_measured ?? false,
    allowPreferenceOrderWarmup:
      profile.document.allow_preference_order_warmup ?? false,
    taskFitEnabled: profile.document.task_fit_policy !== undefined,
    taskFitClass: profile.document.task_fit_policy?.taskClass ?? "",
    taskFitMinimumDistinctTasks: String(
      profile.document.task_fit_policy?.minimumDistinctTasks ?? 30,
    ),
    taskFitMinimumWilsonLowerBound95: String(
      profile.document.task_fit_policy?.minimumWilsonLowerBound95 ?? 0.8,
    ),
    taskFitMaximumAgeDays: String(
      (profile.document.task_fit_policy?.maximumAgeSeconds ?? 30 * 86400) /
        86400,
    ),
    taskFitRequireObservedModelIdentity:
      profile.document.task_fit_policy?.requireObservedModelIdentity ?? true,
    candidates: profile.document.candidates.map(candidateDraft),
    preferenceText: profile.document.preference_order.join("\n"),
  };
}

function newDraft(): RouteProfileDraft {
  return {
    id: `new-route-${Date.now().toString(36)}`,
    name: "New route profile",
    dataPolicy: "local-only",
    strictContextFit: false,
    maxCostUsd: "",
    minEffectiveOutputTokensPerSecondMilli: null,
    preferFastestMeasured: false,
    allowPreferenceOrderWarmup: false,
    taskFitEnabled: false,
    taskFitClass: "",
    taskFitMinimumDistinctTasks: "30",
    taskFitMinimumWilsonLowerBound95: "0.8",
    taskFitMaximumAgeDays: "30",
    taskFitRequireObservedModelIdentity: true,
    candidates: [
      {
        id: "local",
        provider: "openai",
        model: "",
        data_location: "local",
        prompt_addendum: "",
        inputCostUsdPerMillion: "",
        outputCostUsdPerMillion: "",
      },
    ],
    preferenceText: "local",
  };
}

function draftFromPreset(preset: RoutePreset): RouteProfileDraft {
  return {
    id: `${preset.id}-${Date.now().toString(36)}`,
    name: preset.name,
    dataPolicy: preset.dataPolicy,
    strictContextFit: preset.contextCapacityTokens !== undefined,
    maxCostUsd: "",
    minEffectiveOutputTokensPerSecondMilli: null,
    preferFastestMeasured: false,
    allowPreferenceOrderWarmup: false,
    taskFitEnabled: false,
    taskFitClass: "",
    taskFitMinimumDistinctTasks: "30",
    taskFitMinimumWilsonLowerBound95: "0.8",
    taskFitMaximumAgeDays: "30",
    taskFitRequireObservedModelIdentity: true,
    candidates: [
      {
        id: preset.candidateId,
        provider: preset.provider,
        model: preset.model,
        data_location: preset.dataLocation,
        ...(preset.contextCapacityTokens === undefined
          ? {}
          : { context_capacity_tokens: preset.contextCapacityTokens }),
        prompt_addendum: "",
        inputCostUsdPerMillion: "",
        outputCostUsdPerMillion: "",
      },
    ],
    preferenceText: preset.candidateId,
  };
}

export function AgentRouteProfileLibrary({ onBack }: { onBack: () => void }) {
  const queryClient = useQueryClient();
  const [showTaskFitEvidence, setShowTaskFitEvidence] = React.useState(false);
  const profilesQuery = useQuery({
    queryKey: PROFILES_KEY,
    queryFn: listAgentRouteProfiles,
  });
  const [selectedId, setSelectedId] = React.useState<string | null>(null);
  const [isCreating, setIsCreating] = React.useState(false);
  const [draft, setDraft] = React.useState<RouteProfileDraft | null>(null);
  const [savedProfile, setSavedProfile] =
    React.useState<AgentRouteProfile | null>(null);
  const [saveError, setSaveError] = React.useState<string | null>(null);
  const [candidateTestReceipt, setCandidateTestReceipt] =
    React.useState<AgentRouteCandidateTestReceipt | null>(null);
  const [candidateTestError, setCandidateTestError] = React.useState<
    string | null
  >(null);
  const [pendingHostedTest, setPendingHostedTest] =
    React.useState<AgentRouteProfileCandidate | null>(null);
  const [hostedPreview, setHostedPreview] =
    React.useState<AgentRouteCandidateTestReceipt | null>(null);

  const selectedQuery = useQuery({
    queryKey: [...PROFILES_KEY, selectedId],
    queryFn: () => readAgentRouteProfile(selectedId ?? ""),
    enabled: selectedId !== null && !isCreating,
  });

  React.useEffect(() => {
    const profile = selectedQuery.data;
    if (!profile || profile.id !== selectedId) return;
    setSavedProfile(profile);
    setDraft(toDraft(profile));
    setSaveError(null);
    setCandidateTestReceipt(null);
    setCandidateTestError(null);
    setPendingHostedTest(null);
    setHostedPreview(null);
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
  const saveBlockersForDraft = draft ? saveBlockers(draft) : [];
  const measurementsProfile =
    !isCreating && !isDirty && savedProfile?.id === selectedId
      ? savedProfile
      : null;
  const throughputQuery = useQuery({
    queryKey: [
      "agent-route-throughput",
      measurementsProfile?.id,
      measurementsProfile?.version,
    ],
    queryFn: () =>
      listAgentRouteThroughputSummaries(
        measurementsProfile?.id ?? "",
        measurementsProfile?.version ?? 0,
      ),
    enabled: measurementsProfile !== null,
    staleTime: 30_000,
  });
  const saveMutation = useMutation({
    mutationFn: saveAgentRouteProfile,
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

  const candidateTestMutation = useMutation({
    mutationFn: testAgentRouteCandidate,
    onSuccess: (receipt) => {
      setCandidateTestError(null);
      if (receipt.status === "confirmation_required") {
        setCandidateTestReceipt(null);
        setPendingHostedTest(
          savedProfile?.document.candidates.find(
            (candidate) => candidate.id === receipt.candidateId,
          ) ?? null,
        );
        setHostedPreview(receipt);
        return;
      }
      setCandidateTestReceipt(receipt);
      setPendingHostedTest(null);
      setHostedPreview(null);
    },
    onError: (error) => {
      setCandidateTestError(
        error instanceof Error ? error.message : String(error),
      );
      setPendingHostedTest(null);
      setHostedPreview(null);
    },
  });

  function requestCandidateTest(
    candidate: AgentRouteProfileCandidate,
    confirmHosted = false,
  ) {
    if (!savedProfile) return;
    setCandidateTestError(null);
    candidateTestMutation.mutate({
      profileId: savedProfile.id,
      candidateId: candidate.id,
      expectedProfileVersion: savedProfile.version,
      expectedProfileDocumentHash: savedProfile.documentHash,
      confirmHosted,
    });
  }

  function startNew() {
    setSelectedId(null);
    setSavedProfile(null);
    setDraft(newDraft());
    setIsCreating(true);
    setSaveError(null);
  }

  function startPreset(preset: RoutePreset) {
    setSelectedId(null);
    setSavedProfile(null);
    setDraft(draftFromPreset(preset));
    setIsCreating(true);
    setSaveError(null);
  }

  function cancelEdits() {
    if (isCreating) {
      setIsCreating(false);
      setDraft(null);
      setSelectedId(null);
    } else if (savedProfile) {
      setDraft(toDraft(savedProfile));
    }
    setSaveError(null);
  }

  function save() {
    if (!draft || saveBlockers(draft).length > 0) return;
    saveMutation.mutate({
      id: draft.id.trim(),
      name: draft.name.trim(),
      document: {
        version: 1,
        data_policy: draft.dataPolicy,
        strict_context_fit: draft.strictContextFit,
        ...(draft.maxCostUsd === ""
          ? {}
          : { max_turn_cost_microusd: parseUsdMicrousd(draft.maxCostUsd) }),
        min_effective_output_tokens_per_second_milli:
          draft.minEffectiveOutputTokensPerSecondMilli,
        prefer_fastest_measured: draft.preferFastestMeasured,
        allow_preference_order_warmup: draft.allowPreferenceOrderWarmup,
        ...(draft.taskFitEnabled
          ? {
              task_fit_policy: {
                taskClass: draft.taskFitClass.trim(),
                taskClassTaxonomyVersion: "operator-defined-v1",
                evaluationPolicyVersion: "task-fit-outcomes-v1",
                minimumDistinctTasks: Number(draft.taskFitMinimumDistinctTasks),
                minimumWilsonLowerBound95: Number(
                  draft.taskFitMinimumWilsonLowerBound95,
                ),
                maximumAgeSeconds: Math.round(
                  Number(draft.taskFitMaximumAgeDays) * 86400,
                ),
                requireObservedModelIdentity:
                  draft.taskFitRequireObservedModelIdentity,
              },
            }
          : {}),
        preference_order: preferences(draft.preferenceText),
        candidates: draft.candidates.map(candidateDocument),
      },
      expectedVersion: isCreating ? null : (savedProfile?.version ?? null),
    });
  }

  const profiles = profilesQuery.data ?? [];
  const isLoading = profilesQuery.isLoading || selectedQuery.isLoading;

  return (
    <section className="space-y-5" data-testid="agent-route-profile-library">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">Routing profiles</h2>
          <p className="max-w-3xl text-sm text-muted-foreground">
            Buzz Agent chooses the first eligible provider for each prompt.
            Profiles are local-only by default. Hosted candidates run only after
            you change the data policy. Each provider uses the agent’s existing
            local credentials and endpoint settings.
          </p>
        </div>
        <Button
          disabled={isDirty || saveMutation.isPending}
          onClick={onBack}
          size="sm"
          variant="outline"
        >
          <X />
          Back to agents
        </Button>
      </div>

      <details
        className="rounded-xl border border-border/70 px-4 py-3"
        data-testid="task-fit-evidence-disclosure"
        onToggle={(event) => setShowTaskFitEvidence(event.currentTarget.open)}
      >
        <summary className="cursor-pointer font-medium">
          Task-fit evidence review
        </summary>
        {showTaskFitEvidence ? <AgentTaskFitEvidenceReview /> : null}
      </details>

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
          <details className="rounded-lg border border-border/60 px-3 py-2">
            <summary className="cursor-pointer text-sm font-medium">
              Start from a model tier
            </summary>
            <p className="py-2 text-xs text-muted-foreground">
              Creates an editable local draft. No provider is contacted and no
              key is stored. Save only after checking the model and policy.
            </p>
            <div className="space-y-1 pb-1">
              {ROUTE_PRESETS.map((preset) => (
                <button
                  className="w-full rounded-md px-2 py-1.5 text-left text-xs text-foreground hover:bg-muted/70"
                  disabled={isDirty || saveMutation.isPending}
                  key={preset.id}
                  onClick={() => startPreset(preset)}
                  type="button"
                >
                  {preset.label}
                </button>
              ))}
            </div>
          </details>
          {profilesQuery.isError ? (
            <p className="text-sm text-destructive">
              {profilesQuery.error instanceof Error
                ? profilesQuery.error.message
                : String(profilesQuery.error)}
            </p>
          ) : null}
          {profiles.length === 0 && !profilesQuery.isLoading ? (
            <p className="px-2 py-4 text-sm text-muted-foreground">
              No routing profiles saved.
            </p>
          ) : null}
          <div className="max-h-[60vh] space-y-1 overflow-y-auto">
            {profiles.map((profile) => (
              <button
                aria-current={profile.id === selectedId ? "page" : undefined}
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
                  {profile.dataPolicy} · {profile.candidateCount} candidates · v
                  {profile.version}
                </span>
              </button>
            ))}
          </div>
        </aside>

        <div className="min-w-0 rounded-xl border border-border/70 p-4 sm:p-5">
          {isLoading && !draft ? (
            <p className="text-sm text-muted-foreground">
              Loading routing profile…
            </p>
          ) : draft ? (
            <div className="space-y-5">
              <div className="grid gap-4 sm:grid-cols-2">
                <label
                  className="space-y-1.5 text-sm font-medium"
                  htmlFor="route-profile-name"
                >
                  Profile name
                  <Input
                    id="route-profile-name"
                    onChange={(event) =>
                      updateDraft(setDraft, "name", event.target.value)
                    }
                    value={draft.name}
                  />
                </label>
                <label
                  className="space-y-1.5 text-sm font-medium"
                  htmlFor="route-profile-id"
                >
                  Stable ID
                  <Input
                    disabled={!isCreating}
                    id="route-profile-id"
                    onChange={(event) =>
                      updateDraft(setDraft, "id", event.target.value)
                    }
                    value={draft.id}
                  />
                </label>
              </div>

              {ROUTE_PRESETS.find((preset) =>
                draft.id.startsWith(`${preset.id}-`),
              ) ? (
                <p
                  className="rounded-lg border border-border/60 bg-muted/30 px-3 py-2 text-xs text-muted-foreground"
                  data-testid="route-preset-disclosure"
                >
                  {
                    ROUTE_PRESETS.find((preset) =>
                      draft.id.startsWith(`${preset.id}-`),
                    )?.description
                  }
                </p>
              ) : null}

              <div className="space-y-1.5 text-sm font-medium">
                <label htmlFor="route-profile-policy">Hosted data policy</label>
                <PersonaDropdownField
                  disabled={saveMutation.isPending}
                  id="route-profile-policy"
                  onValueChange={(value) =>
                    updateDraft(
                      setDraft,
                      "dataPolicy",
                      value as RouteProfileDraft["dataPolicy"],
                    )
                  }
                  options={[
                    { label: "Local candidates only", value: "local-only" },
                    {
                      label: "Allow hosted candidates",
                      value: "allow-hosted",
                    },
                  ]}
                  placeholder="Choose a data policy"
                  value={draft.dataPolicy}
                />
                <p className="text-xs font-normal text-muted-foreground">
                  Hosted requests share the current prompt with that provider.
                  Buzz does not treat missing local/credential configuration as
                  permission to switch candidates.
                </p>
              </div>

              <label
                className="block space-y-1.5 text-sm font-medium"
                htmlFor="route-profile-max-cost-usd"
              >
                Per-turn estimated cost ceiling (USD)
                <Input
                  disabled={saveMutation.isPending}
                  id="route-profile-max-cost-usd"
                  inputMode="decimal"
                  onChange={(event) =>
                    updateDraft(setDraft, "maxCostUsd", event.target.value)
                  }
                  placeholder="No ceiling"
                  value={draft.maxCostUsd}
                />
                <span className="block text-xs font-normal text-muted-foreground">
                  Buzz reserves a conservative input-size estimate plus the
                  configured output-token limit for each provider request and
                  all of its possible retries. A candidate needs both entered
                  rates to pass. Unknown sizing or pricing stops the request.
                  The actual provider bill may differ because provider
                  tokenization and cache pricing vary. This does not cap
                  project-wide spend.
                </span>
              </label>

              <div className="space-y-2 rounded-lg border border-border/70 p-3">
                <label
                  className="flex items-center gap-2 text-sm font-medium"
                  htmlFor="route-profile-strict-context-fit"
                >
                  <Checkbox
                    checked={draft.strictContextFit}
                    disabled={saveMutation.isPending}
                    id="route-profile-strict-context-fit"
                    onCheckedChange={(checked) =>
                      updateDraft(
                        setDraft,
                        "strictContextFit",
                        checked === true,
                      )
                    }
                  />
                  Require strict context fit
                </label>
                <p className="text-xs text-muted-foreground">
                  Compares each candidate&apos;s operator-declared context
                  capacity with a conservative UTF-8 upper-bound estimate. It
                  counts UTF-8 bytes, message/tool framing allowance, and the
                  configured output reserve. It is not exact provider
                  tokenization or guaranteed context-window accounting. Unknown,
                  multimodal, or opaque replay input abstains before the
                  provider request. Tool rounds are checked again.
                </p>
              </div>

              <fieldset className="space-y-3 rounded-lg border border-border/70 p-3">
                <legend className="px-1 text-sm font-medium">
                  Task-fit evidence gate
                </legend>
                <label
                  className="flex items-center gap-2 text-sm font-medium"
                  htmlFor="route-profile-task-fit-enabled"
                >
                  <Checkbox
                    checked={draft.taskFitEnabled}
                    disabled={saveMutation.isPending}
                    id="route-profile-task-fit-enabled"
                    onCheckedChange={(checked) =>
                      updateDraft(setDraft, "taskFitEnabled", checked === true)
                    }
                  />
                  Require reviewed task-fit evidence
                </label>
                <p className="text-xs text-muted-foreground">
                  This hard gate stays closed when evidence is missing, stale,
                  unreviewed, undersampled, or bound to another route. It
                  applies only to the task class below. Benchmark source
                  authenticity is not verified.
                </p>
                {draft.taskFitEnabled ? (
                  <div className="grid gap-3 sm:grid-cols-2">
                    <label
                      className="space-y-1.5 text-sm font-medium"
                      htmlFor="route-profile-task-fit-class"
                    >
                      Task class ID
                      <Input
                        disabled={saveMutation.isPending}
                        id="route-profile-task-fit-class"
                        maxLength={64}
                        onChange={(event) =>
                          updateDraft(
                            setDraft,
                            "taskFitClass",
                            event.target.value,
                          )
                        }
                        placeholder="for example, coding"
                        value={draft.taskFitClass}
                      />
                    </label>
                    <label
                      className="space-y-1.5 text-sm font-medium"
                      htmlFor="route-profile-task-fit-minimum-tasks"
                    >
                      Minimum distinct tasks
                      <Input
                        disabled={saveMutation.isPending}
                        id="route-profile-task-fit-minimum-tasks"
                        inputMode="numeric"
                        max={1_000_000}
                        min={1}
                        onChange={(event) =>
                          updateDraft(
                            setDraft,
                            "taskFitMinimumDistinctTasks",
                            event.target.value,
                          )
                        }
                        type="number"
                        value={draft.taskFitMinimumDistinctTasks}
                      />
                    </label>
                    <label
                      className="space-y-1.5 text-sm font-medium"
                      htmlFor="route-profile-task-fit-wilson"
                    >
                      Minimum confidence bound
                      <Input
                        disabled={saveMutation.isPending}
                        id="route-profile-task-fit-wilson"
                        max="1"
                        min="0"
                        onChange={(event) =>
                          updateDraft(
                            setDraft,
                            "taskFitMinimumWilsonLowerBound95",
                            event.target.value,
                          )
                        }
                        step="0.01"
                        type="number"
                        value={draft.taskFitMinimumWilsonLowerBound95}
                      />
                    </label>
                    <label
                      className="space-y-1.5 text-sm font-medium"
                      htmlFor="route-profile-task-fit-age-days"
                    >
                      Evidence freshness (days)
                      <Input
                        disabled={saveMutation.isPending}
                        id="route-profile-task-fit-age-days"
                        max="365"
                        min="0.000012"
                        onChange={(event) =>
                          updateDraft(
                            setDraft,
                            "taskFitMaximumAgeDays",
                            event.target.value,
                          )
                        }
                        step="any"
                        type="number"
                        value={draft.taskFitMaximumAgeDays}
                      />
                    </label>
                    <label
                      className="flex items-center gap-2 text-sm font-medium sm:col-span-2"
                      htmlFor="route-profile-task-fit-observed-model"
                    >
                      <Checkbox
                        checked={draft.taskFitRequireObservedModelIdentity}
                        disabled={saveMutation.isPending}
                        id="route-profile-task-fit-observed-model"
                        onCheckedChange={(checked) =>
                          updateDraft(
                            setDraft,
                            "taskFitRequireObservedModelIdentity",
                            checked === true,
                          )
                        }
                      />
                      Require benchmark to observe the exact model ID
                    </label>
                    <p className="text-xs text-muted-foreground sm:col-span-2">
                      Policy versions are fixed to the supported local report
                      format. Route eligibility also requires a signed local
                      association for the exact saved profile and candidate.
                    </p>
                  </div>
                ) : null}
              </fieldset>

              <div className="space-y-3 rounded-lg border border-border/70 p-3">
                <div className="space-y-1.5">
                  <label
                    className="block space-y-1.5 text-sm font-medium"
                    htmlFor="route-profile-min-effective-output-tps"
                  >
                    Minimum effective output speed (tokens/s)
                    <Input
                      disabled={saveMutation.isPending}
                      id="route-profile-min-effective-output-tps"
                      max={1_000_000}
                      min={0.001}
                      onChange={(event) => {
                        const value = event.currentTarget.value;
                        updateDraft(
                          setDraft,
                          "minEffectiveOutputTokensPerSecondMilli",
                          value === ""
                            ? null
                            : Math.round(Number(value) * 1_000),
                        );
                      }}
                      placeholder="No minimum"
                      step={0.001}
                      type="number"
                      value={
                        draft.minEffectiveOutputTokensPerSecondMilli === null
                          ? ""
                          : draft.minEffectiveOutputTokensPerSecondMilli / 1_000
                      }
                    />
                  </label>
                  <p className="text-xs text-muted-foreground">
                    Effective speed includes provider wait. A speed policy uses
                    a candidate&apos;s rate only after five fresh samples match
                    its exact route, endpoint, effort, and input-size identity.
                  </p>
                </div>
                <label
                  className="flex items-center gap-2 text-sm font-medium"
                  htmlFor="route-profile-prefer-fastest-measured"
                >
                  <Checkbox
                    checked={draft.preferFastestMeasured}
                    disabled={saveMutation.isPending}
                    id="route-profile-prefer-fastest-measured"
                    onCheckedChange={(checked) =>
                      updateDraft(
                        setDraft,
                        "preferFastestMeasured",
                        checked === true,
                      )
                    }
                  />
                  Prefer the fastest measured candidate
                </label>
                <label
                  className="flex items-center gap-2 text-sm font-medium"
                  htmlFor="route-profile-allow-preference-order-warmup"
                >
                  <Checkbox
                    checked={draft.allowPreferenceOrderWarmup}
                    disabled={saveMutation.isPending}
                    id="route-profile-allow-preference-order-warmup"
                    onCheckedChange={(checked) =>
                      updateDraft(
                        setDraft,
                        "allowPreferenceOrderWarmup",
                        checked === true,
                      )
                    }
                  />
                  Allow temporary preference-order warm-up
                </label>
                <p className="text-xs text-muted-foreground">
                  Warm-up admits candidates with unknown speed; a measured rate
                  below the minimum stays excluded. With fastest-measured on,
                  unknown candidates use saved preference order first, then
                  measured candidates rank by speed. With it off, saved
                  preference order stays in effect.
                </p>
              </div>

              <div className="space-y-3">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div>
                    <h3 className="text-sm font-semibold">
                      Provider candidates
                    </h3>
                    <p className="text-xs text-muted-foreground">
                      Credentials and API base URLs stay in the agent’s normal
                      configuration.
                    </p>
                    <p className="mt-1 max-w-3xl text-xs text-muted-foreground">
                      Measured rates use fresh local calls, include provider
                      wait, and stay unknown until five matching samples.
                    </p>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {measurementsProfile ? (
                      <Button
                        disabled={throughputQuery.isFetching}
                        onClick={() => void throughputQuery.refetch()}
                        size="sm"
                        variant="outline"
                      >
                        <RefreshCw />
                        Refresh measurements
                      </Button>
                    ) : null}
                    <Button
                      disabled={
                        saveMutation.isPending || draft.candidates.length >= 16
                      }
                      onClick={() =>
                        setDraft((current) =>
                          current ? addCandidate(current) : current,
                        )
                      }
                      size="sm"
                      variant="outline"
                    >
                      <Plus />
                      Add candidate
                    </Button>
                  </div>
                </div>
                {draft.candidates.map((candidate, index) => (
                  <CandidateEditor
                    candidate={candidate}
                    disabled={saveMutation.isPending}
                    index={index}
                    key={candidate.id}
                    onChange={(next) =>
                      setDraft((current) =>
                        current
                          ? replaceCandidate(current, index, next)
                          : current,
                      )
                    }
                    onRemove={() =>
                      setDraft((current) =>
                        current ? removeCandidate(current, index) : current,
                      )
                    }
                    throughputGroups={throughputQuery.data ?? []}
                    throughputLoading={throughputQuery.isLoading}
                    throughputError={throughputQuery.isError}
                    measurementsEnabled={measurementsProfile !== null}
                    removable={draft.candidates.length > 1}
                  />
                ))}
                {throughputQuery.isError && measurementsProfile ? (
                  <div className="flex items-center justify-between gap-3 text-xs text-destructive">
                    <span>Local measurements could not be loaded.</span>
                    <Button
                      onClick={() => void throughputQuery.refetch()}
                      size="sm"
                      variant="outline"
                    >
                      Retry
                    </Button>
                  </div>
                ) : null}
              </div>

              <label
                className="block space-y-1.5 text-sm font-medium"
                htmlFor="route-profile-order"
              >
                Preference order
                <Textarea
                  className="min-h-24 resize-y font-mono text-xs leading-relaxed"
                  disabled={saveMutation.isPending}
                  id="route-profile-order"
                  onChange={(event) =>
                    updateDraft(setDraft, "preferenceText", event.target.value)
                  }
                  value={draft.preferenceText}
                />
                <span className="block text-xs font-normal text-muted-foreground">
                  Put every candidate ID on its own line, best fit first. Hard
                  privacy gates always win. Buzz makes one provider choice per
                  prompt and does not retry another provider after dispatch.
                </span>
              </label>

              {savedProfile ? (
                <p className="text-xs text-muted-foreground">
                  Saved as version {savedProfile.version} · SHA-256{" "}
                  <code>{savedProfile.documentHash}</code>
                </p>
              ) : null}
              {isDirty && saveBlockersForDraft.length > 0 ? (
                <div
                  aria-label="Save profile requirements"
                  className="rounded-md border border-amber-500/40 bg-amber-500/5 p-3 text-xs"
                  role="status"
                >
                  <p className="font-medium">Fix these items before saving:</p>
                  <ul className="mt-1 list-disc space-y-1 pl-5">
                    {saveBlockersForDraft.map((blocker) => (
                      <li key={blocker}>{blocker}</li>
                    ))}
                  </ul>
                </div>
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
                    saveBlockersForDraft.length > 0 ||
                    !isDirty ||
                    saveMutation.isPending
                  }
                  onClick={save}
                  size="sm"
                >
                  <Save />
                  {saveMutation.isPending ? "Saving…" : "Save profile"}
                </Button>
              </div>
              {savedProfile && !isDirty && savedProfile.id === selectedId ? (
                <section
                  aria-labelledby="candidate-test-heading"
                  className="space-y-3 rounded-lg border border-border/70 p-4"
                  data-testid="route-candidate-tests"
                >
                  <div>
                    <h3
                      className="text-sm font-semibold"
                      id="candidate-test-heading"
                    >
                      Test a provider candidate
                    </h3>
                    <p className="mt-1 max-w-3xl text-xs text-muted-foreground">
                      Sends one fixed, project-free prompt using credentials
                      from Global Agent Settings. The check caps output at 256
                      tokens and 30 seconds. It does not include project text,
                      saved prompt packs, or model output in the receipt. Hosted
                      tests show the destination and ask before sending;
                      provider charges may apply.
                    </p>
                  </div>
                  <div className="divide-y divide-border/60 rounded-md border border-border/60">
                    {savedProfile.document.candidates.map((candidate) => (
                      <div
                        className="flex flex-wrap items-center justify-between gap-3 p-3"
                        key={candidate.id}
                      >
                        <div className="min-w-0">
                          <p className="truncate text-sm font-medium">
                            {candidate.id} · {candidate.provider} ·{" "}
                            {candidate.model}
                          </p>
                          <p className="text-xs text-muted-foreground">
                            {candidate.data_location === "local"
                              ? "Local · loopback endpoint required"
                              : "Hosted · confirmation required"}
                          </p>
                        </div>
                        <Button
                          disabled={candidateTestMutation.isPending}
                          onClick={() => requestCandidateTest(candidate)}
                          size="sm"
                          type="button"
                          variant="outline"
                        >
                          {candidateTestMutation.isPending
                            ? "Testing…"
                            : "Test candidate"}
                        </Button>
                      </div>
                    ))}
                  </div>
                  {candidateTestError ? (
                    <p className="text-sm text-destructive" role="alert">
                      {candidateTestError}
                    </p>
                  ) : null}
                  {candidateTestReceipt ? (
                    <div
                      aria-label="Candidate test receipt"
                      className="space-y-2 rounded-md bg-muted/50 p-3 text-xs"
                      data-testid="candidate-test-receipt"
                      role="status"
                    >
                      <p className="flex items-center gap-2 font-medium">
                        {candidateTestReceipt.status === "responded" ? (
                          <Check className="size-4 text-emerald-600" />
                        ) : null}
                        Connection {candidateTestReceipt.status}
                        {candidateTestReceipt.failureClass
                          ? ` · ${candidateTestReceipt.failureClass}`
                          : ""}
                      </p>
                      <p>
                        Synthetic prompt check:{" "}
                        {candidateTestReceipt.responseMarkerMatched === true
                          ? "passed (marker matched)"
                          : candidateTestReceipt.responseMarkerMatched === false
                            ? "did not match expected marker"
                            : "not available"}
                      </p>
                      <p>
                        {candidateTestReceipt.providerId} · requested model{" "}
                        {candidateTestReceipt.requestedModelId} ·{" "}
                        {candidateTestReceipt.dataLocation}
                      </p>
                      {candidateTestReceipt.endpointOrigin ? (
                        <p>
                          Destination: {candidateTestReceipt.endpointOrigin}
                        </p>
                      ) : null}
                      <p>
                        {candidateTestReceipt.elapsedMs} ms · output cap{" "}
                        {candidateTestReceipt.outputTokenCap} · fallback count{" "}
                        {candidateTestReceipt.fallbackCount}
                      </p>
                      <p>
                        Model identity: requested configuration only; provider
                        response identity was not observed.
                      </p>
                      <p className="break-all text-muted-foreground">
                        Profile v{candidateTestReceipt.profileVersion} ·
                        resolved SHA-256{" "}
                        {candidateTestReceipt.resolvedProfileHash}
                      </p>
                    </div>
                  ) : null}
                </section>
              ) : null}
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
      <Dialog
        onOpenChange={(open) => {
          if (!open && !candidateTestMutation.isPending) {
            setPendingHostedTest(null);
            setHostedPreview(null);
          }
        }}
        open={pendingHostedTest !== null && hostedPreview !== null}
      >
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>Confirm hosted synthetic test</DialogTitle>
            <DialogDescription>
              Buzz will send a fixed synthetic prompt to this hosted provider.
              No project text, saved prompt pack, API key, or prior chat is sent
              as prompt content. The provider may log or forward requests, and
              usage may be billed.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-2 rounded-md bg-muted/50 p-3 text-sm">
            <p className="font-medium">
              {pendingHostedTest?.provider} · {pendingHostedTest?.model}
            </p>
            <p className="break-all text-muted-foreground">
              Destination: {hostedPreview?.endpointOrigin}
            </p>
            <p className="text-xs text-muted-foreground">
              This verifies that the configured endpoint responds. It does not
              verify which model the provider served or measure task quality.
            </p>
          </div>
          <div className="flex justify-end gap-2">
            <Button
              disabled={candidateTestMutation.isPending}
              onClick={() => {
                setPendingHostedTest(null);
                setHostedPreview(null);
              }}
              size="sm"
              variant="outline"
            >
              Cancel
            </Button>
            <Button
              disabled={candidateTestMutation.isPending || !pendingHostedTest}
              onClick={() =>
                pendingHostedTest &&
                requestCandidateTest(pendingHostedTest, true)
              }
              size="sm"
            >
              {candidateTestMutation.isPending
                ? "Testing…"
                : "Send synthetic test"}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </section>
  );
}

function CandidateEditor({
  candidate,
  disabled,
  index,
  measurementsEnabled,
  onChange,
  onRemove,
  removable,
  throughputError,
  throughputGroups,
  throughputLoading,
}: {
  candidate: RouteCandidateDraft;
  disabled: boolean;
  index: number;
  measurementsEnabled: boolean;
  onChange: (candidate: RouteCandidateDraft) => void;
  onRemove: () => void;
  removable: boolean;
  throughputError: boolean;
  throughputGroups: AgentRouteThroughputGroupSummary[];
  throughputLoading: boolean;
}) {
  const providerOption = ROUTE_PROVIDERS.some(
    (option) => option.id === candidate.provider,
  )
    ? candidate.provider
    : "";
  return (
    <fieldset className="space-y-3 rounded-lg border border-border/70 p-3">
      <legend className="px-1 text-xs font-medium text-muted-foreground">
        Candidate {index + 1}
      </legend>
      <div className="grid gap-3 sm:grid-cols-2">
        <label
          className="space-y-1.5 text-sm font-medium"
          htmlFor={`route-candidate-id-${index}`}
        >
          Candidate ID
          <Input
            disabled={disabled}
            id={`route-candidate-id-${index}`}
            onChange={(event) =>
              onChange({ ...candidate, id: event.target.value })
            }
            value={candidate.id}
          />
        </label>
        <div className="space-y-1.5 text-sm font-medium">
          <label htmlFor={`route-candidate-provider-${index}`}>Provider</label>
          <PersonaDropdownField
            disabled={disabled}
            id={`route-candidate-provider-${index}`}
            onValueChange={(provider) => onChange({ ...candidate, provider })}
            options={ROUTE_PROVIDERS.map((option) => ({
              label: option.label,
              value: option.id,
            }))}
            placeholder="Choose provider"
            value={providerOption}
          />
        </div>
        <label
          className="space-y-1.5 text-sm font-medium"
          htmlFor={`route-candidate-model-${index}`}
        >
          Exact model ID
          <Input
            disabled={disabled}
            id={`route-candidate-model-${index}`}
            onChange={(event) =>
              onChange({ ...candidate, model: event.target.value })
            }
            placeholder="For example: deepseek-chat"
            value={candidate.model}
          />
        </label>
        <div className="space-y-1.5 text-sm font-medium">
          <label htmlFor={`route-candidate-location-${index}`}>
            Where prompt data goes
          </label>
          <PersonaDropdownField
            disabled={disabled}
            id={`route-candidate-location-${index}`}
            onValueChange={(data_location) =>
              onChange({
                ...candidate,
                data_location: data_location as "local" | "hosted",
              })
            }
            options={[
              { label: "Local (loopback endpoint)", value: "local" },
              { label: "Hosted provider", value: "hosted" },
            ]}
            placeholder="Choose data location"
            value={candidate.data_location}
          />
        </div>
      </div>
      <label
        className="block space-y-1.5 text-sm font-medium"
        htmlFor={`route-candidate-context-capacity-${index}`}
      >
        Context window capacity (tokens)
        <Input
          disabled={disabled}
          id={`route-candidate-context-capacity-${index}`}
          max={1_000_000_000}
          min={1}
          onChange={(event) =>
            onChange({
              ...candidate,
              context_capacity_tokens:
                event.currentTarget.value === ""
                  ? undefined
                  : Number(event.currentTarget.value),
            })
          }
          placeholder="Unknown"
          step={1}
          type="number"
          value={candidate.context_capacity_tokens ?? ""}
        />
        <span className="block text-xs font-normal text-muted-foreground">
          Declared by you; Buzz does not verify this against provider metadata.
          Strict fit abstains if the capacity is missing or too small.
        </span>
      </label>
      <div className="grid gap-3 sm:grid-cols-2">
        <label
          className="space-y-1.5 text-sm font-medium"
          htmlFor={`route-candidate-input-rate-${index}`}
        >
          Input rate (USD per 1M tokens)
          <Input
            disabled={disabled}
            id={`route-candidate-input-rate-${index}`}
            inputMode="decimal"
            onChange={(event) =>
              onChange({
                ...candidate,
                inputCostUsdPerMillion: event.target.value,
              })
            }
            placeholder="Unknown"
            value={candidate.inputCostUsdPerMillion}
          />
        </label>
        <label
          className="space-y-1.5 text-sm font-medium"
          htmlFor={`route-candidate-output-rate-${index}`}
        >
          Output rate (USD per 1M tokens)
          <Input
            disabled={disabled}
            id={`route-candidate-output-rate-${index}`}
            inputMode="decimal"
            onChange={(event) =>
              onChange({
                ...candidate,
                outputCostUsdPerMillion: event.target.value,
              })
            }
            placeholder="Unknown"
            value={candidate.outputCostUsdPerMillion}
          />
        </label>
        <p className="text-xs text-muted-foreground sm:col-span-2">
          Rates are operator-entered and should be checked against the
          provider&apos;s current pricing. They are not inferred from model
          names.
        </p>
      </div>
      <label
        className="block space-y-1.5 text-sm font-medium"
        htmlFor={`route-candidate-addendum-${index}`}
      >
        Target-specific prompt addendum
        <p className="text-xs font-normal text-muted-foreground">
          Buzz also appends the matching saved Buzz Agent prompt profile at
          launch. An exact model profile takes precedence over a provider-wide
          profile; manage these in Prompt Profiles.
        </p>
        <Textarea
          className="min-h-20 resize-y text-sm"
          disabled={disabled}
          id={`route-candidate-addendum-${index}`}
          onChange={(event) =>
            onChange({ ...candidate, prompt_addendum: event.target.value })
          }
          placeholder="Optional instructions appended only for this provider/model."
          value={candidate.prompt_addendum}
        />
      </label>
      <CandidateThroughputSummary
        candidate={candidate}
        enabled={measurementsEnabled}
        error={throughputError}
        groups={throughputGroups}
        loading={throughputLoading}
      />
      {removable ? (
        <div className="flex justify-end">
          <Button
            aria-label={`Remove candidate ${candidate.id || index + 1}`}
            disabled={disabled}
            onClick={onRemove}
            size="sm"
            type="button"
            variant="ghost"
          >
            <Trash2 />
            Remove
          </Button>
        </div>
      ) : null}
    </fieldset>
  );
}

function CandidateThroughputSummary({
  candidate,
  enabled,
  error,
  groups,
  loading,
}: {
  candidate: AgentRouteProfileCandidate;
  enabled: boolean;
  error: boolean;
  groups: AgentRouteThroughputGroupSummary[];
  loading: boolean;
}) {
  const matching = groups.filter((group) => group.candidateId === candidate.id);
  return (
    <section
      aria-label={`Measured performance for ${candidate.id || `candidate`}`}
      className="space-y-2 rounded-md bg-muted/35 p-3"
      data-testid={`route-throughput-${candidate.id || "candidate"}`}
    >
      <p className="text-xs font-semibold">Measured performance</p>
      {!enabled ? (
        <p className="text-xs text-muted-foreground">
          Save this profile to view measurements for its current version.
        </p>
      ) : loading ? (
        <p className="text-xs text-muted-foreground">Loading local samples…</p>
      ) : error ? (
        <p className="text-xs text-muted-foreground">
          Measurements are unavailable right now.
        </p>
      ) : matching.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          No fresh measurements for this candidate.
        </p>
      ) : (
        <ul className="space-y-2">
          {matching.map((group) => (
            <li
              className="flex flex-wrap items-start justify-between gap-x-4 gap-y-1 text-xs"
              key={[
                group.profileHash,
                group.endpointHash,
                group.thinkingEffort,
                group.inputBucket,
              ].join(":")}
            >
              <span className="text-muted-foreground">
                {inputBucketLabel(group.inputBucket)} ·{" "}
                {effortLabel(group.thinkingEffort)} effort
                {matching.length > 1 ? (
                  <span className="block">
                    Provider setup {group.endpointHash.slice(0, 8)} · route
                    setup {group.profileHash.slice(0, 8)}
                  </span>
                ) : null}
              </span>
              <span className="font-medium">
                {group.effectiveOutputTokensPerSecondMilli === null ? (
                  <span className="text-muted-foreground">
                    {group.freshSampleCount}/5 fresh samples · speed unknown
                  </span>
                ) : (
                  <>
                    {formatMeasuredRate(
                      group.effectiveOutputTokensPerSecondMilli,
                    )}{" "}
                    effective output tokens/s · {group.freshSampleCount} fresh
                  </>
                )}
                <span className="mt-0.5 block font-normal text-muted-foreground">
                  Updated {formatSampleAge(group.freshestSampleAtMs)}
                </span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function inputBucketLabel(
  bucket: AgentRouteThroughputGroupSummary["inputBucket"],
): string {
  switch (bucket) {
    case "tiny":
      return "Up to 2k input tokens";
    case "small":
      return "2k–8k input tokens";
    case "medium":
      return "8k–32k input tokens";
    case "large":
      return "32k+ input tokens";
  }
}

function effortLabel(effort: string): string {
  return effort === "default" ? "Default" : effort.toUpperCase();
}

function formatMeasuredRate(rateMilli: number): string {
  return `${(rateMilli / 1_000).toLocaleString(undefined, {
    maximumFractionDigits: 1,
  })}`;
}

function formatSampleAge(timestampMs: number): string {
  const minutes = Math.max(0, Math.floor((Date.now() - timestampMs) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function updateDraft<K extends keyof RouteProfileDraft>(
  setDraft: React.Dispatch<React.SetStateAction<RouteProfileDraft | null>>,
  key: K,
  value: RouteProfileDraft[K],
) {
  setDraft((current) => (current ? { ...current, [key]: value } : current));
}

function addCandidate(draft: RouteProfileDraft): RouteProfileDraft {
  const id = `candidate-${draft.candidates.length + 1}`;
  return {
    ...draft,
    candidates: [
      ...draft.candidates,
      {
        id,
        provider: "openai",
        model: "",
        data_location: "local",
        prompt_addendum: "",
        inputCostUsdPerMillion: "",
        outputCostUsdPerMillion: "",
      },
    ],
    preferenceText: [...preferences(draft.preferenceText), id].join("\n"),
  };
}

function replaceCandidate(
  draft: RouteProfileDraft,
  index: number,
  next: RouteCandidateDraft,
): RouteProfileDraft {
  const previous = draft.candidates[index];
  const candidates = draft.candidates.map((candidate, currentIndex) =>
    currentIndex === index ? next : candidate,
  );
  const preferenceOrder = preferences(draft.preferenceText).map((id) =>
    id === previous.id ? next.id : id,
  );
  return { ...draft, candidates, preferenceText: preferenceOrder.join("\n") };
}

function removeCandidate(
  draft: RouteProfileDraft,
  index: number,
): RouteProfileDraft {
  const removedId = draft.candidates[index]?.id;
  const candidates = draft.candidates.filter(
    (_, currentIndex) => currentIndex !== index,
  );
  return {
    ...draft,
    candidates,
    preferenceText: preferences(draft.preferenceText)
      .filter((id) => id !== removedId)
      .join("\n"),
  };
}

function preferences(value: string): string[] {
  return value
    .split(/[\n,]+/)
    .map((id) => id.trim())
    .filter(Boolean);
}

function saveBlockers(draft: RouteProfileDraft): string[] {
  const blockers: string[] = [];
  const candidateIds = draft.candidates.map((candidate) => candidate.id);
  const order = preferences(draft.preferenceText);
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(draft.id.trim())) {
    blockers.push(
      "Stable ID must use lowercase letters, numbers, and single hyphens.",
    );
  }
  if (draft.id.trim().length > 64) {
    blockers.push("Stable ID must be 64 characters or fewer.");
  }
  if (!draft.name.trim()) blockers.push("Profile name is required.");
  if (draft.name.trim().length > 120) {
    blockers.push("Profile name must be 120 characters or fewer.");
  }
  if (draft.maxCostUsd !== "" && parseUsdMicrousd(draft.maxCostUsd) === null) {
    blockers.push(
      "Cost ceiling must be a non-negative USD amount with up to 6 decimals.",
    );
  }
  if (
    draft.minEffectiveOutputTokensPerSecondMilli !== null &&
    (!Number.isSafeInteger(draft.minEffectiveOutputTokensPerSecondMilli) ||
      draft.minEffectiveOutputTokensPerSecondMilli < 1 ||
      draft.minEffectiveOutputTokensPerSecondMilli > 1_000_000_000)
  ) {
    blockers.push(
      "Minimum effective speed must be between 0.001 and 1,000,000 tokens/s.",
    );
  }
  if (draft.taskFitEnabled) {
    if (!/^[a-z][a-z0-9._-]{0,63}$/.test(draft.taskFitClass.trim())) {
      blockers.push(
        "Task class ID must start with a lowercase letter and use only lowercase letters, numbers, periods, underscores, or hyphens (up to 64 characters).",
      );
    }
    const minimumTasks = Number(draft.taskFitMinimumDistinctTasks);
    if (
      !Number.isInteger(minimumTasks) ||
      minimumTasks < 1 ||
      minimumTasks > 1_000_000
    ) {
      blockers.push(
        "Minimum distinct tasks must be a whole number from 1 to 1,000,000.",
      );
    }
    const minimumWilsonBound = Number(draft.taskFitMinimumWilsonLowerBound95);
    if (
      !Number.isFinite(minimumWilsonBound) ||
      minimumWilsonBound < 0 ||
      minimumWilsonBound > 1
    ) {
      blockers.push("Minimum confidence bound must be from 0 to 1.");
    }
    const maximumAgeDays = Number(draft.taskFitMaximumAgeDays);
    if (
      !Number.isFinite(maximumAgeDays) ||
      maximumAgeDays <= 0 ||
      maximumAgeDays > 365 ||
      Math.round(maximumAgeDays * 86400) < 1
    ) {
      blockers.push(
        "Evidence freshness must be at least 1 second and no more than 365 days.",
      );
    }
  }
  if (draft.candidates.length < 1 || draft.candidates.length > 16) {
    blockers.push("Add between 1 and 16 provider candidates.");
  }
  if (candidateIds.some((id) => !/^[a-z][a-z0-9-]{0,63}$/.test(id))) {
    blockers.push(
      "Each candidate ID must start with a lowercase letter and use only lowercase letters, numbers, or hyphens (up to 64 characters).",
    );
  }
  if (new Set(candidateIds).size !== candidateIds.length) {
    blockers.push("Candidate IDs must be unique.");
  }
  draft.candidates.forEach((candidate, index) => {
    const label = `Candidate ${index + 1}`;
    if (!ROUTE_PROVIDERS.some((option) => option.id === candidate.provider)) {
      blockers.push(`${label} uses an unsupported provider.`);
    }
    if (!candidate.model.trim()) {
      blockers.push(`${label} needs an exact model ID.`);
    } else if (candidate.model.trim().length > 256) {
      blockers.push(`${label} model ID must be 256 characters or fewer.`);
    }
    if (
      candidate.context_capacity_tokens !== undefined &&
      (!Number.isSafeInteger(candidate.context_capacity_tokens) ||
        candidate.context_capacity_tokens < 1 ||
        candidate.context_capacity_tokens > 1_000_000_000)
    ) {
      blockers.push(
        `${label} context capacity must be a whole number from 1 to 1,000,000,000.`,
      );
    }
    if (
      !validRatePair(
        candidate.inputCostUsdPerMillion,
        candidate.outputCostUsdPerMillion,
      )
    ) {
      blockers.push(
        `${label} pricing needs both rates, each a valid non-negative USD amount with up to 6 decimals.`,
      );
    }
    if (candidate.prompt_addendum.length > 16 * 1024) {
      blockers.push(
        `${label} prompt addendum must be 16,384 characters or fewer.`,
      );
    }
  });
  if (
    order.length !== candidateIds.length ||
    new Set(order).size !== candidateIds.length ||
    !candidateIds.every((id) => order.includes(id))
  ) {
    blockers.push(
      "Preference order must list every candidate ID exactly once.",
    );
  }
  return blockers;
}

function validRatePair(input: string, output: string): boolean {
  if (!input && !output) return true;
  return parseUsdMicrousd(input) !== null && parseUsdMicrousd(output) !== null;
}

function parseUsdMicrousd(value: string): number | null {
  const normalized = value.trim();
  if (!/^(?:0|[1-9]\d*)(?:\.\d{0,6})?$/.test(normalized)) return null;
  const [whole, fraction = ""] = normalized.split(".");
  const microusd = Number(whole) * 1_000_000 + Number(fraction.padEnd(6, "0"));
  return Number.isSafeInteger(microusd) && microusd <= 1_000_000_000_000
    ? microusd
    : null;
}

function formatMicrousd(microusd: number): string {
  const whole = Math.floor(microusd / 1_000_000);
  const fraction = String(microusd % 1_000_000)
    .padStart(6, "0")
    .replace(/0+$/, "");
  return fraction ? `${whole}.${fraction}` : String(whole);
}
