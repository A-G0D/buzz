# Coding map

Read the repository's own `AGENTS.md` and nearest scoped guidance before
editing. Inspect current code and reuse its existing patterns. Make the smallest
complete change and validate the affected production path.

## Skill route

- First inspect the active harness's skill inventory and descriptions.
- Load only a skill that names the concrete language/tool/procedure in this task.
- The bundled Buzz CLI skill is `.agents/skills/buzz-cli/SKILL.md`; it applies
  when using Buzz commands, not to ordinary code edits.
- If no exact skill exists, proceed without one. Propose a new skill only for a
  recurring procedure with a demonstrated gap; include a baseline comparison.

Do not treat a checklist as permission to run unrelated tests, destructive
commands, external services, or broader changes. Follow the repository's own
test and validation instructions.
