-- TMDB decoration cache (D42, docs/ENRICHMENT-TMDB.md). Owned by enrich/tmdb.
-- Needs `media` (enrich/imdb migration). Rows are refreshed or purged within 6 months
-- (TMDB API terms §1.C). source: 'tmdb' (details per lang), 'tmdb_season:<n>' (episode names per
-- lang), 'tmdb_find' (imdb -> tmdb lookup, lang ''), 'tmdb_image' (cached poster, lang '').
CREATE TABLE media_extra (
    media_id   bigint REFERENCES media (id) ON DELETE CASCADE,
    source     text,
    lang       text,
    json       jsonb NOT NULL,
    fetched_at bigint NOT NULL,
    PRIMARY KEY (media_id, source, lang)
);
CREATE INDEX media_extra_fetched ON media_extra (fetched_at);
