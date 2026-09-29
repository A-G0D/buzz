import type { AgentRouteProfileSummary } from "@/shared/api/tauriAgentRouteProfiles";
import { PersonaDropdownField } from "./PersonaDropdownField";

export function AgentRouteProfileField({
  error,
  disabled,
  isLoading,
  onChange,
  profiles,
  selectedId,
  supported,
}: {
  error: Error | null;
  disabled: boolean;
  isLoading: boolean;
  onChange: (id: string) => void;
  profiles: AgentRouteProfileSummary[];
  selectedId: string;
  supported: boolean;
}) {
  const selectedExists = profiles.some((profile) => profile.id === selectedId);
  const options = [
    { label: "Off · use configured provider/model", value: "" },
    ...profiles.map((profile) => ({
      label: `${profile.name} · ${profile.dataPolicy} · v${profile.version}`,
      value: profile.id,
    })),
  ];
  const currentSelection = options.find(
    (option) => option.value === selectedId,
  );
  const currentUnknownSelection =
    selectedId && !selectedExists
      ? {
          label: isLoading
            ? `${selectedId} · loading saved profile`
            : `${selectedId} · profile unavailable`,
          value: selectedId,
        }
      : null;

  return (
    <section
      aria-label="Provider routing profile"
      className="space-y-2 rounded-lg border border-border/70 p-3"
      data-testid="agent-route-profile-field"
    >
      <div className="space-y-1">
        <h3 className="text-sm font-medium">Provider routing</h3>
        <p className="text-xs text-muted-foreground">
          Choose one versioned route profile for this agent’s Buzz Agent API
          prompts. Buzz makes one provider choice per prompt and does not retry
          a different provider after a request starts.
        </p>
      </div>
      <PersonaDropdownField
        disabled={disabled}
        id="agent-route-profile-select"
        onValueChange={onChange}
        options={
          supported
            ? currentUnknownSelection
              ? [...options, currentUnknownSelection]
              : options
            : [
                options[0],
                ...(currentSelection ? [currentSelection] : []),
                ...(currentUnknownSelection ? [currentUnknownSelection] : []),
              ]
        }
        placeholder="Choose a routing profile"
        value={selectedId}
      />
      {supported && profiles.length === 0 && !isLoading && !error ? (
        <p className="text-xs text-muted-foreground">
          No profiles saved yet. Create one in Agents → Routing profiles.
        </p>
      ) : null}
      {!supported ? (
        <p className="text-xs text-amber-700 dark:text-amber-300">
          This profile runs only with a local Buzz Agent runtime. Clear it here
          before saving a different runtime.
        </p>
      ) : null}
      {error ? (
        <p className="text-xs text-destructive">{error.message}</p>
      ) : null}
      {supported ? (
        <p className="text-xs text-muted-foreground">
          Hosted routes are controlled by the profile’s explicit data policy.
          Provider credentials stay in the agent’s existing local settings.
        </p>
      ) : null}
    </section>
  );
}
