# Orchestration map

Start with one coordinator. Delegate only bounded work that can proceed
independently or benefits from a genuinely independent review. Record each
worker's objective, exact target, inputs, budget, and return format; preserve the
original user intent and append later steering rather than replacing it.

## Critics and steering

- For an explicit critic request, follow `CRITICS.md` for the trigger and review
  protocol. Keep critics separate from ordinary project delegation.
- For Desktop critic rounds, select and verify a saved Local route separately
  for each role; preserve each route identity with its reviewer result. Route
  labels alone do not establish distinct model weights or independent results.
- If no critic pack is configured, report that capability gap and perform a
  clearly labeled single-agent self-review; do not imply independent review.
- Query the exact thread/run brief before steering. Record the guidance source
  and transport acknowledgement separately from task completion.
- Do not infer completion from a quiet worker or a successful tool call. If the
  adapter cannot verify lifecycle state, report it as unknown.

## Skill route

Use existing orchestration, critic, or policy packs only after checking their
scope and version. A new pack must declare its triggers, limits, capabilities,
provenance, and evaluation; it cannot expand the caller's permissions.

## Optional typed-decision routing

- A Jev-compatible System One model such as local Laya may classify a bounded
  task into a finite task class, latency tier, modality, or critic-needed flag.
  It does not write plans, implement code, verify claims, or generate prose.
- Use it only when the exact model and local endpoint are verified. Do not send
  private project context to hosted Jev or another hosted classifier without
  explicit user approval for that data path.
- Treat the result as a suggestion. Buzz policy still applies locality,
  credentials, accepted tools, context fit, cost ceilings, measured throughput,
  and task-fit evidence. A confidence score cannot approve an action or override
  a refusal.
- Begin in shadow mode: record the structured decision and a human-corrected
  label without changing route selection. Measure held-out accuracy, calibration,
  low-confidence abstention, and out-of-domain cases before enabling suggestions.
- If the endpoint is unavailable or unverified, the task exceeds the model's
  supported context, or confidence is low, use the configured default route or
  abstain. Never route a refusal to an abliterated model to bypass it.
