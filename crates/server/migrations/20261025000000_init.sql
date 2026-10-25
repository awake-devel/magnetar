-- Magnetar server schema (ARCHITECTURE.md §4-§5, wot-relays.md §9, rust-crates.md §9).
-- Hex ids/pubkeys are lowercase text with the "C" collation so ordering is bytewise.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Every stored event, verbatim (re-served by the relay endpoint and NIP-77).
CREATE TABLE events (
    id          text COLLATE "C" PRIMARY KEY,
    pubkey      text COLLATE "C" NOT NULL,
    kind        integer NOT NULL,
    created_at  bigint NOT NULL CHECK (created_at >= 0),
    raw         text NOT NULL,
    received_at bigint NOT NULL
);
CREATE INDEX events_kind_ts_id ON events (kind, created_at, id);
CREATE INDEX events_pubkey_kind ON events (pubkey, kind, created_at);
CREATE INDEX events_created_at ON events (created_at);

-- First value of every single-letter tag, for REQ #e/#x/#t/... filters.
CREATE TABLE event_tags (
    event_id text COLLATE "C" NOT NULL REFERENCES events (id) ON DELETE CASCADE,
    name     text COLLATE "C" NOT NULL,
    value    text COLLATE "C" NOT NULL,
    PRIMARY KEY (name, value, event_id)
);
CREATE INDEX event_tags_event ON event_tags (event_id);

-- One row per kind-2003 event (D6); grouped by infohash for presentation.
CREATE TABLE torrents (
    event_id      text COLLATE "C" PRIMARY KEY REFERENCES events (id) ON DELETE CASCADE,
    pubkey        text COLLATE "C" NOT NULL,
    created_at    bigint NOT NULL,
    infohash      text COLLATE "C" NOT NULL,
    infohash_v2   text,
    title         text NOT NULL DEFAULT '',
    description   text NOT NULL DEFAULT '',
    files         jsonb NOT NULL DEFAULT '[]',
    file_count    integer NOT NULL DEFAULT 0,
    files_text    text NOT NULL DEFAULT '',
    total_size    bigint,
    trackers      text[] NOT NULL DEFAULT '{}',
    category      text NOT NULL,
    category_path text[] NOT NULL DEFAULT '{}',
    hashtags      text[] NOT NULL DEFAULT '{}',
    external_ids  jsonb NOT NULL DEFAULT '[]',
    source_url    text,
    published_at  bigint,
    pow_bits      smallint NOT NULL DEFAULT 0,
    pow_committed smallint,
    -- clamp(publisher score + pow_bonus(pow_bits)), refreshed by put_trust.
    trust         smallint NOT NULL DEFAULT 0,
    search_tsv    tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('simple', regexp_replace(coalesce(title, ''), '[._\-\[\]()+]+', ' ', 'g')), 'A') ||
        setweight(to_tsvector('simple', regexp_replace(coalesce(files_text, ''), '[._\-\[\]()+/]+', ' ', 'g')), 'B') ||
        setweight(to_tsvector('english', coalesce(description, '')), 'C')
    ) STORED
);
CREATE INDEX torrents_infohash ON torrents (infohash);
CREATE INDEX torrents_pubkey ON torrents (pubkey, created_at);
CREATE INDEX torrents_created_at ON torrents (created_at);
CREATE INDEX torrents_search_tsv ON torrents USING GIN (search_tsv);
CREATE INDEX torrents_title_trgm ON torrents USING GIN (lower(title) gin_trgm_ops);

CREATE TABLE comments (
    event_id         text COLLATE "C" PRIMARY KEY REFERENCES events (id) ON DELETE CASCADE,
    pubkey           text COLLATE "C" NOT NULL,
    created_at       bigint NOT NULL,
    kind             integer NOT NULL,
    torrent_event_id text COLLATE "C" NOT NULL,
    content          text NOT NULL
);
CREATE INDEX comments_torrent ON comments (torrent_event_id, created_at);
CREATE INDEX comments_pubkey ON comments (pubkey);

-- Replaceable heads: newest per (pubkey, kind, d); lowest id wins a created_at tie.
CREATE TABLE replaceable_heads (
    pubkey     text COLLATE "C" NOT NULL,
    kind       integer NOT NULL,
    d          text NOT NULL DEFAULT '',
    created_at bigint NOT NULL,
    event_id   text COLLATE "C" NOT NULL,
    PRIMARY KEY (pubkey, kind, d)
);

CREATE TABLE follows (
    follower text COLLATE "C" NOT NULL,
    followee text COLLATE "C" NOT NULL,
    PRIMARY KEY (follower, followee)
);
CREATE INDEX follows_followee ON follows (followee);

CREATE TABLE mutes (
    muter       text COLLATE "C" NOT NULL,
    target      text COLLATE "C" NOT NULL,
    source_kind integer NOT NULL,
    PRIMARY KEY (muter, target, source_kind)
);

CREATE TABLE reports (
    event_id    text COLLATE "C" PRIMARY KEY,
    reporter    text COLLATE "C" NOT NULL,
    target      text COLLATE "C" NOT NULL,
    report_type text NOT NULL,
    created_at  bigint NOT NULL
);
CREATE INDEX reports_reporter ON reports (reporter);
CREATE INDEX reports_target ON reports (target);

CREATE TABLE relay_lists (
    pubkey text COLLATE "C" NOT NULL,
    url    text NOT NULL,
    read   boolean NOT NULL,
    write  boolean NOT NULL,
    PRIMARY KEY (pubkey, url)
);

CREATE TABLE relay_list_meta (
    pubkey    text COLLATE "C" PRIMARY KEY,
    oversized boolean NOT NULL,
    source    text NOT NULL CHECK (source IN ('nip65', 'kind3_content'))
);

CREATE TABLE profiles (
    pubkey     text COLLATE "C" PRIMARY KEY,
    name       text,
    created_at bigint NOT NULL
);

-- One cached score per publisher (for the configured root, which is stored alongside).
CREATE TABLE trust_cache (
    pubkey        text COLLATE "C" PRIMARY KEY,
    root          text,
    score         smallint NOT NULL,
    factors       jsonb NOT NULL,
    computed_at   bigint NOT NULL,
    graph_version bigint NOT NULL
);

CREATE TABLE local_overrides (
    pubkey     text COLLATE "C" PRIMARY KEY,
    action     text NOT NULL CHECK (action IN ('block', 'trust')),
    created_at bigint NOT NULL
);

CREATE TABLE relays (
    url                  text PRIMARY KEY,
    source               text NOT NULL,
    enabled              boolean NOT NULL DEFAULT true,
    last_ok              bigint,
    last_err             text,
    consecutive_failures integer NOT NULL DEFAULT 0,
    next_try_at          bigint NOT NULL DEFAULT 0,
    events_found         bigint NOT NULL DEFAULT 0,
    supports_neg         boolean
);

CREATE TABLE cursors (
    relay          text NOT NULL,
    kind           integer NOT NULL,
    newest_seen    bigint,
    backfill_until bigint,
    backfill_done  boolean NOT NULL DEFAULT false,
    PRIMARY KEY (relay, kind)
);

CREATE TABLE liveness (
    infohash        text COLLATE "C" PRIMARY KEY,
    seeders         integer,
    leechers        integer,
    completed       integer,
    dht_peers_est   integer,
    source          text NOT NULL,
    trackers_ok     integer NOT NULL,
    trackers_failed integer NOT NULL,
    checked_at      bigint NOT NULL,
    expires_at      bigint NOT NULL
);
CREATE INDEX liveness_expires ON liveness (expires_at);

CREATE TABLE settings (
    key   text PRIMARY KEY,
    value text NOT NULL
);

CREATE TABLE saved_searches (
    id                   bigserial PRIMARY KEY,
    name                 text NOT NULL,
    query                jsonb NOT NULL,
    notify               boolean NOT NULL DEFAULT false,
    last_checked         bigint,
    last_seen_created_at bigint
);

CREATE TABLE meta (
    key   text PRIMARY KEY,
    value bigint NOT NULL
);
INSERT INTO meta (key, value) VALUES ('graph_version', 1);
