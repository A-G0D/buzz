import { ChevronDown, Plus } from "lucide-react";
import * as React from "react";

import { ChannelPermissionsSettings } from "@/features/channels/ui/ChannelPermissionsSettings";
import { AgentArchetypeSelect } from "@/features/agents/ui/AgentArchetypeSelect";
import { PersonaDropdownField } from "@/features/agents/ui/PersonaDropdownField";
import type { CreateProjectFormSettingsState } from "@/features/projects/ui/useCreateProjectFormSettings";
import { ProjectAgentResourceDefaultsFields } from "@/features/projects/ui/ProjectAgentResourceDefaultsFields";
import { ProjectResourcePreflight } from "@/features/projects/ui/ProjectResourcePreflight";
import { TemplateFormDialog } from "@/features/settings/ui/ChannelTemplatesSettingsCard";
import { Button } from "@/shared/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/shared/ui/dropdown-menu";
import { cn } from "@/shared/lib/cn";

const NONE_AGENT_VALUE = "__none__";
const NONE_TEAM_VALUE = "__no-team__";
const NO_TEMPLATE_VALUE = "__no-template__";

const SETTINGS_ROW_CLASS =
  "flex min-h-12 items-center justify-between gap-4 rounded-xl border border-input bg-background px-3 py-3";

export function CreateProjectFormSettings({
  agentPersonaId,
  disabled,
  executionProfileId,
  resourceDefaultsDraft,
  handleTemplateChange,
  handleTemplateCreated,
  personas,
  projectVisibility,
  routeProfileId,
  routeProfiles,
  routeProfilesError,
  routeProfilesLoading,
  runtimesAvailable,
  setAgentPersonaId,
  setExecutionProfileId,
  setRouteProfileId,
  setResourceDefaultsDraft,
  setChannelVisibility,
  setProjectVisibility,
  setTeamId,
  teamId,
  teams,
  templateId,
  templates,
  channelVisibility,
}: CreateProjectFormSettingsState & { disabled: boolean }) {
  const [isCreateTemplateOpen, setIsCreateTemplateOpen] = React.useState(false);
  const selectedPersona = personas.find(
    (persona) => persona.id === agentPersonaId,
  );
  const selectedTeam = teams.find((team) => team.id === teamId);
  const selectedTemplate = templates.find(
    (template) => template.id === templateId,
  );
  const selectedRouteProfile = routeProfiles.find(
    (profile) => profile.id === routeProfileId,
  );
  const routeProfileOptions = [
    { label: "Off · use configured provider/model", value: "" },
    ...routeProfiles.map((profile) => ({
      label: `${profile.name} · ${profile.dataPolicy} · v${profile.version}`,
      value: profile.id,
    })),
    ...(routeProfileId && !selectedRouteProfile
      ? [
          {
            label: `${routeProfileId} · unavailable`,
            value: routeProfileId,
          },
        ]
      : []),
  ];
  const listingLabel = projectVisibility === "unlisted" ? "Unlisted" : "Listed";
  const agentLabel = selectedPersona?.displayName ?? "None";
  const agentDisabled = disabled || (!runtimesAvailable && personas.length > 0);
  const teamDisabled = disabled || (!runtimesAvailable && teams.length > 0);

  return (
    <>
      <ChannelPermissionsSettings
        disabled={disabled}
        onVisibilityChange={setChannelVisibility}
        testIdPrefix="create-project-channel"
        visibility={channelVisibility}
      />

      <div className={cn(SETTINGS_ROW_CLASS, disabled && "opacity-50")}>
        <span className="text-sm font-medium text-foreground">
          Template
          <span className="ml-1 text-xs font-normal text-muted-foreground/50">
            Project home by default
          </span>
        </span>
        <DropdownMenu modal={false}>
          <DropdownMenuTrigger asChild>
            <Button
              aria-label={`Template: ${selectedTemplate?.name ?? "None"}`}
              className="-mr-2.5 ml-auto h-9 min-w-0 max-w-[60%] justify-end px-2.5 text-right text-sm font-medium text-foreground hover:bg-muted/50"
              data-testid="create-project-template"
              disabled={disabled}
              type="button"
              variant="ghost"
            >
              <span className="truncate text-right">
                {selectedTemplate?.name ?? "None"}
              </span>
              <ChevronDown className="size-4 shrink-0 text-muted-foreground/70" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuRadioGroup
              onValueChange={(value) =>
                handleTemplateChange(value === NO_TEMPLATE_VALUE ? "" : value)
              }
              value={templateId || NO_TEMPLATE_VALUE}
            >
              <DropdownMenuRadioItem value={NO_TEMPLATE_VALUE}>
                None
              </DropdownMenuRadioItem>
              {templates.map((template) => (
                <DropdownMenuRadioItem key={template.id} value={template.id}>
                  {template.name}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => setIsCreateTemplateOpen(true)}>
              <Plus className="size-4" />
              Create new channel template…
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <TemplateFormDialog
          onCreated={handleTemplateCreated}
          onOpenChange={setIsCreateTemplateOpen}
          open={isCreateTemplateOpen}
          template={null}
        />
      </div>

      <div className={cn(SETTINGS_ROW_CLASS, teamDisabled && "opacity-50")}>
        <span className="text-sm font-medium text-foreground">
          Team
          <span className="ml-1 text-xs font-normal text-muted-foreground/50">
            Optional
          </span>
        </span>
        <DropdownMenu modal={false}>
          <DropdownMenuTrigger asChild>
            <Button
              aria-label={`Team: ${selectedTeam?.name ?? "None"}`}
              className="-mr-2.5 ml-auto h-9 min-w-0 max-w-[60%] justify-end px-2.5 text-right text-sm font-medium text-foreground hover:bg-muted/50"
              data-testid="create-project-team"
              disabled={teamDisabled}
              type="button"
              variant="ghost"
            >
              <span className="truncate text-right">
                {selectedTeam?.name ?? "None"}
              </span>
              <ChevronDown className="size-4 shrink-0 text-muted-foreground/70" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuRadioGroup
              onValueChange={(value) =>
                setTeamId(value === NONE_TEAM_VALUE ? "" : value)
              }
              value={teamId || NONE_TEAM_VALUE}
            >
              <DropdownMenuRadioItem value={NONE_TEAM_VALUE}>
                None
              </DropdownMenuRadioItem>
              {teams.map((team) => (
                <DropdownMenuRadioItem key={team.id} value={team.id}>
                  {team.name}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <div className={cn(SETTINGS_ROW_CLASS, disabled && "opacity-50")}>
        <span className="text-sm font-medium text-foreground">
          Project list
        </span>
        <DropdownMenu modal={false}>
          <DropdownMenuTrigger asChild>
            <Button
              aria-label={`Project list: ${listingLabel}`}
              className="-mr-2.5 ml-auto h-9 w-fit justify-end px-2.5 text-right text-sm font-medium text-foreground hover:bg-muted/50"
              data-testid="create-project-listing"
              disabled={disabled}
              type="button"
              variant="ghost"
            >
              <span className="text-right">{listingLabel}</span>
              <ChevronDown className="size-4 shrink-0 text-muted-foreground/70" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            align="end"
            onCloseAutoFocus={(event) => event.preventDefault()}
            style={{
              minWidth: "var(--radix-dropdown-menu-trigger-width)",
            }}
          >
            <DropdownMenuRadioGroup
              onValueChange={(value) =>
                setProjectVisibility(
                  value === "unlisted" ? "unlisted" : "listed",
                )
              }
              value={projectVisibility}
            >
              <DropdownMenuRadioItem
                data-testid="create-project-listing-option-listed"
                value="listed"
              >
                Listed
              </DropdownMenuRadioItem>
              <DropdownMenuRadioItem
                data-testid="create-project-listing-option-unlisted"
                value="unlisted"
              >
                Unlisted
              </DropdownMenuRadioItem>
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <div className={cn(SETTINGS_ROW_CLASS, agentDisabled && "opacity-50")}>
        <span className="text-sm font-medium text-foreground">
          Coding agent
        </span>
        <DropdownMenu modal={false}>
          <DropdownMenuTrigger asChild>
            <Button
              aria-label={`Coding agent: ${agentLabel}`}
              className="-mr-2.5 ml-auto h-9 min-w-0 max-w-[60%] justify-end px-2.5 text-right text-sm font-medium text-foreground hover:bg-muted/50"
              data-testid="create-project-agent"
              disabled={agentDisabled}
              type="button"
              variant="ghost"
            >
              <span className="truncate text-right">{agentLabel}</span>
              <ChevronDown className="size-4 shrink-0 text-muted-foreground/70" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            align="end"
            onCloseAutoFocus={(event) => event.preventDefault()}
            style={{
              minWidth: "var(--radix-dropdown-menu-trigger-width)",
            }}
          >
            <DropdownMenuRadioGroup
              onValueChange={(value) =>
                setAgentPersonaId(value === NONE_AGENT_VALUE ? "" : value)
              }
              value={agentPersonaId || NONE_AGENT_VALUE}
            >
              <DropdownMenuRadioItem
                data-testid="create-project-agent-option-none"
                value={NONE_AGENT_VALUE}
              >
                None
              </DropdownMenuRadioItem>
              {personas.map((persona) => (
                <DropdownMenuRadioItem
                  data-testid={`create-project-agent-option-${persona.id}`}
                  key={persona.id}
                  value={persona.id}
                >
                  {persona.displayName}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {teamId || agentPersonaId ? (
        <div className="space-y-2 rounded-xl border border-input bg-background px-3 py-3">
          <AgentArchetypeSelect
            disabled={disabled}
            id="create-project-agent-archetype"
            onChange={setExecutionProfileId}
            value={executionProfileId}
          />
          <p className="text-xs leading-relaxed text-muted-foreground">
            Applies to agents added with this project. Buzz creates
            profile-specific instances so existing agents keep their settings.
          </p>
          <details className="rounded-lg border border-sidebar-border/60 px-2.5 py-2">
            <summary className="cursor-pointer text-xs font-medium">
              Provider route for new local Buzz Agents
            </summary>
            <div className="space-y-2 pt-2">
              <p
                className="text-xs leading-relaxed text-muted-foreground"
                id="create-project-agent-route-help"
              >
                Pins a saved route to initial local Buzz Agents and future local
                Buzz Agents added to this project. Other runtimes, hosted
                agents, and existing agents are unchanged.
              </p>
              <PersonaDropdownField
                ariaDescribedBy="create-project-agent-route-help"
                disabled={routeProfilesLoading}
                id="create-project-agent-route-profile"
                onValueChange={setRouteProfileId}
                options={routeProfileOptions}
                placeholder="Choose a saved route profile"
                value={routeProfileId}
              />
              <p aria-live="polite" className="text-xs text-muted-foreground">
                {routeProfilesError
                  ? "Could not load saved routes. Turn routing off or reopen this form to retry."
                  : routeProfilesLoading
                    ? "Loading saved route profiles…"
                    : routeProfileId && !selectedRouteProfile
                      ? "This route profile is unavailable. Choose another saved profile or turn routing off."
                      : selectedRouteProfile
                        ? `Selected ${selectedRouteProfile.name} · ${selectedRouteProfile.dataPolicy} · v${selectedRouteProfile.version}. Buzz pins this exact profile.`
                        : routeProfiles.length === 0
                          ? "No saved route profiles. Create one in Agents → Provider routes."
                          : "Off. New agents keep their configured provider/model."}
              </p>
            </div>
          </details>
          <details className="rounded-lg border border-sidebar-border/60 px-2.5 py-2">
            <summary className="cursor-pointer text-xs font-medium">
              Starting limits for these agents
            </summary>
            <div className="space-y-2 pt-2">
              <p className="text-xs leading-relaxed text-muted-foreground">
                Per-agent defaults apply to the initial agents and are saved
                locally for later additions. They are not shared project caps;
                token, spend, RAM, and VRAM measurements are unavailable here.
              </p>
              <ProjectAgentResourceDefaultsFields
                disabled={disabled}
                idPrefix="create-project-agent-default"
                onChange={setResourceDefaultsDraft}
                value={resourceDefaultsDraft}
              />
              <ProjectResourcePreflight draft={resourceDefaultsDraft} />
            </div>
          </details>
        </div>
      ) : null}
    </>
  );
}
