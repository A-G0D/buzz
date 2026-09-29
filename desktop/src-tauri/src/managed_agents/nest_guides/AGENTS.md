# Task instruction map

Read only the domain map that fits the current request. These maps route to
existing project/runtime skills; they do not replace Buzz's policy or grant
new tools.

| Work | Read |
|---|---|
| Code, repository changes, debugging | [`CODING.md`](CODING.md) |
| User-facing prose, plans, documents | [`WRITING.md`](WRITING.md) |
| Current facts, papers, external sources | [`RESEARCH.md`](RESEARCH.md) |
| Multi-agent work, critics, long-running tasks | [`ORCHESTRATION.md`](ORCHESTRATION.md) |
| Explicit critic rounds or “run critics on this” | [`CRITICS.md`](CRITICS.md) |

## Skill decision

1. Identify the task procedure that would benefit from reusable guidance.
2. Check the active harness's available skill names and descriptions, then inspect
   only the closest candidate. Buzz CLI guidance lives at
   `.agents/skills/buzz-cli/SKILL.md`; use it only for Buzz CLI operations.
3. Choose **reuse**, **reuse with a proposed edit**, or **no skill** before
   considering a new one. Similar names are not proof of duplicate behavior.
4. Skip new skills for one-off tasks. For recurring procedures with a real gap,
   draft a candidate with scope, trigger, provenance, and a small paired task
   set comparing the agent with and without it. Record regressions and cost.
5. Suggest candidates by default. Import or activate one only through an
   explicitly configured local workflow; never silently ingest a downloaded
   skill or execute code bundled with one. Until evaluation controls exist,
   proposal-only is the default and local import stays an explicit UI action.
   Keep versions and a revert path.

A skill may be relevant but still harmful to a task. Load the smallest exact
match, and leave it unloaded when no-match is the better answer. Instructions
at lower levels can add detail but cannot loosen higher-level policy.
