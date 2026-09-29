import type { CriticRole } from "@/shared/api/types";

export const MAX_ESTIMATED_CRITIC_BUDGET_MICROUSD = 1_000_000_000_000;

/** Parse plain USD text exactly to micro-USD; null means no ceiling. */
export function parseEstimatedCriticBudgetUsd(
  raw: string,
): number | null | undefined {
  const value = raw.trim();
  if (!value) return null;
  if (!/^(?:\d+(?:\.\d{0,6})?|\.\d{1,6})$/.test(value)) return undefined;

  const [wholeText = "0", fractionalText = ""] = value.split(".");
  const whole = Number(wholeText || "0");
  const fractional = Number(fractionalText.padEnd(6, "0") || "0");
  const total = whole * 1_000_000 + fractional;
  return Number.isSafeInteger(total) &&
    total <= MAX_ESTIMATED_CRITIC_BUDGET_MICROUSD
    ? total
    : undefined;
}

/** Stable equal split; earlier role names receive any remainder micro-USD. */
export function allocateEstimatedCriticBudget(
  budgetMicrousd: number,
  roles: readonly CriticRole[],
): Partial<Record<CriticRole, number>> {
  const uniqueRoles = [...new Set(roles)].sort();
  if (
    !Number.isSafeInteger(budgetMicrousd) ||
    budgetMicrousd < 0 ||
    budgetMicrousd > MAX_ESTIMATED_CRITIC_BUDGET_MICROUSD ||
    uniqueRoles.length === 0
  ) {
    return {};
  }
  const base = Math.floor(budgetMicrousd / uniqueRoles.length);
  const remainder = budgetMicrousd % uniqueRoles.length;
  return Object.fromEntries(
    uniqueRoles.map((role, index) => [role, base + Number(index < remainder)]),
  );
}

export function formatEstimatedCriticBudgetUsd(microusd: number): string {
  return (microusd / 1_000_000).toFixed(6).replace(/\.?(?:0+)$/, "");
}

export function criticFailureLabel(errorCode: string): string {
  switch (errorCode) {
    case "estimated_cost_ceiling":
      return "The estimated cost ceiling stopped this reviewer before another model request was sent.";
    case "estimated_cost_unavailable":
      return "Buzz could not estimate the next model request from the route's rates, so it stopped before sending it.";
    default:
      return errorCode;
  }
}
