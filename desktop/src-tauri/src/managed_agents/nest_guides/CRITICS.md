# Critic round guide

Use this guide when the user explicitly asks to “run critics on this,” “get
independent reviews,” or names a critic role. Do not launch reviewers because a
conversation merely mentions criticism or review.

## Before dispatch

1. Preserve the user's original objective and current scope. Query the exact
   thread/run brief when its source IDs are available; include later steering
   and report freshness or truncation.
2. Select only relevant roles from Buzz's critic archetype catalog: correctness,
   security, UI/accessibility, performance, architecture, or product. Reuse the
   smallest set that covers the requested review.
3. Use the same frozen change/context snapshot for each reviewer. Give each the
   full task, target files/revision, requested review scope, and an independent
   return format. Do not show peer findings before independent passes finish.
4. Apply configured model, reasoning, time, and cost preferences only when the
   current runtime can enforce or report them. If a setting is unavailable, say
   so; do not imply a prompt instruction enforces a resource or permission cap.

## Run honestly

- If `run_critics` is available, call it only after an explicit user request.
  Pass the original objective, review scope, one frozen text snapshot, and the
  smallest relevant role set (one to three roles). It verifies a SHA-256 for
  the exact snapshot supplied to every pass.
- `run_critics` uses Buzz Agent's review-only mode: it accepts only loopback
  model endpoints, rejects remote route candidates, disables tools, MCP
  servers, hooks, skill discovery, and summary-model calls. Per pass, callers
  may set `max_output_tokens` from 64–2,048 (default 2,048),
  `time_limit_seconds` from 15–120 (default 120), and requested
  `thinking_effort` (`none|minimal|low|medium|high|xhigh|max`). Buzz enforces
  the token and model-turn time caps. The provider may reject, clamp, or ignore
  the requested effort; the result labels it as requested, not effective. The
  worker has a separate 15-second startup grace. Desktop runs one bounded local
  process per role, re-resolves each saved route at launch, and records that
  exact profile identity on the corresponding reviewer result. Each reviewer
  reserves one slot against the global local-agent process limit; every worker
  start checks the configured free-system-RAM reserve. The RAM sample is
  point-in-time and does not estimate worker RAM or GPU memory. If a configured
  resource limit, local endpoint, or worker blocks a start, report that instead
  of silently reducing checks or routing to a hosted provider.
- Each role can select a different Local only route. This makes provider/model
  choice explicit; it does not prove statistical independence because routes
  can share a provider, local service, or model weights. Do not claim
  cross-model independence or treat agreement as proof. “No tools” is enforced
  at the Buzz Agent ACP boundary, not by an OS sandbox. Buzz verifies that each
  endpoint is loopback, but cannot verify whether that local service forwards
  requests elsewhere; only send a private snapshot after checking the service's
  egress behavior.
- The tool attempts to persist each bounded result in Buzz's current
  identity-scoped local run journal. It stores hashes of the snapshot,
  objective, and scope plus reviewer output/metadata; it does not store those
  submitted texts. A local round ID is returned only after a successful write;
  use `critic_run_status` with that ID to retrieve it. If the identity or local
  journal is unavailable, the review response still returns with
  `ledger_status: not_saved` and a generic error code.
- The local ledger does not measure model cost/token usage or impose a dollar
  spend cap. Estimated route ceilings are informational per reviewer turn.
  Per-round records are size bounded, but this version has no automatic
  retention or purge policy.
- If independent dispatch is unavailable, perform one self-review and label it
  as such. Do not claim a critic round, consensus, or independent confirmation.
- A critique is advisory. It does not approve a change, authorize an action,
  or override the user's request or project policy.

## Return format

For each finding, include severity, concise title, exact evidence, user impact,
and uncertainty. Separate confirmed defects from hypotheses. Include dissent and
duplicate findings rather than forcing agreement. A no-findings result must say
what was inspected and what remains unverified. Never treat reviewer count or
agreement as proof.

For recurring critic packs, retain provenance and compare against a no-pack
baseline using pinned tasks. Track actionable detection, false positives,
unique evidence, accepted fixes, regressions, latency, and cost. Do not make a
pack the default from one successful review.

## Current Buzz boundary

Buzz exposes focused, versioned local ACP critic archetypes with bounded
per-agent settings and a literal prompt preview, plus the `run_critics` MCP tool
for small loopback-only rounds and `critic_run_status` for local result lookup.
Desktop critic rounds let the user select one Local route per role and pin that
profile's resolved identity to its reviewer result. The MCP tool still uses one
configured route for its round. Neither flow is an OS filesystem/process
sandbox. When the tool or a local model route is unavailable, use the self-review
path above and state the limitation.
