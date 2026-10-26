-- Native DHT crawler (ARCHITECTURE.md D49–D55, docs/DHT.md §2.3).
-- Nostr tables and their foreign keys are untouched: DHT torrents live in `dht_torrents`, keyed by
-- infohash, and infohash-level readers move to the `torrent_rows` view.

-- `ADD COLUMN … DEFAULT false` is metadata-only (PG ≥ 11) but takes an ACCESS EXCLUSIVE lock on
-- torrent_infohashes: fail the start loudly instead of queueing behind a stray session.
SET LOCAL lock_timeout = '10s';

-- An infohash row now exists while it has Nostr events OR a DHT row.
ALTER TABLE torrent_infohashes ADD COLUMN dht boolean NOT NULL DEFAULT false;

-- Verified metadata fetched from peers (BEP 9): name and files are bound to the infohash by SHA-1.
CREATE TABLE dht_torrents (
    infohash        text COLLATE "C" PRIMARY KEY,           -- v1, 40 lowercase hex
    infohash_v2     text,                                    -- 64 hex SHA-256, hybrid torrents only
    title           text NOT NULL,                           -- info.name (name.utf-8 preferred)
    files           jsonb NOT NULL DEFAULT '[]',             -- [{path,size}], padding dropped, first 5000
    file_count      integer NOT NULL,                        -- real count (may exceed 5000)
    files_text      text NOT NULL DEFAULT '',                -- paths joined by '\n', first 64 KiB
    total_size      bigint NOT NULL,
    category        text NOT NULL,                           -- core::model::Category snake_case
    metadata_size   integer NOT NULL,
    piece_length    integer,
    first_seen      bigint NOT NULL,                         -- first time the hash reached triage
    fetched_at      bigint NOT NULL,
    discovered_via  text NOT NULL CHECK (discovered_via IN ('sample','announce','get_peers')),
    peers_seen      integer NOT NULL DEFAULT 0,              -- distinct peers known at fetch time
    classifier_version smallint NOT NULL,
    search_tsv      tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('simple', regexp_replace(title, '[._\-\[\]()+]+', ' ', 'g')), 'A') ||
        setweight(to_tsvector('simple', regexp_replace(files_text, '[._\-\[\]()+/]+', ' ', 'g')), 'B')
    ) STORED
);
CREATE INDEX dht_torrents_search_tsv ON dht_torrents USING GIN (search_tsv);  -- SQL fallback (D11)
CREATE INDEX dht_torrents_first_seen ON dht_torrents (first_seen);

-- Hashes the crawler has triaged but not (yet) stored: the persisted fetch queue. Stored ones
-- leave this table (dht_torrents is their record); blocked ones go to blocked_infohashes.
-- status: 0 queued, 2 failed, 3 invalid, 4 private, 5 v2_only, 6 oversize, 7 skipped_known, 8 alias
-- via:    0 sample, 1 announce, 2 get_peers
-- A queued row holds a lease (next_attempt_at = queued time + 1 h): a fetch that never ran
-- (queue overflow, crash) comes due again in the retry loop. NULL = never retried.
CREATE TABLE dht_seen (
    infohash        bytea PRIMARY KEY CHECK (length(infohash) = 20),
    first_seen      bigint NOT NULL,
    last_seen       bigint NOT NULL,                         -- last time it was (re)queued
    via             smallint NOT NULL,
    status          smallint NOT NULL,
    attempts        smallint NOT NULL DEFAULT 0,
    next_attempt_at bigint,
    last_error      text
);
CREATE INDEX dht_seen_retry ON dht_seen (next_attempt_at) WHERE next_attempt_at IS NOT NULL;

-- D53: illegal content. Only the hash, never a name; from any origin.
CREATE TABLE blocked_infohashes (
    infohash     text COLLATE "C" PRIMARY KEY,
    blocked_at   bigint NOT NULL,
    rule_version smallint NOT NULL,
    origin       text NOT NULL CHECK (origin IN ('dht','nostr','sweep'))
);
-- Nostr events rejected by D53 (id only), so NIP-77 does not fetch them again on every sync.
CREATE TABLE blocked_events (
    id         text COLLATE "C" PRIMARY KEY,
    blocked_at bigint NOT NULL
);

-- Counters, schedule and the index log, as torrent_counts_ins / liveness_schedule_ins do for
-- torrents. Same-event triggers fire in name order: dht_counts_ins (creates the infohash row)
-- before dht_schedule_ins (liveness_schedule has an FK to it).
CREATE FUNCTION dht_counts_ins() RETURNS trigger LANGUAGE plpgsql AS $$
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
        PERFORM stats_bump('dht_torrents_added', fresh);
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER dht_counts_ins AFTER INSERT ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION dht_counts_ins();

CREATE FUNCTION dht_schedule_ins() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO liveness_schedule AS s (infohash, born, due_at)
        SELECT infohash, first_seen, -first_seen FROM new_rows ORDER BY infohash
        ON CONFLICT (infohash) DO NOTHING;      -- a Nostr row already scheduled it
    RETURN NULL;
END
$$;
CREATE TRIGGER dht_schedule_ins AFTER INSERT ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION dht_schedule_ins();

CREATE FUNCTION dht_counts_del() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    gone bigint;
BEGIN
    UPDATE torrent_infohashes SET dht = false WHERE infohash IN (SELECT infohash FROM old_rows);
    WITH del AS (DELETE FROM torrent_infohashes WHERE events <= 0 AND NOT dht
                 AND infohash IN (SELECT infohash FROM old_rows) RETURNING 1)
    SELECT count(*) INTO gone FROM del;
    IF gone > 0 THEN
        UPDATE meta SET value = value - gone WHERE key = 'infohash_count';
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER dht_counts_del AFTER DELETE ON dht_torrents
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION dht_counts_del();

CREATE TRIGGER search_changes_ins AFTER INSERT ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_upd AFTER UPDATE ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_del AFTER DELETE ON dht_torrents
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_old_rows();

-- The Nostr delete trigger must not drop an infohash the DHT still holds (same body as
-- 20261028000000_search_index.sql plus "AND NOT dht").
CREATE OR REPLACE FUNCTION torrent_counts_del() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    gone bigint;
BEGIN
    UPDATE torrent_infohashes ti SET events = ti.events - d.n
        FROM (SELECT infohash, count(*) AS n FROM old_rows GROUP BY infohash) d
        WHERE ti.infohash = d.infohash;
    WITH del AS (DELETE FROM torrent_infohashes WHERE events <= 0 AND NOT dht
                 AND infohash IN (SELECT infohash FROM old_rows) RETURNING 1)
    SELECT count(*) INTO gone FROM del;
    IF gone > 0 THEN
        UPDATE meta SET value = value - gone WHERE key = 'infohash_count';
    END IF;
    UPDATE publisher_counts pc SET torrents = pc.torrents - d.n
        FROM (SELECT pubkey, count(*) AS n FROM old_rows GROUP BY pubkey) d
        WHERE pc.pubkey = d.pubkey;
    DELETE FROM publisher_counts WHERE torrents <= 0 AND pubkey IN (SELECT pubkey FROM old_rows);
    RETURN NULL;
END
$$;

-- Infohash-level readers: every source, Nostr rows first in canonical order (trust DESC).
CREATE VIEW torrent_rows AS
    SELECT event_id, pubkey, created_at, infohash, infohash_v2, title, description, files,
           file_count, files_text, total_size, trackers, category, category_path, hashtags,
           external_ids, source_url, published_at, pow_bits, trust, search_tsv,
           'nostr'::text AS origin
    FROM torrents
    UNION ALL
    SELECT ''::text COLLATE "C", ''::text COLLATE "C", first_seen, infohash, infohash_v2, title,
           ''::text, files, file_count, files_text, total_size, '{}'::text[], category,
           '{}'::text[], '{}'::text[], '[]'::jsonb, NULL::text, NULL::bigint, 0::smallint,
           (-1)::smallint,                     -- below every publisher: Nostr wins canonical
           search_tsv, 'dht'::text
    FROM dht_torrents;

INSERT INTO meta (key, value) VALUES ('dht_blocklist_version', 0) ON CONFLICT (key) DO NOTHING;
