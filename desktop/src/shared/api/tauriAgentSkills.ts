import { invokeTauri } from "@/shared/api/tauri";

export type AgentSkillSummary = {
  name: string;
  description: string;
  contentHash: string;
  validationError: string | null;
};

export type AgentSkillDetails = AgentSkillSummary & {
  content: string;
  runtimeCompatibility: AgentSkillRuntimeCompatibility[];
};

export type AgentSkillRuntimeCompatibility = {
  runtimeId: string;
  runtimeLabel: string;
  skillDirectory: string;
  status: "linked" | "missing" | "conflict" | "blocked";
};

export type AgentSkillPackPreview = {
  version: number;
  exportedFrom: string;
  skills: AgentSkillPackPreviewSkill[];
};

export type AgentSkillPackPreviewSkill = {
  name: string;
  description: string;
  license: string | null;
  content: string;
  contentHash: string;
  alreadyInstalled: boolean;
};

export type AgentSkillPackSelection = {
  name: string;
  expectedContentHash: string;
};

export async function listAgentSkills(): Promise<AgentSkillSummary[]> {
  return invokeTauri<AgentSkillSummary[]>("list_agent_skills");
}

export async function readAgentSkill(name: string): Promise<AgentSkillDetails> {
  return invokeTauri<AgentSkillDetails>("read_agent_skill", { name });
}

export async function saveAgentSkill(input: {
  name: string;
  content: string;
  expectedContentHash: string | null;
}): Promise<AgentSkillDetails> {
  return invokeTauri<AgentSkillDetails>("save_agent_skill", input);
}

export async function exportAgentSkillPack(input: {
  skills: AgentSkillPackSelection[];
}): Promise<boolean> {
  return invokeTauri<boolean>("export_agent_skill_pack", input);
}

export async function previewAgentSkillPack(
  fileBytes: number[],
): Promise<AgentSkillPackPreview> {
  return invokeTauri<AgentSkillPackPreview>("preview_agent_skill_pack", {
    fileBytes,
  });
}

export async function installAgentSkillPack(input: {
  fileBytes: number[];
  expectedSkills: Array<{ name: string; contentHash: string }>;
}): Promise<AgentSkillDetails[]> {
  return invokeTauri<AgentSkillDetails[]>("install_agent_skill_pack", input);
}
