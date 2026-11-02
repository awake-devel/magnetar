-- Header probes (ARCHITECTURE.md D117, docs/PROBES.md): what a crawl worker's probe read from
-- a torrent's files (real file type, console ids, video / audio quality), uploaded with the
-- info dict it fetched and checked by the server against its own file list.
--
-- Only for DHT rows (the dict is the trust anchor, D113): `probe_attrs` references
-- `dht_torrents`, so deleting the row (by `supplied_by` after a token leak, or any other way)
-- removes its probe. Written only for rows the server classified as not adult and did not
-- block. Only `ok` and `partial` results are stored; failures are worker counts.
--
-- `tags` holds facets for a later search filter (`k:video`, `r:2160p`, `v:hevc`,
-- `h:dolby_vision`, `a:fre`, `p:ps5`, ...). No GIN index on it yet: with `ORDER BY probed_at
-- LIMIT` it collapses to a full backward scan on rare tag mixes (check1 #3); the facets go to
-- the search index when filters land. No `search_changes` trigger for the same reason.

SET LOCAL lock_timeout = '10s';

CREATE TABLE probe_attrs (
    infohash       text COLLATE "C" PRIMARY KEY
                   REFERENCES dht_torrents (infohash) ON DELETE CASCADE,
    probe_version  smallint NOT NULL,
    probed_at      bigint NOT NULL,
    supplied_by    text,                                   -- the worker that probed it
    status         text NOT NULL CHECK (status IN ('ok', 'partial')),
    files_probed   smallint NOT NULL,
    files_verified smallint NOT NULL,
    bytes_in       integer NOT NULL,
    -- Summary of the main payload (the largest probed file).
    kind           text,                                   -- label family: video, game, ...
    platform       text,
    title_id       text,
    container      text,
    vcodec         text,
    height         integer,
    hdr            text[] NOT NULL DEFAULT '{}',
    bit_depth      smallint,
    duration_ms    bigint,
    audio_codecs   text[] NOT NULL DEFAULT '{}',
    audio_langs    text[] NOT NULL DEFAULT '{}',           -- ISO 639-2/B
    sub_langs      text[] NOT NULL DEFAULT '{}',
    flags          text[] NOT NULL DEFAULT '{}',
    tags           text[] NOT NULL DEFAULT '{}'
);
CREATE INDEX probe_attrs_probed_at ON probe_attrs (probed_at);

-- Per-file results ([ProbeFile], at most 16 per torrent, at most 64 KiB), kept apart from the
-- narrow summary so summary scans stay small (storage report S4).
CREATE TABLE probe_files (
    infohash text COLLATE "C" PRIMARY KEY REFERENCES probe_attrs (infohash) ON DELETE CASCADE,
    files    jsonb NOT NULL
);
