# Private read-state accessory API

`BUZZ_V1_ENABLED=true` opts into `/buzz/v1`; it is disabled by default.
Conversation history, live events, edits and deletion remain Nostr-authoritative.
This API neither replaces Nostr reads nor writes artificial signed events.
Legacy NIP-RS continues unchanged, but does not synchronize with these tables.

## Discovery and identity

On a known community host, NIP-11 (`GET /` with `Accept:
application/nostr+json`, or `GET /info`) includes `buzz_v1` only when enabled:

```json
{"buzz_v1":{"version":1,"base_path":"/buzz/v1","retention_seconds":2592000,
"max_channels":20,"max_intents":100,"max_contexts":20,"max_context_messages":100}}
```

Use the requesting origin plus this relative prefix. Discovery is a configured
capability, not a promise that the next request cannot fail. Unknown hosts and
disabled deployments omit it. Capability loss must not restart NIP-RS writes.

Every API request requires NIP-98, including on development relays. Sign the
exact externally addressed URL, including the encoded query, and method. POST
also requires the SHA-256 payload tag for the exact body bytes. Each retry needs
fresh authorization; replay protection is shared with the bridge. Applicable
NIP-FI admission is enforced and its asserted key must match the request signer.
The host chooses the community; the signer chooses `/me`. NIP-OA admission does
not grant access to the owner's personal state. Relay membership, bans and
resource access are enforced; moderation timeouts do not prohibit reading.

All API responses are `Cache-Control: private, no-store`. Errors use
`{"error":{"code":"invalid_request","request_id":"..."}}`, with 400 invalid,
401 unauthorized/replay, 403 forbidden, 404 unavailable capability/host/path,
429 rate limited or 503 temporarily unavailable. 429/503 include `Retry-After`.
Unknown request fields are rejected. Never turn a transport failure into read.

## Sidebar

`GET /buzz/v1/me/sidebar?limit=20&cursor=<exclusive-channel-uuid>` returns
`account`, `channels`, and `next_cursor`. Omit the cursor on the first request.
Each channel includes identity/name/type, archived and hidden flags, `unread`,
`attention`, `latest_message_id`, and `latest_message_complete`.
Only joined, nondeleted channels are listed. Hidden/archived presentation remains
client-owned. Each page has a writer-consistent snapshot; separate pages do not
share a snapshot, and an unfinished traversal cannot prove channel removal.

Counts have exactly three representations:

```json
[{"status":"exact","value":0},{"status":"at_least","value":7},{"status":"unknown"}]
```

Only exact zero proves absence. Unknown has no numeric value. Ordinary unread
counts eligible non-own, nondeleted conversation kinds 9, 40002, 45001 and 45003
beyond the matching context frontier. Attention is the unread subset consisting
of DMs, direct actor mentions, `broadcast=1`, and replies in a thread containing
a live eligible message authored by the actor. Participation is independent of
read progress and retention. This is not Desktop notification policy: follows,
mutes and earlier mentions elsewhere in a thread do not affect this count.

The receipt-time horizon defaults to 30 days (`BUZZ_V1_RETENTION_SECONDS`). It
filters unread/attention, not latest activity, event storage or frontier state.
A later configuration expansion can change counts without having lost progress.
Latest activity is independent of actor and frontiers. A null latest ID proves
an empty eligible history only when `latest_message_complete=true`.

## Explicit contexts

`GET /buzz/v1/me/read-state?targets=<URL-encoded-JSON-array>` accepts up to 20
contexts and 100 total concrete message selectors. It is not event history or a
global export of frontiers. Example decoded `targets`:

```json
[{"target":{"channel_id":"<uuid>","root_id":"<64-hex-root>"},
  "message_ids":["<64-hex-event>"]}]
```

Omitting `root_id` selects the channel timeline. The result contains `account`
and one `contexts` entry per request entry, in order. Context status is
`available` (with nullable `through_timestamp` and `messages`), `unknown`, or
`unavailable`. Message status is `read`, `not_counted`, `unread` (with nullable
`attention`), `unknown`, or `unavailable`. Wrong-context, missing and forbidden
selectors share unavailable. Null attention means participation is unproved.
Conversation bytes must still come from the existing Nostr path.

## Fixed-operand writes

`POST /buzz/v1/me/read-state` accepts 1–100 independent intents:

```json
{"intents":[
 {"type":"mark_through","target":{"channel_id":"<uuid>"},"message_id":"<64-hex-event>"},
 {"type":"legacy_prefix","target":{"channel_id":"<uuid>","root_id":"<64-hex-root>"},"through_timestamp":1700000000},
 {"type":"complete_import"}
]}
```

Each intent commits atomically and returns its own `applied`, `blocked` or
`invalid` outcome. An ambiguous timeout/storage failure returns
`{"status":"unknown","retryable":true}`. Earlier committed outcomes survive
later failures. `projection_status` is `not_requested`; a successful write does
not assert a client has refreshed. Retry the same operands, never substitute
latest. Keep pending intent durably on the client until its outcome is resolved.

A mark-through validates a fixed message and advances its context's monotone
second-resolution author-time prefix. Equal-time messages and later-arriving
backdated messages at/below it are covered. Channel and thread frontiers never
inherit in either direction. Opening a view is not itself a reading action;
client dwell/focus policy determines when to send an actual observed anchor.
Old or deleted valid anchors may advance a frontier. Legacy prefixes preserve
the original nonnegative timestamp; more than DB-now + 900 seconds is invalid,
not clamped. Complete-import is a client declaration, not proof that the server
verified or decrypted a legacy snapshot. Null `imported_at_ms` means provisional.

Sparse legacy seen hints must not be converted to the largest timestamp: that
would acknowledge unseen holes. The client retains recovery data and discloses
unsupported hints/manual overrides before declaring import complete. Manual
unread remains device-local. There is no automatic dual-authority rollback.

## Bounds and deployment

- 20 sidebar rows, 100 intents, 20 contexts / 100 selectors per request.
- 64 KiB write body; 16 KiB context URL; 1 MiB serialized API response.
- 4096 raw receipts per channel plus one exhaustion sentinel, before eligibility.
- Latest activity probes 256 events plus a sentinel; long ineligible tails may
  leave latest incomplete even when unread is exact.
- Tag documents over 8192 bytes or malformed relevant tags yield uncertainty.
  Compact boolean facts cross the database boundary, never raw tag payloads.
- Optional participation: at most 1024 unique roots and 257 metadata candidates
  per root, with a 500 ms savepoint budget. Budget exhaustion preserves ordinary
  counts and leaves unproved attention unknown/lower-bound.
- DB statement/lock deadlines and HTTP read deadlines bound work; writes use a
  shared eight-second intent-processing deadline after admission. Limits are
  containment, not a production capacity claim.

Apply migrations 0051 and 0052 (or the equivalent desired schema). Brownfield
operators must prebuild the receipt index using
[the concurrent deployment procedure](events-channel-received-deployment.md).
Do not run an unbounded blocking index build on a large events table. No new
per-message ingest write path or stored unread counters are introduced.

Use existing HTTP route/status/latency metrics for `/buzz/v1/me/sidebar` and
`/buzz/v1/me/read-state`, plus database pool/statement metrics. Inspect exact /
lower-bound / unknown proportions in controlled acceptance captures; no payload,
actor, channel or frontier values should become metric labels. The measured
local seed is not a DAU/concurrency or p95/p99 production acceptance result.

## Compatibility and extension rules

Within `/buzz/v1`, clients must ignore unknown response object fields. Existing
required fields, status variants and their meanings remain stable; additive
fields do not authorize silently changing `attention` or frontier semantics.
Breaking changes require an explicitly negotiated contract or a new API version.
Requests remain strict: send new parameters or intent types only after the relay
advertises the corresponding capability. Missing optional data means unsupported
or not requested, never an empty list, zero count or unchanged revision.

Follow-up design constraints are recorded in
[the extension design note](buzz-v1-extension-design.md); they do not advertise
additional capabilities.

## Privacy and lifecycle

These typed relational frontiers are signer-private application state, **not
self-encrypted**. Database operators can see reading progress; ordinary Nostr
queries, search and moderator interfaces do not expose it. No public receipts
are emitted. Storage grows by touched contexts, not observed messages, and has
no fixed context-count ceiling.

Leaving/rejoining does not erase progress; revoked access hides it. Soft-deleted
channels are inaccessible, while hard channel deletion cascades their frontiers.
Deleting an account row cascades that actor's frontiers in the same community;
community erasure inventories both tables under the existing write fence. There
is no new public account export/reset endpoint. Operator-assisted erasure/export
must use the established authenticated operational process and explicitly scope
both community and actor; never equate the read-time horizon with data erasure.

Roll out disabled-by-default to controlled accounts after agent and human live
acceptance. The client migration is separate work. Disabling the API leaves
migrated clients stale with their pending journal intact; it cannot silently
restore the old encrypted snapshot as current authority.
