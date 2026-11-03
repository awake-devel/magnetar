-- Search filters on header-probe results (ARCHITECTURE.md D122, docs/FILTERS.md "Probe
-- filters"). The facets go to the search index (index SCHEMA_VERSION 5 reads
-- `probe_attrs.tags`), not to a GIN index here: with `ORDER BY … LIMIT` a `tags` GIN collapses
-- on rare tag mixes (D117), and the index's one-pass collector counts facets disjunctively.
--
-- 1. A probe written, replaced or removed (directly or by the `dht_torrents` cascade) re-indexes
--    its torrent through the change log, as the other search inputs do (D43).
-- 2. The resolution class now follows the width too (`probes::resolution`): a 2.39:1 UHD film
--    (3840×1606) is `r:2160p`, not `r:1080p`. Stored rows get their `r:` tag recomputed from the
--    main probed file (largest size, then lowest index, as `probes::summary` picks it) in
--    `probe_files`; rows whose main file has no video height keep their tags.
-- Re-runnable (`OR REPLACE`): `probe_search::stored_rows_get_the_width_aware_class` runs it again.

SET LOCAL lock_timeout = '10s';

CREATE OR REPLACE TRIGGER search_changes_ins AFTER INSERT ON probe_attrs
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE OR REPLACE TRIGGER search_changes_upd AFTER UPDATE ON probe_attrs
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE OR REPLACE TRIGGER search_changes_del AFTER DELETE ON probe_attrs
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_old_rows();

WITH main AS (
    SELECT f.infohash,
           (m.e -> 'video' -> 'video' -> 0 ->> 'width')::bigint AS w,
           (m.e -> 'video' -> 'video' -> 0 ->> 'height')::bigint AS h
    FROM probe_files f
    CROSS JOIN LATERAL (
        SELECT e FROM jsonb_array_elements(f.files) e
        ORDER BY (e ->> 'size')::bigint DESC, (e ->> 'i')::bigint ASC
        LIMIT 1) m
), cls AS (
    SELECT infohash,
           'r:' || CASE
               WHEN greatest(h, coalesce(w * 9 / 16, 0)) >= 2000 THEN '2160p'
               WHEN greatest(h, coalesce(w * 9 / 16, 0)) >= 1000 THEN '1080p'
               WHEN greatest(h, coalesce(w * 9 / 16, 0)) >= 700 THEN '720p'
               WHEN greatest(h, coalesce(w * 9 / 16, 0)) >= 560 THEN '576p'
               WHEN greatest(h, coalesce(w * 9 / 16, 0)) >= 470 THEN '480p'
               ELSE 'sd' END AS tag
    FROM main WHERE h IS NOT NULL
)
UPDATE probe_attrs p
   SET tags = array(SELECT t FROM unnest(p.tags) t WHERE t NOT LIKE 'r:%') || c.tag
  FROM cls c
 WHERE c.infohash = p.infohash AND NOT (c.tag = ANY (p.tags));
