# Buzz routing milestone — profile-gated ACP dispatch

**Status:** versioned route-profile parsing, the local Desktop profile store and
editor, per-agent assignment, trusted launch resolution, per-prompt ACP
candidate selection, explicit one-shot dispatch, and per-prompt route-decision
provenance in managed ACP attempt history are implemented. Strict task-fit
eligibility is an opt-in gate using validated local reports and
route-bound attestations.

## Scope and evidence

The backup already has explicit `Provider` values and a Rust model-capability
resolver in `buzz-agent`; that resolver covers selected wire/reasoning facts,
not locality, availability, tools, context, cost, time, throughput, or task
quality. The new `buzz-agent::route_preview` module therefore accepts
caller-supplied, provenance-labeled evidence for those facts. Missing evidence
stays `Unknown` and fails any hard gate that needs it.

The preview defaults to local-only. It can select a sole eligible candidate, a
single explicit preference, or the first eligible candidate in an explicit
preference order. The order skips candidates excluded by hard privacy/resource
gates; stale or duplicate IDs and conflicting preference forms abstain. Multiple
eligible candidates without a preference still abstain because this slice has
no task-quality evidence or ranking policy. A configured order is a user's
priority, not a measured model-quality score. Each candidate records chosen,
eligible, excluded with reasons, or not evaluated. A safety refusal is terminal
and skips route evaluation.

Buzz Desktop stores strict version-1 route profiles in its local agent
workspace and lets the owner assign one to a local Buzz Agent instance. The
trusted launch resolver serializes the validated snapshot into
BUZZ_AGENT_ROUTE_PROFILE_JSON. Standalone Buzz Agent launches can still opt in
through that environment value. The JSON is capped at 64 KiB, accepts at most
16 API candidates, rejects unknown fields (including credential fields), and
defaults to local-only. Provider keys and endpoints are resolved only from
their existing provider environment variables. Each candidate can carry a
bounded prompt addendum that is combined with the ACP session prompt for that
provider/model. A local candidate must target a loopback URL (localhost,
127.0.0.1, or ::1); the route client ignores ambient HTTP proxies and refuses
redirects. Hosted candidates require the profile's explicit allow-hosted policy.

At each session/prompt, Buzz selects one route before sending the user task
content. An explicit `session/set_model` choice pins selection to a unique
candidate matching the active provider and model, then passes that candidate
through the active profile's ordinary eligibility checks. It does not bypass
the profile's configured privacy, task-fit, context, cost, throughput, or tool
gates. A missing candidate credential/config excludes that candidate; if the
profile cannot select an eligible candidate, the turn ends before any model
request. Once selected, the same provider/model handles every round in that
prompt. Provider failures do not switch models.

`BoundRouteCandidate` binds candidate metadata to one provider configuration
without making credentials part of the preview or trace. Binding rejects a
provider mismatch and a blank model ID. `complete_routed` evaluates the preview,
then makes one request through only the selected provider/model and returns the
preview with its result. Desktop automatically appends the exact provider/model
prompt profile when present, otherwise the provider-wide profile, and pins the
profile version and hash into the immutable launch document. An abstention,
safety refusal, setup error, or provider error never tries a second candidate
in this API.

## Limits

- Preview accepts only metadata and has no task prompt/content. Dispatch is a
  separate explicit call and sends task content only to the selected candidate.
- Desktop stores and edits profiles locally, then assigns them to individual
  local Buzz Agent instances. Project/archetype inheritance is not implemented.
- The environment JSON is an internal launch snapshot in Desktop. Provider
  credentials and endpoints remain in existing per-provider configuration.
- This profile routes Buzz Agent API adapters only. It does not route between
  ACP CLI harnesses such as Codex, Claude Code, or DSH.
- The order is explicit user priority, not task-aware ranking. An opt-in
  task-fit policy is a hard candidate-eligibility gate for one declared task
  class; it does not rank eligible candidates. There is no measured quality
  ranker or intra-prompt step routing.
- Route-decision events are captured only for channel-scoped managed Buzz ACP
  turns. Standalone runs and heartbeat turns do not enter this managed-turn
  journal path.
- A `selected` event means Buzz Agent bound and initialized the chosen client
  before dispatch. It does not prove a provider request succeeded or that the
  task completed. Provider success/failure and fallback are not recorded here.
- The ACP update uses the standard opaque `session_info_update._meta` extension
  point with a versioned `buzz.routeDecisionV1` object. Other ACP consumers may
  ignore it; non-Buzz adapters do not emit this record.
- The receiver joins `sessionId` and active `attemptId`, then the journal checks
  the managed `turn_id` and launch-pinned route profile ID/version/hash. A
  malformed, stale, mismatched, or conflicting record is rejected.
- The event stores only candidate/provider/model IDs or a fixed reason code,
  profile ID/version/hash, session ID, and attempt ID. It never stores prompt
  text, credentials, raw provider errors, or completion content.
- Locality is checked against the configured URL host and local-route HTTP
  proxy/redirect behavior. This is not a process-wide network sandbox.
- Cost rates and context capacity are operator-declared; cost and context fit
  remain estimates. Throughput comes from recent local managed-run history and
  measures end-to-end latency, not model decode speed. Task-fit evidence is
  covered below. These sources do not verify provider metadata or benchmark
  provenance. This profile does not expose a verified time-to-completion fact.
- Optional resource thresholds are enforced when configured. Evidence sources
  such as live endpoint availability, model capability, cost, and measured
  throughput must be connected to verified sources before the decision is
  production automatic.
- No refusal recovery or provider fallback exists; this is intentional until a
  typed provider error policy and a user-visible fallback trace exist.
- DSH prompt overlays are a separate ACP runtime feature; DSH is not a
  candidate in this API-only route profile.

## Verification

Nineteen focused route tests cover hard gates, ordered selection, profile
schema/version/size limits, rejection of credential fields and stale IDs, provider
prompt binding, local endpoint validation, local-only default, explicit hosted
opt-in, and skipping an unconfigured candidate. Routed completion tests cover
selected-model dispatch, zero calls after safety abstention, and no alternate
model after provider failure. Local HTTP tests verify cross-provider selection
with the selected candidate's prompt addendum and that the local-route client
does not follow redirects. Profile binding also revalidates programmatically
built documents instead of relying on callers to have parsed them first. The
full Buzz Agent library suite passes **581 tests, 1 ignored**; Rust formatting
and git diff checks pass using the installed MacOSX26.5 SDK.

## Basic selected-provider smoke

The existing manual provider path is now exercised end-to-end against a local
mock HTTP server. Selecting the Buzz Agent DeepSeek provider sends the configured
model to its configured `/chat/completions` endpoint and parses the response;
the test also checks DeepSeek's `max_tokens` request field. It sends no prompt to
DeepSeek or another hosted provider.

Earlier focused verification: selected-provider mock smoke **1/1**; route-preview
eligibility **8/8**; provider/config UI checks **58/58**; and Buzz Agent library
suite **568 passed, 1 ignored**. Latest route-profile verification: route tests
**19/19**, Buzz Agent library **581 passed, 1 ignored**, Tauri route-store and
launch tests **3/3**, route editor UI test **1/1**, Desktop typecheck, targeted
Biome, all-target check, Rust formatting, and `git diff --check` pass. The full
Desktop suite passed **6,695 unit tests plus 103 UI tests**, with zero failures.

This establishes manual provider/model selection and the direct DeepSeek API
adapter at a basic local level. It does not verify a live API key, live network
request, DeepSeek Harness runtime, or local-model server. The new route profile
uses explicit candidate order by default. Strict task-fit eligibility is an
opt-in hard gate described below; measured quality ranking and technical
failover remain disabled.

## Research cross-check — 2026-09-25

The route remains an explicit ordered policy because LLMRouterBench finds that
many routers do not reliably beat a best-single-model baseline under unified
evaluation, despite model complementarity; it also reports diminishing returns
from larger ensembles. Candidate order is therefore configuration, not a claim
that Buzz has learned model fit. SkillRouter supports exposing full relevant
skill content to selection rather than choosing from labels alone, but its
expert query set is small compared with its roughly 80K-skill catalog. See the
[research update](../../outputs/BUZZ-RESEARCH-UPDATE-2026-09-25.md) for source
links and limitations.

## Example route profile

Put the following JSON in the BUZZ_AGENT_ROUTE_PROFILE_JSON environment
setting for a Buzz Agent process. This preference tries the local
OpenAI-compatible endpoint first and can use the direct DeepSeek API only when
the data_policy is changed to allow-hosted. API keys stay in the existing
provider-specific environment variables; never add them to this JSON.

```json
{
  "version": 1,
  "data_policy": "local-only",
  "preference_order": ["local-qwen", "deepseek"],
  "candidates": [
    {
      "id": "local-qwen",
      "provider": "openai",
      "model": "qwen3-coder",
      "data_location": "local",
      "prompt_addendum": "Keep code changes focused and report the checks run."
    },
    {
      "id": "deepseek",
      "provider": "deepseek",
      "model": "deepseek-chat",
      "data_location": "hosted"
    }
  ]
}
```

The local candidate uses OPENAI_COMPAT_BASE_URL and OPENAI_COMPAT_API_KEY; the
hosted candidate uses DEEPSEEK_BASE_URL and DEEPSEEK_API_KEY. No live provider
request was made during validation. The route remains explicit candidate order;
task-fit ranking and technical failover are not implemented.

## Per-prompt route-decision provenance — 2026-09-26

Buzz Agent now emits a namespaced route decision in the same ACP
`session_info_update` that carries its active run ID. Buzz ACP accepts only
records whose ACP session and attempt match its current managed channel turn;
the local journal then checks the route profile identity against the immutable
launch snapshot and persists one `route_decision_v1` event per turn. The event
is idempotent for identical repeats and rejects a conflicting second decision.

Decision outcomes are `selected`, `abstained`, `refused`, or `overridden`.
Reasons are allowlisted codes; unknown route abstention text collapses to the
generic `route_abstained` code. The receiver schema rejects extra fields. Tests
cover the sender payload, exact ACP session/attempt matching, extra-field
rejection, durable event joins, identity mismatch, duplicate/conflict handling,
and safe-reason validation.

Validation in the backup: the three focused route-decision tests pass, and the
full affected library suites pass (**966 buzz-acp passed, 3 ignored; 582
buzz-agent passed, 1 ignored; 20 buzz-run-journal passed**). A full affected
suite run exposed that the new nullable route-profile keys were written into an
older `agent_profile_v1` event even when absent. The snapshot builder now omits
those optional keys unless the launch pinned a route profile; the legacy
snapshot regression and the route-profile join test both pass. Tauri
`cargo check --all-targets` and `git diff --check` also pass in the backup.
These are mock/unit and compile checks; no provider, DeepSeek, or external
service was contacted. Next: connect remaining capability and endpoint
requirements to verified sources, then evaluate the opt-in task-fit gate
against a best-fixed-model baseline before claiming quality, cost, or speed
gains. Keep technical fallback opt-in and typed; safety refusal remains
terminal.

```sh
cargo test -p buzz-agent --lib route_preview::tests
```

## Estimated per-turn cost ceiling — 2026-09-26

Route profiles can optionally declare a maximum estimated cost for one Buzz
Agent ACP prompt/turn. Each candidate's input and output rates are explicit
operator-entered USD per million tokens, stored as integer micro-USD. Under an
active ceiling, candidates with missing/invalid rates or an unknown conservative
estimate are excluded before dispatch. At each direct selected-route provider
call, Buzz reserves the estimated current history plus the configured maximum
output for each request and all retries. Tool-loop calls accumulate against
the same turn ceiling and stop before dispatch when the next reservation would
exceed it. Existing profiles omit the new fields and retain prior routing
behavior.

The estimate uses the existing UTF-8-byte/framing heuristic, not the provider's
tokenizer. It deliberately reserves the whole output cap for the request and
all maximum retry attempts, and does not refund unused capacity. It is not a
bill guarantee or a project-wide budget: provider tokenization, cache pricing,
nested summarizers, and other provider-backed work are outside this control.
The UI labels it an estimate and requires complete declared rates. The
call-boundary estimator reads the incoming task already appended to history,
avoiding a second empty prompt frame on subsequent tool-loop requests.

## Local task-fit report review — 2026-09-26

The route-profile screen can preview a Harbor v0.16.1 task-fit report, show its
task-class results and exact evaluation identities, and import the reviewed
bytes into `.agents/task-fit-evidence/`. Import repeats structural validation
and requires the report SHA-256 shown during review. Stored reports are
content-addressed, owner-only, size-limited, revalidated when listed, and never
overwrite an existing report. The native UI rejects files over 1 MiB before
transferring bytes to Rust.

This is a local review shelf, not an evidence trust system. A consistent hash
does not authenticate the report producer or prove that the benchmark inputs
and results are genuine. Imported reports are visibly marked unverified. A
bare imported report is not consumed by route selection: an opted-in strict
task-fit policy also requires the separate route-bound review described below
and applies configured freshness, sample-count, and score thresholds.

## Local review attestation — 2026-09-26

An imported report can now receive a local review attestation signed by the
current Buzz Nostr identity. The signed event is stored only in the local
`.agents/task-fit-evidence/attestations/` directory and is never published.
Buzz verifies the event signature, current identity, report hash, and task-class
version when it displays the attestation. Signing requires the identity to be in
a signable state.

This attestation records that the identity reviewed that exact report. It does
not authenticate the Harbor process, dataset, endpoint, or result artifacts;
the UI states this at the signing point. It is not a producer signature and
does not make a report route-eligible by itself. Strict route eligibility uses
a separate route-bound attestation, not this generic report-review record.

## Opt-in route-bound task-fit eligibility

A route profile may opt into a hard task-fit gate for one task class. Its
policy names the task-class taxonomy and evaluation-policy versions, requires
a minimum number of distinct task checksums and a minimum 95% Wilson lower
bound, and limits report age. It can also require the report to have observed
the exact configured model ID. These settings determine whether a candidate
is eligible; they do not rank eligible candidates. Saved preference order
continues to choose among eligible candidates.

The gate requires a structurally validated local report and a separate
current-Buzz-identity-signed route attestation. That attestation binds the
report hash to the exact route profile ID, version, hash, and candidate ID.
The gate also checks the report's task class, policy versions, provider, and
model against the configured route and policy. The generic local report-review
attestation above does not supply this route binding. Under strict task-fit,
missing, invalid, mismatched, stale, under-sampled, or below-threshold
evidence cannot qualify a candidate; if no candidate passes, Buzz abstains
before making the first provider request.

Strict task-fit prompts also require supported task-class metadata from the
Desktop path. This is an operator-selected class, not an independently
verified classification. These fail-closed checks constrain use of the local
report; they do not authenticate Harbor or the report producer, benchmark
inputs, endpoint, or results, and they do not prove a quality gain over a
best-fixed-model baseline. Treat the gate as an operator-configured eligibility
policy over unverified benchmark evidence, not as a quality guarantee.

## Task-class ingress review follow-up — 2026-09-26

The Desktop task-class ingress accepts only the eight built-in IDs (`coding`,
`code_review`, `research`, `writing`, `analysis`, `planning`, `summarization`,
`classification`) on owner-signed kind-9 stream messages. The Tauri channel-send
path rejects the field for other event kinds; no DM task-class send path is
present. Buzz Agent's ACP metadata decoder enforces the same list. Literal
`unknown` and arbitrary syntactically valid IDs are rejected into the unknown
path. Buzz ACP serializes this
extension only after the peer advertises exact version 1. For non-strict turns,
ordinary ACP peers and legacy Buzz Agent peers receive the ordinary prompt with
the unsupported Buzz-only metadata omitted. Under a strict task-fit profile,
ACP rejects before writing `session/prompt` unless the peer is identifiable as
Buzz Agent, negotiated exact version 1, and received a known task class. The
identity comes from the peer's self-reported `initialize.agentInfo.name`; ACP
does not authenticate it as a process identity. The strict gate reads the
effective child launch profile, including inherited and authoritative
environment values. A profile ID without its JSON, malformed JSON, or unknown
peer identity activates fail-closed behavior, so a renamed wrapper cannot
remove the gate by changing its command name.

Mixed-class queued batches currently fail closed as unknown. FIFO partitioning
into homogeneous groups is deferred; event boundaries are retained, so this is
a known limitation and a follow-up gate before mixed-class queues can route
automatically. The CLI also supports an explicit operator-supplied class via
`buzz messages send --task-class` on kind-9 messages; it validates the class and
emits the owner-signed metadata tag with source `cli_explicit`. Automatic or
ambient CLI classification remains unsupported. Heartbeat, external-client,
and other ingress remain unimplemented and unknown by default.

The earlier pre-acquisition and manual-override gates are now implemented.
Route admission snapshots session state, evaluates eligibility, checks that
the captured revision is still current, and reserves the turn before dispatch.
An explicit model override is restricted to one unique candidate in the
profile and runs through the same eligibility path; the focused route tests
cover stale state, cancellation, and competing admissions. This does not
verify a real provider/model identity or service-egress boundary, and it does
not establish model quality. Mixed-class queued batches still fail closed as
unknown; FIFO grouping remains deferred.

The fake ACP regressions pass: the `task_class` filter reports 12 passed, and
the non-strict legacy Buzz compatibility case reports 1 passed. They verify
plain prompts for ordinary and non-strict legacy peers, exact-v1 metadata for
new Buzz peers, strict-v1 metadata delivery for an identified Buzz peer, and
pre-write rejection for strict legacy or unidentified peers. `git diff --check`
passes. Whole-file ACP rustfmt check still reports only pre-existing formatting
differences outside this follow-up's hunks; the new launch classifier and
peer-path tests have no formatter diffs.

The ignored signed-owner task-class integration test was run separately and
passed 1/1 with a loopback fake provider. Missing/unknown class metadata is
rejected by the ACP client before it writes `session/prompt`; a known but
policy-mismatched class produces one prompt write and is rejected by Buzz Agent
before a provider request. The accepted CLI class produces one fake request,
and its selected route receipt's `attemptId` matches `goose.activeRunId` in the
same `session/update`. This is a synthetic signed-event/ACP test, not relay
intake or live-provider evidence; it observes the wire receipt, not journal
persistence or model quality.
