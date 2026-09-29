import { invokeTauri } from "@/shared/api/tauri";

export type AgentPromptProfileTargetKind =
  | "buzz_agent_api"
  | "acp_harness"
  | "cli_harness"
  | "consumer_app";

export type AgentPromptProfileTarget = {
  kind: AgentPromptProfileTargetKind;
  targetId: string;
  modelId: string | null;
};

export type AgentPromptProfileSummary = {
  id: string;
  name: string;
  version: number;
  target: AgentPromptProfileTarget;
  promptHash: string;
  updatedAt: string;
};

export type AgentPromptProfile = AgentPromptProfileSummary & {
  schemaVersion: number;
  prompt: string;
};

export async function listAgentPromptProfiles(): Promise<
  AgentPromptProfileSummary[]
> {
  return invokeTauri<AgentPromptProfileSummary[]>("list_agent_prompt_profiles");
}

export async function readAgentPromptProfile(
  id: string,
): Promise<AgentPromptProfile> {
  return invokeTauri<AgentPromptProfile>("read_agent_prompt_profile", { id });
}

export async function saveAgentPromptProfile(input: {
  id: string;
  name: string;
  target: AgentPromptProfileTarget;
  prompt: string;
  expectedVersion: number | null;
}): Promise<AgentPromptProfile> {
  return invokeTauri<AgentPromptProfile>("save_agent_prompt_profile", {
    input,
  });
}
