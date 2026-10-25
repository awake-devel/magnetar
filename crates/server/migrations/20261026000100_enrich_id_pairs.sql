-- D40 (review): private to the IMDb enricher. For each torrent that links an IMDb id, the other
-- external ids it carries, with that IMDb id. Description links of another kind are attached to
-- the IMDb row only when the evidence agrees: an id seen next to two different IMDb ids is a
-- template link (YGG's themoviedb.org/movie/324544 under other films' releases) and is never
-- attached; one seen next to a single IMDb id is, when it is deliberate (an i tag) or the
-- title of the TMDB-only row holding it is one of the IMDb title's names.
CREATE TABLE enrich_id_pairs (
    kind     smallint NOT NULL,             -- 0 tmdb_movie, 1 tmdb_tv, 2 tvdb, 3 mal, 4 anilist, 5 anidb
    value    integer NOT NULL,
    infohash text COLLATE "C" NOT NULL,
    imdb     integer NOT NULL,              -- tconst without "tt"
    PRIMARY KEY (kind, value, infohash)
);
