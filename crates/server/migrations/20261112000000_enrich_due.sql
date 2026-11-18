-- Enrichment work queue (ARCHITECTURE.md D144, docs/ENRICHMENT.md §1).
--
-- `enrich::due` used to anti-join torrent_infohashes with enrich_checked on every batch: with
-- 2.4 M rows in each and the few due rows spread over the key space, that walked both primary
-- keys (2.6 s, 2.56 M buffer hits) once per batch, all day. `enrich_due` holds the infohashes
-- that may need a look, so finding them costs what they number, not the table size.
--
-- The queue is a SUPERSET of "no current check": `due` re-checks each candidate against the old
-- predicate and drops the ones that are not due (row gone, or checked since), so a spurious row
-- is harmless and a missing one is the only bug. Rows get in through:
--   * a new torrent_infohashes row (trigger below; Nostr and DHT alike, every writer),
--   * a DELETE from enrich_checked of a torrent that still exists (trigger below: the forced
--     rechecks of put_torrent, the heal queries, the rematch run),
--   * a matcher upgrade (enrich::sync_queue at the first batch, meta `enrich_queue_version`),
--   * this migration's backfill.
-- Pending-title retries (checked row, `pending_title`, `checked_at` <= dataset import) are NOT
-- queued: the partial index `enrich_checked_pending` already finds them in proportion to their
-- number.
--
-- Locks: CREATE TRIGGER takes SHARE ROW EXCLUSIVE on torrent_infohashes and enrich_checked until
-- the commit, which also makes the backfill race-free (a torrent inserted after the commit is
-- caught by the trigger, none can slip in between). Fail the start loudly instead of queueing
-- behind a stray session. Cost: one anti-join over both tables (the old query without LIMIT),
-- about 3 s warm at 2.4 M rows, tens of seconds on a cold, loaded disk (the old query peaked at
-- 47 s); writers wait for it.
SET LOCAL lock_timeout = '10s';

CREATE SEQUENCE IF NOT EXISTS enrich_due_seq;
CREATE TABLE IF NOT EXISTS enrich_due (
    infohash text COLLATE "C" PRIMARY KEY,
    -- Queue order (oldest first) and a version for the guarded clean-up in `due`: a trigger that
    -- re-queues an infohash gives it a new seq, so a clean-up that read the old one cannot drop it.
    seq      bigint NOT NULL DEFAULT nextval('enrich_due_seq')
) WITH (autovacuum_vacuum_scale_factor = 0.02, autovacuum_vacuum_threshold = 50);
CREATE INDEX IF NOT EXISTS enrich_due_seq_idx ON enrich_due (seq);

CREATE OR REPLACE FUNCTION enrich_due_new() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO enrich_due (infohash)
        SELECT infohash FROM new_rows ORDER BY infohash
        ON CONFLICT (infohash) DO UPDATE SET seq = nextval('enrich_due_seq');
    RETURN NULL;
END
$$;
DROP TRIGGER IF EXISTS enrich_due_new ON torrent_infohashes;
CREATE TRIGGER enrich_due_new AFTER INSERT ON torrent_infohashes
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION enrich_due_new();

CREATE OR REPLACE FUNCTION enrich_due_recheck() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO enrich_due (infohash)
        SELECT o.infohash FROM old_rows o
        WHERE EXISTS (SELECT 1 FROM torrent_infohashes t WHERE t.infohash = o.infohash)
        ORDER BY o.infohash
        ON CONFLICT (infohash) DO UPDATE SET seq = nextval('enrich_due_seq');
    RETURN NULL;
END
$$;
DROP TRIGGER IF EXISTS enrich_due_recheck ON enrich_checked;
CREATE TRIGGER enrich_due_recheck AFTER DELETE ON enrich_checked
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION enrich_due_recheck();

-- Everything with no check at all. Rows checked with an older matcher version are queued by
-- `enrich::sync_queue` (the migration does not know MATCHER_VERSION), pending retries need no
-- queue.
INSERT INTO enrich_due (infohash)
    SELECT t.infohash FROM torrent_infohashes t
    WHERE NOT EXISTS (SELECT 1 FROM enrich_checked c WHERE c.infohash = t.infohash)
    ON CONFLICT (infohash) DO NOTHING;

-- Make the next start sync the matcher version again (a re-run of this file in a test).
DELETE FROM meta WHERE key = 'enrich_queue_version';
