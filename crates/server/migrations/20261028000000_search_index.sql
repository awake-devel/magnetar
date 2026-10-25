-- Search index support (ARCHITECTURE.md D43). The Tantivy index under <state dir>/search-index
-- is derived from these tables; Postgres stays the source of truth.

-- Change log for the index: every write that changes what a search returns for an infohash
-- appends a row here, from triggers, so no write path (store, enrichers, raw SQL) can forget.
-- Append-only, so a writer never waits on a row lock held by another transaction. The indexer
-- reads the oldest rows, re-indexes their infohashes, then deletes exactly the ids it read: a
-- row committed late with a lower id is simply picked up on the next pass.
CREATE TABLE search_changes (
    id        bigserial PRIMARY KEY,
    infohash  text COLLATE "C" NOT NULL,
    queued_at bigint NOT NULL
);

CREATE FUNCTION search_changes_mark(ihs text[]) RETURNS void LANGUAGE sql AS $$
    INSERT INTO search_changes (infohash, queued_at)
    SELECT DISTINCT ih, extract(epoch FROM now())::bigint FROM unnest(ihs) AS ih
    WHERE ih IS NOT NULL
$$;

-- Statement-level triggers with transition tables: one INSERT per statement, however many
-- rows it touched (put_trust updates a publisher's torrents in one statement).
CREATE FUNCTION search_changes_new_rows() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM search_changes_mark(ARRAY(SELECT infohash FROM new_rows));
    RETURN NULL;
END
$$;

CREATE FUNCTION search_changes_old_rows() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM search_changes_mark(ARRAY(SELECT infohash FROM old_rows));
    RETURN NULL;
END
$$;

CREATE TRIGGER search_changes_ins AFTER INSERT ON torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_upd AFTER UPDATE ON torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_del AFTER DELETE ON torrents
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_old_rows();

CREATE TRIGGER search_changes_ins AFTER INSERT ON liveness
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_upd AFTER UPDATE ON liveness
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_del AFTER DELETE ON liveness
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_old_rows();

CREATE TRIGGER search_changes_ins AFTER INSERT ON torrent_media
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_upd AFTER UPDATE ON torrent_media
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_del AFTER DELETE ON torrent_media
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_old_rows();

-- A media row change reaches every linked infohash, but only when a field the index holds
-- changed (the weekly IMDb refresh rewrites votes of every row).
CREATE FUNCTION search_changes_media() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM search_changes_mark(ARRAY(
        SELECT tm.infohash FROM new_rows n
        JOIN old_rows o ON o.id = n.id
        JOIN torrent_media tm ON tm.media_id = n.id
        WHERE (n.kind, n.title, n.year, n.rating, n.genres)
              IS DISTINCT FROM (o.kind, o.title, o.year, o.rating, o.genres)));
    RETURN NULL;
END
$$;
CREATE TRIGGER search_changes_upd AFTER UPDATE ON media
    REFERENCING NEW TABLE AS new_rows OLD TABLE AS old_rows
    FOR EACH STATEMENT EXECUTE FUNCTION search_changes_media();

-- Block/trust overrides are baked into the documents (a blocked publisher's events are left
-- out), so an override change re-indexes that publisher's torrents.
CREATE FUNCTION search_changes_overrides() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM search_changes_mark(ARRAY(
            SELECT t.infohash FROM torrents t WHERE t.pubkey IN (SELECT pubkey FROM old_rows)));
    ELSE
        PERFORM search_changes_mark(ARRAY(
            SELECT t.infohash FROM torrents t WHERE t.pubkey IN (SELECT pubkey FROM new_rows)));
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER search_changes_ins AFTER INSERT ON local_overrides
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_overrides();
CREATE TRIGGER search_changes_upd AFTER UPDATE ON local_overrides
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_overrides();
CREATE TRIGGER search_changes_del AFTER DELETE ON local_overrides
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_overrides();

-- Counters for /api/v1/stats and the crawler's publisher ranking, which used to scan the whole
-- torrents table (wide rows: 3 GB for 466k events, 14 s cold for count(DISTINCT infohash)).
-- torrent_infohashes also gives the full index build a narrow keyset to walk.
CREATE TABLE torrent_infohashes (
    infohash text COLLATE "C" PRIMARY KEY,
    events   integer NOT NULL
);
CREATE TABLE publisher_counts (
    pubkey   text COLLATE "C" PRIMARY KEY,
    torrents integer NOT NULL
);
CREATE INDEX publisher_counts_rank ON publisher_counts (torrents DESC, pubkey);

INSERT INTO torrent_infohashes (infohash, events)
    SELECT infohash, count(*) FROM torrents GROUP BY infohash;
INSERT INTO publisher_counts (pubkey, torrents)
    SELECT pubkey, count(*) FROM torrents GROUP BY pubkey;
INSERT INTO meta (key, value) VALUES ('infohash_count', (SELECT count(*) FROM torrent_infohashes));

CREATE FUNCTION torrent_counts_ins() RETURNS trigger LANGUAGE plpgsql AS $$
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
    END IF;
    INSERT INTO publisher_counts AS pc (pubkey, torrents)
        SELECT pubkey, count(*) FROM new_rows GROUP BY pubkey ORDER BY pubkey
        ON CONFLICT (pubkey) DO UPDATE SET torrents = pc.torrents + EXCLUDED.torrents;
    RETURN NULL;
END
$$;

CREATE FUNCTION torrent_counts_del() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    gone bigint;
BEGIN
    UPDATE torrent_infohashes ti SET events = ti.events - d.n
        FROM (SELECT infohash, count(*) AS n FROM old_rows GROUP BY infohash) d
        WHERE ti.infohash = d.infohash;
    WITH del AS (DELETE FROM torrent_infohashes WHERE events <= 0
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

CREATE TRIGGER torrent_counts_ins AFTER INSERT ON torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION torrent_counts_ins();
CREATE TRIGGER torrent_counts_del AFTER DELETE ON torrents
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION torrent_counts_del();

-- Identifies this database to the index (an index built from another database is rebuilt).
INSERT INTO settings (key, value) VALUES ('search.index_id', to_json(gen_random_uuid()::text)::text)
    ON CONFLICT (key) DO NOTHING;
