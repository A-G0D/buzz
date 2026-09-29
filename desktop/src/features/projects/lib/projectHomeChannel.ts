import { useProjectsQuery } from "@/features/projects/hooks";
import type { Project } from "@/features/projects/projectModels";
import type { Channel } from "@/shared/api/types";
import {
  findProjectHomeByChannelId,
  isProjectHomeChannel,
} from "./projectHomeSelection";

export {
  findProjectHomeByChannelId,
  hasAuthoritativeHomeBinding,
  isProjectHomeChannel,
  type ProjectHomeCandidate,
} from "./projectHomeSelection";

/**
 * Resolve a project home only when the current project list confirms the
 * authoritative project/repository binding and the signed-in identity is a
 * member of the exact channel. Otherwise callers keep their existing DM path.
 */
export function resolveMemberProjectHomeChannel(
  project: Project,
  projects: readonly Project[],
  channels: readonly Channel[],
): Channel | null {
  const channelId = project.projectChannelId;
  if (!channelId) return null;

  const authoritativeHome = findProjectHomeByChannelId(channelId, projects);
  if (authoritativeHome?.id !== project.id) return null;

  return (
    channels.find((channel) => channel.id === channelId && channel.isMember) ??
    null
  );
}

export function useIsProjectHomeChannel(channelId: string | null | undefined) {
  const projectsQuery = useProjectsQuery();
  return isProjectHomeChannel(channelId, projectsQuery.data ?? []);
}
