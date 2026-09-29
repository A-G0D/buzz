import type { ProjectAgentResourceDefaultsDraft } from "@/features/projects/projectAgentProfileDefault";
import { buildProjectResourcePreflight } from "@/features/projects/projectResourcePreflight";

export function ProjectResourcePreflight({
  draft,
}: {
  draft: ProjectAgentResourceDefaultsDraft;
}) {
  const preflight = buildProjectResourcePreflight(draft);

  if (!preflight.valid) {
    return (
      <p className="text-xs text-destructive" role="status">
        Fix the resource values to complete this preflight.
      </p>
    );
  }

  const configuredAgentMetrics = preflight.policy.metrics.filter(
    (metric) => metric.scope === "agent",
  );
  const unknownMetrics = preflight.policy.metrics.filter(
    (metric) => metric.source === "unavailable",
  );

  return (
    <div
      className="space-y-2 rounded-lg bg-muted/40 px-2.5 py-2 text-xs"
      data-testid="project-resource-preflight"
    >
      <p className="font-medium">Preflight · per new agent</p>
      <ul className="space-y-1 text-muted-foreground">
        {configuredAgentMetrics.map((metric) => (
          <li className="flex justify-between gap-3" key={metric.id}>
            <span>{metric.label}</span>
            <span className="text-right">
              {metric.value === null
                ? "Profile/runtime default"
                : `${metric.value} ${metric.unit}`}
            </span>
          </li>
        ))}
      </ul>
      <p className="leading-relaxed text-muted-foreground">
        Buzz passes these values into managed runtime configuration. Parallelism
        may be capped by the selected harness. This is not an aggregate project
        or run budget.
      </p>
      <p className="leading-relaxed text-muted-foreground">
        Unknown project/run telemetry:{" "}
        {unknownMetrics.map((metric) => metric.label).join(", ")}.
      </p>
    </div>
  );
}
