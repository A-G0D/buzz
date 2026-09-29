import {
  parseProjectAgentResourceDefaultsDraft,
  type ProjectAgentResourceDefaultsDraft,
} from "@/features/projects/projectAgentProfileDefault";

export type ResourceMeasurementState =
  | "configured"
  | "measured"
  | "estimated"
  | "unknown";

export type ResourceEnforcementMode =
  | "hard_preflight"
  | "hard_runtime"
  | "turn_boundary"
  | "soft_warning"
  | "unknown";

export type ProjectResourcePolicyMetricV1 = {
  id: string;
  label: string;
  value: number | null;
  unit: string;
  scope: "agent" | "project" | "run" | "device" | "model";
  state: ResourceMeasurementState;
  source: "project_local_default" | "runtime_default" | "unavailable";
  enforcement: ResourceEnforcementMode;
};

export type ProjectResourcePolicyV1 = {
  schemaVersion: 1;
  scope: "project";
  appliesTo: "new_managed_agents";
  metrics: ProjectResourcePolicyMetricV1[];
};

export type ProjectResourcePreflight =
  | { valid: false; policy: null }
  | { valid: true; policy: ProjectResourcePolicyV1 };

const AGENT_METRICS: Array<{
  id: keyof ProjectAgentResourceDefaultsDraft;
  label: string;
  unit: string;
}> = [
  { id: "parallelism", label: "Parallelism", unit: "workers per agent" },
  { id: "idleTimeoutSeconds", label: "Idle timeout", unit: "seconds" },
  {
    id: "maxTurnDurationSeconds",
    label: "Maximum turn duration",
    unit: "seconds",
  },
];

const UNAVAILABLE_METRICS: ProjectResourcePolicyMetricV1[] = [
  {
    id: "project.aggregate_concurrency",
    label: "Aggregate project concurrency",
    value: null,
    unit: "agents",
    scope: "project",
    state: "unknown",
    source: "unavailable",
    enforcement: "unknown",
  },
  {
    id: "run.aggregate_concurrency",
    label: "Aggregate run concurrency",
    value: null,
    unit: "agents",
    scope: "run",
    state: "unknown",
    source: "unavailable",
    enforcement: "unknown",
  },
  {
    id: "run.token_usage",
    label: "Token usage and spend",
    value: null,
    unit: "provider-specific",
    scope: "run",
    state: "unknown",
    source: "unavailable",
    enforcement: "unknown",
  },
  {
    id: "device.memory",
    label: "RAM and VRAM usage",
    value: null,
    unit: "bytes",
    scope: "device",
    state: "unknown",
    source: "unavailable",
    enforcement: "unknown",
  },
  {
    id: "model.throughput",
    label: "Model throughput",
    value: null,
    unit: "tokens per second",
    scope: "model",
    state: "unknown",
    source: "unavailable",
    enforcement: "unknown",
  },
];

/** Build a versioned, honest policy view for newly provisioned managed agents. */
export function buildProjectResourcePreflight(
  draft: ProjectAgentResourceDefaultsDraft,
): ProjectResourcePreflight {
  const defaults = parseProjectAgentResourceDefaultsDraft(draft);
  if (!defaults) return { valid: false, policy: null };

  const configuredMetrics = AGENT_METRICS.map(({ id, label, unit }) => {
    const value = defaults[id] ?? null;
    const configured = value !== null;
    return {
      id: `agent.${id}`,
      label,
      value,
      unit,
      scope: "agent" as const,
      state: configured ? ("configured" as const) : ("unknown" as const),
      source: configured
        ? ("project_local_default" as const)
        : ("runtime_default" as const),
      enforcement: !configured
        ? ("unknown" as const)
        : id === "parallelism"
          ? ("hard_runtime" as const)
          : ("turn_boundary" as const),
    };
  });

  return {
    valid: true,
    policy: {
      schemaVersion: 1,
      scope: "project",
      appliesTo: "new_managed_agents",
      metrics: [...configuredMetrics, ...UNAVAILABLE_METRICS],
    },
  };
}
