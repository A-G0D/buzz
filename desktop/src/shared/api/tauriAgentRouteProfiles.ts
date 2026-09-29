import { invokeTauri } from "./tauri";

export type AgentRouteProfileCandidate = {
  id: string;
  provider: string;
  model: string;
  data_location: "local" | "hosted";
  context_capacity_tokens?: number;
  input_cost_microusd_per_million_tokens?: number;
  output_cost_microusd_per_million_tokens?: number;
  prompt_profile?: { id: string; version: number; prompt_hash: string };
  prompt_addendum: string;
};

export type AgentTaskFitEligibilityPolicy = {
  taskClass: string;
  taskClassTaxonomyVersion: string;
  evaluationPolicyVersion: string;
  minimumDistinctTasks: number;
  minimumWilsonLowerBound95: number;
  maximumAgeSeconds: number;
  requireObservedModelIdentity: boolean;
};

export type AgentRouteProfileDocument = {
  version: 1;
  data_policy: "local-only" | "allow-hosted";
  preference_order: string[];
  strict_context_fit?: boolean;
  max_turn_cost_microusd?: number | null;
  min_effective_output_tokens_per_second_milli?: number | null;
  prefer_fastest_measured?: boolean;
  allow_preference_order_warmup?: boolean;
  task_fit_policy?: AgentTaskFitEligibilityPolicy;
  candidates: AgentRouteProfileCandidate[];
};

export type AgentRouteProfileSummary = {
  id: string;
  name: string;
  version: number;
  dataPolicy: "local-only" | "allow-hosted";
  candidateCount: number;
  documentHash: string;
  updatedAt: string;
};

export type AgentRouteProfile = AgentRouteProfileSummary & {
  schemaVersion: 1;
  document: AgentRouteProfileDocument;
};

export type AgentRouteCandidateTestReceipt = {
  profileId: string;
  profileVersion: number;
  profileDocumentHash: string;
  resolvedProfileHash: string;
  candidateId: string;
  providerId: string;
  runtimeProviderId: string | null;
  requestedModelId: string;
  dataLocation: "local" | "hosted";
  endpointOrigin: string | null;
  status: "confirmation_required" | "responded" | "failed";
  startedAt: string;
  elapsedMs: number;
  outputTokenCap: number;
  timeoutSeconds: number;
  inputTokens: number | null;
  outputTokens: number | null;
  totalTokens: number | null;
  responseMarkerMatched: boolean | null;
  modelIdentityObserved: false;
  identityEvidence: "requested_configuration_only";
  targetPromptProfileIncluded: false;
  fallbackCount: 0;
  failureClass: string | null;
};

export type AgentRouteThroughputGroupSummary = {
  profileHash: string;
  endpointHash: string;
  candidateId: string;
  providerId: string;
  modelId: string;
  thinkingEffort: string;
  inputBucket: "tiny" | "small" | "medium" | "large";
  freshSampleCount: number;
  effectiveOutputTokensPerSecondMilli: number | null;
  freshestSampleAtMs: number;
};

export type AgentTaskFitEvidenceSummary = {
  reportSha256: string;
  taskClass: string;
  taskClassTaxonomyVersion: string;
  evaluationPolicyVersion: string;
  dataset: string;
  jobId: string;
  providerId: string;
  modelId: string;
  endpointId: string;
  conditionId: string;
  manifestSha256: string;
  generation: Record<string, unknown>;
  endpointConfigSha256: string;
  promptSha256: string;
  runtimeBinarySha256: Record<string, string>;
  caseSetSha256: string;
  taskCount: number;
  taskSuccessCount: number;
  taskSuccessRate: number;
  wilsonLowerBound95: number;
  createdAt: string;
  finishedAt: string;
  modelIdentityObserved: boolean;
  localAttestation: AgentTaskFitLocalAttestation | null;
  routeAttestations: AgentTaskFitRouteAttestation[];
};

export type AgentTaskFitLocalAttestation = {
  publicKey: string;
  eventId: string;
  createdAt: number;
};

export type AgentTaskFitRouteAttestation = {
  publicKey: string;
  eventId: string;
  createdAt: number;
  profileId: string;
  profileVersion: number;
  profileHash: string;
  candidateId: string;
};

export async function listAgentRouteProfiles(): Promise<
  AgentRouteProfileSummary[]
> {
  return invokeTauri<AgentRouteProfileSummary[]>("list_agent_route_profiles");
}

export async function readAgentRouteProfile(
  id: string,
): Promise<AgentRouteProfile> {
  return invokeTauri<AgentRouteProfile>("read_agent_route_profile", { id });
}

export async function saveAgentRouteProfile(input: {
  id: string;
  name: string;
  document: AgentRouteProfileDocument;
  expectedVersion: number | null;
}): Promise<AgentRouteProfile> {
  return invokeTauri<AgentRouteProfile>("save_agent_route_profile", { input });
}

export async function testAgentRouteCandidate(input: {
  profileId: string;
  candidateId: string;
  expectedProfileVersion: number;
  expectedProfileDocumentHash: string;
  confirmHosted: boolean;
}): Promise<AgentRouteCandidateTestReceipt> {
  return invokeTauri<AgentRouteCandidateTestReceipt>(
    "test_agent_route_candidate",
    input,
  );
}

export async function listAgentRouteThroughputSummaries(
  profileId: string,
  profileVersion: number,
): Promise<AgentRouteThroughputGroupSummary[]> {
  return invokeTauri<AgentRouteThroughputGroupSummary[]>(
    "list_agent_route_throughput_summaries",
    { profileId, profileVersion },
  );
}

export async function listAgentTaskFitReports(): Promise<
  AgentTaskFitEvidenceSummary[]
> {
  return invokeTauri<AgentTaskFitEvidenceSummary[]>(
    "list_agent_task_fit_reports",
  );
}

export async function previewAgentTaskFitReport(
  fileBytes: number[],
): Promise<AgentTaskFitEvidenceSummary> {
  return invokeTauri<AgentTaskFitEvidenceSummary>(
    "preview_agent_task_fit_report",
    { fileBytes },
  );
}

export async function importAgentTaskFitReport(input: {
  fileBytes: number[];
  expectedReportSha256: string;
}): Promise<AgentTaskFitEvidenceSummary> {
  return invokeTauri<AgentTaskFitEvidenceSummary>(
    "import_agent_task_fit_report",
    input,
  );
}

export async function attestAgentTaskFitReport(
  reportSha256: string,
): Promise<AgentTaskFitLocalAttestation> {
  return invokeTauri<AgentTaskFitLocalAttestation>(
    "attest_agent_task_fit_report",
    { reportSha256 },
  );
}

export async function attestAgentTaskFitReportForRoute(input: {
  reportSha256: string;
  profileId: string;
  candidateId: string;
}): Promise<AgentTaskFitRouteAttestation> {
  return invokeTauri<AgentTaskFitRouteAttestation>(
    "attest_agent_task_fit_report_for_route",
    input,
  );
}
