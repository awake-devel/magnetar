-- Parsed release attributes per infohash (ARCHITECTURE.md D44, docs/FILTERS.md): resolution,
-- source, codecs, HDR, audio, languages, subtitles, group, season/episode, year, editions,
-- music/book formats. `attrs` is `core::release::ReleaseAttrs` as JSON; the columns the SQL
-- search filters on are generated from it, so writers only ever set `attrs`.
--
-- Written by `PgStore::put_torrent` for new infohashes (first event wins: same files, and the
-- title differences between publishers are minor) and by the backfill (`attrs::backfill`),
-- which re-parses rows whose `parser_version` is older than `release::PARSER_VERSION`.

-- jsonb array of strings → text[] (IMMUTABLE so generated columns can use it).
CREATE FUNCTION jsonb_text_array(j jsonb) RETURNS text[] LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT coalesce(array_agg(x ORDER BY ord), '{}')
    FROM jsonb_array_elements_text(CASE WHEN jsonb_typeof(j) = 'array' THEN j ELSE '[]' END)
         WITH ORDINALITY AS t(x, ord)
$$;

CREATE TABLE torrent_attrs (
    infohash       text COLLATE "C" PRIMARY KEY
                   REFERENCES torrent_infohashes (infohash) ON DELETE CASCADE,
    parser_version smallint NOT NULL,
    attrs          jsonb NOT NULL,
    resolution     text GENERATED ALWAYS AS (attrs ->> 'resolution') STORED,
    source         text GENERATED ALWAYS AS (attrs ->> 'source') STORED,
    video_codec    text GENERATED ALWAYS AS (attrs ->> 'video_codec') STORED,
    hdr            text[] GENERATED ALWAYS AS (jsonb_text_array(attrs -> 'hdr')) STORED,
    audio          text[] GENERATED ALWAYS AS (jsonb_text_array(attrs -> 'audio')) STORED,
    languages      text[] GENERATED ALWAYS AS (jsonb_text_array(attrs -> 'languages')) STORED,
    subtitles      text[] GENERATED ALWAYS AS (jsonb_text_array(attrs -> 'subtitles')) STORED,
    year           integer GENERATED ALWAYS AS ((attrs ->> 'year')::integer) STORED
);
-- The backfill looks for rows of an older parser.
CREATE INDEX torrent_attrs_version ON torrent_attrs (parser_version);

-- Attribute changes reach the search index like every other change (D43).
CREATE TRIGGER search_changes_ins AFTER INSERT ON torrent_attrs
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
-- A re-parse that comes out the same (only `parser_version` moves) is not a change.
CREATE FUNCTION search_changes_attrs() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM search_changes_mark(ARRAY(
        SELECT n.infohash FROM new_rows n JOIN old_rows o ON o.infohash = n.infohash
        WHERE n.attrs IS DISTINCT FROM o.attrs));
    RETURN NULL;
END
$$;
CREATE TRIGGER search_changes_upd AFTER UPDATE ON torrent_attrs
    REFERENCING NEW TABLE AS new_rows OLD TABLE AS old_rows
    FOR EACH STATEMENT EXECUTE FUNCTION search_changes_attrs();
CREATE TRIGGER search_changes_del AFTER DELETE ON torrent_attrs
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_old_rows();
