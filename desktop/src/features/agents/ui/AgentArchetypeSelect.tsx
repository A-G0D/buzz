import { useAgentArchetypesQuery } from "../hooks";
import { PersonaDropdownField } from "./PersonaDropdownField";

export function AgentArchetypeSelect({
  disabled,
  id = "agent-archetype",
  onChange,
  value,
}: {
  disabled: boolean;
  id?: string;
  onChange: (id: string) => void;
  value: string;
}) {
  const archetypesQuery = useAgentArchetypesQuery();
  const archetypes = archetypesQuery.data ?? [];
  const selected = archetypes.find((archetype) => archetype.id === value);
  const snapshot = selected?.snapshot;
  const promptPreview = snapshot?.promptAddendum
    ? `<agent-archetype id="${snapshot.id}" version="${snapshot.version}">\n${snapshot.promptAddendum}\n</agent-archetype>`
    : null;
  const appliedLimits = [
    snapshot?.parallelism ? `Parallel agents: ${snapshot.parallelism}` : null,
    snapshot?.idleTimeoutSeconds
      ? `Idle timeout: ${formatDuration(snapshot.idleTimeoutSeconds)}`
      : null,
    snapshot?.maxTurnDurationSeconds
      ? `Maximum turn: ${formatDuration(snapshot.maxTurnDurationSeconds)}`
      : null,
  ].filter(Boolean);

  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium" htmlFor={id}>
        Agent starting style
      </label>
      <PersonaDropdownField
        disabled={disabled || archetypes.length === 0}
        id={id}
        onValueChange={onChange}
        options={archetypes.map((archetype) => ({
          label: archetype.name,
          value: archetype.id,
        }))}
        placeholder={
          archetypesQuery.isLoading ? "Loading styles…" : "Choose a style"
        }
        value={value}
      />
      <p className="text-xs leading-relaxed text-muted-foreground">
        {selected?.description ??
          (archetypesQuery.isError
            ? "Styles could not be loaded. The agent will use its normal behavior."
            : "Local prompt and Buzz-enforced run defaults. This does not change shared persona settings or tool access.")}
      </p>
      {selected ? (
        <details className="rounded-lg bg-muted/35 px-2.5 py-2 text-xs leading-relaxed">
          <summary className="cursor-pointer font-medium text-foreground">
            Preview prompt and limits
          </summary>
          <div className="mt-2 space-y-1" data-testid="agent-archetype-preview">
            {promptPreview ? (
              <>
                <p className="font-medium text-foreground">Prompt section</p>
                <pre className="max-h-32 overflow-y-auto whitespace-pre-wrap font-sans text-muted-foreground">
                  {promptPreview}
                </pre>
              </>
            ) : (
              <p className="text-muted-foreground">
                No additional prompt instructions.
              </p>
            )}
            {appliedLimits.length > 0 ? (
              <p className="text-muted-foreground">
                {appliedLimits.join(" · ")}
              </p>
            ) : (
              <p className="text-muted-foreground">
                No additional Buzz run limits.
              </p>
            )}
          </div>
        </details>
      ) : null}
    </div>
  );
}

function formatDuration(seconds: number) {
  if (seconds % 3600 === 0) return `${seconds / 3600} hr`;
  if (seconds % 60 === 0) return `${seconds / 60} min`;
  return `${seconds} sec`;
}
