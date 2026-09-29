# Agent Configuration — Contributor Rules

Scope: `desktop/src/features/agents/` (config surfaces, shared config renderer,
and the agent config core). Read this before changing how harness / provider /
model / effort configuration is modeled, rendered, persisted, or applied.

Plan of record: `Buzz/Harness-Provider-Model.md` in Morgan's Obsidian vault
(PR sequence, decisions log). PRs: #2140 (rename), #2148 (flag reduction),
#2156 (honest model states), #2158 (Agent Config Core).

## The one rule

**Harness capability facts have exactly one source: the Rust runtime catalog.**
`KnownAcpRuntime` (`desktop/src-tauri/src/managed_agents/discovery/runtime_metadata.rs`)
declares each harness's model/provider/effort env keys and capabilities. Spawn
applies them; `AcpRuntimeCatalogEntry` exposes them over IPC; and
`lib/agentConfigCore.ts` projects them into field descriptors. The frontend
never maintains a rival copy of this table. Setup guidance follows the same
rule: `requires_external_cli` is derived from `KnownAcpRuntime` and projected
to the UI rather than inferred from a runtime ID in a component.

**Second metadata source: command-keyed execution policy.**
`harness_max_parallelism` (`managed_agents/parallelism.rs`) maps the harness's
static command string to a spawn-time cap (`OPENCLAW_MAX_PARALLELISM = 5` for
OpenClaw). This cap is not a `KnownAcpRuntime` field because it applies to
preset harnesses (like OpenClaw) that are not in the builtin catalog. It is
projected onto `AcpRuntimeCatalogEntry.max_parallelism` by all four
catalog-producing constructors (builtin discovery, preset catalog, custom
discovery, custom-save response) using the **static definition command**, not
the resolved `entry.command` (which may be `null` for unavailable entries).
The frontend reads `maxParallelism` from the catalog entry and never keeps a
separate constant.

If you need a new capability fact (a new env key, a native option, a "supports
X" flag): add it to `KnownAcpRuntime` first, expose it on
`AcpRuntimeCatalogEntry`, then project it through the core. Do not shortcut
with a TypeScript lookup table or an id comparison in a component.

## Rules

1. **No hardcoded harness-ID checks in render code.** `runtime.id === "claude"`
   belongs in `deriveAgentConfigFieldModel` (once, with a named reason), never
   in a component. Components ask the field model what exists
   (`hasRenderableAgentConfigField`, `getRenderableEffortField`).
2. **Effort reads/writes go through the descriptor.** Use the effort
   descriptor's `currentPersistence` key — never a raw
   `BUZZ_AGENT_THINKING_EFFORT` literal in UI code. `currentPersistence` is
   where the value lives *today*; `targetApplication` is how the harness
   *should* receive it. They intentionally differ until PR 2.7 migrates
   Goose/Claude — do not "fix" one to match the other without doing the
   migration work.
3. **Field absence has a named reason, not a boolean.** Codex effort is
   `ownedByModelId`; Claude effort is `deferredUntilNativeOptionsAvailable`.
   New absences get new named reasons in `AgentConfigOmission` /
   `render` — never a `showX` prop.
4. **The clearing policy is the named types.** `onContextChange:
   "resetDependentValues"` (user changed harness/provider → dependent values
   reset everywhere) vs `onCatalogMismatch: "explainOnly" | "onboardingCleanup"`
   (an async catalog miss never silently erases saved state outside
   onboarding's named cleanup). Do not add mutation booleans like
   `clearInvalidModel`; extend the policy types.
5. **"Metadata unknown" ≠ "harness lacks the capability".** Passing
   `runtime: undefined` to the core means fields won't render. Surfaces must
   gate on the runtime catalog query settling (loading/error states) rather
   than letting fields silently vanish — see `AgentDefaultsEditor` /
   `DefaultConfigStep` for the pattern.
6. **One canonical behavior, disclosure presets for visibility.** Behavior
   flags were deliberately killed in #2148 (`CANONICAL_CONFIG_BEHAVIORS`).
   Surface differences are expressed via the `disclosure` preset, not new
   boolean props.  **Exception:** `onboarding-essential` hides happy-path
   helper copy (provider/model descriptions) but a non-null model-discovery
   status always bypasses the preset and renders the status line — enforced
   via `shouldShowModelStatusMessage()` (`AgentConfigFields.tsx`).
   Additionally, a successful discovery response that yields no usable options
   (`supportsSwitching:false` or empty model list) synthesizes a warning status
   via `synthesizeEmptyDiscoveryStatus()` and is intentionally **not cached**
   so that closing → reopening the dialog re-runs discovery after the user
   installs or signs into the CLI (`isCacheableDiscoveryResponse()`).
7. **Onboarding setup detects readiness; it does not select defaults.** The
   setup page derives visible and ready harnesses from the runtime catalog and
   only offers install or sign-in actions. The following defaults page is the
   sole onboarding surface that chooses `preferred_runtime`. Its complete draft
   lives in machine-onboarding session state, so Back performs no write and
   restores even incomplete edits when the user returns. Skip abandons that
   draft and advances with zero config writes. Next is the only persistence
   boundary: it consumes the shared renderer's `onValidityChange` signal,
   disables editing while awaiting `set_global_agent_config`, advances only on
   success, and leaves the draft in place with a retryable inline error on
   failure. A harness selection alone does not enable Next when the harness
   requires provider/model/credential config (e.g. buzz-agent with no
   provider). Baked build env and runtime-file config satisfy the gate. Drafts
   intentionally do not survive an app restart.
   `onboarding-agent-defaults.spec.ts` is the acceptance gate for anything
   touching this flow or the shared renderer.
8. **Omit the Model control only after a confirmed successful empty
   discovery on an optional-model harness.** When the field model marks model
   as `acpNative` (Claude Code / Codex), `shouldRenderModelControl` hides the
   picker while discovery is in flight and after IPC resolves with no usable
   options (`modelDiscoverySuccessfulEmpty` / `isSuccessfulEmptyDiscovery`).
   A thrown or unavailable discovery keeps the control so #2246 failure UI can
   render, and must not heal/clear persisted model or effort. Full disclosure
   still shows the control when Custom model is available. Required-model
   harnesses always keep the field. Gate: `defaults hides model when optional
   harness has empty discovery` (and the failed-discovery counterpart) in
   `onboarding-agent-defaults.spec.ts`.
9. **The defaults modal is progressively disclosed.** An unset global config
   starts on the Buzz Agent-first deployment fallback and carries that visible
   harness into the next saved edit. The `progressive-defaults` disclosure
   preset therefore begins at Provider for Buzz Agent, then reveals Model,
   Effort, and Advanced only after a provider is configured. Harnesses whose
   runtime metadata has no provider field skip that gate. Reveals animate their
   height through Motion and become immediate when reduced motion is requested.
   Once the Advanced toggle is visible, its expanded state is exclusively
   user-controlled: provider, harness, and required-env changes must never
   open it automatically in defaults, create, or edit flows. In Create mode,
   `Run on` belongs in Advanced directly after **Who can send instructions**;
   keep it out of the basic create fields. The defaults summary follows
   preferred-harness changes saved while the dialog is open, and its configured
   state includes required credentials as well as provider/model values. If no
   available harness can resolve, Create starts in Customize and lets unavailable
   catalog entries be selected only to expose their setup guidance; submission
   remains blocked.
   Advanced-only required credentials and incomplete remote **Run on** setup
   mark the collapsed Advanced toggle without opening it, and block incomplete
   saves.
   Runtime-file credentials satisfy Global Defaults just as they do Create and
   Edit. In Edit,
   selecting Custom command keeps its required command field beside the harness
   picker rather than hiding it in Advanced.
10. **Catalog visibility is community-scoped relay state, never a global
    definition field.** `AgentDefinition.shared` is only the active
    relay+owner projection returned to the UI. Durable heads and pending
    publications live in the scoped retention database, and explicit share
    toggles await relay acceptance before the UI claims that an agent was
    published or removed. A queued update must stay visibly queued, and the
    catalog itself must render only relay-confirmed publications — never an
    optimistic local persona.
11. **Shared agent access names the consequence where it is selected.** The
   shared respond-to field shows a persistent warning whenever `anyone` **or**
   `allowlist` is selected — both hand the host's access to someone other than
   the owner, so both disclose it and only the audience phrase differs. This
   covers persona-backed create and edit surfaces. Keep that disclosure in
   the shared field instead of adding surface-specific flags. It renders
   directly below the selector for `anyone` but *after* the people picker for
   `allowlist`, so it never sits between the user and the selection they came
   to make. The copy leads with the audience ("Anyone can use this agent to
   access…") so it reads as a warning rather than an explanation, and stays one
   sentence — don't split the mechanism into a second sentence. Both the machine
   and the stakes it names come from `lib/agentAccessWarning.ts`, keyed on an
   optional `runLocation`: instance surfaces resolve it from
   `ManagedAgent.backend` via `runLocationForBackend`, and the create flow from
   `WhereToRunDraft.runOn` via `runLocationForRunOn`. `AgentDialog` is the one
   place that resolves it for dialog surfaces and publishes it through
   `ui/AgentRunLocationContext.tsx`; the field reads that context and lets an
   explicit `runLocation` prop win. Do **not** thread the value as a prop
   through `AgentDefinitionDialog` / `AgentInstanceEditDialog` — neither uses
   the value itself, and the shared context keeps the dialog boundary stable.
   Surfaces rendered outside `AgentDialog` (e.g. `EditRespondToDialog`) pass the
   prop directly. Local names "your
   computer, including files, accounts, and connected tools"; remote names "the
   server it runs on, including any accounts and tools available there" —
   deliberately *not* the owner's files, which aren't theirs to describe on a
   host they don't own. **For a persona-linked deployed agent, the profile Edit
   dialog seeds access from the exact clicked instance and saves access through
   `update_managed_agent`; persona behavior remains the definition default, but
   must never bypass the instance command's stop, persist, publish, and restart
   boundary.** An unknown location falls back to the local wording — never hedge
   with "computer or server". A remote host requires an
   installed `buzz-backend-*` provider, and without one `WhereToRunSection`
   never renders, so "server" would name a concept the owner has never been
   shown; when it *is* remote they picked that host from the selector
   themselves. Never synthesize a run location a surface doesn't have. Don't
   expose `respond-to`, `allowlist`, Nostr, or harness jargon in primary UI
   copy. **The owner-only-access build capability is backend-independent.** When
   `getAgentAccessOwnerOnly()` is true, every managed agent's access control is
   locked to owner-only, including provider-backed agents. A provider backend
   does not prove remote execution and must never create a policy carve-out.
12. **Shared instructions must be reviewable byte-for-byte.** Agent definitions
   execute their `system_prompt` verbatim, so catalog and snapshot review
   surfaces render the literal prompt, never the chat Markdown projection
   (which can conceal spoilers, link destinations, and image sources). Reject
   Unicode default-ignorable, bidirectional-formatting, and non-layout control
   characters at both the untrusted catalog parser and the Rust persistence /
   import boundary. Do not silently strip them: rejection keeps the reviewed
   string identical to the executed string. New sharing paths must reuse the
   same validation before they persist or activate a definition.
   **Execution archetypes are instance-local snapshots.** The create flow may
   select only an ID returned by the Rust archetype catalog. Rust stores its
   versioned snapshot on the managed instance, composes its prompt addendum at
   effective-config resolution, and passes it through the existing ACP prompt
   transport. Never add this selection to `CreatePersonaInput`, persona hashes,
   or relay publications. Presets may set only supported prompt guidance and
   Buzz-enforced run defaults; they cannot choose an unverified model, alter
   provider credentials, change access policy, or grant tools. Unknown IDs are
   errors, never fallback aliases. Focused critic archetypes are review
   protocols, not a technical read-only sandbox or an independent multi-agent
   round. Any “run critics” orchestration must separately enforce selected
   tool/permission limits, isolate passes, and preserve disagreement; never
   describe prompt guidance as an access boundary. Desktop critic rounds bind
   one explicitly selected, launch-resolved Local route profile to each role
   and persist that identity with the reviewer result. A route profile or
   provider label does not prove distinct model weights or independent output.
   Update both the canonical catalog and this rule when adding or changing
   presets. Project creation may
   pass a selected archetype to its initial managed agents; when it does, create profile-bound
   instances rather than reusing a differently configured instance. This
   snapshots each agent's starting style. A separate project default is
   persisted only in local storage scoped to relay, owner, and home-channel ID;
   new channel-agent batches resolve it against the Rust catalog and create
   profile-bound instances. Never publish the default or snapshot with project
   metadata. Clearing the project default affects future creates only; existing
   instances retain their saved profile. Project creation may also select a
   route only from the Rust-backed saved route catalog. Persist that default
   locally under the same project scope and pin the exact profile ID to eligible
   initial Buzz Agent instances; a missing or unavailable route must not silently
   fall back to the configured provider/model. Other runtimes and provider-backed
   agents ignore the route, and existing instances keep their current route.
13. **Profile runtime sections render only reported agent data.** Missing
   runtime, model, status, command, MCP, advanced, or diagnostics values stay
   absent in every build mode. Do not fill profile or agent-panel gaps with
   development/staging examples, preview controls, or synthetic configuration;
   those values can be mistaken for the viewed agent's real configuration.
   Configuration rows show the effective value regardless of whether it came
   from an explicit choice, global default, config file, or runtime override.
   Do not add provenance lines, shadowed/struck-through values, pre-start
   placeholders, or whole-section dimming; use an em dash for an unknown value.
   Info, activity, agent-configuration, and model-setting rows use the same bare
   16px leading-icon treatment as agent management actions. Keep semantic icons
   visible in profile variants and do not wrap them in background shapes. An
   owned agent profile is entry-point invariant: opening the same deployed
   agent from Agents, a DM, or a channel must expose the same actions, tabs,
   fields, and profile-wide activity selection. Caller context may control the
   panel shell or return navigation, but must not filter or replace profile
   content. Explicit public-key targets are always exact, including stopped,
   archived, and relay-only identities. Only explicit persona navigation may
   select a representative or offer persona Start; a relay persona link cannot
   borrow a local sibling's management controls. See
   [the identity contract](../../../../docs/agent-profile-identity.md).
   Availability dots read relay presence, never a saved deployment
   receipt or runtime status. Failed/disconnected reads are unknown; lifecycle
   actions retain their separate routing. Current exact-key Online/Away presence
   suppresses Start for an inactive local record without granting Stop authority;
   list/profile/member startup guards must not interpret Offline as proof of safe
   startup. Deletion also consumes that same exact-key availability reader:
   unknown requests shutdown when a channel exists, request failure retains the
   record, and only established Offline keeps the intentional no-request path.
   Unqueried persona siblings are unknown. No presence state grants deletion or
   Stop authority; native local stop-before-remove remains independent. See
   [the availability contract](../../../../docs/agent-availability.md).
   The shared cloud marker means “Not managed on this device” only
   after ownership and successful local inventory are known. It does not imply
   hosting location, availability, or permission. Keep all identity surfaces on
   the shared provenance context, without per-row directory subscriptions. See
   [the provenance contract](../../../../docs/agent-management-provenance.md).
14. **Thinking effort has two surfaces: a local-only WRITE control and a
   read-only two-facts DISPLAY.** The write control is `EffortPickerField`
   (`ui/EffortPickerField.tsx`), a self-contained section component mounted in
   `AgentInstanceEditDialog` beside the Model block. It is **Save-gated, not
   direct-write**: the control is fully controlled by the parent dialog
   (`value`/`onChange`) and owns no mutation. The dialog persists the selection
   by embedding `effortLevel` in the locked `update_managed_agent` IPC call, so
   the effort write is atomic with any access-policy change and can never race
   or survive a Cancel or failed Save. There is no standalone
   `persistAgentEffortLevel` setter. Its gating and option compute live in the
   pure helper `ui/effortPicker.ts` (`effortPickerState`): the picker renders
   only when `agent.backend.type === "local"` **AND** a `thought_level`
   `effortConfigId` has been discovered from the running session (absent
   pre-first-session and for runtimes/models without effort support). Local-only
   is load-bearing, not cosmetic — the Rust command rejects non-local backends
   because remote effort is set at deploy time via `policy_env`. Because the
   control reads its inputs from the config surface the dialog already fetches
   (`useAgentConfigSurface`), it integrates into the dialog's existing field
   group without additional IPC. The read-only display is the `thinkingEffort`
   normalized field rendered by `AgentConfigPanel` via `NormalizedRow`, which
   already shows both facts — `field.value` (canonical, the effort the next
   spawn will launch with) and, when a running ACP session differs,
   `field.overriddenValue` struck through (the live session's current effort).
   No component owns "configured vs current" logic; the reader's canonical tier
   ordering feeds both facts. Do not add a second effort write path or restate
   the two-facts logic in a component.

   **Cut invariant — live mid-conversation effort machinery was deliberately
   removed.** Effort is spawn-scoped only: the worker holds one `startup_effort`
   read from `BUZZ_ACP_EFFORT_LEVEL` and applies it once at session creation
   (`apply_startup_effort` in `buzz-acp/src/pool.rs`); there is no pool-level
   effort authority, no live effort switching, and no effort-ack frame. Do not
   reintroduce a live effort-switch RPC, a pool effort field, or a
   mid-conversation effort control without a plan ruling. The archived live-effort
   machinery lives on `archive/claude-config-gaps-live-effort` for reference only.

15. **The persona `description` is public display metadata.** It is optional,
   capped at 280 characters, and validated through the shared visible-text
   policy (`validate_agent_description_text` in `definition_validation.rs`)
   on the raw authored bytes at create/update, snapshot import, publication,
   inbound sync, and the untrusted catalog parser — rejected, never stripped.
   It is deliberately EXCLUDED from `persona_content_hash`
   (`description_change_does_not_change_content_hash`), so a description-only
   edit never flips the restart badge on linked instances. Only the AUTHORED
   description exists — there is deliberately no derived/generated fallback;
   a blank description publishes an empty kind:0 `about`, exactly as before
   the field existed. Agent and team snapshots carry the authored description
   in the member profile's `about` and validate it before import. The trim/empty
   resolution exists twice and must stay in
   sync (port changes in the same PR): `lib/agentDescription.ts`
   (`effectiveAgentDescription`) feeds display surfaces, and its Rust twin
   (`managed_agents/agent_description.rs`, `effective_agent_description` /
   `record_effective_description`) feeds the publish path, where
   `profile_needs_sync` compares `about` (None == empty) so description edits
   reconcile instead of being clobbered. Persona-linked instances do not own a
   second description copy; snapshot export materializes the definition value
   only into the portable snapshot, and a dangling link resolves no description
   rather than reviving stale instance metadata. The agents-page card face shows the
   authored description as its second line, falling back to the model label
   when none exists (`UnifiedAgentsSection.tsx` composes it;
   `AgentIdentityCard` takes a presentational `subtitle`). The community catalog
   shows the same authored description before consent: a clamped two-line list
   subtitle for scanning and the full safely wrapped value in persona detail.
   The dialog field
   lives in `ui/AgentDescriptionField.tsx` (`AgentIdentityFields`), not
   inline in the over-1000-line dialogs.

16. **Owner-only builds constrain managed runtimes, not relay-agent mentions.**
    The compiled owner-only capability applies when Desktop starts or deploys a
    managed agent. Independently operated relay agents with NIP-OA ownership
    remain eligible in every build when their verified owner's signed
    `respond_to` policy admits the viewer and relay membership includes the
    target channel at publication. Owned nonmembers may be offered for preparation
    and Invite; this is not permission to publish. Final authorization refreshes
    the exact destination and retains captured selected identities across uploads
    and edits. Denial preserves the draft, never silently removes a selected key.
    See `docs/remote-mention-routing.md`. Marked builds require that verified owner coordinate but do
    not require it to equal the viewer; OSS builds retain compatibility with
    self-authored legacy directory records. Keep native discovery and send-time
    revalidation fail closed on invalid ownership or managed policy evidence,
    and on missing membership or directory evidence; do not add a cross-owner
    clamp to either mention path. Local `agents-data-changed` events
    refresh only local persona/team/managed-agent caches; they must never
    invalidate the remote relay directory.

17. **Databricks model discovery has one shared catalog authority.** Desktop and ACP call the shared `buzz-agent` discovery library; Desktop passes the effective merged `DATABRICKS_MODEL_FILTER` explicitly, and the library applies it to raw workspace endpoint IDs and Unity Catalog model-service FQNs after the additive union. A successful filtered-empty catalog is authoritative: it stays empty, disables switching, and never falls through to configured or known-model fallback. Verified provider-qualified exact records in `scripts/model-capabilities.json` take precedence for UC FQNs: `data_workflow_tools.goose.goose-claude-opus-5-5` uses Anthropic Messages, adaptive thinking, and `output_config.effort` (default medium). This does not confer capabilities on other namespaces or similarly named services. Uncurated UC FQNs do not inherit family effort capabilities: Claude service components select Anthropic Messages and expose no effort choices, while GPT-5-or-newer service components select OpenAI Responses with neutral fallback effort capabilities; other uncurated FQNs use MLflow Chat Completions. Catalog/schema components never infer routing. Keep exact-record precedence and fallback rules identical in the Rust and TypeScript capability interpreters and shared corpus. Effort inheritance, persistence, and clearing semantics are unchanged. Global Defaults preserves the discovered model ID as the selected value while its closed trigger renders the provider-scoped display label; do not force the raw persisted ID over that label.

18. **ACP transport is persona-owned before deployment.** Select `acp_command` in the persona create/edit form beside the harness. Deployment inherits that value; linked instances do not expose a competing post-deploy override. Legacy definitions without the field use `buzz-acp`; definition-less agents retain their stored command. Switching a linked definition back to stock resets the instance transport on the next spawn. Shared persona events and restart snapshots carry the field so edits apply on the next spawn.

19. **ACP command selection is convention-based.** The editor always offers
    stock `buzz-acp` and installed executable `buzz-*-acp` aliases discovered
    from normal executable search directories. It does not offer arbitrary
    command entry. A persisted value outside that set remains visible as an
    unavailable compatibility option but is not editable; selecting a conventional
    option replaces it. Discovery returns the path produced by the same resolver
    used at spawn, so a duplicate alias must never advertise one executable and
    later launch another. Keep these transitions in the pure
    `ui/acpCommandPicker.ts` helper and preserve persisted values across loading,
    failed discovery, and late candidate arrival. ACP-only selections must mark
    the form dirty, including catalog-update and embedded discard protection.
    Catalog and portable agent/team snapshots carry only stock or conventional
    aliases (ASCII letters, digits, hyphens, and underscores in the middle).
    Foreign artifacts with other command values are rejected; exports omit
    legacy machine-local commands. Owner-native and owner-device synchronization
    retain custom-command compatibility and are not an execution sandbox.
    Shared persona heads redact nonportable commands and emit explicit stock
    for resets; owner replay of a redacted head preserves only a nonportable
    local override. That local path is not synchronized through catalog heads.

20. **DeepSeek is a distinct Buzz Agent API provider.** Keep its provider ID
    `deepseek` exact through prompt-profile matching; do not canonicalize it to
    `openai`. Desktop configuration uses `DEEPSEEK_API_KEY`, optional
    `DEEPSEEK_MODEL`, and optional `DEEPSEEK_BASE_URL`. Discovery queries the
    configured base URL's dynamic `/models` endpoint (defaulting to
    `https://api.deepseek.com/models`) and preserves provider model IDs. Keep
    the provider option, typed credential, readiness requirement, model
    fallback, and prompt-profile model mapping synchronized when changing this
    support.

## Channel-only runtime controls

Desktop observer controls identify a channel, not a thread session. The harness
rejects `cancel_turn` and `switch_model` with `ambiguous_target` when that channel
has multiple known session scopes, including retained idle scopes. Do not treat
that result as success or a deferred model switch. Stop feedback waits for the
harness result matching the control type, channel, and request ID; relay delivery
alone does not prove that a turn was signalled. A missing result is unconfirmed,
not success. The activity pane must use its resolved `sessionChannelId` for
both the outgoing control and result correlation, even without a loaded
`Channel` object. Stop is unavailable in an unscoped all-channel pane.

Per-thread observer controls remain a separate protocol/UI change. Do not tell
users to type `!cancel` beside an inline mention: the owner command requires
kind 9, body exactly `!cancel` after trimming, and the agent's separate `p` tag.
The automatic-mention picker also inserts literal `@Name` into the body, so it
does not provide an exact-command workaround. The UI must state this limitation
rather than offer an ineffective command. An authorized owner can instead use
the CLI with the channel and target thread root:

```sh
buzz messages send --channel <channel-id> --reply-to <thread-root-id> \
  --mention <agent-pubkey> --content '!cancel'
```

## The tests that enforce this

- `lib/agentConfigCore.test.mjs` — field model per harness × scope, clearing
  policy. Update when the capability model changes.
- `ui/agentConfigFieldsContract.test.mjs` — canonical behaviors + disclosure
  presets + `shouldShowModelStatusMessage` status-bypass +
  `shouldRenderModelControl` (successful-empty omit vs failure keep). If this
  fails, you probably reintroduced a per-surface flag or conflated empty with
  failed discovery.
- `ui/usePersonaModelDiscovery.test.mjs` — `synthesizeEmptyDiscoveryStatus`,
  `isCacheableDiscoveryResponse`, `deriveModelDiscoveryPending`,
  `isSuccessfulEmptyDiscovery`. If the "reopen to retry" copy becomes inert
  again, these tests will catch it.
- `ui/respondToFieldContract.test.mjs` — plain-language mode labels, the
  persistent warning contract for shared agent access, and its two render
  positions (after the people picker for `allowlist`).
- `lib/agentAccessWarning.test.mjs` — every mode × run-location copy variant
  plus both resolvers, including unknown-reads-as-local and
  blank-`runOn`-is-not-a-provider.
- `lib/personaCatalogRelay.test.mjs` and
  `ui/personaCatalogOwnerLabel.test.mjs` — reject invisible definition text
  and keep Markdown concealment syntax literal in the review surface.
- `../profile/ui/UserProfileRuntimeContent.test.mjs` — profile runtime panels
  cannot reintroduce build-mode previews or synthetic fallback controls.
- `desktop/tests/e2e/profile.spec.ts` — the owned-agent parity flow compares
  every profile tab when opened from Agents and from the agent's DM.
- `ui/AgentConfigPanelPresentation.test.mjs` — shared profile/agent config rows
  show only effective values, with an em dash for unknown values.
- `ui/acpCommandPicker.test.mjs` — stock/discovered/unavailable command mode,
  late discovery, query-failure compatibility, and conventional replacement of
  persisted unknown commands.
- `ui/effortPicker.test.mjs` — `effortPickerState` gating (local + discovered
  `effortConfigId` renders; provider backend or missing configId hides) and
  option/preselect compute, plus `effortSelectionToPersistedValue` sentinel →
  null. This is where the v4 provider regression is pinned: the write control
  must never render for a provider backend.
- `desktop/tests/e2e/onboarding-agent-defaults.spec.ts` — onboarding behavior
  acceptance coverage for readiness, failure states, defaults, session-draft
  restoration, zero-write Skip, Next save failure/retry, navigation, and
  successful-empty vs failed optional-model discovery.
- `desktop/tests/e2e/agents.spec.ts` — community catalog descriptions remain
  visible in the list and full detail before Add agent, including long
  unbroken Unicode text without horizontal overflow.
- `lib/agentDescription.test.mjs` — authored-description resolution: trim,
  blank/missing → null.
- Rust: `runtime_metadata_env_vars` tests pin spawn-time key application.
- Rust: persona sharing/retention tests pin relay+owner scoping, durable
  enqueue errors, relay rejection/unavailability, and accepted publication.
- Rust: `definition_validation` and inbound persona tests pin the shared
  Unicode/control-character policy at local, import, publish, and sync gates.

## Managed avatar media

Desktop-managed profiles retain the saved persona/instance avatar as the desired
source. Never publish another configured community's authenticated `/media/` URL
verbatim: `relay::profile_avatar::localize_avatar` verifies/copies its bytes into
the caller-pinned target before kind:0 comparison/publication. The shared record
keeps the source URL, not the target projection. Transfer or kind:0 rejection
leaves the previous profile intact so normal reconciliation can retry. The
Agent-managed profiles opt-out still disables automatic reconciliation.

The configured community origin set is refreshed through narrow workspace IPC
before startup restore and when inactive communities change, without resetting
active community state. After source removal, the shared writer may reuse an
already-published target-local picture with the same original content hash;
this is not permission to fetch the removed source. It is not learned from profile URLs. Media transfer
uses the fixed agent signer, origin-scoped Blossom auth, no redirects, byte caps,
and hash/descriptor verification; ordinary public external avatars remain
unauthenticated passthrough. No image-reader proxy or tenant isolation exception.

## Local prompt-profile application

## Local API route profile context fit

The routing library's model-tier presets are draft-only and local. Selecting a
preset fills provider/model and policy fields; it does not save, test a
provider, add credentials, or send prompt data. Hosted presets must keep the
explicit `allow-hosted` policy and disclose provider egress. The local
OpenAI-compatible preset deliberately leaves the model ID blank so the operator
must copy the exact ID from that loopback server's `/v1/models` response before
saving. Do not describe a preset as a live connection or a measured speed
result. Refresh hosted IDs against official provider catalogs, and keep current
pricing operator-editable rather than silently baking it into these drafts.

Buzz Agent API route profiles preserve the legacy ordered-candidate behavior
unless `strict_context_fit` is enabled. In strict mode, every candidate must
have an operator-declared `context_capacity_tokens`; Buzz does not verify that
value against provider metadata. The preflight compares each candidate's
capacity with a conservative UTF-8 upper-bound estimate that includes its
resolved system prompt, current session history, incoming text prompt, tool
definitions, framing allowance, and configured output-token reserve. It is not
exact provider tokenization or guaranteed provider context-window accounting.
Unknown prompt blocks, multimodal history, or opaque provider replay metadata
abstain before the first provider call. The pinned candidate is checked again
before every later tool-round request; if the bound no longer fits, that turn
stops without trying another provider. Safety refusals remain terminal.

The run brief may show the estimate and declared capacity. Keep both labels
explicit, and never describe the operator-entered capacity as provider-verified.

## Local route throughput evidence

The routing-profile library reads measured throughput from the device-local run
journal for the launch-resolved route-profile hash. Keep rows separate by that
profile, candidate, keyed provider endpoint, reasoning effort, and coarse
input-size bucket. Only samples from the last seven days count; show speed as
unknown until five matching samples exist. Label it effective output tokens per
second because it includes provider and network wait, not model decode speed.

Routing keeps the saved preference order unless an operator enables a speed
policy. A minimum speed floor excludes candidates with a known lower rate;
unknown speed abstains unless preference-order warm-up is explicitly enabled.
Fastest-measured mode ranks candidates with five fresh matching samples by
effective speed. When fastest-measured and warm-up are both enabled, unknown
candidates run first in saved preference order until they have enough samples;
then measured candidates rank by speed. Warm-up never admits a known rate below
the configured floor. If fastest-measured is off, warm-up only makes unknown
candidates eligible and saved preference order remains unchanged. Speed is a
latency observation, not a measure of answer quality.

## Local task-fit eligibility evidence

`task_fit_policy` is an opt-in hard candidate gate, not a ranker. It names a
task class and supported taxonomy/evaluation-policy versions, sets a minimum
distinct-task count and 95% Wilson lower bound, and limits report age. It may
also require an observed exact model ID. The gate qualifies only a validated
local report with a current Buzz identity-signed route attestation bound to the
report hash and exact route profile ID/version/hash and candidate ID. The
report's task class, versions, provider, and model must match that policy and
candidate. A generic local report-review attestation records that its signer
reviewed a report; it does not supply the route binding and cannot qualify a
candidate by itself.

When strict task-fit is enabled, missing, invalid, mismatched, stale,
under-sampled, or below-threshold evidence cannot qualify a candidate. If no
candidate passes, routing abstains before its first provider request. The
Desktop task-class metadata is an operator-selected label, not a model-verified
classification. These gates do not authenticate the benchmark producer,
inputs, endpoint, or results, and do not establish a quality gain. Describe
them as a local, operator-configured eligibility policy over unverified
benchmark evidence.

## Global managed-agent memory reserve

The device-local resource policy may set `minAvailableMemoryBytes`. Before
starting a managed local agent, Buzz reads current available system RAM and
blocks that start when the reading falls below the configured reserve. The
reserve is disabled by default and is checked only at start time; it does not
stop existing agents, estimate a child process's memory, monitor GPU memory,
or guarantee that other applications will not consume RAM immediately after
the check. Buzz serializes its own final memory check and process creation so
parallel restore workers do not all pass on the same sample. The settings view
presents available and total system RAM as a separate point-in-time snapshot.

## Local prompt-profile application

Prompt profiles live in the device-local Buzz nest. At launch, a local agent
using the catalogued `buzz-agent` runtime may receive an exact API profile, and
a local DSH ACP agent may receive the exact `acp_harness` target `dsh` profile.
Buzz matches the effective `BUZZ_AGENT_PROVIDER` and model from the layered
spawn environment; a model-specific profile wins over that provider's
provider-wide profile. A Buzz Agent API provider/model key is unique, so edit
the existing profile instead of creating a second one for the same target.
Buzz appends the matched profile text after the saved agent instructions and
marks the process as needing restart when that effective prompt changes.

The ACP run journal records the selected profile ID, version, content hash,
and effective system-prompt hash. It never stores prompt text. For DSH ACP,
compose the profile after saved agent instructions and write a versioned overlay
only inside Buzz's workspace. Launch with `dsh --profile acp` and append
`--patch` only for the matched DSH profile. The overlay replaces the effective
`system-prompt` row config for that Buzz-launched process while leaving DSH's
saved profile files untouched. Disclose that it sets `includeHarnessIdentity`,
`includeRuntimeContext`, and `personaPrefix`, while the prior `personaSuffix`,
`toolOrder`, and unknown custom fields in that row are not preserved or
verified. Preserve other rows. Do not claim DSH accepts ACP `systemPrompt`.
Keep the profile ID, version, prompt hash, and overlay path visible in preview.
CLI harness, consumer-app, remote-agent, unsupported-provider, and unresolved
provider/model profiles remain local artifacts and are not auto-applied.

## Local status summarizer model

`BUZZ_AGENT_SUMMARY_MODEL` is an optional per-agent advanced environment
setting. When present on the catalogued local `buzz-agent` runtime, it exposes
the in-process `summarize_status_evidence` tool; when absent, no extra summary
request can be made. The tool uses the agent's selected provider, API key, and
endpoint, with this model ID as the summary override. `BUZZ_AGENT_SUMMARY_MAX_OUTPUT_TOKENS`
sets its bounded output budget (1–4096 tokens; blank inherits the runtime's
1200-token default). The global defaults editor exposes both settings as
structured controls only when the Rust runtime catalog declares them. The
summary model choices use the same provider's discovered model list, and a
saved value remains editable when it is no longer in that list. Without settled
runtime metadata, existing values stay visible in the generic environment
editor. The evidence is sent to that configured
provider only when the tool is called. The call runs inside the parent agent,
so provider credentials are not added to the child MCP environment. The tool
accepts only one or more structured `thread_brief` pages for the same root,
and it keeps task completion and worker liveness unknown unless the evidence
proves them.

## Keep this file true

**If you change how agent configuration is modeled, rendered, persisted,
applied, or cleared — update this file in the same PR.** A rule that no longer
matches the code is worse than no rule; a new pattern that isn't written down
here will be broken by the next agent that never learns it existed. Reviewers:
treat a config-behavior diff without a matching AGENTS.md diff (or an explicit
"no rules changed" note) as incomplete.
