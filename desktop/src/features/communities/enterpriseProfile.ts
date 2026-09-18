import type { BuilderlabAuth } from "./hostedCommunityApi";

export type EnterpriseProfileSeed = {
  username: string;
  displayName: string;
};

export function authoritativeEnterpriseProfile(
  auth: BuilderlabAuth | null | undefined,
): EnterpriseProfileSeed | null {
  const username = auth?.corporateUsername?.trim() ?? "";
  const displayName = auth?.corporateDisplayName?.trim() ?? "";
  if (!username || !displayName) return null;
  return { username, displayName };
}
