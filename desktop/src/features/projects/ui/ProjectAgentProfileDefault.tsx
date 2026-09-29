import * as React from "react";
import { useQuery } from "@tanstack/react-query";

import { AgentArchetypeSelect } from "@/features/agents/ui/AgentArchetypeSelect";
import { PersonaDropdownField } from "@/features/agents/ui/PersonaDropdownField";
import { useAgentArchetypesQuery } from "@/features/agents/hooks";
import { listAgentRouteProfiles } from "@/shared/api/tauriAgentRouteProfiles";
import {
  readProjectAgentProfileDefault,
  readProjectAgentRouteProfileDefault,
  readProjectAgentResourceDefaults,
  parseProjectAgentResourceDefaultsDraft,
  toProjectAgentResourceDefaultsDraft,
  writeProjectAgentProfileDefault,
  writeProjectAgentRouteProfileDefault,
  writeProjectAgentResourceDefaults,
  type ProjectAgentResourceDefaults,
  type ProjectAgentResourceDefaultsDraft,
} from "@/features/projects/projectAgentProfileDefault";
import { ProjectAgentResourceDefaultsFields } from "@/features/projects/ui/ProjectAgentResourceDefaultsFields";
import { Button } from "@/shared/ui/button";

export function ProjectAgentProfileDefault({
  channelId,
  identityPubkey,
  relayUrl,
}: {
  channelId: string;
  identityPubkey?: string;
  relayUrl: string;
}) {
  const scopeReady = Boolean(identityPubkey && channelId && relayUrl);
  const archetypesQuery = useAgentArchetypesQuery();
  const routeProfilesQuery = useQuery({
    queryKey: ["agent-route-profiles"],
    queryFn: listAgentRouteProfiles,
    enabled: scopeReady,
  });
  const [profileId, setProfileId] = React.useState("");
  const [routeProfileId, setRouteProfileId] = React.useState("");
  const [saveError, setSaveError] = React.useState(false);
  const [routeSaveError, setRouteSaveError] = React.useState(false);
  const [resourceDefaults, setResourceDefaults] =
    React.useState<ProjectAgentResourceDefaults | null>(null);
  const [resourceDraft, setResourceDraft] =
    React.useState<ProjectAgentResourceDefaultsDraft>(
      toProjectAgentResourceDefaultsDraft(null),
    );
  const [resourceSaveMessage, setResourceSaveMessage] = React.useState("");

  React.useEffect(() => {
    if (!identityPubkey || !channelId || !relayUrl) {
      setProfileId("");
      return;
    }
    setProfileId(
      readProjectAgentProfileDefault(relayUrl, identityPubkey, channelId) ?? "",
    );
    setRouteProfileId(
      readProjectAgentRouteProfileDefault(
        relayUrl,
        identityPubkey,
        channelId,
      ) ?? "",
    );
    const nextResourceDefaults = readProjectAgentResourceDefaults(
      relayUrl,
      identityPubkey,
      channelId,
    );
    setResourceDefaults(nextResourceDefaults);
    setResourceDraft(toProjectAgentResourceDefaultsDraft(nextResourceDefaults));
    setSaveError(false);
    setRouteSaveError(false);
    setResourceSaveMessage("");
  }, [channelId, identityPubkey, relayUrl]);

  function handleChange(nextProfileId: string) {
    if (!identityPubkey) return;
    const saved = writeProjectAgentProfileDefault(
      relayUrl,
      identityPubkey,
      channelId,
      nextProfileId || null,
    );
    if (saved) setProfileId(nextProfileId);
    setSaveError(!saved);
  }

  function handleRouteProfileChange(nextProfileId: string) {
    if (!identityPubkey) return;
    const saved = writeProjectAgentRouteProfileDefault(
      relayUrl,
      identityPubkey,
      channelId,
      nextProfileId || null,
    );
    if (saved) setRouteProfileId(nextProfileId);
    setRouteSaveError(!saved);
  }

  function handleSaveResourceDefaults(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!identityPubkey) return;
    const parsed = parseProjectAgentResourceDefaultsDraft(resourceDraft);
    if (!parsed) {
      setResourceSaveMessage(
        "Enter whole numbers of 1 or more. Parallelism can be at most 32.",
      );
      return;
    }
    const normalized = Object.keys(parsed).length > 0 ? parsed : null;
    const saved = writeProjectAgentResourceDefaults(
      relayUrl,
      identityPubkey,
      channelId,
      normalized,
    );
    if (saved) {
      setResourceDefaults(normalized);
      setResourceDraft(toProjectAgentResourceDefaultsDraft(normalized));
      setResourceSaveMessage(normalized ? "Saved locally" : "Limits cleared");
    } else {
      setResourceSaveMessage("Could not save these local limits.");
    }
  }

  const resourceSummary = resourceDefaults
    ? [
        resourceDefaults.parallelism === undefined
          ? null
          : `parallelism ${resourceDefaults.parallelism}`,
        resourceDefaults.idleTimeoutSeconds === undefined
          ? null
          : `idle ${resourceDefaults.idleTimeoutSeconds}s`,
        resourceDefaults.maxTurnDurationSeconds === undefined
          ? null
          : `turn ${resourceDefaults.maxTurnDurationSeconds}s`,
      ]
        .filter(Boolean)
        .join(" · ")
    : "Not set";

  if (!scopeReady) return null;
  const savedProfileMissing =
    Boolean(profileId) &&
    !archetypesQuery.isLoading &&
    !archetypesQuery.isError &&
    !(archetypesQuery.data ?? []).some((profile) => profile.id === profileId);

  const selectedName = (archetypesQuery.data ?? []).find(
    (profile) => profile.id === profileId,
  )?.name;
  const routeProfiles = routeProfilesQuery.data ?? [];
  const selectedRouteProfile = routeProfiles.find(
    (profile) => profile.id === routeProfileId,
  );
  const savedRouteProfileMissing =
    Boolean(routeProfileId) &&
    routeProfilesQuery.isSuccess &&
    !selectedRouteProfile;
  const routeProfileOptions = [
    { label: "Off · use configured provider/model", value: "" },
    ...routeProfiles.map((profile) => ({
      label: `${profile.name} · ${profile.dataPolicy} · v${profile.version}`,
      value: profile.id,
    })),
    ...(routeProfileId && !selectedRouteProfile
      ? [
          {
            label: routeProfilesQuery.isLoading
              ? `${routeProfileId} · loading saved profile`
              : `${routeProfileId} · profile unavailable`,
            value: routeProfileId,
          },
        ]
      : []),
  ];
  return (
    <section className="rounded-lg border border-sidebar-border/70 bg-sidebar px-3 py-2.5">
      <details>
        <summary className="flex cursor-pointer list-none items-center justify-between gap-2 text-sm font-medium text-sidebar-foreground [&::-webkit-details-marker]:hidden">
          <span>New agent style</span>
          <span className="truncate text-xs font-normal text-muted-foreground">
            {selectedName ?? (profileId ? "Unavailable" : "Not set")}
          </span>
        </summary>
        <div className="space-y-2 pt-3">
          <AgentArchetypeSelect
            disabled={false}
            id="project-agent-profile-default"
            onChange={handleChange}
            value={profileId}
          />
          <p className="text-xs leading-relaxed text-muted-foreground">
            Default for agents added to this project from this device. Existing
            agents keep their saved style. This setting stays local to your Buzz
            profile and relay.
          </p>
          <p aria-live="polite" className="text-xs text-muted-foreground">
            {saveError
              ? "Could not save this local default. Check local storage and try again."
              : savedProfileMissing
                ? "This saved style is no longer in Buzz's catalog. Choose another style or clear it."
                : profileId
                  ? "Saved locally"
                  : "No project default"}
          </p>
          <details className="rounded-lg border border-sidebar-border/60 px-2.5 py-2">
            <summary className="flex cursor-pointer list-none items-center justify-between gap-2 text-xs font-medium [&::-webkit-details-marker]:hidden">
              <span>Provider route for new local Buzz Agents</span>
              <span className="truncate font-normal text-muted-foreground">
                {selectedRouteProfile?.name ??
                  (routeProfileId ? "Unavailable" : "Not set")}
              </span>
            </summary>
            <div className="space-y-2 pt-2.5">
              <p
                className="text-xs leading-relaxed text-muted-foreground"
                id="project-agent-route-profile-help"
              >
                Pins this saved route profile to new local Buzz Agent instances
                added from this project home. Other runtimes and hosted agents
                ignore it; existing agents keep their current route.
              </p>
              <PersonaDropdownField
                ariaDescribedBy="project-agent-route-profile-help"
                disabled={routeProfilesQuery.isLoading}
                id="project-agent-route-profile-default"
                onValueChange={handleRouteProfileChange}
                options={routeProfileOptions}
                placeholder="Choose a route profile"
                value={routeProfileId}
              />
              <p aria-live="polite" className="text-xs text-muted-foreground">
                {routeSaveError
                  ? "Could not save this local route default. Check local storage and try again."
                  : routeProfilesQuery.isError
                    ? "Could not load saved route profiles."
                    : savedRouteProfileMissing
                      ? "This saved route profile is unavailable. Choose another profile or turn routing off."
                      : routeProfileId
                        ? "Saved locally. The route profile is pinned when a new local Buzz Agent is created."
                        : "No project route default. Existing and future agents keep their configured provider/model."}
              </p>
            </div>
          </details>
          {profileId ? (
            <Button
              className="h-7 px-2 text-xs"
              onClick={() => handleChange("")}
              size="sm"
              type="button"
              variant="ghost"
            >
              Clear default
            </Button>
          ) : null}
          <details className="rounded-lg border border-sidebar-border/60 px-2.5 py-2">
            <summary className="flex cursor-pointer list-none items-center justify-between gap-2 text-xs font-medium [&::-webkit-details-marker]:hidden">
              <span>Limits for new agents</span>
              <span className="truncate font-normal text-muted-foreground">
                {resourceSummary}
              </span>
            </summary>
            <form
              className="space-y-2.5 pt-2.5"
              onSubmit={handleSaveResourceDefaults}
            >
              <p className="text-xs leading-relaxed text-muted-foreground">
                Per-agent starting defaults for this project. They are not
                shared project-wide caps. Blank fields use the style or runtime
                default. Buzz does not yet expose verified per-run token, spend,
                RAM, or VRAM measurements here.
              </p>
              <ProjectAgentResourceDefaultsFields
                idPrefix="project-agent-default"
                onChange={setResourceDraft}
                value={resourceDraft}
              />
              <p aria-live="polite" className="text-xs text-muted-foreground">
                {resourceSaveMessage ||
                  "Stored on this device for this relay and project."}
              </p>
              <Button className="h-7 px-2 text-xs" size="sm" type="submit">
                Save limits
              </Button>
            </form>
          </details>
        </div>
      </details>
    </section>
  );
}
