-- Fleet trust, round 2 (ARCHITECTURE.md D113, docs/WORKERS.md §1 item 6 and §7).
--
-- `dht_torrents.supplied_by`: the crawl worker whose uploaded info dict a row was stored from
-- (NULL = the server's own crawler, and every row stored before this migration). A worker token
-- holder can still triage hashes of its choosing and upload dicts it made for them (within the
-- hourly cap, MAGNETAR_WORKER_TRIAGE_NEW_PER_HOUR); after a token leak
-- `DELETE FROM dht_torrents WHERE supplied_by = '<worker>'` removes what it added (counts and
-- the search index follow through the existing triggers).

-- Nullable column without a default: metadata only, no table rewrite.
SET LOCAL lock_timeout = '10s';

ALTER TABLE dht_torrents ADD COLUMN supplied_by text;
