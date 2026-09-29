import type * as React from "react";

import { useChannelsQuery } from "@/features/channels/hooks";
import { normalizeRelayUrl } from "@/features/communities/communityStorage";
import { useCommunities } from "@/features/communities/useCommunities";
import { resolveMemberProjectHomeChannel } from "@/features/projects/lib/projectHomeChannel";
import { useIdentityQuery } from "@/shared/api/hooks";
import { normalizePubkey } from "@/shared/lib/pubkey";
import type { ProjectDetailAgentContext } from "@/features/projects/lib/projectDetailAgentContext";
import { ProjectAgentChatPanel } from "./ProjectAgentChatPanel";
import { ProjectRepositoryActionsPanel } from "./ProjectRepositoryActionsPanel";
import type { ProjectRightPanelMode } from "./ProjectRightPanelControls";

type RepositoryPanelProps = React.ComponentProps<
  typeof ProjectRepositoryActionsPanel
>;

export function ProjectDetailRightPanel({
  context,
  detachedRepository = false,
  mode,
  onClose,
  ...repositoryProps
}: RepositoryPanelProps & {
  context: ProjectDetailAgentContext;
  detachedRepository?: boolean;
  mode: ProjectRightPanelMode;
  onClose: () => void;
}) {
  const { activeCommunity } = useCommunities();
  const identityQuery = useIdentityQuery();
  const channelsQuery = useChannelsQuery();
  if (mode === "chat") {
    // Remount on community identity as well as repository address: the same
    // repo coordinate can exist in two communities, and retained panel state
    // (conversation, opener) must never cross that tenant boundary.
    const relayScope = activeCommunity?.relayUrl
      ? normalizeRelayUrl(activeCommunity.relayUrl)
      : "";
    const signerScope = identityQuery.data?.pubkey
      ? normalizePubkey(identityQuery.data.pubkey)
      : "";
    const homeChannel = resolveMemberProjectHomeChannel(
      repositoryProps.project,
      repositoryProps.projects,
      channelsQuery.data ?? [],
    );
    return (
      <ProjectAgentChatPanel
        canResetWidth={repositoryProps.canResetWidth}
        constrainToAvailableSpace={false}
        context={context}
        homeChannel={homeChannel}
        key={`${relayScope}:${signerScope}:${context.repoAddress}:${homeChannel?.id ?? ""}`}
        onClose={onClose}
        onResetWidth={repositoryProps.onResetWidth}
        onResizeStart={repositoryProps.onResizeStart}
        widthPx={repositoryProps.widthPx}
      />
    );
  }
  return (
    <ProjectRepositoryActionsPanel
      detached={detachedRepository}
      {...repositoryProps}
    />
  );
}
