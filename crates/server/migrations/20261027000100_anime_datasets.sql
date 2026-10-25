-- Offline anime datasets (D41, docs/ENRICHMENT-ANIME.md). Private to crates/server/src/enrich_anime.rs.
-- Matches are written to the shared media / torrent_media tables (created by the IMDb migration);
-- nothing here references them, so this file does not depend on migration order.

-- anime-offline-database (cedya77 continuation of manami-project; ODbL 1.0 + DbCL 1.0).
-- Replaced wholesale on every import; id = position in the imported file.
CREATE TABLE anime_entries (
    id            integer PRIMARY KEY,
    mal           integer,
    anilist       integer,
    anidb         integer,
    kitsu         integer,
    title         text NOT NULL,
    synonyms      text[] NOT NULL DEFAULT '{}',
    kind          text NOT NULL,
    episodes      integer,
    season        text,
    year          integer,
    status        text,
    score         real,
    duration_secs integer,
    tags          text[] NOT NULL DEFAULT '{}',
    picture       text
);
CREATE INDEX anime_entries_mal ON anime_entries (mal) WHERE mal IS NOT NULL;

-- Fribb anime-lists anime-list-full.json: entry → TVDB / TMDB / IMDb with season and offset.
CREATE TABLE anime_mappings (
    id          integer PRIMARY KEY,
    kind        text,
    mal         integer,
    anilist     integer,
    anidb       integer,
    imdb        text[] NOT NULL DEFAULT '{}',
    tmdb_tv     integer,
    tmdb_movie  integer[] NOT NULL DEFAULT '{}',
    tvdb        integer,
    season_tvdb integer,
    season_tmdb integer,
    offset_tvdb integer,
    offset_tmdb integer
);

-- One row per dataset ('aod', 'fribb'): what was imported and the validators for conditional GET.
CREATE TABLE anime_datasets (
    name          text PRIMARY KEY,
    source        text NOT NULL,
    version       text,
    etag          text,
    last_modified text,
    sha256        text NOT NULL,
    rows          integer NOT NULL,
    imported_at   bigint NOT NULL,
    checked_at    bigint NOT NULL
);

-- Torrents the anime matcher has looked at, so unmatched ones are retried only when the
-- datasets or the matcher change.
CREATE TABLE anime_attempts (
    infohash        text COLLATE "C" PRIMARY KEY,
    dataset         text NOT NULL,
    matcher_version integer NOT NULL,
    matched         boolean NOT NULL,
    tried_at        bigint NOT NULL
);
