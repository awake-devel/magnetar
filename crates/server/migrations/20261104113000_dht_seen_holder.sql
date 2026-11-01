-- Fleet trust (ARCHITECTURE.md D113, docs/WORKERS.md §7): who holds a queued DHT hash.
--
-- `holder` is the crawl worker a queued `dht_seen` row was handed to by its triage; NULL = the
-- server's own crawler (its triage and its retry loop). An upload (info dict or status report)
-- from a worker is applied only to rows it holds, so a worker cannot inject a dict for a hash
-- it was never given, nor finalise another worker's or the server's hashes.
--
-- Worker block reports are recorded under origin 'worker' (was 'dht'), so the hashes blocked on
-- a worker's word can be told apart and cleared if a worker token leaks.

-- Metadata-only column; the check is re-validated on a small table. Both lock briefly.
SET LOCAL lock_timeout = '10s';

ALTER TABLE dht_seen ADD COLUMN holder text;

ALTER TABLE blocked_infohashes DROP CONSTRAINT IF EXISTS blocked_infohashes_origin_check;
ALTER TABLE blocked_infohashes ADD CONSTRAINT blocked_infohashes_origin_check
    CHECK (origin IN ('dht', 'nostr', 'sweep', 'worker')) NOT VALID;
ALTER TABLE blocked_infohashes VALIDATE CONSTRAINT blocked_infohashes_origin_check;
