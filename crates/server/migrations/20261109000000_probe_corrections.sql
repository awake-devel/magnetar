-- Track corrections (ARCHITECTURE.md D128, docs/PROBES.md §12): what the user checked in VLC,
-- stored apart from the detections (`probe_files`) and never written by a probe. Anchored to
-- the current tracks on read (`probe_fix::anchor`). References `dht_torrents`, not
-- `probe_attrs`: a probe deleted after a worker token leak (docs/WORKERS.md §7) and probed
-- again gets its corrections back. No `search_changes` trigger: a correction write recomputes
-- `probe_attrs` in the same transaction, whose D122 triggers log the change.
SET LOCAL lock_timeout = '10s';

CREATE TABLE probe_corrections (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    infohash    text COLLATE "C" NOT NULL REFERENCES dht_torrents (infohash) ON DELETE CASCADE,
    file_i      integer NOT NULL CHECK (file_i >= 0),
    track       text NOT NULL CHECK (track IN ('audio', 'sub')),
    pos         smallint CHECK (pos >= 0),            -- NULL: a track the probe missed
    removed     boolean NOT NULL DEFAULT false,       -- "This track doesn't exist"
    set_lang    boolean NOT NULL DEFAULT false,
    lang        text CHECK (lang ~ '^[a-z]{3}$'),     -- NULL with set_lang: no language
    set_kinds   boolean NOT NULL DEFAULT false,
    kinds       text[] NOT NULL DEFAULT '{}',
    set_name    boolean NOT NULL DEFAULT false,
    name        text CHECK (octet_length(name) BETWEEN 1 AND 64),
    detected    jsonb NOT NULL,                       -- snapshot at correction time (§1)
    device      text NOT NULL DEFAULT 'api' CHECK (octet_length(device) BETWEEN 1 AND 32),
    created_at  bigint NOT NULL,
    updated_at  bigint NOT NULL,
    CHECK (set_lang OR lang IS NULL),
    CHECK (set_kinds OR kinds = '{}'),
    CHECK (set_name OR name IS NULL),
    CHECK (NOT removed OR (pos IS NOT NULL AND NOT set_lang AND NOT set_kinds AND NOT set_name)),
    CHECK (pos IS NOT NULL OR set_lang OR set_kinds OR set_name)
);
CREATE INDEX probe_corrections_infohash ON probe_corrections (infohash);
