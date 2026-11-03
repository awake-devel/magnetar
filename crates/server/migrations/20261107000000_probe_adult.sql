-- Header probes and their backfill include adult and BEP 27 private torrents (ARCHITECTURE.md
-- D119, user decisions of 2026-11-03). The hard blocklist (D53/D68: `blocked_infohashes`, the
-- worker's screen) is unchanged.
--
-- Backfill rows closed as `adult` (at lease time) or `private` (a worker's answer) are queued
-- again exactly as a fresh row is (20261106000000_probe_backfill.sql): due_at = rank (the
-- never-tried order), no attempts, no outcome, no lease. Torrents the fill skipped as category
-- `porn` were never queued: the next fill pass (the first backfill lease after the server
-- starts) queues them.
--
-- Closed rows (due_at NULL) are outside the probe_backfill_due partial index, so this is one
-- sequential pass over the table; only the matching rows are written.

SET LOCAL lock_timeout = '10s';

UPDATE probe_backfill
   SET due_at = rank, attempts = 0, last_outcome = NULL,
       worker = NULL, leased_at = NULL, expires_at = NULL
 WHERE due_at IS NULL AND last_outcome IN ('adult', 'private');
