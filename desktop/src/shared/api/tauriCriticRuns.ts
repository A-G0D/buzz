import { invokeTauri } from "@/shared/api/tauri";
import type {
  CriticRoundRecord,
  CriticRoundSummary,
  CriticCoordinatorGuidePreview,
  CriticRunParams,
  CriticRunResult,
  CriticRouteProfilePreview,
} from "@/shared/api/types";
import type { AgentRouteProfileSummary } from "@/shared/api/tauriAgentRouteProfiles";

export async function previewCriticCoordinatorGuide(): Promise<CriticCoordinatorGuidePreview> {
  return invokeTauri<CriticCoordinatorGuidePreview>(
    "preview_critic_coordinator_guide",
  );
}

export async function listCriticRouteProfiles(): Promise<
  AgentRouteProfileSummary[]
> {
  return invokeTauri<AgentRouteProfileSummary[]>("list_agent_route_profiles");
}

export async function previewCriticRouteProfile(
  profileId: string,
): Promise<CriticRouteProfilePreview> {
  return invokeTauri<CriticRouteProfilePreview>(
    "preview_critic_route_profile",
    {
      profileId,
    },
  );
}

export async function getRecentCriticRounds(
  limit = 20,
): Promise<CriticRoundSummary[]> {
  return invokeTauri<CriticRoundSummary[]>("get_recent_critic_rounds", {
    limit,
  });
}

export async function getCriticRound(
  roundId: string,
): Promise<CriticRoundRecord | null> {
  return invokeTauri<CriticRoundRecord | null>("get_critic_round", {
    roundId,
  });
}

export async function runCriticRound(
  requestId: string,
  params: CriticRunParams,
): Promise<CriticRunResult> {
  return invokeTauri<CriticRunResult>("run_critic_round", {
    requestId,
    params,
  });
}

export async function cancelCriticRound(requestId: string): Promise<boolean> {
  return invokeTauri<boolean>("cancel_critic_round", { requestId });
}
