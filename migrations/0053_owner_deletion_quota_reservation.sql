-- Keep owner quota reservation lookups bounded after relay membership is
-- purged but logical deletion has not yet completed.
SET LOCAL lock_timeout = '5s';

CREATE INDEX community_deletion_requests_owner_quota_reservations
    ON community_deletion_requests (owner_pubkey)
    INCLUDE (community_id)
    WHERE request_origin = 'owner'
      AND stage <> 'aborted'
      AND completed_at IS NULL;
