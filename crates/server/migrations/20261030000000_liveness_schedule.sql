-- Seed checks at scale (ARCHITECTURE.md D45, docs/LIVENESS.md): a schedule row per infohash
-- that the checker picks due rows from through one index, and a downsampled history.

-- `due_at` is when the next check is due. A never-checked infohash has `due_at = -born`: due at
-- once, newest first, and ahead of every re-check (those are positive). `prio` picks the lane:
-- 3 viewed (detail page), 2 matches a saved search, 1 in search results, 0 the rest; a boost
-- lasts until `hot_until`, the first check after that puts the row back in lane 0.
CREATE TABLE liveness_schedule (
    infohash     text COLLATE "C" PRIMARY KEY
                 REFERENCES torrent_infohashes (infohash) ON DELETE CASCADE,
    born         bigint NOT NULL,
    due_at       bigint NOT NULL,
    prio         smallint NOT NULL DEFAULT 0,
    hot_until    bigint,
    checked_at   bigint,
    dead_streak  smallint NOT NULL DEFAULT 0,
    error_streak smallint NOT NULL DEFAULT 0,
    history_at   bigint
);
-- The due query reads each lane in due order and stops at the batch size.
CREATE INDEX liveness_schedule_due ON liveness_schedule (prio, due_at);

INSERT INTO liveness_schedule (infohash, born, due_at, checked_at, dead_streak, error_streak)
SELECT b.infohash, b.born, coalesce(l.expires_at, -b.born), l.checked_at,
       CASE WHEN greatest(l.seeders, l.dht_peers_est) = 0 THEN 1 ELSE 0 END,
       CASE WHEN l.source = 'none' THEN 1 ELSE 0 END
FROM (SELECT infohash, min(created_at) AS born FROM torrents GROUP BY infohash) b
JOIN torrent_infohashes ti ON ti.infohash = b.infohash
LEFT JOIN liveness l ON l.infohash = b.infohash;

-- New infohashes are scheduled by the insert itself; an earlier event of a known one moves
-- `born` back (and, while never checked, its place in the queue). Runs after
-- `torrent_counts_ins` (triggers fire in name order), which creates the torrent_infohashes row.
CREATE FUNCTION liveness_schedule_ins() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO liveness_schedule AS s (infohash, born, due_at)
        SELECT infohash, min(created_at), -min(created_at) FROM new_rows
        GROUP BY infohash ORDER BY infohash
        ON CONFLICT (infohash) DO UPDATE SET born = EXCLUDED.born,
            due_at = CASE WHEN s.checked_at IS NULL THEN EXCLUDED.due_at ELSE s.due_at END
        WHERE EXCLUDED.born < s.born;
    RETURN NULL;
END
$$;
CREATE TRIGGER torrent_schedule_ins AFTER INSERT ON torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION liveness_schedule_ins();

-- Seeders/leechers over time: at most one point per infohash every ~6 h, unchanged values at
-- most weekly, kept 90 days (pruned by the checker). `at` grows with time, so a BRIN index
-- serves the pruning for a few pages.
CREATE TABLE liveness_history (
    infohash  text COLLATE "C" NOT NULL
              REFERENCES torrent_infohashes (infohash) ON DELETE CASCADE,
    at        bigint NOT NULL,
    seeders   integer,
    leechers  integer,
    peers_est integer,
    PRIMARY KEY (infohash, at)
);
CREATE INDEX liveness_history_at ON liveness_history USING brin (at);

-- The answers we already have are the first points.
INSERT INTO liveness_history (infohash, at, seeders, leechers, peers_est)
SELECT l.infohash, l.checked_at, l.seeders, l.leechers, l.dht_peers_est
FROM liveness l JOIN torrent_infohashes ti ON ti.infohash = l.infohash
WHERE l.source <> 'none';
UPDATE liveness_schedule s SET history_at = l.checked_at
FROM liveness l WHERE l.infohash = s.infohash AND l.source <> 'none';
