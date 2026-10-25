-- TEST-ONLY mirror of the shared enrichment contract tables owned by enrich/imdb (D40).
-- Used by tests/common::test_db only while no migration in this branch creates `media`
-- (enrich/tmdb is developed in parallel); once imdb's migration is merged it is skipped.
CREATE TABLE media (
    id             bigserial PRIMARY KEY,
    kind           text NOT NULL CHECK (kind IN ('movie', 'series', 'anime')),
    imdb           text UNIQUE,
    tmdb_movie     integer,
    tmdb_tv        integer,
    tvdb           integer,
    mal            integer,
    anilist        integer,
    anidb          integer,
    title          text NOT NULL,
    original_title text,
    year           integer,
    end_year       integer,
    genres         text[] NOT NULL DEFAULT '{}',
    rating         real,
    votes          integer,
    runtime_min    integer,
    updated_at     bigint NOT NULL
);
CREATE UNIQUE INDEX media_tmdb_movie ON media (tmdb_movie) WHERE tmdb_movie IS NOT NULL;
CREATE UNIQUE INDEX media_tmdb_tv ON media (tmdb_tv) WHERE tmdb_tv IS NOT NULL;
CREATE UNIQUE INDEX media_tvdb ON media (tvdb) WHERE tvdb IS NOT NULL;
CREATE UNIQUE INDEX media_mal ON media (mal) WHERE mal IS NOT NULL;
CREATE UNIQUE INDEX media_anilist ON media (anilist) WHERE anilist IS NOT NULL;
CREATE UNIQUE INDEX media_anidb ON media (anidb) WHERE anidb IS NOT NULL;

CREATE TABLE torrent_media (
    infohash        text COLLATE "C" PRIMARY KEY,
    media_id        bigint NOT NULL REFERENCES media ON DELETE CASCADE,
    season          integer,
    episode         integer,
    method          text NOT NULL
                    CHECK (method IN ('itag', 'desc_link', 'offline_title', 'api_search', 'manual')),
    confidence      real NOT NULL,
    matcher_version integer NOT NULL,
    matched_at      bigint NOT NULL
);
CREATE INDEX torrent_media_media ON torrent_media (media_id);
