import type { BuilderlabAuth } from "./hostedCommunityApi";
import { getBuilderlabAuth, startBuilderlabLogin } from "./hostedCommunityApi";
import { invokeTauri } from "@/shared/api/tauri";

export type EnterpriseLoginGateStatus =
  | { status: "notRequired" }
  | { status: "required" };

export async function enterpriseLoginGate(
  relayUrl: string,
): Promise<EnterpriseLoginGateStatus> {
  return invokeTauri<EnterpriseLoginGateStatus>("enterprise_login_gate", {
    relayUrl,
  });
}

/**
 * Ensures the selected relay's NIP-11 enterprise-auth requirement is honored
 * before the backend applies the community and relay consumers mount.
 *
 * The relay only advertises the privacy-safe requirement. Trusted relay/login
 * provider configuration stays in the signed build and in native commands; this
 * function never reads URLs or issuer details from NIP-11.
 */
export async function ensureEnterpriseLoginForRelay(
  relayUrl: string,
): Promise<BuilderlabAuth | null> {
  const gate = await enterpriseLoginGate(relayUrl);
  if (gate.status === "notRequired") {
    return null;
  }

  try {
    const auth = await getBuilderlabAuth();
    if (auth !== null) {
      return auth;
    }
  } catch {
    // Invalid/expired in-memory credentials are cleared by the native command.
    // Fall through to a fresh browser login.
  }

  return startBuilderlabLogin();
}
