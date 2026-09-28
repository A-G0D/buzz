-- Newest-received events per channel as one ordered index range. The sidebar's
-- receipt window (received_at >= cutoff ORDER BY received_at DESC, id,
-- created_at LIMIT n) otherwise reads each channel's whole history in every
-- created_at partition and filters on received_at, so its cap bounds evidence
-- but not work. Key order matches that ORDER BY so each partition stops at n.
-- Additive: no ingest, trigger or NIP-RS semantics change.
--
-- Partitioned parent: CREATE INDEX recurses to every partition and future
-- partitions inherit it. CONCURRENTLY is not supported on partitioned parents,
-- so startup is bounded exactly like 0049: a populated table fails deployment
-- instead of blocking ingestion. Brownfield operators MUST prebuild per
-- partition as documented in docs/events-channel-received-deployment.md.
SET LOCAL lock_timeout = '1s';
SET LOCAL statement_timeout = '5s';
DO $$
BEGIN
    -- IF NOT EXISTS would still request a writer-conflicting ShareLock.
    IF to_regclass('public.idx_events_community_channel_received') IS NULL THEN
        CREATE INDEX idx_events_community_channel_received
            ON public.events (community_id, channel_id, received_at DESC, id, created_at);
    END IF;

    -- A partitioned index is valid only once every partition has an attached,
    -- valid child index, so this also rejects an incomplete prebuild.
    IF NOT EXISTS (
        SELECT 1 FROM pg_index i
        JOIN pg_class c ON c.oid = i.indexrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND c.relname = 'idx_events_community_channel_received'
          AND i.indisvalid AND i.indisready AND i.indislive
          AND pg_get_indexdef(i.indexrelid) =
              'CREATE INDEX idx_events_community_channel_received ON ONLY public.events USING btree (community_id, channel_id, received_at DESC, id, created_at)'
    ) THEN
        RAISE EXCEPTION 'idx_events_community_channel_received invalid or wrong definition; see docs/events-channel-received-deployment.md';
    END IF;
END $$;
