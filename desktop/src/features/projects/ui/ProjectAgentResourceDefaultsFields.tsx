import { Input } from "@/shared/ui/input";
import type { ProjectAgentResourceDefaultsDraft } from "@/features/projects/projectAgentProfileDefault";

export function ProjectAgentResourceDefaultsFields({
  disabled = false,
  idPrefix,
  onChange,
  value,
}: {
  disabled?: boolean;
  idPrefix: string;
  onChange: (draft: ProjectAgentResourceDefaultsDraft) => void;
  value: ProjectAgentResourceDefaultsDraft;
}) {
  const fields = [
    { id: "parallelism", label: "Parallelism (1–32)", max: 32 },
    { id: "idleTimeoutSeconds", label: "Idle timeout (seconds)" },
    { id: "maxTurnDurationSeconds", label: "Maximum turn (seconds)" },
  ] as const;

  return (
    <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
      {fields.map((field) => {
        const id = `${idPrefix}-${field.id}`;
        return (
          <label className="space-y-1 text-xs" htmlFor={id} key={field.id}>
            <span>{field.label}</span>
            <Input
              disabled={disabled}
              id={id}
              max={"max" in field ? field.max : undefined}
              min={1}
              onChange={(event) =>
                onChange({ ...value, [field.id]: event.target.value })
              }
              step={1}
              type="number"
              value={value[field.id]}
            />
          </label>
        );
      })}
    </div>
  );
}
