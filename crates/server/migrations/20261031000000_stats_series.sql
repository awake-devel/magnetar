-- Activity charts (ARCHITECTURE.md D47, docs/STATS.md): one row per metric and hour (UTC).
-- Counters add up within their hour: `torrents_added` and `relays_discovered` by the triggers
-- below, `crawl_events` and `liveness_checks` by the daemons. Gauges (`db_size`) keep the last
-- sample of their hour, written by the stats loop. `torrents_published` holds whole days
-- (bucket = day start) and is recomputed by the stats loop from `liveness_schedule.born`.
CREATE TABLE stats_series (
    metric text NOT NULL,
    bucket bigint NOT NULL,
    value  bigint NOT NULL,
    PRIMARY KEY (metric, bucket)
);

-- Add `n` to the current hour of a counter (transaction time, like every row it counts).
CREATE FUNCTION stats_bump(m text, n bigint) RETURNS void LANGUAGE sql AS $$
    INSERT INTO stats_series AS s (metric, bucket, value)
    VALUES (m, (extract(epoch FROM now())::bigint / 3600) * 3600, n)
    ON CONFLICT (metric, bucket) DO UPDATE SET value = s.value + EXCLUDED.value
$$;

-- New infohashes are counted where `meta.infohash_count` already is (D43's counter trigger),
-- so the two never disagree and no extra trigger runs per insert.
CREATE OR REPLACE FUNCTION torrent_counts_ins() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    fresh bigint;
BEGIN
    WITH up AS (
        INSERT INTO torrent_infohashes AS ti (infohash, events)
        SELECT infohash, count(*) FROM new_rows GROUP BY infohash ORDER BY infohash
        ON CONFLICT (infohash) DO UPDATE SET events = ti.events + EXCLUDED.events
        RETURNING (xmax = 0) AS inserted)
    SELECT count(*) FILTER (WHERE inserted) INTO fresh FROM up;
    IF fresh > 0 THEN
        UPDATE meta SET value = value + fresh WHERE key = 'infohash_count';
        PERFORM stats_bump('torrents_added', fresh);
    END IF;
    INSERT INTO publisher_counts AS pc (pubkey, torrents)
        SELECT pubkey, count(*) FROM new_rows GROUP BY pubkey ORDER BY pubkey
        ON CONFLICT (pubkey) DO UPDATE SET torrents = pc.torrents + EXCLUDED.torrents;
    RETURN NULL;
END
$$;

-- Only rows really inserted are in an INSERT's transition table (not the ON CONFLICT updates).
CREATE FUNCTION stats_relays_ins() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    n bigint;
BEGIN
    SELECT count(*) INTO n FROM new_rows;
    IF n > 0 THEN
        PERFORM stats_bump('relays_discovered', n);
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER stats_relays_ins AFTER INSERT ON relays
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION stats_relays_ins();

-- The counters run from here on. Torrents added before are backfilled once from
-- `events.received_at` by the stats loop (a full scan of `torrents`: not in a migration).
INSERT INTO meta (key, value) VALUES ('stats_since', extract(epoch FROM now())::bigint)
    ON CONFLICT (key) DO NOTHING;
