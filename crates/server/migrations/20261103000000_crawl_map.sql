-- Crawl map (ARCHITECTURE.md D101–D105, docs/CRAWLMAP.md): where discoveries come from.
--
-- PRIVACY (D101): no column below holds an IP address, raw or hashed. Addresses are resolved
-- to (country, AS number) in memory when counted and dropped; only those codes and aggregate
-- counters are written. `tests/it/crawl_map.rs` checks this schema for address-like columns.

-- Metadata-only changes (nullable / constant default), but they lock the tables briefly.
SET LOCAL lock_timeout = '10s';

-- Country / AS number of the node that told us about a hash (sampled node, querier,
-- announcer), resolved at triage; read when the hash is stored, to count "stored" per country
-- and network. NULL = not resolved (geo off, or rows older than this migration); '' / 0 =
-- resolved but not in the database.
ALTER TABLE dht_seen ADD COLUMN src_cc text, ADD COLUMN src_asn integer;

-- Relays: new torrent events first delivered (kind 2003, counted from here on) and where the
-- relay's host resolved to at the last check (no address kept).
ALTER TABLE relays
    ADD COLUMN torrents_found bigint NOT NULL DEFAULT 0,
    ADD COLUMN geo_cc text,
    ADD COLUMN geo_asn integer,
    ADD COLUMN geo_checked_at bigint;

-- Aggregate counters by country and network. layer: 'dht' | 'nostr'; metric: dht 'hashes',
-- 'stored', 'nodes'; nostr 'events', 'torrents'. cc '' / asn 0 = unknown. Hourly rows are kept
-- 49 h (the "last 24 h" window); the totals since counting started are kept forever.
CREATE TABLE geo_counts_hourly (
    layer  text   NOT NULL,
    metric text   NOT NULL,
    bucket bigint NOT NULL,
    cc     text   NOT NULL,
    asn    integer NOT NULL,
    value  bigint NOT NULL,
    PRIMARY KEY (layer, metric, bucket, cc, asn)
);
CREATE INDEX geo_counts_hourly_bucket ON geo_counts_hourly (bucket);

CREATE TABLE geo_counts_total (
    layer  text   NOT NULL,
    metric text   NOT NULL,
    cc     text   NOT NULL,
    asn    integer NOT NULL,
    value  bigint NOT NULL,
    PRIMARY KEY (layer, metric, cc, asn)
);

-- AS organisation names (public registry data from the ASN database), by AS number.
CREATE TABLE geo_as_names (
    asn        integer PRIMARY KEY,
    org        text   NOT NULL,
    updated_at bigint NOT NULL
);

-- Per-route hourly counters for the "where discoveries come from" card (stats_series):
-- dht_seen_<via> = hashes first triaged through a route (new dht_seen rows; rows re-queued
-- by ON CONFLICT are not in an INSERT's transition table), dht_stored_<via> = DHT torrents
-- stored, by discovered_via.
CREATE FUNCTION dht_seen_via_ins() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    r record;
BEGIN
    FOR r IN SELECT via, count(*) AS n FROM new_rows GROUP BY via ORDER BY via LOOP
        PERFORM stats_bump(CASE r.via WHEN 0 THEN 'dht_seen_sample' WHEN 1 THEN 'dht_seen_announce'
                                      ELSE 'dht_seen_get_peers' END, r.n);
    END LOOP;
    RETURN NULL;
END
$$;
CREATE TRIGGER dht_seen_via_ins AFTER INSERT ON dht_seen
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION dht_seen_via_ins();

CREATE FUNCTION dht_stored_via_ins() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    r record;
BEGIN
    FOR r IN SELECT discovered_via, count(*) AS n FROM new_rows GROUP BY 1 ORDER BY 1 LOOP
        PERFORM stats_bump('dht_stored_' || r.discovered_via, r.n);
    END LOOP;
    RETURN NULL;
END
$$;
CREATE TRIGGER dht_stored_via_ins AFTER INSERT ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION dht_stored_via_ins();

INSERT INTO meta (key, value) VALUES ('crawl_map_since', extract(epoch FROM now())::bigint)
    ON CONFLICT (key) DO NOTHING;
