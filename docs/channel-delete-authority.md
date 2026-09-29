# Channel Delete authority read

`POST /query` accepts an opt-in version-1 `channel_delete_authority` filter.
This is a read-only UI eligibility probe, not a persisted Nostr event, delegated
signing authority, ownership-transfer API or permission token. Kind 9008 remains
the destructive command and performs its existing authorization checks at write
time. A successful read does not protect against subsequent membership changes.

## Discovery and exact request

Host-bound NIP-11 advertises:

```json
{"channel_delete_authority":{"version":1,"community_id":"<host-community-uuid>"}}
```

Unresolved hosts do not advertise the capability. Clients must require version 1
and retain the host-derived community UUID for response validation. Older relays
may ignore unknown filters and return ordinary event arrays; those are **not**
eligibility evidence.

Send exactly one filter through the existing authenticated bridge:

```json
[{"kinds":[9008],"#h":["<canonical-channel-uuid>"],"#p":["<authenticated-viewer-hex>"],"channel_delete_authority":1}]
```

The ordinary bridge Host binding, NIP-98 proof/body validation, admission,
replay guard and relay-membership checks run before this extension. The same
configured development-only auth fallback as other bridge reads still applies.
Unknown fields, mixed filters, unsupported versions and mismatched viewers fail
with 400. The viewer must be an active channel member; a nonmember or unknown
channel returns a generic 403 without ownership or existence data.

## Response and authority

```json
{"channel_delete_authority":1,"community_id":"<host-community-uuid>","pubkey":"<authenticated-viewer-hex>","channel_id":"<canonical-channel-uuid>","can_delete":true}
```

The decision uses the writer's active roster and the same owner predicate as
kind 9008: direct owner, or the human in the persisted community-scoped ownership
mapping for any active owner-role agent. Profile claims are not consulted:
replacing or removing a profile auth tag neither transfers nor revokes the
first-write-wins `users.agent_owner_pubkey` mapping. Archived channels return
false, matching the existing mutation validator's archive guard. The probe is
intentionally narrower than the mutation's existing nonmember-owning-human path;
this does not change that mutation permission.

Database failure or the five-second read deadline returns 503, never a fabricated
false. Clients must bind version/community/viewer/channel, require a boolean,
bound response bytes, and distinguish unavailable authority from denial. They
should retain independently established actions when this optional read fails,
provide retry, and recheck before signing and publishing. The bridge response is
trusted through the configured relay transport; it is not a relay-signed event.

Deploy the relay capability before expecting clients to support owner-agent
Delete. No schema migration or change to kind 9008's wire format is required.

## Validation

`channel_delete_authority_tests.rs` exercises the production router with real
NIP-98 proofs and Postgres/Redis, compares decisions with the actual Delete
validator, and covers profile/mapping disagreement, coowners, demotion/removal,
archived channels, tenant isolation, strict filters, replay and storage failure.
It validates commands without executing deletion. With an isolated test database:

```sh
cargo test -p buzz-relay --lib api::bridge::channel_delete_authority -- --include-ignored
```
