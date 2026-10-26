#!/usr/bin/env bash
# Benchmark database for the search index (ARCHITECTURE.md D43, docs/SEARCH.md).
#
#   scripts/bench-search-db.sh [copies]      default 7: ~3.2 M infohashes from 462 k
#
# Copies the torrents (plus liveness and media links) of a running Magnetar database with a
# read-only COPY ... TO STDOUT, then inflates them: copy k > 0 of every torrent gets a derived
# infohash and event id, a varied title (resolution, codec, language, source, group, year),
# a shifted date and size. The release attributes (D44) are then parsed with
# `magnetar-server attrs` (its JSON report shows how long that takes). The source database is
# only read.
#
# Environment:
#   SRC_PSQL   psql command for the source  (default: docker exec -i magnetar-db-1 psql -U magnetar -d magnetar)
#   DST_PG     container of the target server (default: magnetar-pg, postgres:18 on 127.0.0.1:54329)
#   DST_DB     target database, dropped and recreated (default: mag_night_bench)
#   BIN        magnetar-server binary used for the migrations (default: target/release/magnetar-server)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
copies=${1:-7}
SRC_PSQL=${SRC_PSQL:-docker exec -i magnetar-db-1 psql -U magnetar -d magnetar}
DST_PG=${DST_PG:-magnetar-pg}
DST_DB=${DST_DB:-mag_night_bench}
BIN=${BIN:-$root/target/release/magnetar-server}
port=$(docker port "$DST_PG" 5432/tcp | head -1 | sed 's/.*://')
url="postgres://magnetar:magnetar@127.0.0.1:$port/$DST_DB"

case "$DST_DB" in
  mag_night_* | mag_bench_*) ;;
  *) echo "refusing to drop $DST_DB: use a mag_night_* or mag_bench_* name" >&2; exit 2 ;;
esac

dst() { docker exec -i "$DST_PG" psql -v ON_ERROR_STOP=1 -q -U magnetar -d "$DST_DB" "$@"; }
step() { printf '\n==> %s (%s)\n' "$*" "$(date +%T)"; }

step "fresh database $DST_DB"
docker exec "$DST_PG" dropdb -U magnetar --if-exists "$DST_DB"
docker exec "$DST_PG" createdb -U magnetar "$DST_DB"
DATABASE_URL=$url "$BIN" migrate

step "staging tables"
dst <<'SQL'
CREATE UNLOGGED TABLE src_torrents (
    event_id text COLLATE "C", pubkey text COLLATE "C", created_at bigint, infohash text COLLATE "C",
    title text, description text, files jsonb, file_count integer, files_text text,
    total_size bigint, trackers text[], category text, category_path text[], hashtags text[],
    external_ids jsonb, source_url text, published_at bigint, pow_bits smallint,
    pow_committed smallint, trust smallint);
CREATE UNLOGGED TABLE src_liveness (LIKE liveness);
CREATE UNLOGGED TABLE src_torrent_media (LIKE torrent_media);
SQL

step "copy from the source (read-only)"
$SRC_PSQL -c "\copy (SELECT event_id, pubkey, created_at, infohash, title, description, files, file_count, files_text, total_size, trackers, category, category_path, hashtags, external_ids, source_url, published_at, pow_bits, pow_committed, trust FROM torrents) TO STDOUT" \
  | dst -c "\copy src_torrents FROM STDIN"
$SRC_PSQL -c "\copy (SELECT * FROM liveness) TO STDOUT" | dst -c "\copy src_liveness FROM STDIN"
$SRC_PSQL -c "\copy (SELECT * FROM media) TO STDOUT" | dst -c "\copy media FROM STDIN"
$SRC_PSQL -c "\copy (SELECT * FROM torrent_media) TO STDOUT" | dst -c "\copy src_torrent_media FROM STDIN"
dst -c "SELECT setval('media_id_seq', (SELECT max(id) FROM media))" >/dev/null
dst -t -c "SELECT count(*) || ' torrents, ' || count(DISTINCT infohash) || ' infohashes' FROM src_torrents"

step "inflate x$copies (one connection per copy)"
dst <<'SQL'
DROP INDEX torrents_search_tsv;
DROP INDEX torrents_title_trgm;
-- The index under test is built from scratch: no change log for the bulk load.
ALTER TABLE torrents DISABLE TRIGGER search_changes_ins;
ALTER TABLE liveness DISABLE TRIGGER search_changes_ins;
ALTER TABLE torrent_media DISABLE TRIGGER search_changes_ins;
CREATE FUNCTION bench_ih(ih text, k int) RETURNS text IMMUTABLE LANGUAGE sql AS
  $$ SELECT CASE WHEN k = 0 THEN ih ELSE left(md5(ih || ':' || k), 32) || left(md5(k || ':' || ih), 8) END $$;
CREATE FUNCTION bench_id(id text, k int) RETURNS text IMMUTABLE LANGUAGE sql AS
  $$ SELECT CASE WHEN k = 0 THEN id ELSE md5(id || ':' || k) || md5(k || ':' || id) END $$;
CREATE FUNCTION bench_pick(a text[], seed text) RETURNS text IMMUTABLE LANGUAGE sql AS
  $$ SELECT a[1 + abs(hashtext(seed)) % array_length(a, 1)] $$;
-- Release-name style variations of a title, one per copy number.
CREATE FUNCTION bench_title(t text, k int, seed text) RETURNS text IMMUTABLE LANGUAGE sql AS $$
  SELECT CASE k % 7
    WHEN 0 THEN t
    WHEN 1 THEN CASE WHEN t ~* '1080p' THEN regexp_replace(t, '1080p', '720p', 'i') ELSE t || ' 1080p' END
    WHEN 2 THEN regexp_replace(t, 'x264|h\.?264', 'x265', 'i') || '-REPACK'
    WHEN 3 THEN t || ' ' || bench_pick(ARRAY['MULTI','FRENCH','VOSTFR','TRUEFRENCH','GERMAN','iTALiAN','SPANiSH','VOSTA'], seed)
    WHEN 4 THEN regexp_replace(t, '[ ._-]+', ' ', 'g') || ' ' || bench_pick(ARRAY['WEB-DL','BluRay','HDTV','WEBRip','DVDRip','REMUX','HDR10','DV'], seed)
    WHEN 5 THEN t || '-' || bench_pick(ARRAY['FGT','NTb','SPARKS','YIFY','GalaxyRG','EDITH','QxR','Tigole','FLUX','ZeroTwo'], seed)
    ELSE t || ' (' || (1990 + abs(hashtext(seed)) % 36) || ')'
  END $$;
SQL

pids=()
for k in $(seq 0 $((copies - 1))); do
  dst <<SQL &
INSERT INTO events (id, pubkey, kind, created_at, raw, received_at)
  SELECT bench_id(event_id, $k), pubkey, 2003,
         greatest(0, created_at - CASE WHEN $k = 0 THEN 0 ELSE abs(hashtext(event_id || $k)) % 31536000 END),
         '{}', 0
  FROM src_torrents;
INSERT INTO torrents (event_id, pubkey, created_at, infohash, title, description, files, file_count,
    files_text, total_size, trackers, category, category_path, hashtags, external_ids, source_url,
    published_at, pow_bits, pow_committed, trust)
  SELECT bench_id(event_id, $k), pubkey,
         greatest(0, created_at - CASE WHEN $k = 0 THEN 0 ELSE abs(hashtext(event_id || $k)) % 31536000 END),
         bench_ih(infohash, $k), bench_title(title, $k, event_id || $k),
         CASE WHEN $k = 0 THEN description ELSE left(description, 2000) END,
         files, file_count, files_text,
         CASE WHEN total_size IS NULL OR $k = 0 THEN total_size
              ELSE total_size + abs(hashtext(event_id || $k)) % 100000000 END,
         trackers, category, category_path, hashtags, external_ids, source_url, published_at,
         pow_bits, pow_committed, trust
  FROM src_torrents;
INSERT INTO liveness (infohash, seeders, leechers, completed, dht_peers_est, source, trackers_ok,
    trackers_failed, checked_at, expires_at)
  SELECT bench_ih(infohash, $k), seeders, leechers, completed, dht_peers_est, source, trackers_ok,
         trackers_failed, checked_at, extract(epoch FROM now())::bigint + 30 * 86400
  FROM src_liveness;
INSERT INTO torrent_media (infohash, media_id, season, episode, method, confidence, matcher_version, matched_at)
  SELECT bench_ih(infohash, $k), media_id, season, episode, method, confidence, matcher_version, matched_at
  FROM src_torrent_media;
SQL
  pids+=($!)
done
for p in "${pids[@]}"; do wait "$p"; done

step "release attributes (D44), parsed by the server binary"
dst -c "ALTER TABLE torrent_attrs DISABLE TRIGGER search_changes_ins"
DATABASE_URL=$url "$BIN" attrs
dst -c "ALTER TABLE torrent_attrs ENABLE TRIGGER search_changes_ins"

step "indexes back, analyze"
dst <<'SQL'
ALTER TABLE torrents ENABLE TRIGGER search_changes_ins;
ALTER TABLE liveness ENABLE TRIGGER search_changes_ins;
ALTER TABLE torrent_media ENABLE TRIGGER search_changes_ins;
DROP TABLE src_torrents, src_liveness, src_torrent_media;
DROP FUNCTION bench_title, bench_pick, bench_id, bench_ih;
SET maintenance_work_mem = '1GB';
CREATE INDEX torrents_search_tsv ON torrents USING GIN (search_tsv);
CREATE INDEX torrents_title_trgm ON torrents USING GIN (lower(title) gin_trgm_ops);
VACUUM ANALYZE;
SQL
dst -t -c "SELECT (SELECT count(*) FROM torrents) || ' torrents, ' || (SELECT value FROM meta WHERE key = 'infohash_count') || ' infohashes, ' || pg_size_pretty(pg_database_size(current_database()))"
echo "MAGNETAR_BENCH_DATABASE_URL=$url"
