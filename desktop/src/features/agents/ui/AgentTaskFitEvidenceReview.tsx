import * as React from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import {
  attestAgentTaskFitReport,
  attestAgentTaskFitReportForRoute,
  importAgentTaskFitReport,
  listAgentRouteProfiles,
  listAgentTaskFitReports,
  previewAgentTaskFitReport,
  readAgentRouteProfile,
  type AgentTaskFitEvidenceSummary,
} from "@/shared/api/tauriAgentRouteProfiles";
import { Button } from "@/shared/ui/button";

const MAX_REPORT_BYTES = 1024 * 1024;
const REPORTS_KEY = ["agent-task-fit-reports"] as const;

type ReviewedReport = {
  fileBytes: number[];
  summary: AgentTaskFitEvidenceSummary;
};

function percent(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}

function ReportDetails({ report }: { report: AgentTaskFitEvidenceSummary }) {
  return (
    <details className="mt-3 rounded-md border border-border/60 px-3 py-2">
      <summary className="cursor-pointer text-sm font-medium">
        Evaluation identity
      </summary>
      <dl className="mt-3 grid gap-2 break-all text-xs sm:grid-cols-[10rem_1fr]">
        <dt className="text-muted-foreground">Report SHA-256</dt>
        <dd>{report.reportSha256}</dd>
        <dt className="text-muted-foreground">Case set SHA-256</dt>
        <dd>{report.caseSetSha256}</dd>
        <dt className="text-muted-foreground">Manifest SHA-256</dt>
        <dd>{report.manifestSha256}</dd>
        <dt className="text-muted-foreground">Prompt SHA-256</dt>
        <dd>{report.promptSha256}</dd>
        <dt className="text-muted-foreground">Endpoint config SHA-256</dt>
        <dd>{report.endpointConfigSha256}</dd>
        <dt className="text-muted-foreground">Runtime binaries</dt>
        <dd>
          {Object.entries(report.runtimeBinarySha256).map(([name, hash]) => (
            <div key={name}>
              {name}: {hash}
            </div>
          ))}
        </dd>
        <dt className="text-muted-foreground">Model identity observed</dt>
        <dd>
          {report.modelIdentityObserved ? "Yes" : "No; configured ID only"}
        </dd>
        <dt className="text-muted-foreground">Generation settings</dt>
        <dd>{JSON.stringify(report.generation)}</dd>
      </dl>
    </details>
  );
}

function ReportCard({ report }: { report: AgentTaskFitEvidenceSummary }) {
  const queryClient = useQueryClient();
  const [profileId, setProfileId] = React.useState("");
  const [candidateId, setCandidateId] = React.useState("");
  const profilesQuery = useQuery({
    queryKey: ["agent-route-profiles"],
    queryFn: listAgentRouteProfiles,
  });
  const profileQuery = useQuery({
    queryKey: ["agent-route-profiles", profileId],
    queryFn: () => readAgentRouteProfile(profileId),
    enabled: profileId !== "",
  });
  const attestMutation = useMutation({
    mutationFn: attestAgentTaskFitReport,
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: REPORTS_KEY });
    },
  });
  const routeAttestMutation = useMutation({
    mutationFn: attestAgentTaskFitReportForRoute,
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: REPORTS_KEY });
    },
  });
  const profile = profileQuery.data;
  const candidate = profile?.document.candidates.find(
    (item) => item.id === candidateId,
  );
  const policy = profile?.document.task_fit_policy;
  const matchesPolicy =
    policy?.taskClass === report.taskClass &&
    policy.taskClassTaxonomyVersion === report.taskClassTaxonomyVersion &&
    policy.evaluationPolicyVersion === report.evaluationPolicyVersion;
  const matchesCandidate =
    candidate?.provider === report.providerId &&
    candidate.model === report.modelId;
  const routeAttestation = (report.routeAttestations ?? []).find(
    (item) =>
      item.profileId === profile?.id &&
      item.profileVersion === profile.version &&
      item.profileHash === profile.documentHash &&
      item.candidateId === candidateId,
  );
  const routeSelectionStatus = profilesQuery.isLoading
    ? "Loading saved route profiles…"
    : profilesQuery.isError
      ? null
      : !profileId
        ? profilesQuery.data?.length
          ? "Choose a saved route profile before binding this report."
          : "Create a route profile before binding this report."
        : profileQuery.isLoading
          ? "Loading the selected route profile…"
          : profileQuery.isError
            ? null
            : !profile
              ? "The selected route profile is unavailable. Choose another profile."
              : null;

  return (
    <article className="rounded-lg border border-border/70 bg-background p-3">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h4 className="font-medium">{report.taskClass}</h4>
          <p className="text-sm text-muted-foreground">
            {report.providerId} / {report.modelId}
            {report.modelIdentityObserved
              ? " · model ID observed"
              : " · model ID not observed"}
          </p>
        </div>
        {report.localAttestation ? (
          <span className="rounded-full border border-emerald-600/40 px-2 py-0.5 text-xs text-emerald-700 dark:text-emerald-300">
            Locally attested
          </span>
        ) : (
          <span className="rounded-full border border-amber-500/40 px-2 py-0.5 text-xs text-amber-700 dark:text-amber-300">
            Unverified report
          </span>
        )}
      </div>
      <p className="mt-2 text-sm">
        {report.taskSuccessCount}/{report.taskCount} distinct tasks passed all
        repeats · Wilson lower bound {percent(report.wilsonLowerBound95)}
      </p>
      <p className="text-xs text-muted-foreground">
        Finished {new Date(report.finishedAt).toLocaleString()} ·{" "}
        {report.dataset} · job {report.jobId}
      </p>
      {report.localAttestation ? (
        <p className="mt-2 break-all text-xs text-muted-foreground">
          Reviewed by Buzz identity {report.localAttestation.publicKey} · event{" "}
          {report.localAttestation.eventId}
        </p>
      ) : (
        <div className="mt-2 flex flex-wrap items-center gap-2">
          <p className="basis-full text-xs text-muted-foreground">
            Signs this review with your current Buzz identity. The signature
            stays on this device and is never sent to a relay.
          </p>
          <Button
            disabled={attestMutation.isPending}
            onClick={() => attestMutation.mutate(report.reportSha256)}
            size="sm"
            variant="outline"
          >
            {attestMutation.isPending
              ? "Signing review…"
              : "Attest local review"}
          </Button>
          {attestMutation.isError ? (
            <span className="text-xs text-destructive" role="alert">
              {attestMutation.error instanceof Error
                ? attestMutation.error.message
                : String(attestMutation.error)}
            </span>
          ) : null}
        </div>
      )}
      <div className="mt-3 space-y-2 rounded-md border border-border/60 p-3">
        <p className="text-sm font-medium">Associate with a saved route</p>
        <p className="text-xs text-muted-foreground">
          This signs your local association between this report and one exact
          route candidate. It does not authenticate the benchmark producer.
        </p>
        {routeAttestation ? (
          <p className="break-all text-xs text-emerald-700 dark:text-emerald-300">
            Bound to {profile?.name} / {candidateId} · signer{" "}
            {routeAttestation.publicKey} · event {routeAttestation.eventId}
          </p>
        ) : (
          <div className="grid gap-2 sm:grid-cols-2">
            <label className="space-y-1 text-xs font-medium">
              Route profile
              <select
                aria-label="Task-fit route profile"
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
                disabled={
                  profilesQuery.isLoading || routeAttestMutation.isPending
                }
                onChange={(event) => {
                  setProfileId(event.target.value);
                  setCandidateId("");
                }}
                value={profileId}
              >
                <option value="">Choose a saved profile</option>
                {(profilesQuery.data ?? []).map((item) => (
                  <option key={item.id} value={item.id}>
                    {item.name} · v{item.version}
                  </option>
                ))}
              </select>
            </label>
            <label className="space-y-1 text-xs font-medium">
              Candidate
              <select
                aria-label="Task-fit route candidate"
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
                disabled={!profile || routeAttestMutation.isPending}
                onChange={(event) => setCandidateId(event.target.value)}
                value={candidateId}
              >
                <option value="">Choose a candidate</option>
                {(profile?.document.candidates ?? []).map((item) => (
                  <option key={item.id} value={item.id}>
                    {item.id} · {item.provider}/{item.model}
                  </option>
                ))}
              </select>
            </label>
            {routeSelectionStatus ? (
              <p
                className="text-xs text-muted-foreground sm:col-span-2"
                role="status"
              >
                {routeSelectionStatus}
              </p>
            ) : null}
            {profileId && !profileQuery.isLoading && !profileQuery.isError ? (
              <p
                className={`text-xs sm:col-span-2 ${
                  matchesPolicy && matchesCandidate
                    ? "text-muted-foreground"
                    : "text-amber-700 dark:text-amber-300"
                }`}
                role="status"
              >
                {!policy
                  ? "Enable task-fit policy on this profile first."
                  : !matchesPolicy
                    ? "This report does not match the profile task-fit policy."
                    : !candidate
                      ? "Choose a candidate to check the report identity."
                      : !matchesCandidate
                        ? "The report provider and model must match the selected candidate."
                        : policy?.requireObservedModelIdentity &&
                            !report.modelIdentityObserved
                          ? "The report did not observe the model ID, so this profile's gate will remain closed."
                          : "Identity matches. The runtime still checks score, sample count, and freshness."}
              </p>
            ) : null}
            {profileQuery.isError || profilesQuery.isError ? (
              <p
                className="text-xs text-destructive sm:col-span-2"
                role="alert"
              >
                Could not load saved route profiles.
              </p>
            ) : null}
            <div className="flex flex-wrap items-center gap-2 sm:col-span-2">
              <Button
                disabled={
                  !profile ||
                  !candidate ||
                  !matchesPolicy ||
                  !matchesCandidate ||
                  routeAttestMutation.isPending
                }
                onClick={() =>
                  routeAttestMutation.mutate({
                    reportSha256: report.reportSha256,
                    profileId,
                    candidateId,
                  })
                }
                size="sm"
                variant="outline"
              >
                {routeAttestMutation.isPending
                  ? "Signing route binding…"
                  : "Bind report to route"}
              </Button>
              {routeAttestMutation.isError ? (
                <span className="text-xs text-destructive" role="alert">
                  {routeAttestMutation.error instanceof Error
                    ? routeAttestMutation.error.message
                    : String(routeAttestMutation.error)}
                </span>
              ) : null}
            </div>
          </div>
        )}
      </div>
      <ReportDetails report={report} />
    </article>
  );
}

export function AgentTaskFitEvidenceReview() {
  const queryClient = useQueryClient();
  const fileInputRef = React.useRef<HTMLInputElement>(null);
  const [reviewedReport, setReviewedReport] =
    React.useState<ReviewedReport | null>(null);
  const [fileError, setFileError] = React.useState<string | null>(null);
  const [notice, setNotice] = React.useState<string | null>(null);

  const reportsQuery = useQuery({
    queryKey: REPORTS_KEY,
    queryFn: listAgentTaskFitReports,
  });
  const previewMutation = useMutation({
    mutationFn: previewAgentTaskFitReport,
    onSuccess: (summary, fileBytes) => {
      setReviewedReport({ fileBytes, summary });
      setFileError(null);
      setNotice(null);
    },
    onError: (error) => {
      setReviewedReport(null);
      setFileError(error instanceof Error ? error.message : String(error));
    },
  });
  const importMutation = useMutation({
    mutationFn: importAgentTaskFitReport,
    onSuccess: async (summary) => {
      setReviewedReport(null);
      setFileError(null);
      setNotice(`Imported report for ${summary.taskClass}.`);
      await queryClient.invalidateQueries({ queryKey: REPORTS_KEY });
    },
    onError: (error) => {
      setFileError(error instanceof Error ? error.message : String(error));
    },
  });

  function readSelectedFile(file: File, input: HTMLInputElement) {
    setReviewedReport(null);
    setFileError(null);
    setNotice(null);
    if (file.size > MAX_REPORT_BYTES) {
      setFileError("Task-fit reports must be 1 MiB or smaller.");
      input.value = "";
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      if (reader.result === null || typeof reader.result === "string") {
        setFileError("The selected report could not be read.");
        return;
      }
      previewMutation.mutate(Array.from(new Uint8Array(reader.result)));
    };
    reader.onerror = () =>
      setFileError("The selected report could not be read.");
    reader.readAsArrayBuffer(file);
    input.value = "";
  }

  return (
    <section
      className="mt-4 space-y-4"
      data-testid="agent-task-fit-evidence-review"
    >
      <div className="rounded-lg border border-amber-500/40 bg-amber-500/5 p-3 text-sm">
        Reports are checked for internal consistency, but Harbor and its source
        artifacts are not authenticated. “Attest local review” signs only that
        the current Buzz identity reviewed this exact report; it does not verify
        the benchmark or enable routing by itself.
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <Button
          onClick={() => fileInputRef.current?.click()}
          size="sm"
          variant="outline"
          disabled={previewMutation.isPending || importMutation.isPending}
        >
          Choose Harbor report
        </Button>
        <input
          accept=".json,application/json"
          className="hidden"
          data-testid="task-fit-report-input"
          ref={fileInputRef}
          type="file"
          onChange={(event) => {
            const file = event.target.files?.[0];
            if (file) readSelectedFile(file, event.target);
          }}
        />
        {previewMutation.isPending ? (
          <span className="text-sm text-muted-foreground" role="status">
            Validating report…
          </span>
        ) : null}
        {fileError ? (
          <span className="text-sm text-destructive" role="alert">
            {fileError}
          </span>
        ) : null}
        {notice ? (
          <span className="text-sm text-muted-foreground" role="status">
            {notice}
          </span>
        ) : null}
      </div>

      {reviewedReport ? (
        <article className="rounded-lg border border-primary/30 bg-primary/5 p-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div>
              <h4 className="font-semibold">Review before importing</h4>
              <p className="text-sm text-muted-foreground">
                {reviewedReport.summary.taskClass} ·{" "}
                {reviewedReport.summary.providerId} /{" "}
                {reviewedReport.summary.modelId}
              </p>
            </div>
            <Button
              disabled={importMutation.isPending}
              onClick={() =>
                importMutation.mutate({
                  fileBytes: reviewedReport.fileBytes,
                  expectedReportSha256: reviewedReport.summary.reportSha256,
                })
              }
              size="sm"
            >
              {importMutation.isPending
                ? "Importing…"
                : "Import reviewed report"}
            </Button>
          </div>
          <p className="mt-2 text-sm">
            {reviewedReport.summary.taskSuccessCount}/
            {reviewedReport.summary.taskCount} distinct tasks passed all repeats
            · Wilson lower bound{" "}
            {percent(reviewedReport.summary.wilsonLowerBound95)}
          </p>
          <p className="text-xs text-muted-foreground">
            {reviewedReport.summary.taskClassTaxonomyVersion} ·{" "}
            {reviewedReport.summary.evaluationPolicyVersion}
          </p>
          <ReportDetails report={reviewedReport.summary} />
        </article>
      ) : null}

      {reportsQuery.isError ? (
        <p className="text-sm text-destructive" role="alert">
          {reportsQuery.error instanceof Error
            ? reportsQuery.error.message
            : String(reportsQuery.error)}
        </p>
      ) : null}
      {reportsQuery.data?.length ? (
        <div className="space-y-2">
          <h4 className="text-sm font-semibold">Imported local reports</h4>
          {reportsQuery.data.map((report) => (
            <ReportCard key={report.reportSha256} report={report} />
          ))}
        </div>
      ) : reportsQuery.isLoading ? (
        <p className="text-sm text-muted-foreground" role="status">
          Loading imported reports…
        </p>
      ) : (
        <p className="text-sm text-muted-foreground">
          No task-fit reports imported.
        </p>
      )}
    </section>
  );
}
