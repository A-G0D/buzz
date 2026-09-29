export type CriticTriggerIntent =
  | {
      status: "recognized";
      command: "run_critics";
      source: "slash_command" | "natural_language";
      target: "current_thread";
    }
  | { status: "unrecognized" };

const NATURAL_LANGUAGE_COMMAND =
  /^(?:please[ \t]+)?run[ \t]+critics[ \t]+on[ \t]+this(?:[ \t]+thread)?[.!]?$/i;

/** Recognize only a whole-draft critic command; callers still require preview and confirmation. */
export function parseCriticTriggerIntent(draft: string): CriticTriggerIntent {
  const command = draft.trim();
  if (/^\/critics$/i.test(command)) {
    return {
      status: "recognized",
      command: "run_critics",
      source: "slash_command",
      target: "current_thread",
    };
  }
  if (NATURAL_LANGUAGE_COMMAND.test(command)) {
    return {
      status: "recognized",
      command: "run_critics",
      source: "natural_language",
      target: "current_thread",
    };
  }
  return { status: "unrecognized" };
}
