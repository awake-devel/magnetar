-- Header-probe backfill (ARCHITECTURE.md D118, docs/PROBES.md §10): torrents indexed before
-- probes existed (DHT rows and Nostr-only rows) are probed by crawl workers that lease them,
-- fetch the info dict again from the swarm (verified against the infohash) and probe through
-- the same connection.
--
-- One row per infohash still to probe. The server fills it in the background from
-- `torrent_infohashes` (keyset walk, `server::probe_backfill::fill_pass`), skipping rows that
-- already have a probe, blocked hashes and category `porn`; the full adult rule (the worker's
-- `Classification::adult`) is applied when a row is leased.
--
-- `due_at` is the one ordering key, as `liveness_schedule.due_at` (D45):
--   * negative = never tried: `rank`, i.e. -(seeders tier × 10^10 + born), so torrents with
--     the most known seeders come first, then the newest; never-tried rows all come before
--     any retry;
--   * positive = leased until then, or waiting for a retry (backoff after no peers, a failed
--     fetch or a probe that did not land);
--   * NULL = closed (adult, blocked, private, gone, or every attempt used): never leased again.
-- A probe stored for the infohash (by backfill or by a new-find probe) deletes the row.
--
-- The lease is the row itself (`worker`, `expires_at`): an unanswered lease needs no cleanup,
-- its `due_at` is the lease's end. Results are accepted from the worker the row was leased to
-- (D113 hold rule); recording one clears `worker`, so a hash is answered once per lease.
-- No foreign key: a torrent deleted meanwhile is closed (`gone`) when it is next leased.

SET LOCAL lock_timeout = '10s';

CREATE TABLE probe_backfill (
    infohash     text COLLATE "C" PRIMARY KEY,
    rank         bigint NOT NULL,          -- the never-tried due_at (negative), kept for release
    due_at       bigint,
    attempts     smallint NOT NULL DEFAULT 0,
    last_outcome text,                     -- no_peers, connect, timeout, no_probe, adult, ...
    worker       text,                     -- lease holder, NULL once answered
    leased_at    bigint,
    expires_at   bigint
);
-- The lease reads due rows in due order and stops at the batch size.
CREATE INDEX probe_backfill_due ON probe_backfill (due_at) WHERE due_at IS NOT NULL;

-- A backfill dict for a Nostr-only torrent inserts a dht_torrents row that is not a DHT
-- discovery. The backfill writer marks its transaction (`SET LOCAL`-scoped
-- `magnetar.origin = 'backfill'`, `server::dht::persist::persist_backfill_batch`); these
-- triggers then count the rows under `probe_backfill_dht_rows` instead of
-- `dht_torrents_added` and `dht_stored_<via>`. Bodies otherwise as 20261101000000_dht.sql and
-- 20261103000000_crawl_map.sql.
CREATE OR REPLACE FUNCTION dht_counts_ins() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    fresh bigint;
BEGIN
    WITH up AS (
        INSERT INTO torrent_infohashes AS ti (infohash, events, dht)
        SELECT infohash, 0, true FROM new_rows ORDER BY infohash
        ON CONFLICT (infohash) DO UPDATE SET dht = true
        RETURNING (xmax = 0) AS inserted)
    SELECT count(*) FILTER (WHERE inserted) INTO fresh FROM up;
    IF fresh > 0 THEN
        UPDATE meta SET value = value + fresh WHERE key = 'infohash_count';
        PERFORM stats_bump('torrents_added', fresh);
    END IF;
    -- A batch whose rows all hit ON CONFLICT DO NOTHING still fires this statement trigger with
    -- an empty transition table: no zero rows in stats_series.
    SELECT count(*) INTO fresh FROM new_rows;
    IF fresh > 0 THEN
        IF current_setting('magnetar.origin', true) IS NOT DISTINCT FROM 'backfill' THEN
            PERFORM stats_bump('probe_backfill_dht_rows', fresh);
        ELSE
            PERFORM stats_bump('dht_torrents_added', fresh);
        END IF;
    END IF;
    RETURN NULL;
END
$$;

CREATE OR REPLACE FUNCTION dht_stored_via_ins() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    r record;
BEGIN
    IF current_setting('magnetar.origin', true) IS NOT DISTINCT FROM 'backfill' THEN
        RETURN NULL;
    END IF;
    FOR r IN SELECT discovered_via, count(*) AS n FROM new_rows GROUP BY 1 ORDER BY 1 LOOP
        PERFORM stats_bump('dht_stored_' || r.discovered_via, r.n);
    END LOOP;
    RETURN NULL;
END
$$;
