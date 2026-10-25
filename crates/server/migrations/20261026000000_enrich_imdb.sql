-- Metadata enrichment (D40, docs/ENRICHMENT.md): shared media tables + the private IMDb dataset.
-- Enriched data stays on this server: never republished to Nostr (IMDb non-commercial terms).

-- One identified film / series / anime. Rows are created lazily, only for titles some torrent
-- links to (not one per IMDb title). External ids are unique when present.
CREATE TABLE media (
    id            bigserial PRIMARY KEY,
    kind          text NOT NULL CHECK (kind IN ('movie', 'series', 'anime')),
    imdb          text UNIQUE,
    tmdb_movie    integer,
    tmdb_tv       integer,
    tvdb          integer,
    mal           integer,
    anilist       integer,
    anidb         integer,
    title         text NOT NULL,
    original_title text,
    year          integer,
    end_year      integer,
    genres        text[] NOT NULL DEFAULT '{}',
    rating        real,
    votes         integer,
    runtime_min   integer,
    updated_at    bigint NOT NULL
);
CREATE UNIQUE INDEX media_tmdb_movie ON media (tmdb_movie) WHERE tmdb_movie IS NOT NULL;
CREATE UNIQUE INDEX media_tmdb_tv ON media (tmdb_tv) WHERE tmdb_tv IS NOT NULL;
CREATE UNIQUE INDEX media_tvdb ON media (tvdb) WHERE tvdb IS NOT NULL;
CREATE UNIQUE INDEX media_mal ON media (mal) WHERE mal IS NOT NULL;
CREATE UNIQUE INDEX media_anilist ON media (anilist) WHERE anilist IS NOT NULL;
CREATE UNIQUE INDEX media_anidb ON media (anidb) WHERE anidb IS NOT NULL;

-- Which media an infohash is (one link per infohash). method 'manual' is never overwritten.
CREATE TABLE torrent_media (
    infohash        text COLLATE "C" PRIMARY KEY,
    media_id        bigint NOT NULL REFERENCES media ON DELETE CASCADE,
    season          integer,
    episode         integer,
    method          text NOT NULL CHECK (method IN ('itag', 'desc_link', 'offline_title', 'api_search', 'manual')),
    confidence      real NOT NULL,
    matcher_version integer NOT NULL,
    matched_at      bigint NOT NULL
);
CREATE INDEX torrent_media_media ON torrent_media (media_id);

-- Private to the IMDb enricher: infohashes already looked at (matched or not) by a matcher
-- version, so the loop only touches new torrents, a newer matcher or a newer dataset.
-- A new event for the infohash deletes its row (PgStore::put_torrent), so links in a later
-- description are picked up. pending_title: title matching was wanted but found nothing (or no
-- dataset was loaded yet); retried after the next dataset import.
CREATE TABLE enrich_checked (
    infohash        text COLLATE "C" PRIMARY KEY,
    matcher_version integer NOT NULL,
    checked_at      bigint NOT NULL,
    pending_title   boolean NOT NULL DEFAULT false
);
CREATE INDEX enrich_checked_pending ON enrich_checked (checked_at) WHERE pending_title;

-- IMDb non-commercial datasets (datasets.imdbws.com), filtered to movie/tvMovie/video/tvSeries/
-- tvMiniSeries/tvSpecial (non-adult) + ratings. UNLOGGED: rebuilt from the downloaded files,
-- so WAL is skipped on import; after a crash Postgres truncates them and the next import
-- reloads them from the local files (enrich::imdb checks for an empty table).
CREATE UNLOGGED TABLE imdb_titles (
    id             integer PRIMARY KEY,   -- tconst without "tt"
    title_type     smallint NOT NULL,     -- core::enrich::imdb_match::TitleType
    title          text NOT NULL,
    original_title text,
    year           smallint,
    end_year       smallint,
    runtime_min    integer,
    genres         text[] NOT NULL DEFAULT '{}',
    rating         real,
    votes          integer NOT NULL DEFAULT 0
);

-- Normalised names (primaryTitle, originalTitle, optional akas) → title. Exact-match index plus
-- a trigram index for the low-confidence fuzzy fallback.
CREATE UNLOGGED TABLE imdb_names (
    norm   text COLLATE "C" NOT NULL,
    id     integer NOT NULL,
    source smallint NOT NULL              -- 1 primary, 2 original, 3 aka
);
CREATE INDEX imdb_names_norm ON imdb_names (norm);
CREATE INDEX imdb_names_trgm ON imdb_names USING GIN (norm gin_trgm_ops);

-- tvEpisode → parent series, so an i-tag / link to an episode resolves to its series.
-- ~9.7M episodes packed by tconst >> 8: one row per bucket, `data` = 9-byte records
-- [tconst & 0xff: u8][parent: u32 LE][season: i16 LE][episode: i16 LE] (i16::MIN = unknown).
-- 1 row per episode with a btree took 600 MB; packed it is ~10x smaller (docs/ENRICHMENT.md).
CREATE UNLOGGED TABLE imdb_episodes (
    bucket integer PRIMARY KEY,
    data   bytea NOT NULL
);

-- Last import per dataset file.
CREATE TABLE imdb_datasets (
    name          text PRIMARY KEY,       -- e.g. 'title.basics'
    last_modified text,                   -- HTTP Last-Modified of the imported file
    bytes         bigint NOT NULL,
    rows          bigint NOT NULL,
    imported_at   bigint NOT NULL
);
