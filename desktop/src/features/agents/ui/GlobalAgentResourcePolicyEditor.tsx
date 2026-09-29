import { AlertCircle, Check, Loader } from "lucide-react";
import * as React from "react";

import {
  getDeviceMemorySnapshot,
  getGlobalAgentResourcePolicy,
  setGlobalAgentResourcePolicy,
} from "@/shared/api/tauriGlobalAgentResourcePolicy";
import type {
  DeviceMemorySnapshot,
  GlobalAgentResourcePolicy,
} from "@/shared/api/types";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";

const MAX_RUNNING_AGENTS = 128;
const MIB = 1024 * 1024;
const MIN_MEMORY_RESERVE_MIB = 512;
const MAX_MEMORY_RESERVE_MIB = 1024 * 1024;
type LoadState = "loading" | "ready" | "error";

function displayedLimit(value: number | null) {
  return value === null ? "" : String(value);
}

function validDraft(value: string) {
  if (!value.trim()) return true;
  const parsed = Number(value);
  return (
    Number.isSafeInteger(parsed) && parsed >= 1 && parsed <= MAX_RUNNING_AGENTS
  );
}

function displayedMemoryReserve(bytes: number | null) {
  return bytes === null ? "" : String(bytes / MIB);
}

function parseMemoryReserve(value: string): number | null | undefined {
  if (!value.trim()) return null;
  const parsed = Number(value);
  if (
    !Number.isSafeInteger(parsed) ||
    parsed < MIN_MEMORY_RESERVE_MIB ||
    parsed > MAX_MEMORY_RESERVE_MIB
  ) {
    return undefined;
  }
  return parsed * MIB;
}

function formatMemory(bytes: number) {
  const gib = bytes / (1024 * MIB);
  return gib >= 1 ? `${gib.toFixed(1)} GiB` : `${Math.round(bytes / MIB)} MiB`;
}

export function GlobalAgentResourcePolicyEditor() {
  const [policy, setPolicy] = React.useState<GlobalAgentResourcePolicy | null>(
    null,
  );
  const [draft, setDraft] = React.useState("");
  const [memoryReserveDraft, setMemoryReserveDraft] = React.useState("");
  const [memorySnapshot, setMemorySnapshot] =
    React.useState<DeviceMemorySnapshot | null>(null);
  const [memoryLoading, setMemoryLoading] = React.useState(false);
  const [loadState, setLoadState] = React.useState<LoadState>("loading");
  const [saving, setSaving] = React.useState(false);
  const [saveError, setSaveError] = React.useState<string | null>(null);
  const [saved, setSaved] = React.useState(false);

  const load = React.useCallback(async () => {
    setLoadState("loading");
    try {
      const [current, memory] = await Promise.all([
        getGlobalAgentResourcePolicy(),
        getDeviceMemorySnapshot().catch(() => null),
      ]);
      setPolicy(current);
      setDraft(displayedLimit(current.maxRunningAgents));
      setMemoryReserveDraft(
        displayedMemoryReserve(current.minAvailableMemoryBytes),
      );
      setMemorySnapshot(memory);
      setLoadState("ready");
    } catch {
      setLoadState("error");
    }
  }, []);

  React.useEffect(() => {
    void load();
  }, [load]);

  const memoryReserveBytes = parseMemoryReserve(memoryReserveDraft);
  const dirty =
    policy !== null &&
    (draft !== displayedLimit(policy.maxRunningAgents) ||
      memoryReserveDraft !==
        displayedMemoryReserve(policy.minAvailableMemoryBytes));
  const valid = validDraft(draft) && memoryReserveBytes !== undefined;

  async function refreshMemory() {
    setMemoryLoading(true);
    try {
      setMemorySnapshot(await getDeviceMemorySnapshot());
    } catch {
      setMemorySnapshot(null);
    } finally {
      setMemoryLoading(false);
    }
  }

  async function save() {
    if (!valid || !dirty || saving) return;
    setSaving(true);
    setSaveError(null);
    setSaved(false);
    const next: GlobalAgentResourcePolicy = {
      schemaVersion: 1,
      maxRunningAgents: draft.trim() ? Number(draft) : null,
      minAvailableMemoryBytes: memoryReserveBytes ?? null,
    };
    try {
      const canonical = await setGlobalAgentResourcePolicy(next);
      setPolicy(canonical);
      setDraft(displayedLimit(canonical.maxRunningAgents));
      setMemoryReserveDraft(
        displayedMemoryReserve(canonical.minAvailableMemoryBytes),
      );
      setSaved(true);
    } catch (error) {
      setSaveError(
        typeof error === "string" ? error : "Couldn't save the limit.",
      );
    } finally {
      setSaving(false);
    }
  }

  return (
    <section
      aria-labelledby="global-agent-resource-policy-title"
      className="space-y-3 rounded-lg border border-border/60 bg-muted/20 p-3"
    >
      <div className="space-y-1">
        <h3
          className="text-sm font-medium text-foreground"
          id="global-agent-resource-policy-title"
        >
          Global agent resource policy
        </h3>
        <p className="text-xs leading-relaxed text-muted-foreground">
          Set a hard cap on Buzz-managed local agents and critic reviewers
          running at once. You can also keep a minimum amount of system RAM free
          before another managed agent or critic reviewer starts. Both checks
          affect new starts only; neither stops active work. RAM is a
          point-in-time reading and excludes GPU memory and per-agent memory
          estimates. Buzz serializes its own starts around the reading; other
          apps can still use RAM after the check.
        </p>
      </div>

      {loadState === "loading" ? (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader aria-hidden="true" className="size-3.5 animate-spin" />
          Loading resource policy…
        </div>
      ) : loadState === "error" ? (
        <div className="flex flex-wrap items-center gap-2 text-xs text-destructive">
          <AlertCircle aria-hidden="true" className="size-3.5" />
          Couldn't load the resource policy.
          <Button onClick={() => void load()} size="sm" variant="outline">
            Retry
          </Button>
        </div>
      ) : (
        <>
          <label
            className="block max-w-xs space-y-1 text-xs"
            htmlFor="global-max-running-agents"
          >
            <span>
              Maximum concurrent local agents (1–{MAX_RUNNING_AGENTS})
            </span>
            <Input
              id="global-max-running-agents"
              max={MAX_RUNNING_AGENTS}
              min={1}
              onChange={(event) => {
                setDraft(event.target.value);
                setSaved(false);
                setSaveError(null);
              }}
              step={1}
              type="number"
              value={draft}
            />
          </label>
          <div
            className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground"
            aria-live="polite"
          >
            <p>
              {memorySnapshot
                ? `Available system RAM at last check: ${formatMemory(memorySnapshot.availableMemoryBytes)} of ${formatMemory(memorySnapshot.totalMemoryBytes)}.`
                : "Current system RAM reading is unavailable."}
            </p>
            <Button
              disabled={memoryLoading}
              onClick={() => void refreshMemory()}
              size="sm"
              variant="outline"
            >
              {memoryLoading ? "Checking…" : "Refresh reading"}
            </Button>
          </div>
          <label
            className="block max-w-xs space-y-1 text-xs"
            htmlFor="global-min-available-memory-mib"
          >
            <span>
              Minimum free system RAM before a new local process (MiB)
            </span>
            <Input
              id="global-min-available-memory-mib"
              max={MAX_MEMORY_RESERVE_MIB}
              min={MIN_MEMORY_RESERVE_MIB}
              onChange={(event) => {
                setMemoryReserveDraft(event.target.value);
                setSaved(false);
                setSaveError(null);
              }}
              placeholder="No reserve"
              step={256}
              type="number"
              value={memoryReserveDraft}
            />
          </label>
          {memorySnapshot &&
          memoryReserveBytes !== null &&
          memoryReserveBytes !== undefined &&
          memorySnapshot.availableMemoryBytes < memoryReserveBytes ? (
            <p
              className="text-xs text-amber-700 dark:text-amber-300"
              role="status"
            >
              The last RAM reading is below this reserve. New agent starts will
              be blocked until available RAM recovers or you lower the reserve.
            </p>
          ) : null}
          {!valid ? (
            <p className="text-xs text-destructive" role="alert">
              Enter a whole agent count from 1 to {MAX_RUNNING_AGENTS} and a
              memory reserve from {MIN_MEMORY_RESERVE_MIB} to{" "}
              {MAX_MEMORY_RESERVE_MIB} MiB, or clear either field to disable
              that limit.
            </p>
          ) : null}
          {saveError ? (
            <p
              className="flex items-center gap-1.5 text-xs text-destructive"
              role="alert"
            >
              <AlertCircle aria-hidden="true" className="size-3.5" />
              {saveError}
            </p>
          ) : null}
          {saved ? (
            <p
              aria-live="polite"
              className="flex items-center gap-1.5 text-xs text-green-600 dark:text-green-400"
            >
              <Check aria-hidden="true" className="size-3.5" />
              Resource limit saved.
            </p>
          ) : null}
          <div className="flex justify-end">
            <Button
              disabled={!dirty || !valid || saving}
              onClick={() => void save()}
              size="sm"
            >
              {saving ? (
                <Loader
                  aria-hidden="true"
                  className="mr-1.5 size-3.5 animate-spin"
                />
              ) : null}
              Save limit
            </Button>
          </div>
        </>
      )}
    </section>
  );
}
