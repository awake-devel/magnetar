-- File kinds of every probed file (ARCHITECTURE.md D137, D138; docs/FILTERS.md "File kind").
-- `probes::summary_of` now tags each probed file, verified or not, with `fk:{label}` (the sniff
-- label, `fk:exe.pe.dotnet`) and `fg:{group}` (the label before its first `.`, `fg:exe`); the
-- search index (SCHEMA_VERSION 7) reads them as the `probe_file_kind` facet. Stored rows get
-- the same tags from `probe_files.files` here, without a re-probe: corrections (D128) change
-- tracks only, never a file's label, so the stored labels are the effective ones.
--
-- One statement inside the migration transaction, run before the server serves anything (no
-- worker upload competes for the rows); `lock_timeout` makes it fail fast instead of waiting.
-- Rows whose `fk:`/`fg:` set is already right are skipped, so a second run updates nothing
-- (`it::probe_search::file_kind_migration_matches_summary_of` runs it twice and compares each
-- row with `summary_of`). The statement trigger of 20261108220000 queues the updated rows for
-- re-indexing; the v7 rebuild covers them anyway (D138).

SET LOCAL lock_timeout = '10s';

WITH k AS (
    SELECT f.infohash, array_agg(DISTINCT v.tag) AS tags
    FROM probe_files f
    CROSS JOIN LATERAL jsonb_array_elements(f.files) e
    CROSS JOIN LATERAL (VALUES ('fk:' || (e ->> 'label')),
                               ('fg:' || split_part(e ->> 'label', '.', 1))) v(tag)
    WHERE coalesce(e ->> 'label', '') <> ''
    GROUP BY f.infohash
)
UPDATE probe_attrs p
   SET tags = array(SELECT t FROM unnest(p.tags) t WHERE t NOT LIKE 'fg:%' AND t NOT LIKE 'fk:%')
              || k.tags
  FROM k
 WHERE k.infohash = p.infohash
   AND NOT (p.tags @> k.tags
            AND k.tags @> array(SELECT t FROM unnest(p.tags) t
                                WHERE t LIKE 'fg:%' OR t LIKE 'fk:%'));
