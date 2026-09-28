# Channel receipt index deployment

Migration `0052_events_channel_received_index.sql` adds the index behind the
sidebar's receipt window (`received_at >= cutoff ORDER BY received_at DESC, id,
created_at LIMIT n` per channel). `events` is partitioned by `created_at`, so
without this index every sidebar read scans each channel's full history in
every partition and filters on `received_at`. With it, each partition stops
after `n` rows. The index is maintained on every event insert, like the other
channel indexes:

```sql
CREATE INDEX idx_events_community_channel_received
    ON public.events (community_id, channel_id, received_at DESC, id, created_at);
```

Like 0049, the migration bounds lock acquisition and execution time. It builds
the index on fresh or small databases and deliberately fails on a populated
one rather than block ingestion. Brownfield deployments must prebuild first.

## Brownfield procedure

`events` is partitioned, and PostgreSQL does not support `CREATE INDEX
CONCURRENTLY` on a partitioned parent. Create the parent index on the parent
only (catalog-only, invalid until complete), build each partition's index
concurrently, then attach it. None of these statements may run inside a
transaction block.

```sql
CREATE INDEX idx_events_community_channel_received
    ON ONLY public.events (community_id, channel_id, received_at DESC, id, created_at);
```

List the partitions:

```sql
SELECT c.relname FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
WHERE i.inhparent = 'public.events'::regclass ORDER BY 1;
```

For each partition `<p>`:

```sql
CREATE INDEX CONCURRENTLY <p>_channel_received
    ON public.<p> (community_id, channel_id, received_at DESC, id, created_at);
ALTER INDEX public.idx_events_community_channel_received
    ATTACH PARTITION public.<p>_channel_received;
```

The parent becomes valid only after every partition has an attached, valid
index. Partitions created later inherit the index automatically. Verify:

```sql
SELECT i.indisvalid, i.indisready, i.indislive, pg_get_indexdef(i.indexrelid)
FROM pg_index i
JOIN pg_class c ON c.oid = i.indexrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public' AND c.relname = 'idx_events_community_channel_received';
```

Expected flags are all `true`, with this definition:

```text
CREATE INDEX idx_events_community_channel_received ON ONLY public.events USING btree (community_id, channel_id, received_at DESC, id, created_at)
```

Then deploy normally. Migration 0052 skips `CREATE INDEX`, validates the
catalog shape, and records the migration.

## Recovery

A failed concurrent build leaves an invalid partition index, and the parent
stays invalid; migration 0052 rejects both. Drop only the failed partition
index outside a transaction, then repeat that partition's build and attach:

```sql
DROP INDEX CONCURRENTLY IF EXISTS public.<p>_channel_received;
```

Do not replace this with a non-concurrent build on a populated table.
