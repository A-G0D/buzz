import type {
  AgentSkillDetails,
  AgentSkillSummary,
} from "@/shared/api/tauriAgentSkills";

export type PossibleSkillOverlap = {
  name: string;
  sharedTerms: string[];
  sameContentHash: boolean;
};

const IGNORED_TERMS = new Set([
  "about",
  "after",
  "agent",
  "agents",
  "and",
  "are",
  "for",
  "from",
  "into",
  "skill",
  "skills",
  "that",
  "the",
  "this",
  "with",
]);

function terms(value: string): Set<string> {
  return new Set(
    (
      value
        .normalize("NFKC")
        .toLowerCase()
        .match(/[\p{L}\p{N}]+/gu) ?? []
    ).filter((term) => term.length > 2 && !IGNORED_TERMS.has(term)),
  );
}

function selectedSkillTerms(skill: AgentSkillDetails): Set<string> {
  const headings = skill.content
    .split(/\r?\n/)
    .flatMap((line) => {
      const heading = /^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$/.exec(line);
      return heading ? [heading[1]] : [];
    })
    .join(" ");
  return terms(`${skill.name} ${skill.description} ${headings}`);
}

export function possibleSkillOverlaps(
  selected: AgentSkillDetails,
  installed: readonly AgentSkillSummary[],
): PossibleSkillOverlap[] {
  const selectedTerms = selectedSkillTerms(selected);

  return installed
    .filter((candidate) => candidate.name !== selected.name)
    .flatMap((candidate) => {
      const sameContentHash =
        selected.contentHash.length > 0 &&
        selected.contentHash === candidate.contentHash;
      const candidateTerms = terms(
        `${candidate.name} ${candidate.validationError ? "" : candidate.description}`,
      );
      const sharedTerms = [...selectedTerms]
        .filter((term) => candidateTerms.has(term))
        .sort();

      return sameContentHash || sharedTerms.length >= 2
        ? [{ name: candidate.name, sharedTerms, sameContentHash }]
        : [];
    })
    .sort((left, right) =>
      left.name < right.name ? -1 : left.name > right.name ? 1 : 0,
    );
}
